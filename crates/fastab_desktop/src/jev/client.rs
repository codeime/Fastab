use std::fmt;
use std::time::Duration;

use reqwest::header::{ACCEPT, ACCEPT_ENCODING, AUTHORIZATION, CONTENT_ENCODING, CONTENT_TYPE, HeaderValue};

use super::config::ResolvedProfile;
use super::policy::{MAX_RESPONSE_BYTES, REQUEST_TIMEOUT, cooldown};
use super::types::{Recommendation, RecommendationInput, decode_response, encode_request};

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
        Self { kind, status: None, cooldown: None }
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
    /// Construct only when AI is enabled; no Tokio worker or network call is started.
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
        let body = encode_request(profile, input).map_err(|_error| ClientError::new(ClientErrorKind::InvalidRequest))?;
        if key.is_empty() || key.len() > 4096 || !key.iter().all(|byte| byte.is_ascii_graphic()) {
            return Err(ClientError::new(ClientErrorKind::InvalidCredential));
        }
        let mut bearer = Vec::with_capacity(7 + key.len());
        bearer.extend_from_slice(b"Bearer ");
        bearer.extend_from_slice(key);
        let mut authorization = HeaderValue::from_bytes(&bearer)
            .map_err(|_error| ClientError::new(ClientErrorKind::InvalidCredential))?;
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
        let mut response = self.http.post(profile.endpoint.clone())
            .header(AUTHORIZATION, authorization)
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .header(ACCEPT_ENCODING, "identity")
            .body(body)
            .send().await.map_err(transport_error)?;
        let status = response.status();
        if !status.is_success() {
            let kind = match status.as_u16() {
                401 => ClientErrorKind::Authentication,
                402 => ClientErrorKind::PaymentRequired,
                429 => ClientErrorKind::RateLimited,
                529 | 503 => ClientErrorKind::Overloaded,
                _ => ClientErrorKind::Rejected,
            };
            let delay = matches!(kind, ClientErrorKind::RateLimited | ClientErrorKind::Overloaded)
                .then(|| cooldown(response.headers()));
            // Do not read an error body: it can be huge or contain echoed secrets.
            return Err(ClientError { kind, status: Some(status.as_u16()), cooldown: delay });
        }
        if response.headers().get(CONTENT_ENCODING).is_some_and(|value| value != "identity") {
            return Err(ClientError::new(ClientErrorKind::InvalidResponse));
        }
        let content_type = response.headers().get(CONTENT_TYPE).and_then(|value| value.to_str().ok());
        if !content_type.is_some_and(|value| value.split(';').next().is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"))) {
            return Err(ClientError::new(ClientErrorKind::InvalidResponse));
        }
        if response.content_length().is_some_and(|length| length > MAX_RESPONSE_BYTES as u64) {
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

fn transport_error(error: reqwest::Error) -> ClientError {
    ClientError::new(if error.is_timeout() { ClientErrorKind::Timeout } else { ClientErrorKind::Transport })
}
