//! Jev optionally promotes one existing completion after local results arrive.
//! Requests never own the local completion latch or change insertion metadata.

use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fastab_engine::{CompleteRequest, CompleteResult, Suggestion};
use fastab_gpui::{AiPreview, ClickInsert, SuggestionItem};
use gpui::App;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{LastInput, OverlayController};
use crate::event::Event;
use crate::jev::client::{ClientError, ClientErrorKind, JevClient};
use crate::jev::config::{self, AiConfig, ResolvedProfile};
use crate::jev::credentials::{self, CredentialError};
use crate::jev::policy::{KEEP_LOCAL, MAX_CANDIDATES, MAX_DESCRIPTION_BYTES, REQUESTS_PER_MINUTE};
use crate::jev::types::{Candidate, Recommendation, RecommendationInput};

const INPUT_PAUSE: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RequestToken {
    session: Uuid,
    generation: u64,
    revision: u64,
    request_id: u64,
    settings_epoch: u64,
}

/// Event itself derives Debug; keychain bytes must never inherit that logging.
pub(crate) struct LoadedCredentials(Result<Option<Vec<u8>>, CredentialError>);

impl fmt::Debug for LoadedCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LoadedCredentials([redacted])")
    }
}

#[derive(Clone)]
struct ContextStamp {
    input: LastInput,
    // Keeps the allocation alive so its address cannot be reused as a version.
    environment: Arc<Vec<(String, String)>>,
    details: [u8; 32],
    shell: String,
}

pub(super) struct Prepared {
    input: RecommendationInput,
    insertion: Vec<CandidateInsertion>,
}

struct CandidateInsertion {
    id: String,
    index: usize,
    click: ClickInsert,
}

struct Snapshot {
    token: RequestToken,
    context: ContextStamp,
    prepared: Prepared,
}

#[derive(Default)]
pub(super) struct JevRuntime {
    config: Option<AiConfig>,
    config_revision: u64,
    profile: Option<ResolvedProfile>,
    client: Option<JevClient>,
    key: Option<Arc<Vec<u8>>>,
    epoch: u64,
    next_request: u64,
    request_context: Option<ContextStamp>,
    snapshot: Option<Snapshot>,
    debounce: Option<gpui::Task<()>>,
    credentials: Option<gpui::Task<()>>,
    flight: Option<tokio::task::JoinHandle<()>>,
    admitted: VecDeque<Instant>,
    cooldown: Option<Instant>,
    blocked: bool,
}

impl JevRuntime {
    pub(super) fn is_ready(&self) -> bool {
        self.config_revision == config::runtime_revision()
            && self.profile.is_some()
            && self.client.is_some()
            && self.key.is_some()
            && !self.blocked
    }
}

impl Drop for JevRuntime {
    fn drop(&mut self) {
        if let Some(flight) = &self.flight {
            flight.abort();
        }
    }
}

impl OverlayController {
    pub(crate) fn reload_jev_if_changed(&mut self, cx: &mut App) {
        let config = AiConfig::load().ok();
        if config != self.jev.config || config::runtime_revision() != self.jev.config_revision {
            self.reload_jev(cx);
        }
    }

    /// Also called for a same-profile key replacement, which changes no JSON.
    pub(crate) fn reload_jev(&mut self, cx: &mut App) {
        let revision = config::runtime_revision();
        let config = AiConfig::load().ok();
        // Several settings notifications may describe the same saved state.
        // Keep its pending Keychain operation instead of starting a Busy read.
        if config == self.jev.config && revision == self.jev.config_revision {
            return;
        }
        self.cancel_jev(cx);
        self.jev.epoch = self.jev.epoch.wrapping_add(1);
        self.jev.key = None;
        self.jev.profile = None;
        self.jev.client = None;
        self.jev.credentials = None;
        self.jev.request_context = None;
        self.jev.blocked = false;
        self.jev.config = config;
        self.jev.config_revision = revision;
        let Some(config) = &self.jev.config else {
            return;
        };
        if !config.enabled {
            return;
        }
        let Some(profile) = config.active_profile().and_then(|profile| profile.validate().ok()) else {
            return;
        };
        let Ok(client) = JevClient::new() else {
            return;
        };
        let task = credentials::read(&profile.credential_service, cx);
        self.jev.client = Some(client);
        self.jev.profile = Some(profile);
        let epoch = self.jev.epoch;
        let proxy = self.proxy.clone();
        self.jev.credentials = Some(cx.spawn(async move |_cx| {
            let credentials = LoadedCredentials(task.await);
            let _ = proxy.send_event(Event::JevCredentialsLoaded { epoch, credentials });
        }));
    }

