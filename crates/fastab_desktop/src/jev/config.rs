use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use anyhow::{Result, bail, ensure};
use fastab_settings::{JsonStore, OldSettings};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use super::policy::{DATA_POLICY_VERSION, MAX_PROFILES};

const SETTINGS_KEY: &str = "autocomplete.ai.config";
const TYPESAFE_BASE: &str = "https://api.typesafe.ai";
const OPENROUTER_BASE: &str = "https://openrouter.ai/api";
const ENDPOINT_SUFFIX: &str = "/v1/systemone";
const DIRECT_RESPONSES: &[&str] = &["jev-1.13.0"];
const ROUTER_RESPONSES: &[&str] = &["typesafe/jev-1.13", "typesafe/jev-1.13-20260917"];

// The shared settings store mutates its global cache before writing the file.
// Serialize this wrapper's readers and writers so they cannot observe that
// intermediate state, and keep a failed write disabled even after a file-watch
// reload. Only a subsequent successful explicit save clears the failure latch.
static CONFIG_ACCESS: Mutex<()> = Mutex::new(());
static SAVE_FAILED: AtomicBool = AtomicBool::new(false);
static RUNTIME_REVISION: AtomicU64 = AtomicU64::new(0);

/// Invalidates cached credentials and requests before a settings write starts,
/// including same-value saves used when replacing a credential.
pub fn runtime_revision() -> u64 {
    RUNTIME_REVISION.load(Ordering::Acquire)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    TypeSafe,
    OpenRouter,
    CustomSystemOne,
}

impl Provider {
    fn credential_namespace(self) -> &'static str {
        match self {
            Self::TypeSafe => "typesafe",
            Self::OpenRouter => "openrouter",
            Self::CustomSystemOne => "custom",
        }
    }
}

/// Non-secret saved configuration. A cloned Profile is also a settings draft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub id: String,
    pub provider: Provider,
    pub base_url: String,
    pub model: String,
    pub data_policy_version: u32,
}

/// Only Profile::validate can construct a requestable configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProfile {
    pub endpoint: Url,
    pub credential_service: String,
    pub provider: Provider,
    pub model: String,
    allowed_responses: &'static [&'static str],
    validated_profile: Profile,
}

impl ResolvedProfile {
    pub fn accepts_response_model(&self, model: &str) -> bool {
        self.allowed_responses.contains(&model)
    }

    /// Public fields are convenient for UI display, but never confer permission
    /// to change a request destination after selecting its credential service.
    pub(crate) fn integrity_is_valid(&self) -> bool {
        self.validated_profile.validate().is_ok_and(|expected| expected == *self)
    }
}

impl Profile {
    pub fn typesafe() -> Self {
        Self {
            id: "typesafe".into(),
            provider: Provider::TypeSafe,
            base_url: TYPESAFE_BASE.into(),
            model: "jev-1.13.0".into(),
            data_policy_version: 0,
        }
    }

    pub fn openrouter() -> Self {
        Self {
            id: "openrouter".into(),
            provider: Provider::OpenRouter,
            base_url: OPENROUTER_BASE.into(),
            model: "typesafe/jev-1.13".into(),
            data_policy_version: 0,
        }
    }

    /// Validate the address even for an unconfirmed draft, so credentials can be
    /// saved/deleted without enabling inference or accepting an unknown model.
    pub fn credential_key(&self) -> Result<String> {
        let (_, endpoint) = self.normalized_address()?;
        let digest = Sha256::digest(endpoint.as_str().as_bytes());
        let mut hex = String::with_capacity(64);
        for byte in digest {
            hex.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
            hex.push(char::from(b"0123456789abcdef"[usize::from(byte & 15)]));
        }
        Ok(format!("app.fastab.ai.jev.v1.{}.{}", self.provider.credential_namespace(), hex))
    }

