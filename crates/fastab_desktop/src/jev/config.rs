use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, TryLockError};

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
// intermediate state. The low bit of RUNTIME_STATE is a fail-closed latch; the
// remaining bits are the revision, so pausing cannot race a successful writer
// that would otherwise clear a separate boolean latch.
static CONFIG_ACCESS: Mutex<()> = Mutex::new(());
static RUNTIME_STATE: AtomicU64 = AtomicU64::new(0);
const FAIL_CLOSED_BIT: u64 = 1;

/// A save was based on a configuration or runtime revision that is no longer
/// current. Callers should refresh the settings draft instead of retrying the
/// same stale write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigChanged;

impl std::fmt::Display for ConfigChanged {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AI configuration changed while the save was pending")
    }
}

impl std::error::Error for ConfigChanged {}

/// Invalidates cached credentials and requests before a settings write starts,
/// including same-value saves used when replacing a credential.
pub fn runtime_revision() -> u64 {
    revision(RUNTIME_STATE.load(Ordering::Acquire))
}

/// Immediately invalidates active Jev work and forces all config reads closed.
/// This is safe to call from the UI thread: it uses only an atomic operation.
pub fn pause_runtime() -> u64 {
    pause_state(&RUNTIME_STATE)
}

fn revision(state: u64) -> u64 {
    state >> 1
}

fn fail_closed(state: &AtomicU64) -> bool {
    state.load(Ordering::Acquire) & FAIL_CLOSED_BIT != 0
}

fn pause_state(state: &AtomicU64) -> u64 {
    loop {
        let current = state.load(Ordering::Acquire);
        let next_revision = revision(current).wrapping_add(1);
        let next = (next_revision << 1) | FAIL_CLOSED_BIT;
        if state
            .compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return next_revision;
        }
    }
}

fn begin_save(state: &AtomicU64, expected_revision: Option<u64>) -> Result<u64> {
    loop {
        let current = state.load(Ordering::Acquire);
        let current_revision = revision(current);
        if let Some(expected_revision) = expected_revision
            && current_revision != expected_revision
        {
            return Err(ConfigChanged.into());
        }
        let next_revision = current_revision.wrapping_add(1);
        let next = (next_revision << 1) | (current & FAIL_CLOSED_BIT);
        if state
            .compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return Ok(next);
        }
    }
}