    pub(crate) fn jev_credentials_loaded(&mut self, epoch: u64, credentials: LoadedCredentials, cx: &mut App) {
        if epoch != self.jev.epoch {
            return;
        }
        self.jev.credentials = None;
        match credentials.0 {
            Ok(Some(key)) if !key.is_empty() && key.len() <= 4096 && key.iter().all(u8::is_ascii_graphic) => {
                self.jev.key = Some(Arc::new(key));
                // Produce provenance for the current input only after enabling.
                self.recomplete(cx);
            },
            _ => {
                self.jev.key = None;
                self.jev.blocked = true;
            },
        }
    }

    pub(super) fn cancel_jev(&mut self, cx: &mut App) {
        self.cancel_jev_with_order(true, cx);
    }

    /// Navigation and acceptance operate on the rows the user can see. Stop
    /// late responses without undoing that ordering before an action reads it.
    pub(super) fn cancel_jev_request(&mut self, cx: &mut App) {
        self.cancel_jev_with_order(false, cx);
    }

    fn cancel_jev_with_order(&mut self, restore_local_order: bool, cx: &mut App) {
        let overlay = self.state.read(cx);
        let has_promotion = overlay.has_ai_promotion();
        let changed = overlay.ai_preview.is_some() || (restore_local_order && has_promotion);
        self.jev.debounce = None;
        let snapshot = self.jev.snapshot.take();
        if restore_local_order || !has_promotion {
            self.jev.request_context = None;
        } else if let Some(snapshot) = snapshot {
            // Retain the context guard for a displayed recommendation even
            // after navigation has invalidated the request token.
            self.jev.request_context = Some(snapshot.context);
        }
        if let Some(flight) = &self.jev.flight {
            flight.abort();
        }
        // Keep the join handle until termination: abort is not synchronous.
        self.state.update(cx, |overlay, cx| {
            if restore_local_order {
                overlay.invalidate_ai();
            } else {
                overlay.invalidate_ai_request();
            }
            if changed {
                cx.notify();
            }
        });
        if changed {
            self.relayout_and_sync(cx);
        }
    }

    pub(super) fn capture_jev_request_context(&mut self, request: &CompleteRequest, session: Uuid) {
        self.jev.request_context = if request.include_public_ai && request.buffer.len() <= 256 {
            canonical_shell(request.current_shell.as_deref()).map(|shell| ContextStamp {
                input: LastInput {
                    buffer: request.buffer.clone(),
                    cwd: request.cwd.clone(),
                    cursor: request.cursor.unwrap_or(request.buffer.len() as u32),
                    session_id: session,
                },
                environment: request.environment_variables.clone(),
                details: context_digest(
                    request.alias.as_deref(),
                    request.current_shell.as_deref(),
                    request.current_process.as_deref(),
                ),
                shell: shell.into(),
            })
        } else {
            None
        };
    }

    fn stamp_is_current(&self, stamp: &ContextStamp) -> bool {
        if self.current_session() != Some(stamp.input.session_id)
            || self
                .last_input
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_ref()
                != Some(&stamp.input)
        {
            return false;
        }
        self.figterm_state
            .with(&stamp.input.session_id, |session| {
                let context = session.context.as_ref();
                session.dead_since.is_none()
                    && session.edit_buffer.text == stamp.input.buffer
                    && session.edit_buffer.cursor.max(0) as u32 == stamp.input.cursor
                    && context
                        .and_then(|context| context.current_working_directory.as_deref())
                        .unwrap_or_default()
                        == stamp.input.cwd
                    && Arc::ptr_eq(&session.flattened_env, &stamp.environment)
                    && context_digest(
                        context.and_then(|context| context.alias.as_deref()),
                        context.and_then(|context| context.shell_path.as_deref()),
                        context.and_then(|context| context.process_name.as_deref()),
                    ) == stamp.details
            })
            .unwrap_or(false)
    }

