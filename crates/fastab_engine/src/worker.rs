use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, mpsc};
use std::task::{Context, Poll};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::anyhow;
// Not `tokio::sync::oneshot`. The desktop awaits the reply on GPUI's foreground
// executor, which used to run nested inside one `tokio::Runtime::block_on` poll
// that hosted `NSApplication::run` and never returned. `block_on` fixes tokio's
// cooperative budget (128 units) for the life of that poll, and every tokio
// channel that resolves on that thread spends one. Once they were gone the
// 129th reply never resolved: `poll_proceed` reported Pending and woke the task
// at once, GPUI re-queued it onto the same main-queue drain, and the desktop sat
// at 100% CPU with the overlay frozen. `fastab_desktop` now starts the UI loop
// outside `block_on`; this channel has no budget accounting at all, so the reply
// path stays correct even if a caller polls it from inside one again.
use futures::channel::oneshot;

use crate::cancellation::{CancellationToken, CompletionCancelled};
use crate::diagnostics::{EngineClientDiagnostics, RequestDiagnostics};
use crate::ir::Registry;
use crate::rank::AcceptanceIndex;
use crate::runtime::{CompleteRequest, CompleteResult, Engine};

/// Thread-safe handle around the completion [`Engine`].
#[derive(Clone)]
pub struct EngineClient {
    tx: mpsc::Sender<Job>,
    acceptance: Arc<Mutex<AcceptanceIndex>>,
    submission: Arc<Mutex<Submission>>,
}

/// Caller identity for one terminal session; zero is the legacy anonymous caller.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct SessionId(u128);

impl SessionId {
    pub const ANONYMOUS: Self = Self(0);
    pub const fn new(value: u128) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone)]
pub struct EngineClientOptions {
    /// Override for controlled resource replay. Normal clients use the default.
    pub spec_idle_grace: Duration,
}

impl Default for EngineClientOptions {
    fn default() -> Self {
        Self {
            spec_idle_grace: crate::ir::SPEC_IDLE_GRACE,
        }
    }
}

#[derive(Default)]
struct Submission {
    sequence: u64,
    owner: Option<SessionId>,
    latest: Option<CancellationToken>,
    requests: RequestDiagnostics,
}

/// Already submitted completion. Dropping an unpolled task cancels that request.
/// Its reply uses no Tokio cooperative budget and can be polled by GPUI.
pub struct CompletionTask {
    receiver: oneshot::Receiver<anyhow::Result<CompleteResult>>,
    token: CancellationToken,
}

impl Future for CompletionTask {
    type Output = anyhow::Result<CompleteResult>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.receiver).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(result)) => Poll::Ready(result),
            Poll::Ready(Err(_)) => Poll::Ready(Err(anyhow!("engine dropped the reply"))),
        }
    }
}

impl Drop for CompletionTask {
    fn drop(&mut self) {
        self.token.cancel();
    }
}

struct Job {
    kind: JobKind,
}

enum JobKind {
    Complete {
        request: CompleteRequest,
        reply: oneshot::Sender<anyhow::Result<CompleteResult>>,
        token: CancellationToken,
        session: SessionId,
        sequence: u64,
    },
    RecordAcceptance {
        root_command: String,
        accepted_name: String,
        timestamp: u64,
    },
    RecordScopedAcceptance {
        scope: String,
        accepted_name: String,
        timestamp: u64,
    },
    /// `ftab hook clear-autocomplete-cache`: drop every cached spec and
    /// generator result before the next completion runs.
    ClearCaches,
    Diagnostics {
        reply: oneshot::Sender<EngineClientDiagnostics>,
    },
    EndInput {
        session: SessionId,
    },
    #[cfg(test)]
    InspectIdle {
        relative: String,
        reply: oneshot::Sender<IdleFileState>,
    },
}

#[cfg(test)]
struct IdleFileState {
    cached: bool,
    // Missing, active, and pending are distinct states under test.
    #[allow(clippy::option_option)]
    mark: Option<Option<Instant>>,
}

// A completion attempt can legitimately spend the legacy 5s script timeout
// inside a native generator. Keep the supervisor watchdog above that default
// (and above the 15s scriptTimeout used by several bundled specs) so the
// worker does not reset the engine while the generator is still within its
// configured budget.
const MIN_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(30);

/// Stack for the worker and every attempt thread. Catalog load now keeps
/// each descriptor as `RawValue` (no deep `from_slice` of the IR trees).
/// Attempt threads still walk those trees in `evaluate_inner`, including
/// `git#generateSpec`. macOS gives a secondary thread 512 KB by default;
/// the reservation is virtual and costs nothing unless touched, so both
/// builds keep the same headroom.
const ENGINE_THREAD_STACK: usize = 16 * 1024 * 1024;

/// Slack added to the user's script budget before the UI stops waiting, so a
/// generator that finishes right on its own deadline still gets rendered.
const UI_DEADLINE_MARGIN: Duration = Duration::from_secs(1);
/// Floor for the UI deadline, for the case where `scriptTimeout` is set to
/// something tiny. Below this the overlay would give up on completions the
/// engine was about to return anyway.
const MIN_UI_DEADLINE: Duration = Duration::from_secs(2);

/// Watchdog used by the normal engine client. A user script timeout above the
/// bundled-spec baseline extends the watchdog instead of being silently
/// truncated by it. Read this for every job so changing the setting does not
/// require restarting the desktop process.
pub fn engine_attempt_timeout() -> Duration {
    let configured_ms = fastab_settings::settings::get_int("autocomplete.scriptTimeout")
        .ok()
        .flatten()
        .unwrap_or(crate::generate::DEFAULT_SCRIPT_TIMEOUT_MS);
    engine_attempt_timeout_for(configured_ms)
}

fn engine_attempt_timeout_for(configured_ms: i64) -> Duration {
    let configured = Duration::from_millis(u64::try_from(configured_ms).unwrap_or(0));
    MIN_ATTEMPT_TIMEOUT.max(configured.saturating_add(Duration::from_secs(5)))
}

/// How long the overlay waits for a completion before giving up on it.
///
/// Deliberately not [`engine_attempt_timeout`]. That one is the supervisor's
/// "has this worker thread wedged" floor and sits at 30s so a spec with a long
/// `scriptTimeout` is never killed mid-run. Reusing it for the UI meant a
/// single stuck generator pinned the `···` marker on screen for half a minute.
/// What the user is actually waiting on is their own script budget.
pub fn ui_completion_deadline() -> Duration {
    ui_completion_deadline_for(crate::generate::configured_script_timeout_ms())
}

fn ui_completion_deadline_for(configured_ms: i64) -> Duration {
    let configured = Duration::from_millis(u64::try_from(configured_ms).unwrap_or(0));
    MIN_UI_DEADLINE.max(configured.saturating_add(UI_DEADLINE_MARGIN))
}

#[derive(Debug)]
enum AttemptFailure {
    TimedOut,
    Panicked,
}

type AttemptResult = Result<(Engine, anyhow::Result<CompleteResult>), AttemptFailure>;

impl EngineClient {
    pub fn spawn(specs_dir: PathBuf) -> anyhow::Result<Self> {
        Self::spawn_with_options(specs_dir, EngineClientOptions::default())
    }

    pub fn spawn_with_options(specs_dir: PathBuf, options: EngineClientOptions) -> anyhow::Result<Self> {
        Self::spawn_supervised(specs_dir, None, options.spec_idle_grace)
    }

    #[cfg(test)]
    fn spawn_with_timeout(specs_dir: PathBuf, attempt_timeout: Duration) -> anyhow::Result<Self> {
        Self::spawn_supervised(specs_dir, Some(attempt_timeout), crate::ir::SPEC_IDLE_GRACE)
    }

    #[cfg(test)]
    fn spawn_with_idle_grace(
        specs_dir: PathBuf,
        attempt_timeout: Duration,
        idle_grace: Duration,
    ) -> anyhow::Result<Self> {
        Self::spawn_supervised(specs_dir, Some(attempt_timeout), idle_grace)
    }

