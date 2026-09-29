//! Jev optionally promotes one existing completion after local results arrive.
//! Requests never own the local completion latch or change insertion metadata.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fastab_engine::{CompleteRequest, CompleteResult, Suggestion};
use fastab_gpui::{AiPreview, ClickInsert, SuggestionItem};
use gpui::App;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{LastInput, OverlayController};
use crate::event::Event;
use crate::jev::cache::{CacheKey, RecommendationCache};
use crate::jev::client::{ClientError, ClientErrorKind, JevClient};
use crate::jev::config::{self, AiConfig, ResolvedProfile};
use crate::jev::context;
use crate::jev::credentials::{self, CredentialError};
use crate::jev::diagnostics::{self, Metric, Status, Ticket};
use crate::jev::policy::{KEEP_LOCAL, MAX_CANDIDATES, MAX_DESCRIPTION_BYTES, REQUESTS_PER_MINUTE};
use crate::jev::types::{Candidate, Recommendation, RecommendationInput, encode_request};

const INPUT_PAUSE: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestToken {
    session: Uuid,
    generation: u64,
    revision: u64,
    request_id: u64,
    settings_epoch: u64,
}

/// Contains only bounded candidate IDs, opaque digests and classified errors.
#[derive(Debug)]
pub enum Completed {
    Result {
        result: Result<Recommendation, ClientError>,
        cache_key: Option<CacheKey>,
        cached: bool,
        context_current: bool,
    },
    Skipped(Status),
}

/// Event itself derives Debug; credential bytes must never inherit that logging.
pub struct LoadedCredentials(Result<Option<Vec<u8>>, CredentialError>);

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
    ticket: Ticket,
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
    pending: bool,
}