    pub(super) fn jev_context_is_current(&self) -> bool {
        self.jev
            .snapshot
            .as_ref()
            .map(|snapshot| &snapshot.context)
            .or(self.jev.request_context.as_ref())
            .is_none_or(|context| self.stamp_is_current(context))
    }

    pub(crate) fn reconcile_jev_context(&mut self, cx: &mut App) {
        // Settings/context notifications can be queued behind an input action.
        // Restore local order before that action reads a stale promoted row.
        let stale_promotion = self.state.read(cx).has_ai_promotion()
            && (!self.jev.is_ready() || AiConfig::load().ok() != self.jev.config);
        if stale_promotion || !self.jev_context_is_current() {
            self.cancel_jev(cx);
        }
    }

    pub(super) fn prepare_jev(&self, result: &CompleteResult) -> Option<Prepared> {
        if !self.jev.is_ready() || result.pending_generators {
            return None;
        }
        let context = result.public_ai_context.as_ref()?;
        let stamp = self.jev.request_context.as_ref()?;
        if !self.stamp_is_current(stamp) {
            return None;
        }
        let mut candidates = Vec::new();
        let mut insertion = Vec::new();
        for (index, suggestion) in result.suggestions.iter().enumerate() {
            let Some(public) = &suggestion.public_ai_candidate else {
                continue;
            };
            if !safe_insertion(suggestion) || public.name != suggestion.name {
                continue;
            }
            let id = format!("c{}", candidates.len());
            candidates.push(Candidate {
                id: id.clone(),
                name: public.name.clone(),
                description: truncate_utf8(&public.description, MAX_DESCRIPTION_BYTES),
            });
            // apply_complete_result maps this final vector to UI rows 1:1.
            insertion.push(CandidateInsertion {
                id,
                index,
                click: click_for(suggestion, &result.search_term),
            });
            if candidates.len() == MAX_CANDIDATES {
                break;
            }
        }
        if candidates.len() < 2 {
            return None;
        }
        Some(Prepared {
            input: RecommendationInput {
                shell: stamp.shell.clone(),
                command_path: context.command_path.clone(),
                token_prefix: context.token_prefix.clone(),
                candidates,
            },
            insertion,
        })
    }

    pub(super) fn schedule_jev(&mut self, prepared: Option<Prepared>, cx: &mut App) {
        let context = self.jev.request_context.take();
        self.cancel_jev(cx);
        let Some(prepared) = prepared else {
            return;
        };
        let overlay = self.state.read(cx);
        if !overlay.visible || overlay.loading || overlay.history_mode || overlay.has_changed_index {
            return;
        }
        let Some(context) = context else {
            return;
        };
        if !self.stamp_is_current(&context) {
            return;
        }
        self.jev.next_request = self.jev.next_request.wrapping_add(1);
        let token = RequestToken {
            session: context.input.session_id,
            generation: self.generation.load(std::sync::atomic::Ordering::Relaxed),
            revision: overlay.ai_revision,
            request_id: self.jev.next_request,
            settings_epoch: self.jev.epoch,
        };
        self.jev.snapshot = Some(Snapshot {
            token: token.clone(),
            context,
            prepared,
        });
        let executor = cx.background_executor().clone();
        let proxy = self.proxy.clone();
        self.jev.debounce = Some(cx.spawn(async move |_cx| {
            executor.timer(INPUT_PAUSE).await;
            let _ = proxy.send_event(Event::JevDebounced(token));
        }));
    }