    fn spawn_supervised(
        specs_dir: PathBuf,
        fixed_attempt_timeout: Option<Duration>,
        idle_grace: Duration,
    ) -> anyhow::Result<Self> {
        let (tx, rx) = mpsc::channel::<Job>();
        let supervisor_specs_dir = specs_dir.clone();
        let acceptance = Arc::new(Mutex::new(AcceptanceIndex::load()));
        let worker_acceptance = acceptance.clone();
        let submission = Arc::new(Mutex::new(Submission::default()));
        let worker_submission = Arc::clone(&submission);

        thread::Builder::new()
            .name("ec-engine".into())
            .stack_size(ENGINE_THREAD_STACK)
            .spawn(move || {
                // Initialize lazily for the first real request.  A malformed
                // or temporarily unavailable specs directory must not poison
                // this worker forever: the next request gets another chance
                // to build the registry after the caller repairs the input.
                let mut engine = None;
                // Resource ownership follows committed normal completions, not
                // submissions which may be cancelled before becoming active.
                let mut active_session = None;
                // Pristine index kept aside so recovering from a timed-out
                // attempt does not re-walk the specs directory. It is cloned,
                // never handed out, so a poisoned attempt cannot corrupt it.
                let mut registry_template: Option<Registry> = None;
                let mut jobs_without_attempt = 0;
                loop {
                    let first = match wait_for_engine_job(&rx, engine.as_ref(), idle_grace) {
                        Ok(job) => job,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            // No queued completion. The attempt thread is not
                            // holding this engine, so the deadline may release.
                            if let Some(engine) = engine.as_mut() {
                                engine.release_idle_specs(Instant::now(), idle_grace);
                            }
                            jobs_without_attempt = 0;
                            continue;
                        },
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    maintain_idle_between_queued_jobs(
                        engine.as_mut(),
                        &first,
                        &mut jobs_without_attempt,
                        Instant::now(),
                        idle_grace,
                    );
                    // Submission cancels superseded tokens immediately, so the worker
                    // can consume FIFO without draining control messages across jobs.
                    let (request, reply, token, session, _sequence) = match first.kind {
                        JobKind::RecordAcceptance {
                            root_command,
                            accepted_name,
                            timestamp,
                        } => {
                            record_acceptance(
                                &mut engine,
                                &worker_acceptance,
                                &root_command,
                                &accepted_name,
                                timestamp,
                            );
                            continue;
                        },
                        JobKind::RecordScopedAcceptance {
                            scope,
                            accepted_name,
                            timestamp,
                        } => {
                            record_scoped_acceptance(
                                &mut engine,
                                &worker_acceptance,
                                &scope,
                                &accepted_name,
                                timestamp,
                            );
                            continue;
                        },
                        JobKind::ClearCaches => {
                            if clear_caches(&supervisor_specs_dir, &mut engine, &mut registry_template) {
                                active_session = None;
                            }
                            continue;
                        },
                        JobKind::EndInput { session } => {
                            if active_session == Some(session) {
                                if let Some(engine) = engine.as_mut() {
                                    engine.end_input();
                                }
                                active_session = None;
                            }
                            continue;
                        },
                        JobKind::Diagnostics { reply } => {
                            let snapshot = EngineClientDiagnostics {
                                engine: engine.as_ref().map(|engine| engine.diagnostics_with_grace(idle_grace)),
                                requests: worker_submission
                                    .lock()
                                    .unwrap_or_else(|error| error.into_inner())
                                    .requests,
                            };
                            let _ = reply.send(snapshot);
                            continue;
                        },
                        #[cfg(test)]
                        JobKind::InspectIdle { relative, reply } => {
                            let _ = reply.send(idle_file_state(engine.as_ref(), &relative));
                            continue;
                        },
                        JobKind::Complete {
                            request,
                            reply,
                            token,
                            session,
                            sequence,
                        } => (request, reply, token, session, sequence),
                    };
                    if token.is_cancelled() || reply.is_canceled() {
                        token.cancel();
                        update_requests(&worker_submission, |counts| {
                            counts.cancelled = counts.cancelled.saturating_add(1);
                        });
                        let _ = reply.send(Err(CompletionCancelled.into()));
                        continue;
                    }
                    let current_engine = match engine.take() {
                        Some(engine) => engine,
                        None => {
                            match rebuild_engine(&supervisor_specs_dir, &mut registry_template, &worker_acceptance) {
                                Ok(engine) => {
                                    update_requests(&worker_submission, |counts| {
                                        counts.engine_initializations = counts.engine_initializations.saturating_add(1);
                                    });
                                    engine
                                },
                                Err(err) => {
                                    update_requests(&worker_submission, |counts| {
                                        counts.failed = counts.failed.saturating_add(1);
                                    });
                                    let _ = reply.send(Err(anyhow!("completion engine initialization failed: {err}")));
                                    continue;
                                },
                            }
                        },
                    };
                    let history_only = request.history_only;
                    let attempt_timeout = fixed_attempt_timeout.unwrap_or_else(engine_attempt_timeout);
                    let attempt_context = attempt_log_context(&request);
                    jobs_without_attempt = 0;
                    update_requests(&worker_submission, |counts| {
                        counts.started = counts.started.saturating_add(1);
                    });
                    match run_engine_attempt_with_token(current_engine, request, attempt_timeout, token.clone()) {
                        Ok((mut next_engine, result)) => {
                            if result.is_ok() && !history_only {
                                active_session = Some(session);
                            }
                            // `complete` already stamped idle marks. A mark from
                            // this attempt is younger than the grace, so this
                            // only releases files whose grace elapsed while the
                            // attempt held the engine.
                            next_engine.release_idle_specs(Instant::now(), idle_grace);
                            engine = Some(next_engine);
                            update_requests(&worker_submission, |counts| {
                                if result.as_ref().is_err_and(|error| error.is::<CompletionCancelled>()) {
                                    counts.cancelled = counts.cancelled.saturating_add(1);
                                } else if result.is_ok() {
                                    counts.completed = counts.completed.saturating_add(1);
                                } else {
                                    counts.failed = counts.failed.saturating_add(1);
                                }
                            });
                            let _ = reply.send(result);
                        },
                        Err(failure) => {
                            token.cancel();
                            update_requests(&worker_submission, |counts| {
                                counts.failed = counts.failed.saturating_add(1);
                                match failure {
                                    AttemptFailure::TimedOut => {
                                        counts.watchdog_timeouts = counts.watchdog_timeouts.saturating_add(1);
                                    },
                                    AttemptFailure::Panicked => counts.panics = counts.panics.saturating_add(1),
                                }
                            });
                            // The default log filter is ERROR, so this is the
                            // only durable evidence of a wedged or crashed
                            // generator. Keep it at that level.
                            tracing::error!(
                                timeout_ms = attempt_timeout.as_millis() as u64,
                                context = %attempt_context,
                                failure = ?failure,
                                "completion attempt abandoned; engine reset"
                            );
                            let error = failure.error(attempt_timeout);
                            let _ = reply.send(Err(error));
                            // The timed-out/panicked attempt owns the old
                            // engine and may still be unwinding in its
                            // detached thread.  Drop it from the supervisor;
                            // the next request will retry initialization and
                            // can recover if the specs directory was repaired.
                            engine = None;
                            active_session = None;
                        },
                    }
                }
            })
            .map_err(|err| anyhow!("spawn engine thread: {err}"))?;

        Ok(Self {
            tx,
            acceptance,
            submission,
        })
    }

    pub fn complete(&self, request: CompleteRequest) -> CompletionTask {
        self.complete_for_session(SessionId::ANONYMOUS, request)
    }

    /// Submits synchronously: a later call wins even if its future is polled first.
    pub fn complete_for_session(&self, session: SessionId, request: CompleteRequest) -> CompletionTask {
        let (reply, receiver) = oneshot::channel();
        let token = CancellationToken::new();
        let mut submission = self.submission.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(previous) = &submission.latest {
            previous.cancel();
        }
        submission.sequence = submission.sequence.wrapping_add(1);
        submission.owner = Some(session);
        submission.latest = Some(token.clone());
        submission.requests.submitted = submission.requests.submitted.saturating_add(1);
        let sequence = submission.sequence;
        if let Err(error) = self.tx.send(Job {
            kind: JobKind::Complete {
                request,
                reply,
                token: token.clone(),
                session,
                sequence,
            },
        }) {
            if let JobKind::Complete { reply, .. } = error.0.kind {
                let _ = reply.send(Err(anyhow!("engine thread is gone")));
            }
        }
        CompletionTask { receiver, token }
    }

    /// Use from CLI/tests only; the desktop polls CompletionTask on GPUI.
    pub fn complete_blocking(&self, request: CompleteRequest) -> anyhow::Result<CompleteResult> {
        futures::executor::block_on(self.complete(request))
    }

    /// End a session without cancelling another session's submitted work.
    /// The worker also checks the owner of committed resources: a newer
    /// cancelled submission must not swallow the previous owner's end.
    pub fn end_input(&self, session: SessionId) -> anyhow::Result<()> {
        let mut submission = self.submission.lock().unwrap_or_else(|error| error.into_inner());
        if submission.owner == Some(session) {
            if let Some(token) = submission.latest.take() {
                token.cancel();
            }
            submission.owner = None;
        }
        submission.sequence = submission.sequence.wrapping_add(1);
        // Always keep the notification in FIFO order. Only the worker knows
        // whether intervening attempts actually committed a new resource owner.
        self.tx
            .send(Job {
                kind: JobKind::EndInput { session },
            })
            .map_err(|_disconnected| anyhow!("engine thread is gone"))
    }