fn finish_save(state: &AtomicU64, save_state: u64, result: Result<()>) -> Result<(u64, bool)> {
    match result {
        Ok(()) => {
            // A concurrent pause changes the revision, making this CAS fail.
            // The subsequent disabled save then owns clearing the fail-closed
            // state, so an older enabled config cannot resume Jev.
            let committed = state
                .compare_exchange(
                    save_state,
                    save_state & !FAIL_CLOSED_BIT,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok();
            Ok((revision(save_state), committed))
        },
        Err(error) => {
            state.fetch_or(FAIL_CLOSED_BIT, Ordering::AcqRel);
            Err(error)
        },
    }
}

fn write_value_with_revision(
    state: &AtomicU64,
    expected_revision: Option<u64>,
    value: serde_json::Value,
    mut write: impl FnMut(serde_json::Value) -> Result<()>,
    mut on_failure: impl FnMut(),
) -> Result<u64> {
    let save_state = begin_save(state, expected_revision)?;
    let disabled_repair = disabled_copy(&value);
    let first_write = write(value);
    if let Err(error) = first_write {
        on_failure();
        return finish_save(state, save_state, Err(error)).map(|(revision, _)| revision);
    }
    let (saved_revision, committed) = finish_save(state, save_state, Ok(()))?;
    if committed {
        return Ok(saved_revision);
    }

    // A pause invalidated this save while it was writing. The first write may
    // already have changed the settings cache or reached disk, so repair it
    // under the caller's CONFIG_ACCESS guard before returning the conflict.
    match write(disabled_repair) {
        Ok(()) => Err(ConfigChanged.into()),
        Err(error) => {
            on_failure();
            state.fetch_or(FAIL_CLOSED_BIT, Ordering::AcqRel);
            Err(error)
        },
    }
}

fn disabled_copy(value: &serde_json::Value) -> serde_json::Value {
    let mut disabled = value.clone();
    if let Some(config) = disabled.as_object_mut() {
        config.insert("enabled".into(), serde_json::Value::Bool(false));
    }
    disabled
}

fn raw_config_matches_expected(current: &AiConfig, expected: &AiConfig, paused: bool) -> bool {
    if current == expected {
        return true;
    }
    if !paused {
        return false;
    }
    let mut effective = current.clone();
    effective.enabled = false;
    effective == *expected
}

fn load_with(access: &Mutex<()>, state: &AtomicU64, read: impl FnOnce() -> Result<AiConfig>) -> Result<AiConfig> {
    let _guard = match access.try_lock() {
        Ok(guard) => guard,
        Err(TryLockError::WouldBlock) => bail!("AI configuration is being saved"),
        Err(TryLockError::Poisoned(error)) => error.into_inner(),
    };
    load_effective(state, read)
}

fn load_wait_with(access: &Mutex<()>, state: &AtomicU64, read: impl FnOnce() -> Result<AiConfig>) -> Result<AiConfig> {
    let _guard = access.lock().unwrap_or_else(|error| error.into_inner());
    load_effective(state, read)
}

fn load_effective(state: &AtomicU64, read: impl FnOnce() -> Result<AiConfig>) -> Result<AiConfig> {
    let mut config = read()?;
    if fail_closed(state) {
        config.enabled = false;
    }
    config.validate_storage()?;
    Ok(config)
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
        self.validated_profile
            .validate()
            .is_ok_and(|expected| expected == *self)
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
        Ok(format!(
            "app.fastab.ai.jev.v1.{}.{}",
            self.provider.credential_namespace(),
            hex
        ))
    }

    pub fn validate(&self) -> Result<ResolvedProfile> {
        self.validate_identity()?;
        ensure!(
            self.data_policy_version == DATA_POLICY_VERSION,
            "AI data policy has not been confirmed"
        );
        let (_, endpoint) = self.normalized_address()?;
        let allowed_responses = match (self.provider, self.model.as_str()) {
            (Provider::TypeSafe | Provider::CustomSystemOne, "jev-1.13.0") => DIRECT_RESPONSES,
            (Provider::OpenRouter | Provider::CustomSystemOne, "typesafe/jev-1.13" | "jev-1.13") => ROUTER_RESPONSES,
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
            !self.id.is_empty()
                && self.id.len() <= 64
                && self
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte)),
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
            ensure!(
                base == required,
                "Changing a preset address requires a separate custom profile"
            );
        }
        Ok((base, endpoint))
    }
}

/// The supported custom URL subset intentionally excludes escaped path segments,
/// dot segments and duplicate slashes. No guessing of chat/completions endpoints.
pub fn normalize_base_url(value: &str) -> Result<(String, Url)> {
    ensure!(
        value.len() <= 2048 && !value.is_empty(),
        "Invalid System One base URL length"
    );
    ensure!(
        !value.chars().any(|ch| ch.is_whitespace() || ch.is_control()),
        "Invalid whitespace in base URL"
    );
    ensure!(
        !value.contains(['%', '\\']),
        "Encoded or backslash URL paths are unsupported"
    );
    let (scheme, authority_and_path) = value
        .split_once("://")
        .ok_or_else(|| anyhow::anyhow!("Enter an absolute HTTPS base URL"))?;
    let authority = authority_and_path.split('/').next().unwrap_or_default();
    ensure!(
        scheme.eq_ignore_ascii_case("https") && !authority.is_empty(),
        "Enter an absolute HTTPS base URL"
    );
    ensure!(!authority.contains('@'), "URL credentials are forbidden");
    let mut url = Url::parse(value).map_err(|error| anyhow::anyhow!("Invalid System One base URL: {error}"))?;
    ensure!(
        url.scheme() == "https" && url.host_str().is_some(),
        "System One requires an HTTPS host"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "URL credentials are forbidden"
    );
    ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "URL query and fragment are forbidden"
    );
    ensure!(url.port() != Some(0), "Invalid HTTPS port");
    // Check the unnormalized spelling too: URL parsing removes dot segments.
    let raw_path = value
        .split_once("://")
        .and_then(|(_, rest)| rest.split_once('/'))
        .map_or("", |(_, path)| path);
    ensure!(
        raw_path.split('/').all(|segment| segment != "." && segment != "..") && !raw_path.contains("//"),
        "Ambiguous URL path"
    );
    let path = url.path().trim_end_matches('/').to_owned();
    ensure!(
        path.bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/_-.~".contains(&byte)),
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
    /// Separately authorized structured Git state. Defaults off for saved v1 configs.
    #[serde(default)]
    pub share_git_status: bool,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            active_profile_id: Some("typesafe".into()),
            profiles: vec![Profile::typesafe(), Profile::openrouter()],
            share_git_status: false,
        }
    }
}