    fn token_is_current(&self, token: &RequestToken, cx: &App) -> bool {
        let overlay = self.state.read(cx);
        self.enabled
            && self.jev.is_ready()
            && overlay.visible
            && !overlay.loading
            && AiConfig::load().ok() == self.jev.config
            && !overlay.history_mode
            && !overlay.has_changed_index
            && overlay.ai_revision == token.revision
            && self.jev.epoch == token.settings_epoch
            && self.generation.load(std::sync::atomic::Ordering::Relaxed) == token.generation
            && self
                .jev
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.token == *token && self.stamp_is_current(&snapshot.context))
    }

    pub(crate) fn start_jev_request(&mut self, token: RequestToken, cx: &mut App) {
        if !self.token_is_current(&token, cx) {
            return;
        }
        self.jev.debounce = None;
        let now = Instant::now();
        if self.jev.cooldown.is_some_and(|until| now < until) {
            return;
        }
        while self
            .jev
            .admitted
            .front()
            .is_some_and(|time| now.duration_since(*time) >= Duration::from_secs(60))
        {
            self.jev.admitted.pop_front();
        }
        if self.jev.admitted.len() >= REQUESTS_PER_MINUTE {
            return;
        }
        if self.jev.flight.as_ref().is_some_and(|flight| !flight.is_finished()) {
            return;
        }
        self.jev.flight = None;
        let Some(snapshot) = &self.jev.snapshot else {
            return;
        };
        let Some(profile) = self.jev.profile.clone() else {
            return;
        };
        let Some(client) = self.jev.client.clone() else {
            return;
        };
        let Some(key) = self.jev.key.clone() else {
            return;
        };
        let input = snapshot.prepared.input.clone();
        self.jev.admitted.push_back(now);
        self.show_jev_status(text("Jev 正在推荐…", "Jev is recommending…"), cx);
        let proxy = self.proxy.clone();
        let revision = self.jev.config_revision;
        self.jev.flight = Some(tokio::spawn(async move {
            if config::runtime_revision() != revision {
                return;
            }
            let result = client.recommend(&profile, key.as_slice(), &input).await;
            let _ = proxy.send_event(Event::JevComplete { token, result });
        }));
    }

    pub(crate) fn apply_jev(&mut self, token: RequestToken, result: Result<Recommendation, ClientError>, cx: &mut App) {
        // A completed response may be queued just before typing invalidates
        // its snapshot. Its account/rate policy still governs this same
        // configuration, but must never affect a replacement configuration.
        if token.settings_epoch != self.jev.epoch
            || self.jev.config_revision != config::runtime_revision()
            || AiConfig::load().ok() != self.jev.config
        {
            return;
        }
        let current = self.token_is_current(&token, cx);
        match result {
            Ok(result) => {
                if !current {
                    return;
                }
                if result.choice == KEEP_LOCAL {
                    self.show_jev_status(text("Jev · 保留本地建议", "Jev · Keep local suggestions"), cx);
                    return;
                }
                let Some(snapshot) = &self.jev.snapshot else {
                    return;
                };
                let Some(candidate) = snapshot
                    .prepared
                    .insertion
                    .iter()
                    .find(|candidate| candidate.id == result.choice)
                else {
                    self.cancel_jev(cx);
                    return;
                };
                let overlay = self.state.read(cx);
                if !overlay
                    .items
                    .get(candidate.index)
                    .is_some_and(|item| matches_candidate(item, &candidate.click, &overlay.search_term))
                {
                    self.cancel_jev(cx);
                    return;
                }
                let index = candidate.index;
                let promoted = self.state.update(cx, |overlay, cx| {
                    let promoted = overlay.promote_ai_suggestion(index);
                    if promoted {
                        cx.notify();
                    }
                    promoted
                });
                if promoted {
                    self.relayout_and_sync(cx);
                } else {
                    self.cancel_jev(cx);
                }
            },
            Err(error) => {
                if let Some(until) = error.cooldown.and_then(|delay| Instant::now().checked_add(delay)) {
                    self.jev.cooldown = Some(self.jev.cooldown.map_or(until, |previous| previous.max(until)));
                }
                let message = match error.kind {
                    ClientErrorKind::Authentication | ClientErrorKind::InvalidCredential => {
                        self.jev.blocked = true;
                        self.jev.key = None;
                        text("Jev · 请在设置中更新密钥", "Jev · Update the key in Settings")
                    },
                    ClientErrorKind::PaymentRequired => {
                        self.jev.blocked = true;
                        text("Jev · 请检查服务商账户额度", "Jev · Check provider account credit")
                    },
                    ClientErrorKind::RateLimited | ClientErrorKind::Overloaded => text(
                        "Jev · 暂时不可用，继续使用本地建议",
                        "Jev · Unavailable; local suggestions remain",
                    ),
                    ClientErrorKind::Timeout => text(
                        "Jev · 请求超时，继续使用本地建议",
                        "Jev · Timed out; local suggestions remain",
                    ),
                    _ => text(
                        "Jev · 未能推荐，继续使用本地建议",
                        "Jev · No recommendation; local suggestions remain",
                    ),
                };
                if error.cooldown.is_some() || self.jev.blocked {
                    self.cancel_jev(cx);
                }
                if !current {
                    return;
                }
                self.show_jev_status(message, cx);
            },
        }
    }

    fn show_jev_status(&mut self, message: String, cx: &mut App) {
        self.state.update(cx, |overlay, cx| {
            overlay.ai_preview = Some(AiPreview { message });
            cx.notify();
        });
        self.relayout_and_sync(cx);
    }
}