    pub fn validate(&self) -> Result<ResolvedProfile> {
        self.validate_identity()?;
        ensure!(self.data_policy_version == DATA_POLICY_VERSION, "AI data policy has not been confirmed");
        let (_, endpoint) = self.normalized_address()?;
        let allowed_responses = match (self.provider, self.model.as_str()) {
            (Provider::TypeSafe | Provider::CustomSystemOne, "jev-1.13.0") => DIRECT_RESPONSES,
            (Provider::OpenRouter | Provider::CustomSystemOne, "typesafe/jev-1.13" | "jev-1.13") => {
                ROUTER_RESPONSES
            },
            _ => bail!("System One model mapping has not been reviewed"),
        };
        Ok(ResolvedProfile {
            endpoint,
            credential_service: self.credential_key()?,
            provider: self.provider,
            model: self.model.clone(),
            allowed_responses,
            validated_profile: self.clone(),
        })
    }

    fn validate_identity(&self) -> Result<()> {
        ensure!(
            !self.id.is_empty() && self.id.len() <= 64
                && self.id.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte)),
            "Invalid AI profile ID"
        );
        ensure!(!self.model.is_empty() && self.model.len() <= 128, "Invalid AI model ID");
        Ok(())
    }

    fn normalized_address(&self) -> Result<(String, Url)> {
        let (base, endpoint) = normalize_base_url(&self.base_url)?;
        let required = match self.provider {
            Provider::TypeSafe => Some(TYPESAFE_BASE),
            Provider::OpenRouter => Some(OPENROUTER_BASE),
            Provider::CustomSystemOne => None,
        };
        if let Some(required) = required {
            ensure!(base == required, "Changing a preset address requires a separate custom profile");
        }
        Ok((base, endpoint))
    }
}

/// The supported custom URL subset intentionally excludes escaped path segments,
/// dot segments and duplicate slashes. No guessing of chat/completions endpoints.
pub fn normalize_base_url(value: &str) -> Result<(String, Url)> {
    ensure!(value.len() <= 2048 && !value.is_empty(), "Invalid System One base URL length");
    ensure!(!value.chars().any(|ch| ch.is_whitespace() || ch.is_control()), "Invalid whitespace in base URL");
    ensure!(!value.contains(['%', '\\']), "Encoded or backslash URL paths are unsupported");
    let (scheme, authority_and_path) = value.split_once("://")
        .ok_or_else(|| anyhow::anyhow!("Enter an absolute HTTPS base URL"))?;
    let authority = authority_and_path.split('/').next().unwrap_or_default();
    ensure!(scheme.eq_ignore_ascii_case("https") && !authority.is_empty(), "Enter an absolute HTTPS base URL");
    ensure!(!authority.contains('@'), "URL credentials are forbidden");
    let mut url = Url::parse(value).map_err(|error| anyhow::anyhow!("Invalid System One base URL: {error}"))?;
    ensure!(url.scheme() == "https" && url.host_str().is_some(), "System One requires an HTTPS host");
    ensure!(url.username().is_empty() && url.password().is_none(), "URL credentials are forbidden");
    ensure!(url.query().is_none() && url.fragment().is_none(), "URL query and fragment are forbidden");
    ensure!(url.port() != Some(0), "Invalid HTTPS port");
    // Check the unnormalized spelling too: URL parsing removes dot segments.
    let raw_path = value.split_once("://").and_then(|(_, rest)| rest.split_once('/')).map_or("", |(_, path)| path);
    ensure!(
        raw_path.split('/').all(|segment| segment != "." && segment != "..") && !raw_path.contains("//"),
        "Ambiguous URL path"
    );
    let path = url.path().trim_end_matches('/').to_owned();
    ensure!(
        path.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"/_-.~".contains(&byte)),
        "Unsupported URL path"
    );
    ensure!(
        !path.ends_with(ENDPOINT_SUFFIX) && !path.ends_with("/api/v1") && !path.ends_with("/chat/completions"),
        "Enter a base URL without an API endpoint or /api/v1 suffix"
    );
    let endpoint_path = format!("{path}{ENDPOINT_SUFFIX}");
    url.set_path(&path);
    let base = url.as_str().trim_end_matches('/').to_owned();
    url.set_path(&endpoint_path);
    Ok((base, url))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiConfig {
    pub enabled: bool,
    pub active_profile_id: Option<String>,
    pub profiles: Vec<Profile>,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            active_profile_id: Some("typesafe".into()),
            profiles: vec![Profile::typesafe(), Profile::openrouter()],
        }
    }
}