impl AiConfig {
    pub fn load() -> Result<Self> {
        load_with(&CONFIG_ACCESS, &RUNTIME_STATE, Self::load_raw)
    }

    /// Load the settings snapshot while waiting for an in-flight save. Use
    /// only from a background worker; the GPUI foreground path must use load.
    pub fn load_wait() -> Result<Self> {
        load_wait_with(&CONFIG_ACCESS, &RUNTIME_STATE, Self::load_raw)
    }

    fn load_raw() -> Result<Self> {
        let value = fastab_settings::settings::get_value(SETTINGS_KEY)?;
        Self::parse_raw(value)
    }

    fn parse_raw(value: Option<serde_json::Value>) -> Result<Self> {
        let config = match value {
            Some(value) => serde_json::from_value::<Self>(value)?,
            None => Self::default(),
        };
        config.validate_storage()?;
        Ok(config)
    }

    /// Persist only if both the on-disk/cache value and the runtime epoch still
    /// match the snapshot this operation was based on.
    pub fn save_if_unchanged(&self, expected: &AiConfig, expected_revision: u64) -> Result<u64> {
        let _guard = CONFIG_ACCESS.lock().unwrap_or_else(|error| error.into_inner());
        self.validate_storage()?;
        let previous = fastab_settings::settings::get_value(SETTINGS_KEY)?;
        let current = Self::parse_raw(previous.clone())?;
        if !raw_config_matches_expected(&current, expected, fail_closed(&RUNTIME_STATE)) {
            return Err(ConfigChanged.into());
        }
        let value = serde_json::to_value(self)?;
        write_value_with_revision(
            &RUNTIME_STATE,
            Some(expected_revision),
            value,
            write_settings_value,
            move || fail_closed_cached_config(previous.clone()),
        )
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

    /// Callers delete the matching local credential before removing a profile.
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
            ensure!(
                credential_services.insert(profile.credential_key()?),
                "Duplicate AI credential endpoint"
            );
        }
        if self.active_profile_id.is_some() {
            ensure!(self.active_profile().is_some(), "Missing active AI profile");
        }
        if self.enabled {
            self.active_profile()
                .ok_or_else(|| anyhow::anyhow!("Missing active AI profile"))?
                .validate()?;
        }
        Ok(())
    }
}

fn write_settings_value(value: serde_json::Value) -> Result<()> {
    fastab_settings::settings::set_value(SETTINGS_KEY, value).map_err(Into::into)
}

fn restore_failed_config(cache: &mut serde_json::Map<String, serde_json::Value>, previous: Option<serde_json::Value>) {
    match previous {
        Some(mut previous) => {
            if let Some(config) = previous.as_object_mut() {
                config.insert("enabled".into(), serde_json::Value::Bool(false));
            }
            cache.insert(SETTINGS_KEY.into(), previous);
        },
        None => {
            cache.remove(SETTINGS_KEY);
        },
    }
}