/// Selection identity intentionally ignores some insertion fields. Promotion
/// must match all original metadata at the original index, not just the title.
fn matches_candidate(item: &SuggestionItem, click: &ClickInsert, search: &str) -> bool {
    search == click.search
        && item.name == click.name
        && item.description == click.description
        && item.kind == click.kind
        && item.args_hint == click.args_hint
        && item.insert_value == click.insert_value
        && item.display_name == click.display_name
        && item.primary_name == click.primary_name
        && item.separator_to_add == click.separator_to_add
        && item.should_add_space == click.should_add_space
        && item.hidden == click.hidden
        && item.priority == click.priority
        && item.icon_identifier == click.icon_identifier
        && item.original_type == click.original_type
        && item.query_term == click.query_term
}

fn context_digest(alias: Option<&str>, shell: Option<&str>, process: Option<&str>) -> [u8; 32] {
    let mut hash = Sha256::new();
    for value in [alias, shell, process] {
        hash.update([u8::from(value.is_some())]);
        let value = value.unwrap_or_default();
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value.as_bytes());
    }
    hash.finalize().into()
}

fn canonical_shell(path: Option<&str>) -> Option<&'static str> {
    match path?.rsplit('/').next()? {
        "zsh" => Some("zsh"),
        "bash" => Some("bash"),
        "fish" => Some("fish"),
        "sh" => Some("sh"),
        _ => None,
    }
}

fn safe_insertion(suggestion: &Suggestion) -> bool {
    !suggestion.is_dangerous
        && !suggestion.hidden
        && matches!(suggestion.kind.as_str(), "cmd" | "subcommand" | "option")
        && !matches!(suggestion.original_type.as_deref(), Some("auto-execute" | "special"))
        && !suggestion.name.is_empty()
        && suggestion.name.len() <= 256
        && suggestion
            .name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        && suggestion
            .insert_value
            .as_ref()
            .is_none_or(|value| value == &suggestion.name)
        && suggestion
            .separator_to_add
            .as_deref()
            .is_none_or(|value| matches!(value, "" | " " | "="))
        && suggestion.query_term.is_none()
}

fn click_for(suggestion: &Suggestion, search: &str) -> ClickInsert {
    ClickInsert {
        name: suggestion.name.clone(),
        description: suggestion.description.clone(),
        search: search.into(),
        kind: suggestion.kind.clone(),
        args_hint: suggestion.args_hint.clone(),
        insert_value: suggestion.insert_value.clone(),
        display_name: suggestion.display_name.clone(),
        primary_name: suggestion.primary_name.clone(),
        separator_to_add: suggestion.separator_to_add.clone(),
        should_add_space: suggestion.should_add_space,
        hidden: suggestion.hidden,
        priority: suggestion.priority,
        icon_identifier: suggestion.icon.clone(),
        original_type: suggestion.original_type.clone(),
        query_term: suggestion.query_term.clone(),
    }
}

fn truncate_utf8(value: &str, bytes: usize) -> String {
    let mut end = value.len().min(bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].into()
}

fn text(zh: &str, en: &str) -> String {
    if crate::settings_ui::locale_is_zh() {
        zh.into()
    } else {
        en.into()
    }
}