impl AiConfig {
    pub fn load() -> Result<Self> {
        let _guard = CONFIG_ACCESS.lock().unwrap_or_else(|error| error.into_inner());
        let value = fastab_settings::settings::get_value(SETTINGS_KEY)?;
        let mut config = match value {
            Some(value) => serde_json::from_value::<Self>(value)?,
            None => Self::default(),
        };
        if SAVE_FAILED.load(Ordering::Acquire) {
            config.enabled = false;
        }
        config.validate_storage()?;
        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        let _guard = CONFIG_ACCESS.lock().unwrap_or_else(|error| error.into_inner());
        self.validate_storage()?;
        let value = serde_json::to_value(self)?;
        RUNTIME_REVISION.fetch_add(1, Ordering::AcqRel);
        match fastab_settings::settings::set_value(SETTINGS_KEY, value) {
            Ok(()) => {
                SAVE_FAILED.store(false, Ordering::Release);
                Ok(())
            },
            Err(error) => {
                SAVE_FAILED.store(true, Ordering::Release);
                // Do not attempt another disk write here: even failure to open
                // or flush may already have changed the global cached value.
                // Preserve all profiles for credential cleanup while forcing
                // that cache off. There may be no global backend at all, in
                // which case the latch still makes every future load fail closed.
                let mut global = OldSettings::data_lock().write();
                if let Some(config) = global.as_mut()
                    .and_then(|map| map.get_mut(SETTINGS_KEY))
                    .and_then(serde_json::Value::as_object_mut)
                {
                    config.insert("enabled".into(), serde_json::Value::Bool(false));
                }
                Err(error.into())
            },
        }
    }

    pub fn active_profile(&self) -> Option<&Profile> {
        let id = self.active_profile_id.as_ref()?;
        self.profiles.iter().find(|profile| &profile.id == id)
    }

    pub fn upsert_profile(&mut self, mut profile: Profile) -> Result<()> {
        profile.validate_identity()?;
        profile.base_url = profile.normalized_address()?.0;
        let credential_service = profile.credential_key()?;
        for existing in &self.profiles {
            ensure!(
                existing.id == profile.id || existing.credential_key()? != credential_service,
                "This endpoint already has a profile; update it instead of sharing a credential entry"
            );
        }
        if let Some(old) = self.profiles.iter_mut().find(|old| old.id == profile.id) {
            ensure!(
                old.provider == profile.provider && old.credential_key()? == profile.credential_key()?,
                "A changed address requires a new profile ID; keep the old profile available for key deletion"
            );
            *old = profile;
        } else {
            ensure!(self.profiles.len() < MAX_PROFILES, "Too many AI profiles");
            self.profiles.push(profile);
        }
        // Configuration changes require the caller to explicitly re-enable.
        self.enabled = false;
        Ok(())
    }

    /// Callers delete the matching Keychain entry before removing a profile.
    pub fn remove_profile(&mut self, id: &str) -> Option<Profile> {
        let index = self.profiles.iter().position(|profile| profile.id == id)?;
        if self.active_profile_id.as_deref() == Some(id) {
            self.enabled = false;
            self.active_profile_id = None;
        }
        Some(self.profiles.remove(index))
    }

    fn validate_storage(&self) -> Result<()> {
        ensure!(self.profiles.len() <= MAX_PROFILES, "Too many AI profiles");
        let mut ids = BTreeSet::new();
        let mut credential_services = BTreeSet::new();
        for profile in &self.profiles {
            profile.validate_identity()?;
            profile.normalized_address()?;
            ensure!(ids.insert(&profile.id), "Duplicate AI profile ID");
            ensure!(credential_services.insert(profile.credential_key()?), "Duplicate AI credential endpoint");
        }
        if self.active_profile_id.is_some() {
            ensure!(self.active_profile().is_some(), "Missing active AI profile");
        }
        if self.enabled {
            self.active_profile().ok_or_else(|| anyhow::anyhow!("Missing active AI profile"))?.validate()?;
        }
        Ok(())
    }
}
