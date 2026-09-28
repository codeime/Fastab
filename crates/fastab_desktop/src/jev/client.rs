use std::fmt;
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::{ACCEPT, ACCEPT_ENCODING, AUTHORIZATION, CONTENT_ENCODING, CONTENT_TYPE, HeaderMap, HeaderValue};

use super::config::ResolvedProfile;
use super::policy::{MAX_RESPONSE_BYTES, REQUEST_TIMEOUT, cooldown};
use super::types::{Candidate, Recommendation, RecommendationInput, decode_response, encode_request};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientErrorKind {
    InvalidRequest,
    InvalidCredential,
    Authentication,
    PaymentRequired,
    RateLimited,
    Overloaded,
    Rejected,
    Transport,
    Timeout,
    InvalidResponse,
    ResponseTooLarge,
}

/// Safe to display/log: no URL, header, body, credential, or upstream error text.
#[derive(Debug, Clone)]
pub struct ClientError {
    pub kind: ClientErrorKind,
    pub status: Option<u16>,
    pub cooldown: Option<Duration>,
}

impl ClientError {
    fn new(kind: ClientErrorKind) -> Self {
        Self {
            kind,
            status: None,
            cooldown: None,
        }
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "System One request failed: {:?}", self.kind)
    }
}

impl std::error::Error for ClientError {}

#[derive(Clone)]
pub struct JevClient {
    http: reqwest::Client,
}

impl JevClient {
    /// Construction starts no Tokio worker or network call.
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .https_only(true)
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .timeout(REQUEST_TIMEOUT)
                .pool_max_idle_per_host(0)
                // Request identity encoding so the body bound applies before
                // any decompression allocation as well as to the decoded JSON.
                .no_gzip()
                .no_brotli()
                .no_deflate()
                .no_zstd()
                .build()?,
        })
    }

    /// Send one fixed, public example through the real System One request path.
    /// This can validate a settings draft before AI is enabled. The caller must
    /// supply a validated profile and the credential being tested; this method
    /// does not load or persist either one. A successful HTTP status alone is
    /// insufficient: the response must also satisfy the System One contract.
    pub async fn probe(&self, profile: &ResolvedProfile, key: &[u8]) -> Result<(), ClientError> {
        let input = probe_input();
        self.recommend(profile, key, &input).await.map(|_| ())
    }

    /// Dropping this future cancels this attempt. The caller owns single-flight,
    /// debouncing, per-minute admission, snapshot identity and error cooldowns.
    /// The key is borrowed for this request and never installed as a default header.
    pub async fn recommend(
        &self,
        profile: &ResolvedProfile,
        key: &[u8],
        input: &RecommendationInput,
    ) -> Result<Recommendation, ClientError> {
        if !profile.integrity_is_valid() {
            return Err(ClientError::new(ClientErrorKind::InvalidRequest));
        }
        let body =
            encode_request(profile, input).map_err(|_error| ClientError::new(ClientErrorKind::InvalidRequest))?;
        if key.is_empty() || key.len() > 4096 || !key.iter().all(|byte| byte.is_ascii_graphic()) {
            return Err(ClientError::new(ClientErrorKind::InvalidCredential));
        }
        let mut bearer = Vec::with_capacity(7 + key.len());
        bearer.extend_from_slice(b"Bearer ");
        bearer.extend_from_slice(key);
        let mut authorization =
            HeaderValue::from_bytes(&bearer).map_err(|_error| ClientError::new(ClientErrorKind::InvalidCredential))?;
        authorization.set_sensitive(true);
        drop(bearer);
        match tokio::time::timeout(REQUEST_TIMEOUT, self.attempt(profile, authorization, body, input)).await {
            Ok(result) => result,
            Err(_elapsed) => Err(ClientError::new(ClientErrorKind::Timeout)),
        }
    }

    async fn attempt(
        &self,
        profile: &ResolvedProfile,
        authorization: HeaderValue,
        body: Vec<u8>,
        input: &RecommendationInput,
    ) -> Result<Recommendation, ClientError> {
        let mut response = self
            .http
            .post(profile.endpoint.clone())
            .header(AUTHORIZATION, authorization)
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .header(ACCEPT_ENCODING, "identity")
            .body(body)
            .send()
            .await
            .map_err(transport_error)?;
        let status = response.status();
        if !status.is_success() {
            // Do not read an error body: it can be huge or contain echoed secrets.
            return Err(status_error(status, response.headers()));
        }
        if response
            .headers()
            .get(CONTENT_ENCODING)
            .is_some_and(|value| value != "identity")
        {
            return Err(ClientError::new(ClientErrorKind::InvalidResponse));
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok());
        if !content_type.is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"))
        }) {
            return Err(ClientError::new(ClientErrorKind::InvalidResponse));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(ClientError::new(ClientErrorKind::ResponseTooLarge));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
            if chunk.len() > MAX_RESPONSE_BYTES - bytes.len() {
                return Err(ClientError::new(ClientErrorKind::ResponseTooLarge));
            }
            bytes.extend_from_slice(&chunk);
        }
        decode_response(&bytes, profile, input).map_err(|_error| ClientError::new(ClientErrorKind::InvalidResponse))
    }
}