fn fail_closed_cached_config(previous: Option<serde_json::Value>) {
    // Do not attempt another disk write here: even failure to open or flush may
    // already have changed the global cached value. Restore the snapshot this
    // save was based on so the UI can retry against the same expected config,
    // while keeping it disabled. The atomic latch covers the no-backend case.
    let mut global = OldSettings::data_lock().write();
    if let Some(cache) = global.as_mut() {
        restore_failed_config(cache, previous);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AiConfig, ConfigChanged, FAIL_CLOSED_BIT, SETTINGS_KEY, fail_closed, load_with, pause_state,
        raw_config_matches_expected, restore_failed_config, revision, write_value_with_revision,
    };
    use anyhow::anyhow;
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;

    fn value(enabled: bool) -> Value {
        json!({ "enabled": enabled, "profiles": [] })
    }

    #[test]
    fn saved_config_without_git_scope_keeps_sharing_disabled() {
        let mut old = serde_json::to_value(AiConfig::default()).unwrap();
        old.as_object_mut().unwrap().remove("share_git_status");
        let restored: AiConfig = serde_json::from_value(old).unwrap();
        assert!(!restored.share_git_status);

        let mut opted_in = restored;
        opted_in.share_git_status = true;
        assert_eq!(
            serde_json::to_value(opted_in).unwrap()["share_git_status"].as_bool(),
            Some(true)
        );
    }

    #[test]
    fn config_load_fails_fast_while_a_writer_holds_the_access_lock() {
        let access = Arc::new(Mutex::new(()));
        let state = Arc::new(AtomicU64::new(0));
        let (writer_ready_tx, writer_ready_rx) = mpsc::channel();
        let (release_writer_tx, release_writer_rx) = mpsc::channel();
        let writer_access = Arc::clone(&access);
        let writer = std::thread::spawn(move || {
            let _guard = writer_access.lock().unwrap();
            writer_ready_tx.send(()).unwrap();
            release_writer_rx.recv().unwrap();
        });
        writer_ready_rx.recv().unwrap();

        let read_called = Arc::new(AtomicBool::new(false));
        let load_access = Arc::clone(&access);
        let load_state = Arc::clone(&state);
        let load_read_called = Arc::clone(&read_called);
        let (load_result_tx, load_result_rx) = mpsc::channel();
        let loader = std::thread::spawn(move || {
            let result = load_with(&load_access, &load_state, || {
                load_read_called.store(true, Ordering::Release);
                Ok(AiConfig::default())
            });
            load_result_tx.send(result.is_err()).unwrap();
        });

        let returned_fast = load_result_rx.recv_timeout(Duration::from_millis(250)) == Ok(true);
        // This is the same atomic-only operation used by pause_runtime; it
        // must proceed while the writer owns the config mutex.
        let paused_revision = pause_state(&state);
        assert_eq!(paused_revision, 1);

        release_writer_tx.send(()).unwrap();
        writer.join().unwrap();
        loader.join().unwrap();
        assert!(returned_fast, "load waited for the settings writer");
        assert!(!read_called.load(Ordering::Acquire));
    }

    #[test]
    fn successful_save_clears_a_prior_pause_and_returns_its_revision() {
        let state = AtomicU64::new(0);
        let paused_revision = pause_state(&state);
        let mut written = None;
        let saved_revision = write_value_with_revision(
            &state,
            Some(paused_revision),
            value(false),
            |next| {
                written = Some(next);
                Ok(())
            },
            || {},
        )
        .unwrap();

        assert_eq!(written, Some(value(false)));
        assert_eq!(saved_revision, paused_revision + 1);
        assert_eq!(revision(state.load(Ordering::Acquire)), saved_revision);
        assert_eq!(state.load(Ordering::Acquire) & FAIL_CLOSED_BIT, 0);
    }

    #[test]
    fn failed_save_keeps_runtime_paused_and_runs_cache_repair() {
        let state = AtomicU64::new(0);
        let paused_revision = pause_state(&state);
        let cache_repaired = AtomicBool::new(false);
        let result = write_value_with_revision(
            &state,
            Some(paused_revision),
            value(false),
            |_| Err(anyhow!("simulated write failure")),
            || cache_repaired.store(true, Ordering::Release),
        );

        assert!(result.is_err());
        assert!(cache_repaired.load(Ordering::Acquire));
        assert!(fail_closed(&state));
    }

    #[test]
    fn failed_first_write_restores_the_expected_cache_for_retry() {
        let state = AtomicU64::new(0);
        let paused_revision = pause_state(&state);
        let previous_config = AiConfig {
            enabled: true,
            ..AiConfig::default()
        };
        let expected = AiConfig {
            enabled: false,
            ..previous_config.clone()
        };
        let mut attempted_config = expected.clone();
        attempted_config.active_profile_id = Some("openrouter".into());
        let previous = serde_json::to_value(&previous_config).unwrap();
        let attempted = serde_json::to_value(&attempted_config).unwrap();
        let expected_value = serde_json::to_value(&expected).unwrap();
        let cache = std::cell::RefCell::new(serde_json::Map::from_iter([(
            SETTINGS_KEY.to_owned(),
            previous.clone(),
        )]));

        let result = write_value_with_revision(
            &state,
            Some(paused_revision),
            attempted.clone(),
            |next| {
                cache.borrow_mut().insert(SETTINGS_KEY.into(), next);
                Err(anyhow!("simulated disk error"))
            },
            || restore_failed_config(&mut cache.borrow_mut(), Some(previous.clone())),
        );

        assert!(result.is_err());
        assert_eq!(cache.borrow().get(SETTINGS_KEY), Some(&expected_value));
        let cached = serde_json::from_value::<AiConfig>(cache.borrow()[SETTINGS_KEY].clone()).unwrap();
        assert!(raw_config_matches_expected(&cached, &expected, true));

        write_value_with_revision(
            &state,
            Some(revision(state.load(Ordering::Acquire))),
            attempted.clone(),
            |next| {
                cache.borrow_mut().insert(SETTINGS_KEY.into(), next);
                Ok(())
            },
            || {},
        )
        .unwrap();
        assert_eq!(cache.borrow().get(SETTINGS_KEY), Some(&attempted));
    }

    #[test]
    fn paused_save_accepts_the_masked_enabled_bit_but_no_other_drift() {
        let raw = AiConfig {
            enabled: true,
            ..AiConfig::default()
        };
        let mut loaded_while_paused = raw.clone();
        loaded_while_paused.enabled = false;

        assert!(raw_config_matches_expected(&raw, &raw, true));
        assert!(raw_config_matches_expected(&raw, &loaded_while_paused, true));
        assert!(!raw_config_matches_expected(&raw, &loaded_while_paused, false));

        let mut stale_other_field = loaded_while_paused;
        stale_other_field.active_profile_id = None;
        assert!(!raw_config_matches_expected(&raw, &stale_other_field, true));
    }

    #[test]
    fn pause_during_enabled_save_repairs_persisted_value_to_disabled() {
        let state = AtomicU64::new(0);
        let writes = Mutex::new(Vec::new());
        let result = write_value_with_revision(
            &state,
            None,
            value(true),
            |next| {
                let mut writes = writes.lock().unwrap();
                writes.push(next.clone());
                if writes.len() == 1 {
                    pause_state(&state);
                }
                Ok(())
            },
            || {},
        );

        assert!(result.unwrap_err().downcast_ref::<ConfigChanged>().is_some());
        let writes = writes.into_inner().unwrap();
        assert_eq!(writes, vec![value(true), value(false)]);
        assert!(fail_closed(&state));
    }

    #[test]
    fn failed_disabled_repair_keeps_runtime_paused_and_returns_write_error() {
        let state = AtomicU64::new(0);
        let writes = AtomicBool::new(false);
        let cache_repaired = AtomicBool::new(false);
        let result = write_value_with_revision(
            &state,
            None,
            value(true),
            |_| {
                if writes.swap(true, Ordering::AcqRel) {
                    Err(anyhow!("simulated repair failure"))
                } else {
                    pause_state(&state);
                    Ok(())
                }
            },
            || cache_repaired.store(true, Ordering::Release),
        );

        assert!(result.unwrap_err().to_string().contains("repair failure"));
        assert!(cache_repaired.load(Ordering::Acquire));
        assert!(fail_closed(&state));
    }
}