struct Exposure {
    ticket: Ticket,
    click: ClickInsert,
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
    exposure: Option<Exposure>,
    debounce: Option<gpui::Task<()>>,
    credentials: Option<gpui::Task<()>>,
    flight: Option<tokio::task::JoinHandle<()>>,
    admitted: Arc<Mutex<VecDeque<Instant>>>,
    cache: Arc<Mutex<RecommendationCache>>,
    cache_scope: Option<(Uuid, String)>,
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
        // Keep its pending credential operation instead of starting a Busy read.
        if config == self.jev.config && revision == self.jev.config_revision {
            return;
        }
        self.cancel_jev(cx);
        self.jev.cache.lock().unwrap_or_else(|error| error.into_inner()).clear();
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
        let task = credentials::read_runtime(&profile.credential_service, cx);
        self.jev.client = Some(client);
        self.jev.profile = Some(profile);
        let epoch = self.jev.epoch;
        let proxy = self.proxy.clone();
        self.jev.credentials = Some(cx.spawn(async move |_cx| {
            let credentials = LoadedCredentials(task.await);
            let _ = proxy.send_event(Event::JevCredentialsLoaded { epoch, credentials });
        }));
    }

    pub(crate) fn jev_credentials_loaded(&mut self, epoch: u64, credentials: LoadedCredentials) {
        if epoch != self.jev.epoch {
            return;
        }
        self.jev.credentials = None;
        match credentials.0 {
            Ok(Some(key)) if !key.is_empty() && key.len() <= 4096 && key.iter().all(u8::is_ascii_graphic) => {
                self.jev.key = Some(Arc::new(key));
                // The next local completion will include AI provenance. Do not
                // restart the current engine request just because the key is
                // ready: that can discard its result and replace the visible
                // suggestions with the engine's loading placeholder.
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
        if let Some(snapshot) = &snapshot
            && snapshot.pending
        {
            diagnostics::record(snapshot.prepared.ticket, Metric::Cancelled, Status::Cancelled);
        }
        if restore_local_order && let Some(exposure) = self.jev.exposure.take() {
            diagnostics::record(exposure.ticket, Metric::NotAccepted, Status::NotAccepted);
        }
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
        let scope = (session, request.cwd.clone());
        if self.jev.cache_scope.as_ref() != Some(&scope) {
            self.jev.cache.lock().unwrap_or_else(|error| error.into_inner()).clear();
            self.jev.cache_scope = Some(scope);
        }
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
        if self
            .jev
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.pending && snapshot.token.revision != self.state.read(cx).ai_revision)
        {
            self.cancel_jev_request(cx);
        }
        // Settings/context notifications can be queued behind an input action.
        // Restore local order before that action reads a stale promoted row.
        let stale_promotion = self.state.read(cx).has_ai_promotion()
            && (!self.jev.is_ready() || AiConfig::load().ok() != self.jev.config);
        if stale_promotion || !self.jev_context_is_current() {
            self.cancel_jev(cx);
        }
    }

    pub(super) fn prepare_jev(&self, result: &CompleteResult) -> Option<Prepared> {
        let ticket = diagnostics::begin();
        let skip = |status| {
            diagnostics::skip(ticket, status);
            None
        };
        if self.jev.config.as_ref().is_some_and(|config| !config.enabled) {
            return skip(Status::Disabled);
        }
        if !self.jev.is_ready() {
            return skip(Status::NotReady);
        }
        if result.pending_generators {
            return skip(Status::PendingLocal);
        }
        let Some(context) = result.public_ai_context.as_ref() else {
            return skip(if result.suggestions.len() < 2 {
                Status::TooFewCandidates
            } else {
                Status::Unsupported
            });
        };
        let Some(stamp) = self.jev.request_context.as_ref() else {
            return skip(Status::ChangedContext);
        };
        if !self.stamp_is_current(stamp) {
            return skip(Status::ChangedContext);
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
            return skip(Status::TooFewCandidates);
        }
        diagnostics::record(ticket, Metric::Eligible, Status::Waiting);
        Some(Prepared {
            ticket,
            input: RecommendationInput {
                shell: stamp.shell.clone(),
                command_path: context.command_path.clone(),
                token_prefix: context.token_prefix.clone(),
                current_input: stamp.input.buffer.clone(),
                terminal_context: Default::default(),
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
            diagnostics::skip(prepared.ticket, Status::Navigating);
            return;
        }
        let Some(context) = context else {
            diagnostics::skip(prepared.ticket, Status::ChangedContext);
            return;
        };
        if !self.stamp_is_current(&context) {
            diagnostics::skip(prepared.ticket, Status::ChangedContext);
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
            pending: true,
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

    fn skip_jev(&mut self, status: Status, cx: &mut App) {
        if let Some(snapshot) = self.jev.snapshot.as_mut() {
            snapshot.pending = false;
            diagnostics::skip(snapshot.prepared.ticket, status);
        }
        self.cancel_jev(cx);
    }

    pub(super) fn take_jev_acceptance(&mut self, item: &ClickInsert) -> Option<(Ticket, bool)> {
        self.jev
            .exposure
            .take()
            .map(|exposure| (exposure.ticket, exposure.click == *item))
    }

    pub(super) fn finish_jev_acceptance(acceptance: Option<(Ticket, bool)>, inserted: bool) {
        if let Some((ticket, matched)) = acceptance {
            let (metric, status) = if matched && inserted {
                (Metric::Accepted, Status::Accepted)
            } else {
                (Metric::NotAccepted, Status::NotAccepted)
            };
            diagnostics::record(ticket, metric, status);
        }
    }

    pub(crate) fn start_jev_request(&mut self, token: RequestToken, cx: &mut App) {
        if !self.token_is_current(&token, cx) {
            // A row can invalidate the token during the debounce without
            // sending a controller action (e.g. horizontal wheel scrolling).
            if self
                .jev
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.token == token)
            {
                self.cancel_jev_request(cx);
            }
            return;
        }
        self.jev.debounce = None;
        let now = Instant::now();
        if self.jev.cooldown.is_some_and(|until| now < until) {
            self.skip_jev(Status::Cooldown, cx);
            return;
        }
        if self.jev.flight.as_ref().is_some_and(|flight| !flight.is_finished()) {
            self.skip_jev(Status::Busy, cx);
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
        let ticket = snapshot.prepared.ticket;
        diagnostics::status(ticket, Status::Collecting);
        let cwd = snapshot.context.input.cwd.clone();
        let cache_seed = cache_seed(snapshot, &profile, self.jev.config_revision);
        let cache = self.jev.cache.clone();
        let admitted = self.jev.admitted.clone();
        let share_git_status = self.jev.config.as_ref().is_some_and(|config| config.share_git_status);
        self.state.update(cx, |overlay, cx| {
            overlay.ai_preview = Some(AiPreview::Loading(overlay.ai_revision));
            cx.notify();
        });
        self.relayout_and_sync(cx);
        let proxy = self.proxy.clone();
        let revision = self.jev.config_revision;
        self.jev.flight = Some(tokio::spawn(async move {
            let work = RecommendationWork {
                input,
                profile: profile.clone(),
                cache_seed,
                cache,
                admitted,
            };
            let result = resolve_recommendation(
                work,
                || context::collect_for_request(&cwd, share_git_status),
                |input| async move {
                    client
                        .recommend_observed(&profile, key.as_slice(), &input, Some(ticket))
                        .await
                },
                || config::runtime_revision() == revision,
            )
            .await;
            let _ = proxy.send_event(Event::JevComplete { token, result });
        }));
    }

    pub(crate) fn apply_jev(&mut self, token: RequestToken, completion: Completed, cx: &mut App) {
        let (result, cache_key, cached, context_current) = match completion {
            Completed::Skipped(status) => {
                if self
                    .jev
                    .snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.token == token)
                {
                    self.skip_jev(status, cx);
                }
                return;
            },
            Completed::Result {
                result,
                cache_key,
                cached,
                context_current,
            } => (result, cache_key, cached, context_current),
        };
        // A completed response may be queued just before typing invalidates
        // its snapshot. Its account/rate policy still governs this same
        // configuration, but must never affect a replacement configuration.
        let same_settings = token.settings_epoch == self.jev.epoch
            && self.jev.config_revision == config::runtime_revision()
            && AiConfig::load().ok() == self.jev.config;
        let current = same_settings && self.token_is_current(&token, cx);
        if current && result.is_ok() && !context_current {
            self.skip_jev(Status::ChangedContext, cx);
            return;
        }
        let ticket = self
            .jev
            .snapshot
            .as_mut()
            .filter(|snapshot| snapshot.token == token)
            .map(|snapshot| {
                if current {
                    snapshot.pending = false;
                }
                snapshot.prepared.ticket
            });
        if !current
            && self
                .jev
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.token == token)
        {
            // Only the owner may clear pending status; a late response must
            // never remove the loading marker of a newer request.
            self.cancel_jev(cx);
        }
        if !same_settings {
            return;
        }
        match result {
            Ok(result) => {
                if !current {
                    return;
                }
                if cached && let Some(ticket) = ticket {
                    diagnostics::record(ticket, Metric::CacheHit, Status::CacheHit);
                }
                if result.choice == KEEP_LOCAL {
                    if let Some(key) = cache_key {
                        self.jev.cache.lock().unwrap_or_else(|error| error.into_inner()).insert(
                            key,
                            result.choice.clone(),
                            Instant::now(),
                        );
                    }
                    if let Some(ticket) = ticket {
                        diagnostics::record(ticket, Metric::KeptLocal, Status::KeptLocal);
                    }
                    // Keeping the existing order is a normal result. Retire
                    // the loading badge without adding a status row.
                    self.cancel_jev(cx);
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
                    self.skip_jev(Status::ChangedContext, cx);
                    return;
                };
                let overlay = self.state.read(cx);
                if !overlay
                    .items
                    .get(candidate.index)
                    .is_some_and(|item| matches_candidate(item, &candidate.click, &overlay.search_term))
                {
                    self.skip_jev(Status::ChangedContext, cx);
                    return;
                }
                let index = candidate.index;
                if let Some(key) = cache_key {
                    self.jev.cache.lock().unwrap_or_else(|error| error.into_inner()).insert(
                        key,
                        result.choice.clone(),
                        Instant::now(),
                    );
                }
                if index == 0 {
                    diagnostics::record(snapshot.prepared.ticket, Metric::KeptLocal, Status::KeptLocal);
                    self.cancel_jev(cx);
                    return;
                }
                let exposure = Exposure {
                    ticket: snapshot.prepared.ticket,
                    click: candidate.click.clone(),
                };
                let promoted = self.state.update(cx, |overlay, cx| {
                    let promoted = overlay.promote_ai_suggestion(index);
                    if promoted {
                        cx.notify();
                    }
                    promoted
                });
                if promoted {
                    diagnostics::record(exposure.ticket, Metric::Promoted, Status::Promoted);
                    self.jev.exposure = Some(exposure);
                    self.relayout_and_sync(cx);
                } else {
                    self.skip_jev(Status::Navigating, cx);
                }
            },
            Err(error) => {
                if current && let Some(ticket) = ticket {
                    // HTTP outcomes are counted by the client even if this
                    // event becomes stale; local validation failures still
                    // need a terminal status without inventing an HTTP call.
                    diagnostics::status(ticket, error.diagnostic_status());
                }
                if let Some(until) = error.cooldown.and_then(|delay| Instant::now().checked_add(delay)) {
                    self.jev.cooldown = Some(self.jev.cooldown.map_or(until, |previous| previous.max(until)));
                }
                let message = match error.kind {
                    ClientErrorKind::Authentication | ClientErrorKind::InvalidCredential => {
                        self.jev.blocked = true;
                        self.jev.key = None;
                        Some(text("Jev · 请在设置中更新密钥", "Jev · Update the key in Settings"))
                    },
                    ClientErrorKind::PaymentRequired => {
                        self.jev.blocked = true;
                        Some(text(
                            "Jev · 请检查服务商账户额度",
                            "Jev · Check provider account credit",
                        ))
                    },
                    // Transient failures only retire the loading icon. Keep
                    // the normal completion footer available for local hints.
                    _ => None,
                };
                if error.cooldown.is_some() || self.jev.blocked || (current && message.is_none()) {
                    self.cancel_jev(cx);
                }
                if !current {
                    return;
                }
                if let Some(message) = message {
                    self.show_jev_status(message, cx);
                }
            },
        }
    }

    fn show_jev_status(&mut self, message: String, cx: &mut App) {
        self.state.update(cx, |overlay, cx| {
            overlay.ai_preview = Some(AiPreview::Status(message));
            cx.notify();
        });
        self.relayout_and_sync(cx);
    }
}

/// Background work is separate from the UI ownership check. Only apply_jev
/// can publish a result or store a validated ID after checking its token.
struct RecommendationWork {
    input: RecommendationInput,
    profile: ResolvedProfile,
    cache_seed: [u8; 32],
    cache: Arc<Mutex<RecommendationCache>>,
    admitted: Arc<Mutex<VecDeque<Instant>>>,
}

async fn resolve_recommendation<C, CF, R, RF>(
    mut work: RecommendationWork,
    mut collect: C,
    recommend: R,
    settings_current: impl Fn() -> bool,
) -> Completed
where
    C: FnMut() -> CF,
    CF: std::future::Future<Output = context::CollectedContext>,
    R: FnOnce(RecommendationInput) -> RF,
    RF: std::future::Future<Output = Result<Recommendation, ClientError>>,
{
    let collected = collect().await;
    if !collected.complete || !settings_current() {
        return Completed::Skipped(Status::ChangedContext);
    }
    work.input.terminal_context = collected.terminal;
    let Ok(body) = encode_request(&work.profile, &work.input) else {
        return Completed::Skipped(Status::Unsupported);
    };
    let cache_key = cache_key(work.cache_seed, &body, collected.fingerprint);
    let cached = work
        .cache
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(cache_key, Instant::now());
    if let Some(choice) = cached {
        return Completed::Result {
            result: Ok(Recommendation { choice }),
            cache_key: None,
            cached: true,
            context_current: true,
        };
    }
    // Hits use no HTTP allowance. Collectors have separate lifetime bounds.
    if !admit_request(&work.admitted, Instant::now()) {
        return Completed::Skipped(Status::RateLimited);
    }
    let result = recommend(work.input).await;
    let context_current = if result.is_ok() && settings_current() {
        let after = collect().await;
        after.complete && after.fingerprint == collected.fingerprint
    } else {
        false
    };
    Completed::Result {
        result,
        cache_key: context_current.then_some(cache_key),
        cached: false,
        context_current,
    }
}

fn admit_request(admitted: &Mutex<VecDeque<Instant>>, now: Instant) -> bool {
    let mut admitted = admitted.lock().unwrap_or_else(|error| error.into_inner());
    while admitted
        .front()
        .is_some_and(|time| now.saturating_duration_since(*time) >= Duration::from_secs(60))
    {
        admitted.pop_front();
    }
    if admitted.len() >= REQUESTS_PER_MINUTE {
        return false;
    }
    admitted.push_back(now);
    true
}

fn digest_part(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

fn cache_seed(snapshot: &Snapshot, profile: &ResolvedProfile, revision: u64) -> [u8; 32] {
    let mut hash = Sha256::new();
    for bytes in [
        snapshot.context.input.session_id.as_bytes().as_slice(),
        snapshot.context.input.cwd.as_bytes(),
        &snapshot.context.input.cursor.to_le_bytes(),
        &snapshot.context.details,
        profile.credential_service.as_bytes(),
        &revision.to_le_bytes(),
    ] {
        digest_part(&mut hash, bytes);
    }
    for (name, value) in snapshot.context.environment.iter() {
        digest_part(&mut hash, name.as_bytes());
        digest_part(&mut hash, value.as_bytes());
    }
    for candidate in &snapshot.prepared.insertion {
        digest_part(&mut hash, &candidate.index.to_le_bytes());
        // Full immutable insertion identity, including future metadata fields.
        // This temporary text is hashed locally, never stored or logged.
        digest_part(&mut hash, format!("{:?}", candidate.click).as_bytes());
    }
    hash.finalize().into()
}

fn cache_key(seed: [u8; 32], request: &[u8], context: [u8; 32]) -> CacheKey {
    let mut hash = Sha256::new();
    for bytes in [seed.as_slice(), request, context.as_slice()] {
        digest_part(&mut hash, bytes);
    }
    CacheKey(hash.finalize().into())
}

/// Promotion must match all original metadata at the original index.
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
        && item.argument_value == click.argument_value
        && item.acceptance_scope == click.acceptance_scope
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
        argument_value: suggestion.argument_value,
        acceptance_scope: suggestion.acceptance_scope.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jev::config::Profile;
    use crate::jev::policy::DATA_POLICY_VERSION;

    fn fixture() -> (Snapshot, ResolvedProfile) {
        let session = Uuid::from_u128(1);
        let mut profile = Profile::typesafe();
        profile.data_policy_version = DATA_POLICY_VERSION;
        let input = RecommendationInput {
            shell: "zsh".into(),
            command_path: vec!["git".into()],
            token_prefix: "ch".into(),
            current_input: "git ch".into(),
            terminal_context: Default::default(),
            candidates: vec![
                Candidate {
                    id: "c0".into(),
                    name: "checkout".into(),
                    description: "Switch branches".into(),
                },
                Candidate {
                    id: "c1".into(),
                    name: "cherry-pick".into(),
                    description: "Apply commits".into(),
                },
            ],
        };
        let insertion = input
            .candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| CandidateInsertion {
                id: candidate.id.clone(),
                index,
                click: ClickInsert {
                    name: candidate.name.clone(),
                    search: "ch".into(),
                    kind: "subcommand".into(),
                    ..Default::default()
                },
            })
            .collect();
        (
            Snapshot {
                token: RequestToken {
                    session,
                    generation: 1,
                    revision: 1,
                    request_id: 1,
                    settings_epoch: 1,
                },
                context: ContextStamp {
                    input: LastInput {
                        buffer: "git ch".into(),
                        cwd: "/project".into(),
                        cursor: 6,
                        session_id: session,
                    },
                    environment: Arc::new(vec![("LANG".into(), "en_US.UTF-8".into())]),
                    details: context_digest(None, Some("/bin/zsh"), Some("zsh")),
                    shell: "zsh".into(),
                },
                prepared: Prepared {
                    ticket: diagnostics::begin(),
                    input,
                    insertion,
                },
                pending: true,
            },
            profile.validate().unwrap(),
        )
    }

    fn key(snapshot: &Snapshot, profile: &ResolvedProfile, revision: u64, context: [u8; 32]) -> CacheKey {
        cache_key(
            cache_seed(snapshot, profile, revision),
            &encode_request(profile, &snapshot.prepared.input).unwrap(),
            context,
        )
    }

    #[test]
    fn cached_recommendation_requires_the_complete_local_and_wire_identity() {
        let (original, profile) = fixture();
        let original_key = key(&original, &profile, 1, [0; 32]);
        let now = Instant::now();
        let mut cache = RecommendationCache::default();
        cache.insert(original_key, "c1".into(), now);
        assert_eq!(
            cache.get(key(&fixture().0, &profile, 1, [0; 32]), now).as_deref(),
            Some("c1")
        );

        // Metadata changes are invisible in the wire request but still make
        // a previously chosen ID unsafe to reuse for insertion.
        for change in 0..12 {
            let (mut snapshot, _) = fixture();
            match change {
                0 => snapshot.context.input.session_id = Uuid::from_u128(2),
                1 => snapshot.context.input.cwd = "/another-project".into(),
                2 => snapshot.context.input.cursor = 5,
                3 => snapshot.context.details = context_digest(Some("g=git"), Some("/bin/zsh"), Some("zsh")),
                4 => snapshot.context.environment = Arc::new(vec![("LANG".into(), "zh_CN.UTF-8".into())]),
                5 => snapshot.prepared.insertion[1].click.insert_value = Some("different".into()),
                6 => snapshot.prepared.insertion[1].index = 2,
                7 => snapshot.prepared.input.terminal_context.current_branch = Some("feature".into()),
                8 => snapshot.prepared.input.terminal_context.recent_commands = vec!["git status".into()],
                9 => {
                    // Wire criteria form an ID-keyed map, so model identity
                    // alone has no ordering. Reorder the actual UI rows too.
                    snapshot.prepared.input.candidates.swap(0, 1);
                    snapshot.prepared.insertion.swap(0, 1);
                    for (index, candidate) in snapshot.prepared.insertion.iter_mut().enumerate() {
                        candidate.index = index;
                    }
                },
                10 => snapshot.prepared.input.current_input = "git che".into(),
                11 => snapshot.prepared.input.shell = "bash".into(),
                _ => unreachable!(),
            }
            assert!(
                cache.get(key(&snapshot, &profile, 1, [0; 32]), now).is_none(),
                "dimension {change}"
            );
        }
        assert!(cache.get(key(&original, &profile, 2, [0; 32]), now).is_none());
        assert!(cache.get(key(&original, &profile, 1, [1; 32]), now).is_none());
        let mut router = Profile::openrouter();
        router.data_policy_version = DATA_POLICY_VERSION;
        assert!(
            cache
                .get(key(&original, &router.validate().unwrap(), 1, [0; 32]), now)
                .is_none()
        );
    }

    #[test]
    fn request_allowance_recovers_at_the_sliding_window_boundary() {
        let now = Instant::now();
        let admitted = Mutex::new(VecDeque::new());
        for _ in 0..REQUESTS_PER_MINUTE {
            assert!(admit_request(&admitted, now));
        }
        assert!(!admit_request(&admitted, now + Duration::from_secs(59)));
        assert!(admit_request(&admitted, now + Duration::from_secs(60)));
        assert_eq!(admitted.lock().unwrap().len(), 1);
    }

    fn work(cache: &Arc<Mutex<RecommendationCache>>, admitted: &Arc<Mutex<VecDeque<Instant>>>) -> RecommendationWork {
        let (snapshot, profile) = fixture();
        RecommendationWork {
            cache_seed: cache_seed(&snapshot, &profile, 1),
            input: snapshot.prepared.input,
            profile,
            cache: cache.clone(),
            admitted: admitted.clone(),
        }
    }

    fn collected(fingerprint: u8, complete: bool) -> context::CollectedContext {
        context::CollectedContext {
            terminal: Default::default(),
            fingerprint: [fingerprint; 32],
            complete,
        }
    }

    #[tokio::test]
    async fn cache_hit_skips_the_request_but_not_context_validation() {
        let cache = Arc::new(Mutex::new(RecommendationCache::default()));
        let admitted = Arc::new(Mutex::new(VecDeque::new()));
        let calls = std::cell::Cell::new(0);
        let result = resolve_recommendation(
            work(&cache, &admitted),
            || std::future::ready(collected(1, true)),
            |_| {
                calls.set(calls.get() + 1);
                std::future::ready(Ok(Recommendation {
                    choice: KEEP_LOCAL.into(),
                }))
            },
            || true,
        )
        .await;
        let Completed::Result {
            result: Ok(result),
            cache_key: Some(key),
            cached: false,
            context_current: true,
        } = result
        else {
            panic!("fresh result must still require UI validation");
        };
        assert!(cache.lock().unwrap().get(key, Instant::now()).is_none());
        // Simulate the UI publishing a response after its token/row checks.
        cache.lock().unwrap().insert(key, result.choice, Instant::now());
        let hit = resolve_recommendation(
            work(&cache, &admitted),
            || std::future::ready(collected(1, true)),
            |_| {
                calls.set(calls.get() + 1);
                std::future::ready(Ok(Recommendation { choice: "c0".into() }))
            },
            || true,
        )
        .await;
        assert!(matches!(hit, Completed::Result { cached: true, .. }));
        assert_eq!(calls.get(), 1);
        assert_eq!(admitted.lock().unwrap().len(), 1);
        let incomplete = resolve_recommendation(
            work(&cache, &admitted),
            || std::future::ready(collected(1, false)),
            |_| {
                calls.set(calls.get() + 1);
                std::future::ready(Ok(Recommendation { choice: "c0".into() }))
            },
            || true,
        )
        .await;
        assert!(matches!(incomplete, Completed::Skipped(Status::ChangedContext)));
        assert_eq!(calls.get(), 1);
    }

    #[tokio::test]
    async fn changes_during_a_request_and_failures_cannot_supply_cache_entries() {
        let cache = Arc::new(Mutex::new(RecommendationCache::default()));
        let admitted = Arc::new(Mutex::new(VecDeque::new()));
        for (after_fingerprint, after_complete) in [(2, true), (1, false)] {
            let mut contexts = VecDeque::from([collected(1, true), collected(after_fingerprint, after_complete)]);
            let result = resolve_recommendation(
                work(&cache, &admitted),
                || std::future::ready(contexts.pop_front().unwrap()),
                |_| std::future::ready(Ok(Recommendation { choice: "c1".into() })),
                || true,
            )
            .await;
            assert!(matches!(
                result,
                Completed::Result {
                    cache_key: None,
                    context_current: false,
                    ..
                }
            ));
            assert!(contexts.is_empty());
        }
        let result = resolve_recommendation(
            work(&cache, &admitted),
            || std::future::ready(collected(1, true)),
            |_| {
                std::future::ready(Err(ClientError {
                    kind: ClientErrorKind::Timeout,
                    status: None,
                    cooldown: None,
                }))
            },
            || true,
        )
        .await;
        assert!(matches!(
            result,
            Completed::Result {
                result: Err(_),
                cache_key: None,
                ..
            }
        ));
        let blocked = resolve_recommendation(
            work(&cache, &admitted),
            || std::future::ready(collected(1, true)),
            |_| std::future::ready(Ok(Recommendation { choice: "c1".into() })),
            || false,
        )
        .await;
        assert!(matches!(blocked, Completed::Skipped(Status::ChangedContext)));
    }
}