fn probe_input() -> RecommendationInput {
    RecommendationInput {
        shell: "zsh".into(),
        command_path: vec!["git".into()],
        token_prefix: "ch".into(),
        current_input: "git ch".into(),
        terminal_context: Default::default(),
        candidates: vec![
            Candidate {
                id: "checkout".into(),
                name: "checkout".into(),
                description: "Switch to a branch in an example repository".into(),
            },
            Candidate {
                id: "cherry_pick".into(),
                name: "cherry-pick".into(),
                description: "Apply an example commit to the current branch".into(),
            },
        ],
    }
}

fn status_error(status: StatusCode, headers: &HeaderMap) -> ClientError {
    let kind = match status.as_u16() {
        401 => ClientErrorKind::Authentication,
        402 => ClientErrorKind::PaymentRequired,
        429 => ClientErrorKind::RateLimited,
        529 | 503 => ClientErrorKind::Overloaded,
        _ => ClientErrorKind::Rejected,
    };
    let delay = matches!(kind, ClientErrorKind::RateLimited | ClientErrorKind::Overloaded).then(|| cooldown(headers));
    ClientError {
        kind,
        status: Some(status.as_u16()),
        cooldown: delay,
    }
}

fn transport_error(error: reqwest::Error) -> ClientError {
    ClientError::new(if error.is_timeout() {
        ClientErrorKind::Timeout
    } else {
        ClientErrorKind::Transport
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::{Value, json};

    use super::*;
    use crate::jev::config::Profile;
    use crate::jev::policy::{DATA_POLICY_VERSION, MAX_REQUEST_BYTES};

    #[test]
    fn probe_fixture_is_public_bounded_and_accepted_by_the_response_contract() {
        let mut draft = Profile::typesafe();
        draft.data_policy_version = DATA_POLICY_VERSION;
        let profile = draft.validate().unwrap();
        let input = probe_input();
        let body = encode_request(&profile, &input).unwrap();
        assert!(body.len() <= MAX_REQUEST_BYTES);

        let request: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(request["model"], "jev-1.13.0");
        assert_eq!(
            request["state"],
            json!({
                "shell": "zsh",
                "command_path": ["git"],
                "token_prefix": "ch",
                "current_input": "git ch",
                "recent_commands": []
            })
        );
        let top_level: BTreeSet<_> = request.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(top_level, BTreeSet::from(["model", "questions", "state"]));
        let criteria = request["questions"]["recommendation"]["criteria"].as_object().unwrap();
        let ids: BTreeSet<_> = criteria.keys().map(String::as_str).collect();
        assert_eq!(ids, BTreeSet::from(["checkout", "cherry_pick", "keep_local"]));
        assert_eq!(criteria["checkout"]["name"], "checkout");
        assert_eq!(
            criteria["checkout"]["description"],
            "Switch to a branch in an example repository"
        );
        assert_eq!(criteria["cherry_pick"]["name"], "cherry-pick");
        assert_eq!(
            criteria["cherry_pick"]["description"],
            "Apply an example commit to the current branch"
        );

        let response = json!({
            "model": "jev-1.13.0",
            "answers": {
                "recommendation": {
                    "type": "choice",
                    "choice": "checkout",
                    "probabilities": {"checkout": 0.7, "cherry_pick": 0.2, "keep_local": 0.1},
                    "confidence": 0.7
                }
            },
            "usage": {"input_tokens": 1, "output_tokens": 1}
        });
        let response = serde_json::to_vec(&response).unwrap();
        assert_eq!(decode_response(&response, &profile, &input).unwrap().choice, "checkout");
    }

    #[test]
    fn recommendations_encode_terminal_context_with_bounded_history() {
        let mut draft = Profile::typesafe();
        draft.data_policy_version = DATA_POLICY_VERSION;
        let profile = draft.validate().unwrap();
        let mut input = probe_input();
        input.terminal_context.current_branch = Some("feat/example".into());
        input.terminal_context.recent_commands = vec!["git status".into(), "git diff".into()];

        let body = encode_request(&profile, &input).unwrap();
        let request: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(request["state"]["current_input"], "git ch");
        assert_eq!(request["state"]["current_branch"], "feat/example");
        assert_eq!(request["state"]["recent_commands"], json!(["git status", "git diff"]));
        assert!(request["state"].get("cwd").is_none());
        assert!(request["state"].get("environment").is_none());

        input.terminal_context.recent_commands = vec!["x".repeat(super::super::context::MAX_COMMAND_BYTES + 1)];
        assert!(encode_request(&profile, &input).is_err());
        input.terminal_context.recent_commands =
            vec!["git status".into(); super::super::context::MAX_HISTORY_COMMANDS + 1];
        assert!(encode_request(&profile, &input).is_err());
        input.terminal_context.recent_commands = vec!["x".repeat(512); 5];
        assert!(encode_request(&profile, &input).is_err());
    }

    #[tokio::test]
    async fn probe_reuses_credential_and_endpoint_integrity_guards() {
        let mut draft = Profile::typesafe();
        draft.data_policy_version = DATA_POLICY_VERSION;
        let profile = draft.validate().unwrap();
        let client = JevClient::new().unwrap();

        let error = client.probe(&profile, b"invalid key with spaces").await.unwrap_err();
        assert_eq!(error.kind, ClientErrorKind::InvalidCredential);

        let mut altered = profile.clone();
        altered.endpoint.set_path("/v1/unreviewed");
        let error = client.probe(&altered, b"synthetic-test-key").await.unwrap_err();
        assert_eq!(error.kind, ClientErrorKind::InvalidRequest);
    }

    #[test]
    fn status_errors_distinguish_key_billing_rate_limit_and_server_failures() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after-ms", HeaderValue::from_static("2500"));
        for (status, kind, delay) in [
            (401, ClientErrorKind::Authentication, None),
            (402, ClientErrorKind::PaymentRequired, None),
            (403, ClientErrorKind::Rejected, None),
            (429, ClientErrorKind::RateLimited, Some(Duration::from_millis(2500))),
            (503, ClientErrorKind::Overloaded, Some(Duration::from_millis(2500))),
            (529, ClientErrorKind::Overloaded, Some(Duration::from_millis(2500))),
            (302, ClientErrorKind::Rejected, None),
        ] {
            let error = status_error(StatusCode::from_u16(status).unwrap(), &headers);
            assert_eq!(error.kind, kind, "status {status}");
            assert_eq!(error.status, Some(status), "status {status}");
            assert_eq!(error.cooldown, delay, "status {status}");
        }
    }
}