    /// An ordered, read-only snapshot. Does not initialize or interrupt Engine.
    pub fn diagnostics(&self) -> impl Future<Output = anyhow::Result<EngineClientDiagnostics>> + Send + 'static {
        let (reply, receiver) = oneshot::channel();
        let sent = self.send_control(JobKind::Diagnostics { reply });
        async move {
            sent?;
            receiver
                .await
                .map_err(|_cancelled| anyhow!("engine dropped diagnostics reply"))
        }
    }

    fn send_control(&self, kind: JobKind) -> anyhow::Result<()> {
        let _submission = self.submission.lock().unwrap_or_else(|error| error.into_inner());
        self.tx
            .send(Job { kind })
            .map_err(|_disconnected| anyhow!("engine thread is gone"))
    }

    /// Queue a successful acceptance without participating in completion
    /// latest-job cancellation. Sending is unbounded and returns immediately;
    /// the worker applies the record between completion attempts and persists
    /// it best-effort. This keeps shell/UI acceptance independent of a slow or
    /// timed-out generator.
    pub fn record_acceptance(
        &self,
        root_command: impl Into<String>,
        accepted_name: impl Into<String>,
    ) -> anyhow::Result<()> {
        let root_command = root_command.into();
        let accepted_name = accepted_name.into();
        let timestamp = AcceptanceIndex::now_millis();
        {
            let mut acceptance = self.acceptance.lock().unwrap_or_else(|err| err.into_inner());
            let _ = acceptance.record_at(&root_command, &accepted_name, timestamp);
        }
        self.send_control(JobKind::RecordAcceptance {
            root_command,
            accepted_name,
            timestamp,
        })
        .map_err(|_err| {
            self.acceptance.lock().unwrap_or_else(|err| err.into_inner()).persist();
            anyhow!("engine thread is gone")
        })
    }

    /// Update in-memory scoped ranking immediately; persistence is replayed
    /// only by the worker so the GPUI insertion path never waits on SQLite.
    pub fn record_scoped_acceptance(
        &self,
        scope: impl Into<String>,
        accepted_name: impl Into<String>,
    ) -> anyhow::Result<()> {
        let scope = scope.into();
        let accepted_name = accepted_name.into();
        let timestamp = AcceptanceIndex::now_millis();
        let valid = self
            .acceptance
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .record_scoped_at(&scope, &accepted_name, timestamp);
        if !valid {
            return Ok(());
        }
        self.send_control(JobKind::RecordScopedAcceptance {
            scope,
            accepted_name,
            timestamp,
        })
        .map_err(|_err| anyhow!("engine thread is gone"))
    }

    /// Forget every cached spec and generator result. Applied on the worker
    /// between completions, like an acceptance record, so it is never
    /// coalesced away by a newer completion request.
    pub fn clear_caches(&self) -> anyhow::Result<()> {
        self.send_control(JobKind::ClearCaches)
    }

    #[cfg(test)]
    fn inspect_idle(&self, relative: &str) -> anyhow::Result<IdleFileState> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Job {
                kind: JobKind::InspectIdle {
                    relative: relative.to_string(),
                    reply,
                },
            })
            .map_err(|_err| anyhow!("engine thread is gone"))?;
        futures::executor::block_on(rx).map_err(|_err| anyhow!("engine dropped the reply"))
    }
}

fn update_requests(submission: &Mutex<Submission>, update: impl FnOnce(&mut RequestDiagnostics)) {
    update(&mut submission.lock().unwrap_or_else(|error| error.into_inner()).requests);
}

fn clear_caches(specs_dir: &Path, engine: &mut Option<Engine>, registry_template: &mut Option<Registry>) -> bool {
    if let Some(engine) = engine.as_mut() {
        // Keep the old snapshot/template when the canonical directory is
        // temporarily absent during publication. A failed reset must not
        // turn a valid generation into an empty/stale worker state.
        if engine.clear_caches_and_report() {
            *registry_template = None;
            return true;
        }
        return false;
    }

    // A timed-out attempt may have left only the pristine template. Refresh
    // it when the new generation is available, but retain the last good one
    // across a missing-window so the next completion can still use it.
    if let Ok(registry) = Engine::load_registry(specs_dir) {
        *registry_template = Some(registry);
    }
    false
}

impl AttemptFailure {
    fn error(self, timeout: Duration) -> anyhow::Error {
        match self {
            Self::TimedOut => anyhow!(
                "completion attempt timed out after {}ms; engine reset",
                timeout.as_millis()
            ),
            Self::Panicked => anyhow!("completion attempt panicked; engine reset"),
        }
    }
}

/// Run one completion in an isolated thread.  The engine is returned with the
/// result so successful attempts retain the registry/frecency caches.  If the
/// attempt gets stuck, its thread (and the engine it owns) is intentionally
/// abandoned; the supervisor can then rebuild a fresh engine and accept the
/// next request.
#[cfg(test)]
fn run_engine_attempt(engine: Engine, request: CompleteRequest, timeout: Duration) -> AttemptResult {
    run_engine_attempt_with_token(engine, request, timeout, CancellationToken::new())
}

fn run_engine_attempt_with_token(
    engine: Engine,
    request: CompleteRequest,
    timeout: Duration,
    token: CancellationToken,
) -> AttemptResult {
    run_attempt(engine, request, timeout, move |mut engine, request| {
        let _scope = crate::cancellation::enter(token);
        let result = engine.complete(request);
        (engine, result)
    })
}

fn run_attempt<F>(engine: Engine, request: CompleteRequest, timeout: Duration, complete: F) -> AttemptResult
where
    F: FnOnce(Engine, CompleteRequest) -> (Engine, anyhow::Result<CompleteResult>) + Send + 'static,
{
    let (tx, rx) = mpsc::sync_channel(1);
    let spawn_result = thread::Builder::new()
        .name("ec-engine-attempt".into())
        .stack_size(ENGINE_THREAD_STACK)
        .spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| complete(engine, request)));
            let message = match outcome {
                Ok(result) => Ok(result),
                Err(_) => Err(()),
            };
            let _ = tx.send(message);
        });
    if spawn_result.is_err() {
        return Err(AttemptFailure::Panicked);
    }

    match rx.recv_timeout(timeout) {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(())) | Err(mpsc::RecvTimeoutError::Disconnected) => Err(AttemptFailure::Panicked),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(AttemptFailure::TimedOut),
    }
}

/// Root command plus cwd, enough to identify which spec's generator wedged
/// without copying the whole edit buffer into the log.
fn attempt_log_context(request: &CompleteRequest) -> String {
    let root = request.buffer.split_whitespace().next().unwrap_or("");
    format!("root_command={root:?} cwd={:?}", request.cwd)
}

fn record_acceptance(
    engine: &mut Option<Engine>,
    acceptance: &Arc<Mutex<AcceptanceIndex>>,
    root_command: &str,
    accepted_name: &str,
    timestamp: u64,
) {
    if let Some(engine) = engine.as_mut() {
        engine.record_acceptance_at(root_command, accepted_name, timestamp);
    } else {
        // As in `Engine::record_acceptance_at`: persist a snapshot outside
        // the lock so a slow SQLite write cannot stall threads cloning the
        // index for ranking. This runs on the supervisor thread, where any
        // stall also delays every queued completion.
        let snapshot = {
            let mut index = acceptance.lock().unwrap_or_else(|err| err.into_inner());
            index
                .record_at(root_command, accepted_name, timestamp)
                .then(|| index.clone())
        };
        if let Some(snapshot) = snapshot {
            snapshot.persist();
        }
    }
}

fn record_scoped_acceptance(
    engine: &mut Option<Engine>,
    acceptance: &Arc<Mutex<AcceptanceIndex>>,
    scope: &str,
    accepted_name: &str,
    timestamp: u64,
) {
    if let Some(engine) = engine.as_mut() {
        engine.record_scoped_acceptance_at(scope, accepted_name, timestamp);
    } else {
        let snapshot = {
            let mut index = acceptance.lock().unwrap_or_else(|err| err.into_inner());
            index
                .record_scoped_at(scope, accepted_name, timestamp)
                .then(|| index.scoped_snapshot())
        };
        if let Some(snapshot) = snapshot {
            snapshot.persist();
        }
    }
}

/// Build an engine from the last known generation when it is still current;
/// refresh a stale template before handing it to a replacement attempt.
///
/// A timed-out attempt keeps the engine it was given, so the supervisor has to
/// construct a fresh one. Re-reading the index on every reset made the first
/// completion after a wedged generator slow enough to show the loading marker
/// again, which reads to the user as the overlay never recovering.
fn rebuild_engine(
    specs_dir: &Path,
    template: &mut Option<Registry>,
    acceptance: &Arc<Mutex<AcceptanceIndex>>,
) -> anyhow::Result<Engine> {
    let registry = match template {
        Some(registry) if !registry.needs_refresh() => registry.clone(),
        Some(registry) => {
            // A successful generation replacement must not leave a timed-out
            // worker rebuilding from the old template. If the publisher is
            // in its brief missing window, keep that last good template as a
            // fallback; Engine::complete will retry the atomic rebind at the
            // next request boundary.
            let previous = registry.clone();
            match Engine::load_registry(specs_dir) {
                Ok(registry) => {
                    *template = Some(registry.clone());
                    registry
                },
                Err(error) => {
                    tracing::debug!(%error, "engine rebuild deferred while retaining the last registry template");
                    previous
                },
            }
        },
        None => {
            let registry = Engine::load_registry(specs_dir)?;
            template.get_or_insert(registry).clone()
        },
    };
    Ok(Engine::from_registry(specs_dir, registry, acceptance.clone()))
}

// Control traffic can keep recv_timeout ready forever. Give expired trees a
// bounded maintenance opportunity without moving a control across a request.
const MAX_JOBS_WITHOUT_ATTEMPT: usize = 64;

fn maintain_idle_between_queued_jobs(
    engine: Option<&mut Engine>,
    head: &Job,
    jobs_without_attempt: &mut usize,
    now: Instant,
    grace: Duration,
) {
    *jobs_without_attempt = jobs_without_attempt.saturating_add(1);
    let live_completion = matches!(&head.kind, JobKind::Complete { token, reply, .. }
        if !token.is_cancelled() && !reply.is_canceled());
    if *jobs_without_attempt < MAX_JOBS_WITHOUT_ATTEMPT || live_completion {
        return;
    }
    if let Some(engine) = engine {
        engine.release_idle_specs(now, grace);
    }
    *jobs_without_attempt = 0;
}

/// Block until the next job, or until the earliest idle deadline.
/// A job already queued is returned without waiting out the grace.
fn wait_for_engine_job(
    rx: &mpsc::Receiver<Job>,
    engine: Option<&Engine>,
    grace: Duration,
) -> Result<Job, mpsc::RecvTimeoutError> {
    let Some(deadline) = engine.and_then(|engine| engine.next_idle_deadline(grace)) else {
        return rx.recv().map_err(|_disconnected| mpsc::RecvTimeoutError::Disconnected);
    };
    let timeout = deadline.saturating_duration_since(Instant::now());
    rx.recv_timeout(timeout)
}

#[cfg(test)]
fn idle_file_state(engine: Option<&Engine>, relative: &str) -> IdleFileState {
    match engine {
        Some(engine) => {
            let (cached, mark) = engine.idle_file_state(relative);
            IdleFileState { cached, mark }
        },
        None => IdleFileState {
            cached: false,
            mark: None,
        },
    }
}

pub fn default_specs_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("EC_SPECS_DIR") {
        return PathBuf::from(dir);
    }
    if let Ok(dir) = bundled_specs_ir_dir() {
        return dir;
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bundle/specs-ir")
}

fn bundled_specs_ir_dir() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    match exe.parent().filter(|dir| dir.ends_with("MacOS")) {
        Some(macos) => Ok(specs_ir_in_bundle(macos)),
        None => anyhow::bail!("not running from an app bundle"),
    }
}

/// `Fastab.app/Contents/MacOS` → `Contents/Resources/specs-ir`. Only the
/// compiled IR is bundled; the JS specs it was built from never enter the `.app`,
/// so pointing at `Resources/specs` would silently yield an empty registry.
fn specs_ir_in_bundle(macos_dir: &Path) -> PathBuf {
    macos_dir.join("../Resources/specs-ir")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completion_job(request: CompleteRequest, reply: oneshot::Sender<anyhow::Result<CompleteResult>>) -> Job {
        Job {
            kind: JobKind::Complete {
                request,
                reply,
                token: CancellationToken::new(),
                session: SessionId::ANONYMOUS,
                sequence: 0,
            },
        }
    }

    #[test]
    fn the_ui_deadline_tracks_the_script_budget_not_the_supervisor_floor() {
        // The supervisor waits 30s before declaring a worker wedged. Showing
        // `···` for that long reads as a hang, so the overlay follows the
        // user's own script budget instead.
        assert_eq!(ui_completion_deadline_for(5_000), Duration::from_secs(6));
        assert!(ui_completion_deadline_for(5_000) < engine_attempt_timeout_for(5_000));
    }

    #[test]
    fn a_long_script_budget_extends_the_ui_deadline() {
        assert_eq!(ui_completion_deadline_for(15_000), Duration::from_secs(16));
    }

    #[test]
    fn a_tiny_script_budget_still_leaves_the_engine_time_to_answer() {
        assert_eq!(ui_completion_deadline_for(0), MIN_UI_DEADLINE);
        assert_eq!(ui_completion_deadline_for(-1), MIN_UI_DEADLINE);
    }

    #[test]
    fn rebuilding_the_engine_indexes_the_specs_directory_once() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("git.json"), r#"{"names":["git"]}"#).unwrap();
        std::fs::write(dir.path().join("index.json"), r#"{"files":{"git":"git.json"}}"#).unwrap();
        let acceptance = Arc::new(Mutex::new(AcceptanceIndex::default()));
        let mut template = None;

        rebuild_engine(dir.path(), &mut template, &acceptance).expect("first build");
        assert!(template.is_some(), "the index should be retained for reuse");

        // Deleting the index proves the rebuild came from the cached template
        // rather than the disk, which is what keeps the first completion after
        // a timed-out attempt fast.
        std::fs::remove_file(dir.path().join("index.json")).unwrap();
        let engine = rebuild_engine(dir.path(), &mut template, &acceptance).expect("rebuild without disk");
        assert!(!engine.registry().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn rebuilding_engine_does_not_reuse_a_stale_registry_template() {
        let root = tempfile::tempdir().unwrap();
        let generation_a = root.path().join("generation-a");
        let generation_b = root.path().join("generation-b");
        for (dir, child) in [(&generation_a, "from-a"), (&generation_b, "from-b")] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(
                dir.join("git.json"),
                serde_json::json!({
                    "names": ["git"],
                    "subcommands": [{"names": [child]}]
                })
                .to_string(),
            )
            .unwrap();
            std::fs::write(dir.join("index.json"), r#"{"files":{"git":"git.json"}}"#).unwrap();
        }
        let canonical = root.path().join("specs-ir");
        let backup = root.path().join("specs-ir.backup");
        std::fs::rename(&generation_a, &canonical).unwrap();
        let acceptance = Arc::new(Mutex::new(AcceptanceIndex::default()));
        let mut template = None;
        let _engine_a = rebuild_engine(&canonical, &mut template, &acceptance).expect("generation A");

        // The old template is retained for a missing-window fallback, but a
        // stable replacement must be indexed before a timed-out rebuild uses
        // it again.
        std::fs::rename(&canonical, &backup).unwrap();
        std::fs::rename(&generation_b, &canonical).unwrap();
        let mut engine_b = rebuild_engine(&canonical, &mut template, &acceptance).expect("generation B");
        let result = engine_b
            .complete(CompleteRequest {
                buffer: "git ".into(),
                cwd: "/tmp".into(),
                include_history: false,
                ..CompleteRequest::default()
            })
            .expect("generation B completion");
        assert!(
            result.suggestions.iter().any(|suggestion| suggestion.name == "from-b"),
            "the stale A template must not survive a stable B replacement"
        );
        assert!(!template.as_ref().expect("refreshed template").needs_refresh());

        // Cleanup must not be needed for the safety assertion, but mirrors
        // the publisher's backup lifecycle.
        std::fs::remove_dir_all(backup).unwrap();
    }

    #[test]
    fn synchronous_submission_cancels_only_older_tasks_and_preserves_control_order() {
        let (tx, rx) = mpsc::channel();
        let client = EngineClient {
            tx,
            acceptance: Arc::new(Mutex::new(AcceptanceIndex::default())),
            submission: Arc::new(Mutex::new(Submission::default())),
        };
        let older = client.complete_for_session(SessionId::new(1), CompleteRequest::default());
        client.clear_caches().unwrap();
        let diagnostic = client.diagnostics();
        let newer = client.complete_for_session(SessionId::new(2), CompleteRequest::default());
        assert!(older.token.is_cancelled());
        drop(older);
        assert!(!newer.token.is_cancelled());
        assert!(matches!(rx.recv().unwrap().kind, JobKind::Complete { sequence: 1, .. }));
        assert!(matches!(rx.recv().unwrap().kind, JobKind::ClearCaches));
        assert!(matches!(rx.recv().unwrap().kind, JobKind::Diagnostics { .. }));
        assert!(matches!(rx.recv().unwrap().kind, JobKind::Complete { sequence: 2, .. }));
        drop(diagnostic);
        let token = newer.token.clone();
        drop(newer);
        assert!(token.is_cancelled(), "even an unpolled task cancels its own work");
    }

    #[test]
    fn queued_controls_and_cancelled_jobs_cannot_starve_idle_release_or_reorder_live_work() {
        let dir = tempfile::tempdir().unwrap();
        write_idle_specs(dir.path());
        let now = Instant::now();
        let mut registry = Registry::load(dir.path()).unwrap();
        let child = registry.load_referenced_spec("child").unwrap();
        let weak = Arc::downgrade(&child);
        drop(child);
        registry.set_idle_since("child", Some(now));
        let mut engine = Engine::from_registry(dir.path(), registry, Arc::new(Mutex::new(AcceptanceIndex::default())));
        let (tx, rx) = mpsc::channel();
        let mut receivers = Vec::new();
        for index in 0..MAX_JOBS_WITHOUT_ATTEMPT {
            if index % 2 == 0 {
                let (reply, receiver) = oneshot::channel();
                receivers.push(receiver);
                tx.send(Job {
                    kind: JobKind::Diagnostics { reply },
                })
                .unwrap();
            } else {
                let (reply, receiver) = oneshot::channel();
                let job = completion_job(CompleteRequest::default(), reply);
                if let JobKind::Complete { token, .. } = &job.kind {
                    token.cancel();
                }
                tx.send(job).unwrap();
                drop(receiver);
            }
        }
        let (live_reply, live_receiver) = oneshot::channel();
        tx.send(completion_job(CompleteRequest::default(), live_reply)).unwrap();
        tx.send(Job {
            kind: JobKind::ClearCaches,
        })
        .unwrap();
        let mut streak = 0;
        for _ in 0..MAX_JOBS_WITHOUT_ATTEMPT {
            let head = rx.recv().unwrap();
            maintain_idle_between_queued_jobs(
                Some(&mut engine),
                &head,
                &mut streak,
                now + Duration::from_secs(1),
                Duration::ZERO,
            );
        }
        assert!(
            weak.upgrade().is_none(),
            "a continuously nonempty queue must still release an expired file"
        );
        assert!(matches!(rx.recv().unwrap().kind, JobKind::Complete { .. }));
        assert!(matches!(rx.recv().unwrap().kind, JobKind::ClearCaches));
        assert!(rx.try_recv().is_err());
        drop(live_receiver);

        // At the same threshold a live FIFO head wins, so its cached tree
        // remains available until the attempt can touch it again.
        let mut registry = Registry::load(dir.path()).unwrap();
        let child = registry.load_referenced_spec("child").unwrap();
        let weak = Arc::downgrade(&child);
        drop(child);
        registry.set_idle_since("child", Some(now));
        let mut engine = Engine::from_registry(dir.path(), registry, Arc::new(Mutex::new(AcceptanceIndex::default())));
        let (reply, receiver) = oneshot::channel();
        let live = completion_job(CompleteRequest::default(), reply);
        let mut streak = MAX_JOBS_WITHOUT_ATTEMPT;
        maintain_idle_between_queued_jobs(
            Some(&mut engine),
            &live,
            &mut streak,
            now + Duration::from_secs(1),
            Duration::ZERO,
        );
        assert!(weak.upgrade().is_some(), "a live completion must precede maintenance");
        drop(receiver);
    }

    #[test]
    fn end_input_is_ordered_with_submission_and_does_not_initialize_engine() {
        let (tx, rx) = mpsc::channel();
        let client = EngineClient {
            tx,
            acceptance: Arc::new(Mutex::new(AcceptanceIndex::default())),
            submission: Arc::new(Mutex::new(Submission::default())),
        };
        let old = client.complete_for_session(SessionId::new(1), CompleteRequest::default());
        client.end_input(SessionId::new(1)).unwrap();
        assert!(old.token.is_cancelled());
        client.end_input(SessionId::new(1)).unwrap();
        let fresh = client.complete_for_session(SessionId::new(1), CompleteRequest::default());
        assert!(!fresh.token.is_cancelled());
        assert!(matches!(rx.recv().unwrap().kind, JobKind::Complete { .. }));
        assert!(matches!(rx.recv().unwrap().kind, JobKind::EndInput { .. }));
        assert!(matches!(rx.recv().unwrap().kind, JobKind::EndInput { .. }));
        assert!(matches!(rx.recv().unwrap().kind, JobKind::Complete { .. }));
        assert!(rx.try_recv().is_err());

        let dir = tempfile::tempdir().unwrap();
        let lazy = EngineClient::spawn(dir.path().to_path_buf()).unwrap();
        lazy.end_input(SessionId::new(1)).unwrap();
        assert!(
            futures::executor::block_on(lazy.diagnostics())
                .unwrap()
                .engine
                .is_none()
        );
    }

    #[test]
    fn background_end_and_repeated_end_cannot_change_current_input_or_restart_grace() {
        let dir = tempfile::tempdir().unwrap();
        write_idle_specs(dir.path());
        let client = EngineClient::spawn(dir.path().to_path_buf()).unwrap();
        let request = CompleteRequest {
            buffer: "tool child ".into(),
            cwd: dir.path().display().to_string(),
            ..CompleteRequest::default()
        };
        futures::executor::block_on(client.complete_for_session(SessionId::new(1), request.clone())).unwrap();
        client.end_input(SessionId::new(2)).unwrap();
        assert_eq!(
            client.inspect_idle("child").unwrap().mark,
            Some(None),
            "an empty background session must not guess the owner ended"
        );
        client.end_input(SessionId::new(1)).unwrap();
        let since = client.inspect_idle("child").unwrap().mark.unwrap().unwrap();
        client.end_input(SessionId::new(1)).unwrap();
        assert_eq!(client.inspect_idle("child").unwrap().mark, Some(Some(since)));
        futures::executor::block_on(client.complete_for_session(SessionId::new(2), request)).unwrap();
        client.end_input(SessionId::new(1)).unwrap();
        assert_eq!(client.inspect_idle("child").unwrap().mark, Some(None));
    }

    #[cfg(unix)]
    #[test]
    fn end_input_cancels_the_running_owner_and_old_completion_cannot_restore_activity() {
        let dir = tempfile::tempdir().unwrap();
        slow_fixture(dir.path());
        let client = EngineClient::spawn(dir.path().to_path_buf()).unwrap();
        let old = client.complete_for_session(SessionId::new(1), fixture_request(dir.path(), "slow "));
        await_fixture_start(dir.path());
        client.end_input(SessionId::new(1)).unwrap();
        assert!(
            futures::executor::block_on(old)
                .unwrap_err()
                .is::<CompletionCancelled>()
        );
        let mark = client.inspect_idle("slow").unwrap().mark.unwrap();
        assert!(
            mark.is_some(),
            "cancelled attempt cannot restore an active marker after end"
        );
        std::fs::write(dir.path().join("allow"), "").unwrap();
        let next = futures::executor::block_on(
            client.complete_for_session(SessionId::new(1), fixture_request(dir.path(), "slow ")),
        )
        .unwrap();
        assert!(next.suggestions.iter().any(|row| row.name == "recovered"));
        assert_eq!(client.inspect_idle("slow").unwrap().mark, Some(None));
    }

    #[cfg(unix)]
    #[test]
    fn cancelled_new_session_cannot_swallow_the_active_sessions_end() {
        let dir = tempfile::tempdir().unwrap();
        slow_fixture(dir.path());
        let client = EngineClient::spawn_with_idle_grace(
            dir.path().to_path_buf(),
            WATCHDOG_UNDER_TEST,
            Duration::from_millis(100),
        )
        .unwrap();
        futures::executor::block_on(
            client.complete_for_session(SessionId::new(1), fixture_request(dir.path(), "fast ")),
        )
        .unwrap();
        assert_eq!(client.inspect_idle("fast").unwrap().mark, Some(None));
        let abandoned = client.complete_for_session(SessionId::new(2), fixture_request(dir.path(), "slow "));
        await_fixture_start(dir.path());
        drop(abandoned);
        client.end_input(SessionId::new(1)).unwrap();
        let state = client.inspect_idle("fast").unwrap();
        assert!(
            state.mark.is_none_or(|mark| mark.is_some()),
            "old active owner must enter grace"
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        while futures::executor::block_on(client.diagnostics())
            .unwrap()
            .engine
            .unwrap()
            .registry
            .cached_file_count
            != 0
        {
            assert!(
                Instant::now() < deadline,
                "cancelled session swallowed the resource owner's end"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let counts = futures::executor::block_on(client.diagnostics()).unwrap().requests;
        assert_eq!(counts.engine_initializations, 1);
        assert_eq!(counts.cancelled, 1);
    }

    #[cfg(unix)]
    #[test]
    fn committed_session_wins_over_older_end_after_an_interrupted_attempt() {
        let dir = tempfile::tempdir().unwrap();
        slow_fixture(dir.path());
        let client = EngineClient::spawn(dir.path().to_path_buf()).unwrap();
        futures::executor::block_on(
            client.complete_for_session(SessionId::new(1), fixture_request(dir.path(), "fast ")),
        )
        .unwrap();
        let old = client.complete_for_session(SessionId::new(1), fixture_request(dir.path(), "slow "));
        await_fixture_start(dir.path());
        let newer = client.complete_for_session(SessionId::new(2), fixture_request(dir.path(), "fast "));
        client.end_input(SessionId::new(1)).unwrap();
        assert!(!newer.token.is_cancelled(), "ending A must not cancel B");
        assert!(
            futures::executor::block_on(old)
                .unwrap_err()
                .is::<CompletionCancelled>()
        );
        assert!(
            futures::executor::block_on(newer)
                .unwrap()
                .suggestions
                .iter()
                .any(|row| row.name == "ready")
        );
        assert_eq!(client.inspect_idle("fast").unwrap().mark, Some(None));

        // A history-only success has no normal idle commit and cannot steal
        // the resource owner from B, even though it is the latest submission.
        let mut history = fixture_request(dir.path(), "fast ");
        history.history_only = true;
        futures::executor::block_on(client.complete_for_session(SessionId::new(3), history)).unwrap();
        client.end_input(SessionId::new(3)).unwrap();
        assert_eq!(client.inspect_idle("fast").unwrap().mark, Some(None));
        client.end_input(SessionId::new(2)).unwrap();
        assert!(client.inspect_idle("fast").unwrap().mark.unwrap().is_some());
    }

    #[cfg(unix)]
    fn slow_fixture(dir: &Path) {
        let body = serde_json::json!({
            "names": ["slow"], "args": [{
                "name": "value", "cacheStrategy": "max-age", "splitOn": "\n",
                "script": ["/bin/sh", "-c",
                    "if [ ! -e \"$2\" ]; then : > \"$1\"; while :; do sleep 1; done; fi; printf 'recovered\\n'",
                    "cancel-fixture", dir.join("started"), dir.join("allow")]
            }]
        });
        std::fs::write(dir.join("slow.json"), body.to_string()).unwrap();
        std::fs::write(
            dir.join("fast.json"),
            r#"{"names":["fast"],"subcommands":[{"names":["ready"]}]}"#,
        )
        .unwrap();
    }

    #[cfg(unix)]
    fn await_fixture_start(dir: &Path) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !dir.join("started").exists() {
            assert!(Instant::now() < deadline, "real script did not signal startup");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[cfg(unix)]
    fn fixture_request(dir: &Path, buffer: &str) -> CompleteRequest {
        CompleteRequest {
            buffer: buffer.into(),
            cwd: dir.display().to_string(),
            ..CompleteRequest::default()
        }
    }

    #[cfg(unix)]
    #[test]
    fn newer_input_cancels_a_running_script_without_resetting_engine_or_caching_empty_rows() {
        let dir = tempfile::tempdir().unwrap();
        slow_fixture(dir.path());
        let client = EngineClient::spawn(dir.path().to_path_buf()).unwrap();
        assert!(
            futures::executor::block_on(client.diagnostics())
                .unwrap()
                .engine
                .is_none()
        );
        let old = client.complete_for_session(SessionId::new(1), fixture_request(dir.path(), "slow "));
        await_fixture_start(dir.path());
        let latest = client.complete_for_session(SessionId::new(2), fixture_request(dir.path(), "fast "));
        let error = futures::executor::block_on(old).unwrap_err();
        assert!(error.is::<CompletionCancelled>(), "{error}");
        let result = futures::executor::block_on(latest).unwrap();
        assert!(result.suggestions.iter().any(|row| row.name == "ready"));
        let snapshot = futures::executor::block_on(client.diagnostics()).unwrap();
        assert_eq!(snapshot.requests.engine_initializations, 1);
        assert_eq!(
            (
                snapshot.requests.cancelled,
                snapshot.requests.completed,
                snapshot.requests.failed
            ),
            (1, 1, 0)
        );
        assert_eq!(snapshot.engine.unwrap().hooks.script_output.entries, 0);
        std::fs::write(dir.path().join("allow"), "").unwrap();
        let retried = client.complete_blocking(fixture_request(dir.path(), "slow ")).unwrap();
        assert!(
            retried.suggestions.iter().any(|row| row.name == "recovered"),
            "must rerun instead of replaying cancelled generator state"
        );
    }

    #[cfg(unix)]
    #[test]
    fn dropping_a_running_task_releases_its_new_tree_without_another_completion() {
        let dir = tempfile::tempdir().unwrap();
        slow_fixture(dir.path());
        let client = EngineClient::spawn_with_options(
            dir.path().to_path_buf(),
            EngineClientOptions {
                spec_idle_grace: Duration::from_millis(40),
            },
        )
        .unwrap();
        let task = client.complete(fixture_request(dir.path(), "slow "));
        await_fixture_start(dir.path());
        drop(task);
        let returned = futures::executor::block_on(client.diagnostics()).unwrap();
        assert_eq!(returned.requests.cancelled, 1);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            thread::sleep(Duration::from_millis(20));
            let snapshot = futures::executor::block_on(client.diagnostics()).unwrap();
            assert_eq!(snapshot.requests.submitted, 1);
            if snapshot.engine.unwrap().registry.cached_file_count == 0 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "cancelled first load must get an idle deadline"
            );
        }
    }

    #[test]
    fn spawn_completes_on_a_plain_thread() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("git.json"),
            r#"{"names":["git"],"subcommands":[{"names":["status"]}]}"#,
        )
        .unwrap();
        let engine = EngineClient::spawn(dir.path().to_path_buf()).expect("spawn");
        let result = engine
            .complete_blocking(CompleteRequest {
                buffer: "git ".into(),
                cwd: dir.path().display().to_string(),
                ..CompleteRequest::default()
            })
            .expect("complete");
        assert!(
            result.suggestions.iter().any(|s| s.name == "status"),
            "{:?}",
            result.suggestions
        );
    }

    fn counting_script(count: &std::path::Path) -> String {
        format!("printf 'src\\n'; printf x >> '{}'", count.display())
    }

    /// Watchdog for the reset tests. The request after a reset runs on a
    /// fresh engine that still has to load shell history, so this needs
    /// real headroom over that on a loaded test host; 100ms did not have it.
    const WATCHDOG_UNDER_TEST: Duration = Duration::from_secs(1);

    #[test]
    fn generator_session_survives_attempt_threads() {
        let dir = tempfile::tempdir().unwrap();
        let count = dir.path().join("count");
        let spec = serde_json::json!({
            "names": ["demo"],
            "args": [{
                "name": "slot",
                "script": ["sh", "-c", counting_script(&count)],
                "splitOn": "\n",
                "debounceMs": 200
            }]
        });
        std::fs::write(dir.path().join("demo.json"), spec.to_string()).unwrap();
        let engine = EngineClient::spawn(dir.path().to_path_buf()).expect("spawn");
        let cwd = dir.path().display().to_string();
        let request = CompleteRequest {
            buffer: "demo ".into(),
            cwd,
            include_history: false,
            ..CompleteRequest::default()
        };
        let first = engine.complete_blocking(request.clone()).expect("first");
        assert!(
            first.pending_generators,
            "debounce should delay the first run: {first:?}"
        );
        assert!(!count.exists() || std::fs::read_to_string(&count).unwrap().is_empty());

        let second = engine.complete_blocking(request).expect("follow-up");
        assert!(
            second.suggestions.iter().any(|row| row.name == "src"),
            "{:?}",
            second.suggestions
        );
        assert_eq!(std::fs::read_to_string(&count).unwrap().matches('x').count(), 1);
        assert!(!second.pending_generators);
    }

    #[test]
    fn clear_caches_rereads_specs_and_forgets_generator_results() {
        let dir = tempfile::tempdir().unwrap();
        let count = dir.path().join("count");
        let spec = |names: &[&str]| {
            serde_json::json!({
                "names": ["demo"],
                "subcommands": names.iter().map(|name| serde_json::json!({"names": [name]})).collect::<Vec<_>>(),
                "args": [{
                    "name": "slot",
                    "script": ["sh", "-c", counting_script(&count)],
                    "splitOn": "\n",
                    "cacheTtl": 60000
                }]
            })
        };
        std::fs::write(dir.path().join("demo.json"), spec(&["before"]).to_string()).unwrap();
        let engine = EngineClient::spawn(dir.path().to_path_buf()).expect("spawn");
        let request = CompleteRequest {
            buffer: "demo ".into(),
            cwd: dir.path().display().to_string(),
            include_history: false,
            ..CompleteRequest::default()
        };
        let first = engine.complete_blocking(request.clone()).expect("first");
        assert!(first.suggestions.iter().any(|row| row.name == "before"), "{first:?}");
        assert_eq!(std::fs::read_to_string(&count).unwrap().matches('x').count(), 1);

        // Edit the spec on disk; a cached spec and a cached script result
        // would both hide the change.
        std::fs::write(dir.path().join("demo.json"), spec(&["after"]).to_string()).unwrap();
        let stale = engine.complete_blocking(request.clone()).expect("stale");
        assert!(stale.suggestions.iter().any(|row| row.name == "before"), "{stale:?}");
        assert_eq!(std::fs::read_to_string(&count).unwrap().matches('x').count(), 1);

        engine.clear_caches().expect("clear");
        let fresh = engine.complete_blocking(request).expect("fresh");
        assert!(fresh.suggestions.iter().any(|row| row.name == "after"), "{fresh:?}");
        assert!(fresh.suggestions.iter().all(|row| row.name != "before"), "{fresh:?}");
        assert_eq!(std::fs::read_to_string(&count).unwrap().matches('x').count(), 2);
    }

    #[test]
    fn trailing_space_and_typed_token_share_generator_session_across_threads() {
        let dir = tempfile::tempdir().unwrap();
        let count = dir.path().join("count");
        let script = counting_script(&count);
        let spec = serde_json::json!({
            "names": ["git"],
            "subcommands": [
                {"names": ["add"], "args": [{"name": "pathspec", "script": ["sh", "-c", script.clone()], "splitOn": "\n"}]},
                {"names": ["rm"], "args": [{"name": "pathspec", "script": ["sh", "-c", script], "splitOn": "\n"}]}
            ]
        });
        std::fs::write(dir.path().join("git.json"), spec.to_string()).unwrap();
        let engine = EngineClient::spawn(dir.path().to_path_buf()).expect("spawn");
        let cwd = dir.path().display().to_string();
        let after_space = engine
            .complete_blocking(CompleteRequest {
                buffer: "git add ".into(),
                cwd: cwd.clone(),
                include_history: false,
                ..CompleteRequest::default()
            })
            .expect("git add ");
        let while_typing = engine
            .complete_blocking(CompleteRequest {
                buffer: "git add s".into(),
                cwd: cwd.clone(),
                include_history: false,
                ..CompleteRequest::default()
            })
            .expect("git add s");
        let other_command = engine
            .complete_blocking(CompleteRequest {
                buffer: "git rm ".into(),
                cwd,
                include_history: false,
                ..CompleteRequest::default()
            })
            .expect("git rm ");
        assert!(
            after_space.suggestions.iter().any(|row| row.name == "src"),
            "{after_space:?}"
        );
        assert!(
            while_typing.suggestions.iter().any(|row| row.name == "src"),
            "{while_typing:?}"
        );
        assert!(
            other_command.suggestions.iter().any(|row| row.name == "src"),
            "{other_command:?}"
        );
        assert_eq!(std::fs::read_to_string(&count).unwrap().matches('x').count(), 2);
    }

    #[test]
    fn supervisor_retries_engine_initialization_after_specs_are_repaired() {
        let dir = tempfile::tempdir().unwrap();
        // Registry::load parses index.json during Engine::new, so an invalid
        // index gives us a deterministic initialization failure before any
        // completion attempt starts.
        std::fs::write(dir.path().join("index.json"), b"{").unwrap();
        let client = EngineClient::spawn(dir.path().to_path_buf()).expect("spawn");

        let first = client.complete_blocking(CompleteRequest {
            buffer: "git ".into(),
            cwd: dir.path().display().to_string(),
            ..CompleteRequest::default()
        });
        let error = first.expect_err("the malformed registry should fail initialization");
        assert!(
            error.to_string().contains("completion engine initialization failed"),
            "{error}"
        );

        // Repair the directory after the first request.  A permanently
        // failed supervisor would return the old initialization error again;
        // a retrying supervisor should build the registry and complete.
        std::fs::write(dir.path().join("index.json"), r#"{"files":{"git":"git.json"}}"#).unwrap();
        std::fs::write(
            dir.path().join("git.json"),
            r#"{"names":["git"],"subcommands":[{"names":["status"]}]}"#,
        )
        .unwrap();
        let second = client
            .complete_blocking(CompleteRequest {
                buffer: "git ".into(),
                cwd: dir.path().display().to_string(),
                ..CompleteRequest::default()
            })
            .expect("the repaired registry should be retried");
        assert!(second.suggestions.iter().any(|suggestion| suggestion.name == "status"));
    }

    #[test]
    fn cancelled_latest_job_is_detected_before_running_an_attempt() {
        let (tx, rx) = mpsc::channel::<Job>();
        let (reply, caller) = oneshot::channel();
        tx.send(completion_job(CompleteRequest::default(), reply)).unwrap();
        drop(caller);

        let job = rx.recv().unwrap();
        let JobKind::Complete { reply, .. } = job.kind else {
            unreachable!("job should be a completion")
        };
        assert!(
            reply.is_canceled(),
            "a cancelled caller must be observable before execution"
        );
    }

    /// Reproduces the desktop's shape exactly: a tokio `block_on` whose poll
    /// never returns, and inside it a foreign executor driving `complete()`
    /// futures to completion by hand. tokio's budget for that poll is 128 and
    /// is never refilled, so a `tokio::sync::oneshot` reply stops resolving on
    /// the 129th round and wakes itself forever instead — the livelock that
    /// pinned the desktop at 100% CPU after a few hours of typing.
    #[test]
    fn replies_keep_resolving_after_tokio_exhausts_its_budget_on_the_polling_thread() {
        use std::task::{Context, Poll, Waker};

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("git.json"),
            r#"{"names":["git"],"subcommands":[{"names":["status"]}]}"#,
        )
        .unwrap();
        let client = EngineClient::spawn(dir.path().to_path_buf()).expect("spawn");
        let request = CompleteRequest {
            buffer: "git ".into(),
            cwd: dir.path().display().to_string(),
            ..CompleteRequest::default()
        };

        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        runtime.block_on(async {
            // Well past tokio's 128-unit budget. Each round polls the reply
            // future directly, never yielding back to `block_on`.
            for round in 0..200u32 {
                let mut future = Box::pin(client.complete(request.clone()));
                // A livelocked reply never resolves, so a wall-clock bound
                // catches it; a spin count did not survive a loaded test
                // host, where the first round also pays for the engine's
                // history load.
                let deadline = std::time::Instant::now() + Duration::from_secs(20);
                let result = loop {
                    match future.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
                        Poll::Ready(result) => break result,
                        Poll::Pending => {
                            assert!(
                                std::time::Instant::now() < deadline,
                                "round {round}: the reply future stopped making progress on this thread"
                            );
                            thread::yield_now();
                        },
                    }
                };
                let result = result.expect("completion");
                assert!(result.suggestions.iter().any(|s| s.name == "status"), "round {round}");
            }
        });
    }

    #[test]
    fn supervisor_resets_after_a_timeout_before_serving_the_next_request() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("hang.json"),
            r#"{"names":["hang"],"args":[{"script":["sleep","5"]}]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("git.json"),
            r#"{"names":["git"],"subcommands":[{"names":["status"]}]}"#,
        )
        .unwrap();

        let client = EngineClient::spawn_with_timeout(dir.path().to_path_buf(), WATCHDOG_UNDER_TEST).expect("spawn");
        let first = client.complete_blocking(CompleteRequest {
            buffer: "hang ".into(),
            cwd: dir.path().display().to_string(),
            ..CompleteRequest::default()
        });
        let error = first.expect_err("the hanging attempt should time out");
        assert!(error.to_string().contains("timed out"), "{error}");

        let second = client
            .complete_blocking(CompleteRequest {
                buffer: "git ".into(),
                cwd: dir.path().display().to_string(),
                ..CompleteRequest::default()
            })
            .expect("the reset engine should serve the next request");
        assert!(second.suggestions.iter().any(|suggestion| suggestion.name == "status"));
    }

    #[test]
    fn a_reset_after_a_timeout_survives_the_specs_directory_going_bad() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("hang.json"),
            r#"{"names":["hang"],"args":[{"script":["sleep","5"]}]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("git.json"),
            r#"{"names":["git"],"subcommands":[{"names":["status"]}]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.json"),
            r#"{"files":{"git":"git.json","hang":"hang.json"}}"#,
        )
        .unwrap();

        let client = EngineClient::spawn_with_timeout(dir.path().to_path_buf(), WATCHDOG_UNDER_TEST).expect("spawn");
        let first = client.complete_blocking(CompleteRequest {
            buffer: "hang ".into(),
            cwd: dir.path().display().to_string(),
            ..CompleteRequest::default()
        });
        assert!(
            first
                .expect_err("the hanging attempt should time out")
                .to_string()
                .contains("timed out")
        );

        // The reset rebuilds from the index cached at startup, so corrupting
        // the directory afterwards cannot take completions down with it. A
        // directory that never loaded still retries — see
        // `supervisor_retries_engine_initialization_after_specs_are_repaired`.
        std::fs::write(dir.path().join("index.json"), b"{").unwrap();
        let second = client
            .complete_blocking(CompleteRequest {
                buffer: "git ".into(),
                cwd: dir.path().display().to_string(),
                ..CompleteRequest::default()
            })
            .expect("the cached index should carry the reset engine");
        assert!(second.suggestions.iter().any(|suggestion| suggestion.name == "status"));
    }

    #[test]
    fn timed_out_attempt_can_be_followed_by_a_fresh_engine_attempt() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("git.json"),
            r#"{"names":["git"],"subcommands":[{"names":["status"]}]}"#,
        )
        .unwrap();
        let engine = Engine::new(dir.path().to_path_buf()).unwrap();
        let timed_out = run_attempt(
            engine,
            CompleteRequest::default(),
            Duration::from_millis(10),
            |engine, _request| {
                thread::sleep(Duration::from_millis(80));
                (engine, Ok(CompleteResult::default()))
            },
        );
        assert!(matches!(timed_out, Err(AttemptFailure::TimedOut)));

        // The timed-out attempt owns the old engine, so the supervisor's reset
        // path creates a new one before accepting the next request.
        let fresh_engine = Engine::new(dir.path().to_path_buf()).unwrap();
        let (_, result) = run_engine_attempt(
            fresh_engine,
            CompleteRequest {
                buffer: "git ".into(),
                cwd: dir.path().display().to_string(),
                ..CompleteRequest::default()
            },
            Duration::from_secs(1),
        )
        .expect("fresh attempt should not be blocked by the old one");
        let result = result.expect("fresh completion should succeed");
        assert!(result.suggestions.iter().any(|suggestion| suggestion.name == "status"));
    }

    #[test]
    fn default_watchdog_does_not_cut_off_the_legacy_five_second_generator_budget() {
        assert!(engine_attempt_timeout() >= Duration::from_secs(5));
        assert_eq!(engine_attempt_timeout_for(5_000), Duration::from_secs(30));
        assert_eq!(engine_attempt_timeout_for(60_000), Duration::from_secs(65));
        assert_eq!(engine_attempt_timeout_for(-1), Duration::from_secs(30));
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::new(dir.path().to_path_buf()).unwrap();
        let result = run_attempt(
            engine,
            CompleteRequest::default(),
            engine_attempt_timeout(),
            |engine, _request| {
                // This is deliberately just beyond the old three-second
                // watchdog while remaining below the legacy 5s default.
                thread::sleep(Duration::from_millis(3_200));
                (engine, Ok(CompleteResult::default()))
            },
        )
        .expect("the attempt should remain within the default watchdog");
        assert!(result.1.is_ok());
    }

    #[test]
    fn panicked_attempt_returns_an_error_and_does_not_block_reset() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::new(dir.path().to_path_buf()).unwrap();
        let panicked = run_attempt(
            engine,
            CompleteRequest::default(),
            Duration::from_secs(1),
            |_engine, _request| panic!("simulated completion panic"),
        );
        assert!(matches!(panicked, Err(AttemptFailure::Panicked)));

        let fresh_engine = Engine::new(dir.path().to_path_buf()).unwrap();
        let (_, result) = run_engine_attempt(fresh_engine, CompleteRequest::default(), Duration::from_secs(1))
            .expect("fresh attempt should run after a panic");
        assert!(result.is_ok());
    }

    fn write_idle_specs(dir: &std::path::Path) {
        std::fs::write(
            dir.join("tool.json"),
            r#"{"names":["tool"],"subcommands":[{"names":["child"],"loadSpec":"child"}]}"#,
        )
        .unwrap();
        std::fs::write(dir.join("child.json"), r#"{"names":["child"],"description":"nested"}"#).unwrap();
        std::fs::write(
            dir.join("git.json"),
            r#"{"names":["git"],"subcommands":[{"names":["status"]}]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("index.json"),
            r#"{"files":{"tool":"tool.json","git":"git.json"}}"#,
        )
        .unwrap();
    }

    fn complete_buf(client: &EngineClient, buffer: &str) {
        client
            .complete_blocking(CompleteRequest {
                buffer: buffer.into(),
                cwd: "/".into(),
                include_history: false,
                ..CompleteRequest::default()
            })
            .expect("complete");
    }

    #[test]
    fn supervisor_releases_an_idle_file_without_another_completion() {
        let dir = tempfile::tempdir().unwrap();
        write_idle_specs(dir.path());
        let grace = Duration::from_millis(200);
        let client =
            EngineClient::spawn_with_idle_grace(dir.path().to_path_buf(), WATCHDOG_UNDER_TEST, grace).expect("spawn");
        complete_buf(&client, "tool child ");
        complete_buf(&client, "git status");
        let pending = client.inspect_idle("child").expect("inspect");
        assert!(pending.cached, "the grace keeps the child");
        assert!(pending.mark.expect("tracked").is_some(), "git status starts the grace");

        // Stay quiet so the supervisor's own deadline fires. An inspect in
        // this window would be a job and would restart the wait.
        thread::sleep(grace + Duration::from_millis(80));
        let mut released = client.inspect_idle("child").expect("inspect");
        let started = Instant::now();
        while released.cached {
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "supervisor did not release the idle child"
            );
            thread::sleep(Duration::from_millis(30));
            released = client.inspect_idle("child").expect("inspect");
        }
        assert!(released.mark.is_none());
    }

    #[test]
    fn completion_during_the_grace_runs_and_clears_the_idle_mark() {
        let dir = tempfile::tempdir().unwrap();
        write_idle_specs(dir.path());
        let client =
            EngineClient::spawn_with_idle_grace(dir.path().to_path_buf(), WATCHDOG_UNDER_TEST, Duration::from_secs(5))
                .expect("spawn");
        complete_buf(&client, "tool child ");
        complete_buf(&client, "git status");
        let started = Instant::now();
        complete_buf(&client, "tool child ");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a queued completion waited out the grace"
        );
        let state = client.inspect_idle("child").expect("inspect");
        assert!(state.cached);
        assert_eq!(state.mark, Some(None));
    }

    #[test]
    fn clear_caches_drops_a_pending_idle_file() {
        let dir = tempfile::tempdir().unwrap();
        write_idle_specs(dir.path());
        let client =
            EngineClient::spawn_with_idle_grace(dir.path().to_path_buf(), WATCHDOG_UNDER_TEST, Duration::from_secs(5))
                .expect("spawn");
        complete_buf(&client, "tool child ");
        complete_buf(&client, "git status");
        let pending = client.inspect_idle("child").expect("inspect");
        assert!(pending.cached);
        assert!(pending.mark.expect("tracked").is_some());

        client.clear_caches().expect("clear");
        let state = client.inspect_idle("child").expect("inspect");
        assert!(!state.cached);
        assert!(state.mark.is_none());
    }

    #[test]
    fn app_bundle_uses_specs_ir_not_js_specs() {
        let macos = Path::new("/Applications/Fastab.app/Contents/MacOS");
        let dir = specs_ir_in_bundle(macos);
        assert_eq!(dir.file_name().unwrap(), "specs-ir");
        assert_eq!(
            dir,
            PathBuf::from("/Applications/Fastab.app/Contents/MacOS/../Resources/specs-ir")
        );
    }
}
