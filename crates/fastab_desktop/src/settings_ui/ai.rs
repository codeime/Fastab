use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures::FutureExt;
use gpui::prelude::*;
use gpui::{App, Context, Entity, FocusHandle, MouseButton, Window, div, px, rgb};

use crate::EventLoopProxy;
use crate::event::Event;
use crate::jev::client::{ClientErrorKind, JevClient};
use crate::jev::config::{
    AiConfig, ConfigChanged, Profile, Provider, normalize_base_url, pause_runtime, runtime_revision,
};
use crate::jev::credentials::{self, CredentialError};
use crate::jev::policy::DATA_POLICY_VERSION;

use super::input::{Commit, Input};
use super::theme::Chrome;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProbeKind {
    Idle,
    Testing,
    Success,
    Failure,
}

impl ProbeKind {
    fn color(self, chrome: Chrome) -> u32 {
        let dark = chrome.text > 0x808080;
        match self {
            Self::Idle => chrome.muted,
            Self::Testing => chrome.accent,
            Self::Success => {
                if dark {
                    0x32d583
                } else {
                    0x067647
                }
            },
            Self::Failure => {
                if dark {
                    0xff6961
                } else {
                    0xb42318
                }
            },
        }
    }
}

enum AutoSaveOutcome {
    Disabled(AiConfig, bool),
    Enabled {
        config: AiConfig,
        profile: Profile,
        service: String,
        key_revision: u64,
        secret: Vec<u8>,
    },
    NeedsKey(AiConfig, String),
    Invalid(&'static str),
    Failed {
        staged: Option<AiConfig>,
        message: &'static str,
        conflict: bool,
    },
    Stale,
}

#[derive(Debug, PartialEq, Eq)]
enum KeyDraftTransition {
    Keep,
    Clear,
}

fn key_draft_transition(
    bound_service: Option<&str>,
    current_service: Option<&str>,
    has_key: bool,
) -> KeyDraftTransition {
    if !has_key {
        return if bound_service.is_some() {
            KeyDraftTransition::Clear
        } else {
            KeyDraftTransition::Keep
        };
    }
    match (bound_service, current_service) {
        (Some(bound), Some(current)) if bound != current => KeyDraftTransition::Clear,
        _ => KeyDraftTransition::Keep,
    }
}

fn key_draft_binding_after_edit(
    bound_service: Option<&str>,
    current_service: Option<&str>,
    has_key: bool,
) -> Option<String> {
    if has_key {
        // Keep an existing binding even while the URL is temporarily invalid.
        // Only an entirely new key can await a destination to be committed.
        bound_service.or(current_service).map(str::to_owned)
    } else {
        None
    }
}

fn key_draft_matches(bound_service: Option<&str>, requested_service: &str) -> bool {
    bound_service == Some(requested_service)
}

#[derive(Debug, PartialEq, Eq)]
enum QueuedProbeDisposition {
    None,
    Wait,
    Run,
    Fail,
}

fn queued_probe_disposition(queued: bool, settled: bool, can_test: bool) -> QueuedProbeDisposition {
    if !queued {
        QueuedProbeDisposition::None
    } else if !settled {
        QueuedProbeDisposition::Wait
    } else if can_test {
        QueuedProbeDisposition::Run
    } else {
        QueuedProbeDisposition::Fail
    }
}

pub(super) struct AiSettings {
    config: AiConfig,
    profile: Profile,
    key: Entity<Input>,
    model: Entity<Input>,
    base: Entity<Input>,
    enabled: bool,
    busy: bool,
    load_failed: bool,
    persistence_failed: bool,
    epoch: u64,
    save_generation: u64,
    save_pending: bool,
    save_ready: bool,
    save_running: bool,
    dismissed: bool,
    own_writes: Vec<AiConfig>,
    test_after_save: bool,
    status: String,
    probe_status: String,
    probe_kind: ProbeKind,
    key_presence_service: Option<String>,
    key_present: Option<bool>,
    key_presence_generation: u64,
    key_presence_pending: bool,
    probe_task: Option<gpui::Task<()>>,
    probe_abort: Option<tokio::task::AbortHandle>,
    pause_write: Option<futures::future::Shared<gpui::Task<Result<u64, bool>>>>,
    operation_cancel: Option<Arc<AtomicU64>>,
    dismissal_revision: Option<u64>,
    buttons: BTreeMap<String, FocusHandle>,
    proxy: EventLoopProxy,
    edit_revisions: [u64; 3],
    saved_key_revision: Option<u64>,
    saved_key_service: Option<String>,
    key_draft_service: Option<String>,
    _input_observations: [gpui::Subscription; 3],
    _input_commits: [gpui::Subscription; 3],
}

impl AiSettings {
    pub(super) fn new(proxy: EventLoopProxy, cx: &mut Context<'_, Self>) -> Self {
        let config = AiConfig::default();
        let profile = config.active_profile().cloned().unwrap_or_else(Profile::typesafe);
        let base = cx.new(|cx| Input::new("jev-base", profile.base_url.clone(), false, 2048, cx));
        let key = cx.new(|cx| Input::new("jev-key", String::new(), true, 4096, cx));
        let model = cx.new(|cx| Input::new("jev-model", profile.model.clone(), false, 128, cx));
        let input_observations = [
            cx.observe(&key, |this, input, cx| {
                let revision = input.read(cx).edit_revision();
                this.input_edited(0, revision, cx);
            }),
            cx.observe(&model, |this, input, cx| {
                let revision = input.read(cx).edit_revision();
                this.input_edited(1, revision, cx);
            }),
            cx.observe(&base, |this, input, cx| {
                let revision = input.read(cx).edit_revision();
                this.input_edited(2, revision, cx);
            }),
        ];
        let input_commits = [
            cx.subscribe(&key, |this, _, _: &Commit, cx| this.commit_edit(cx)),
            cx.subscribe(&model, |this, _, _: &Commit, cx| this.commit_edit(cx)),
            cx.subscribe(&base, |this, _, _: &Commit, cx| this.commit_edit(cx)),
        ];
        for field in [&key, &model, &base] {
            field.update(cx, |input, _| input.enabled = false);
        }
        let executor = cx.background_executor().clone();
        let loading = executor.spawn(async { AiConfig::load_wait() });
        cx.spawn(async move |this, cx| {
            let timeout = executor.timer(std::time::Duration::from_secs(10));
            futures::pin_mut!(loading, timeout);
            let loaded = match futures::future::select(loading, timeout).await {
                futures::future::Either::Left((result, _)) => result.ok(),
                futures::future::Either::Right(_) => None,
            };
            let _ = this.update(cx, |this, cx| {
                if let Some(config) = loaded {
                    let profile = config.active_profile().cloned().unwrap_or_else(Profile::typesafe);
                    this.model.update(cx, |input, cx| input.set(profile.model.clone(), cx));
                    this.base
                        .update(cx, |input, cx| input.set(profile.base_url.clone(), cx));
                    this.profile = profile;
                    this.enabled = config.enabled;
                    this.config = config;
                    this.load_failed = false;
                    this.status.clear();
                } else {
                    this.status = Self::label(
                        "无法读取 AI 配置；请关闭并重新打开设置。",
                        "Could not read AI settings. Close and reopen settings.",
                    )
                    .into();
                }
                this.set_busy(false, cx);
                if !this.load_failed {
                    this.refresh_key_presence(cx);
                }
            });
        })
        .detach();
        Self {
            enabled: config.enabled,
            config,
            profile: profile.clone(),
            key,
            model,
            base,
            busy: false,
            load_failed: true,
            persistence_failed: false,
            epoch: 0,
            save_generation: 0,
            save_pending: false,
            save_ready: false,
            save_running: false,
            dismissed: false,
            own_writes: Vec::new(),
            test_after_save: false,
            status: Self::label("正在读取配置…", "Loading settings…").into(),
            buttons: BTreeMap::new(),
            probe_status: String::new(),
            probe_kind: ProbeKind::Idle,
            key_presence_service: None,
            key_present: None,
            key_presence_generation: 0,
            key_presence_pending: false,
            probe_task: None,
            probe_abort: None,
            pause_write: None,
            operation_cancel: None,
            dismissal_revision: None,
            proxy,
            edit_revisions: [0; 3],
            saved_key_revision: None,
            saved_key_service: None,
            key_draft_service: None,
            _input_observations: input_observations,
            _input_commits: input_commits,
        }
    }

    fn label(zh: &'static str, en: &'static str) -> &'static str {
        if super::locale_is_zh() { zh } else { en }
    }

    /// Finish the current edit when the user leaves the page or closes Settings.
    /// The detached save keeps this entity alive until the write completes.
    pub(super) fn clear_draft(&mut self, cx: &mut Context<'_, Self>) {
        self.observe_current_edits(cx);
        self.cancel_probe(cx);
        if self.busy {
            self.discard_draft(cx);
            return;
        }
        self.dismissed = true;
        if self.save_pending {
            self.save_ready = true;
            self.start_auto_save(cx);
        }
        if !self.save_running && !self.save_pending {
            self.clear_saved_key_draft(cx);
        }
        cx.notify();
    }

    /// Abort an in-progress destructive operation such as profile deletion.
    fn discard_draft(&mut self, cx: &mut Context<'_, Self>) {
        self.cancel_probe(cx);
        let operation_pending = self.busy || self.save_running || self.save_pending;
        self.save_generation = self.save_generation.wrapping_add(1);
        self.save_pending = false;
        self.save_ready = false;
        self.test_after_save = false;
        if operation_pending {
            // A credential/save continuation must not enable requests after
            // the draft was dismissed while it was awaiting background work.
            // Closing the GPUI window calls clear_draft before removal and may
            // call it again from the host's Close event. Keep one revision for
            // all close repairs so they remain valid through Drop.
            let revision = *self.dismissal_revision.get_or_insert_with(pause_runtime);
            if let Some(cancel) = &self.operation_cancel {
                cancel.store(revision, Ordering::Release);
            }
            self.config.enabled = false;
            self.changed();
            let disabled = self.config.clone();
            cx.background_executor()
                .spawn(async move {
                    // Also persist a close that happened before the pending write
                    // even began. A newer save wins through the revision check.
                    let _ = disabled.save_if_unchanged(&disabled, revision);
                })
                .detach();
        }
        self.epoch = self.epoch.wrapping_add(1);
        self.key.update(cx, |input, cx| {
            input.take(cx);
        });
        self.saved_key_revision = None;
        self.saved_key_service = None;
        self.key_draft_service = None;
        cx.notify();
    }

    fn clear_saved_key_draft(&mut self, cx: &mut Context<'_, Self>) {
        let key = self.key.read(cx);
        let clear = key.value().is_empty()
            || (self.saved_key_revision == Some(key.edit_revision())
                && self.saved_key_service.as_deref() == self.key_presence_service.as_deref());
        if clear {
            self.key.update(cx, |input, cx| {
                input.take(cx);
            });
            self.saved_key_revision = None;
            self.saved_key_service = None;
            self.key_draft_service = None;
        }
    }

    fn draft(&self, cx: &App) -> Profile {
        let mut profile = self.profile.clone();
        profile.base_url = self.base.read(cx).value().to_owned();
        profile.model = self.model.read(cx).value().to_owned();
        // Enabling or finishing an edit accepts the disclosed data scope for
        // this draft. Existing persisted profiles are not rewritten on load.
        profile.data_policy_version = DATA_POLICY_VERSION;
        profile
    }

    fn select(&mut self, profile: Profile, cx: &mut Context<'_, Self>) {
        if self.busy {
            return;
        }
        self.cancel_probe(cx);
        self.key.update(cx, |input, cx| {
            input.take(cx);
        });
        self.saved_key_revision = None;
        self.saved_key_service = None;
        self.key_draft_service = None;
        self.model.update(cx, |input, cx| input.set(profile.model.clone(), cx));
        self.base
            .update(cx, |input, cx| input.set(profile.base_url.clone(), cx));
        self.profile = profile;
        self.refresh_key_presence(cx);
        self.queue_save(true, cx);
    }

    fn provider(&mut self, provider: Provider, cx: &mut Context<'_, Self>) {
        let profile = self
            .config
            .profiles
            .iter()
            .find(|profile| profile.provider == provider)
            .cloned()
            .unwrap_or_else(|| match provider {
                Provider::TypeSafe => Profile::typesafe(),
                Provider::OpenRouter => Profile::openrouter(),
                Provider::CustomSystemOne => Profile {
                    id: uuid::Uuid::new_v4().to_string(),
                    provider,
                    base_url: String::new(),
                    model: "jev-1.13.0".into(),
                    data_policy_version: 0,
                },
            });
        self.select(profile, cx);
    }

    fn set_busy(&mut self, busy: bool, cx: &mut Context<'_, Self>) {
        self.busy = busy;
        if busy {
            self.dismissal_revision = None;
        } else if !self.save_running {
            self.operation_cancel = None;
            self.dismissal_revision = None;
        }
        let editable = !busy && !self.load_failed;
        for field in [&self.key, &self.model, &self.base] {
            field.update(cx, |input, cx| {
                input.enabled = editable;
                cx.notify();
            });
        }
        if !busy {
            self.schedule_key_presence(cx);
            if self.save_pending && self.save_ready {
                self.start_auto_save(cx);
            }
        }
        cx.notify();
    }

    fn update_key_placeholder(&mut self, cx: &mut Context<'_, Self>) {
        let placeholder = self.key_present.filter(|present| *present).map(|_| "********".into());
        self.key.update(cx, |input, cx| input.set_placeholder(placeholder, cx));
    }

    fn refresh_key_presence(&mut self, cx: &mut Context<'_, Self>) {
        self.key_presence_generation = self.key_presence_generation.wrapping_add(1);
        let service = self.draft(cx).credential_key().ok();
        let transition = key_draft_transition(
            self.key_draft_service.as_deref(),
            service.as_deref(),
            !self.key.read(cx).value().is_empty(),
        );
        match transition {
            KeyDraftTransition::Keep => {},
            KeyDraftTransition::Clear => {
                self.key.update(cx, |input, cx| {
                    input.take(cx);
                });
                self.key_draft_service = None;
                self.saved_key_revision = None;
                self.saved_key_service = None;
            },
        }
        self.key_presence_service = service;
        self.key_present = None;
        self.update_key_placeholder(cx);
        self.schedule_key_presence(cx);
    }

    fn record_key_presence(&mut self, service: &str, present: bool, cx: &mut Context<'_, Self>) {
        if self.key_presence_service.as_deref() != Some(service) {
            return;
        }
        self.key_presence_generation = self.key_presence_generation.wrapping_add(1);
        self.key_present = Some(present);
        self.update_key_placeholder(cx);
    }

    fn invalidate_key_presence(&mut self, service: &str, cx: &mut Context<'_, Self>) {
        if self.key_presence_service.as_deref() == Some(service) {
            self.key_presence_generation = self.key_presence_generation.wrapping_add(1);
            self.key_present = None;
            self.update_key_placeholder(cx);
        }
    }

    fn schedule_key_presence(&mut self, cx: &mut Context<'_, Self>) {
        if self.key_presence_pending
            || self.key_presence_service.is_none()
            || self.key_present.is_some()
            || self.busy
            || self.load_failed
        {
            return;
        }
        self.key_presence_pending = true;
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let mut delay = Duration::from_millis(350);
            let mut busy_retries = 0u8;
            loop {
                let Ok(wait_generation) = this.update(cx, |this, _| this.key_presence_generation) else {
                    return;
                };
                executor.timer(delay).await;
                let mut edited_while_waiting = false;
                let query = this
                    .update(cx, |this, cx| {
                        if this.busy || this.load_failed || this.key_present.is_some() {
                            this.key_presence_pending = false;
                            return None;
                        }
                        if this.key_presence_generation != wait_generation {
                            edited_while_waiting = true;
                            return None;
                        }
                        let Some(service) = this.key_presence_service.clone() else {
                            this.key_presence_pending = false;
                            return None;
                        };
                        let generation = this.key_presence_generation;
                        Some((service.clone(), generation, credentials::contains(&service, cx)))
                    })
                    .ok()
                    .flatten();
                if edited_while_waiting {
                    delay = Duration::from_millis(350);
                    busy_retries = 0;
                    continue;
                }
                let Some((service, generation, task)) = query else {
                    return;
                };
                let result = task.await;
                let retry_delay = this
                    .update(cx, |this, cx| {
                        if this.busy || this.load_failed || this.key_present.is_some() {
                            this.key_presence_pending = false;
                            return None;
                        }
                        if this.key_presence_generation != generation
                            || this.key_presence_service.as_deref() != Some(service.as_str())
                        {
                            // A changed service waits for the old OS operation to
                            // finish before querying the current one.
                            busy_retries = 0;
                            return Some(Duration::from_millis(350));
                        }
                        match result {
                            Ok(present) => {
                                this.key_present = Some(present);
                                this.key_presence_pending = false;
                                this.update_key_placeholder(cx);
                                None
                            },
                            Err(CredentialError::Busy) if busy_retries < 11 => {
                                busy_retries += 1;
                                Some(Duration::from_millis((350u64 << busy_retries.min(4)).min(4000)))
                            },
                            Err(_) => {
                                // Unknown is distinct from a confirmed missing key.
                                this.key_presence_pending = false;
                                None
                            },
                        }
                    })
                    .ok()
                    .flatten();
                let Some(next_delay) = retry_delay else {
                    return;
                };
                delay = next_delay;
            }
        })
        .detach();
    }

    fn changed(&self) {
        let _ = self.proxy.send_event(Event::JevSettingsChanged);
    }

    fn recognizes_baseline(&self, baseline: &AiConfig) -> bool {
        std::iter::once(&self.config)
            .chain(self.own_writes.iter())
            .any(|known| {
                if known == baseline {
                    return true;
                }
                let mut paused = known.clone();
                paused.enabled = false;
                &paused == baseline
            })
    }

    fn remember_write(&mut self, config: &AiConfig) {
        if self.own_writes.last() != Some(config) {
            self.own_writes.push(config.clone());
            if self.own_writes.len() > 8 {
                self.own_writes.remove(0);
            }
        }
    }

    fn input_edited(&mut self, field: usize, revision: u64, cx: &mut Context<'_, Self>) {
        if self.edit_revisions[field] == revision {
            return;
        }
        self.edit_revisions[field] = revision;
        self.test_after_save = false;
        self.probe_status.clear();
        self.probe_kind = ProbeKind::Idle;
        if field == 0 {
            self.key_draft_service = key_draft_binding_after_edit(
                self.key_draft_service.as_deref(),
                self.draft(cx).credential_key().ok().as_deref(),
                !self.key.read(cx).value().is_empty(),
            );
            self.saved_key_revision = None;
            self.saved_key_service = None;
        } else if field == 2 {
            self.refresh_key_presence(cx);
        }
        self.queue_save(false, cx);
    }

    fn observe_current_edits(&mut self, cx: &mut Context<'_, Self>) {
        let revisions = [
            self.key.read(cx).edit_revision(),
            self.model.read(cx).edit_revision(),
            self.base.read(cx).edit_revision(),
        ];
        if revisions != self.edit_revisions {
            let key_changed = revisions[0] != self.edit_revisions[0];
            let base_changed = revisions[2] != self.edit_revisions[2];
            self.edit_revisions = revisions;
            self.test_after_save = false;
            self.probe_status.clear();
            self.probe_kind = ProbeKind::Idle;
            if key_changed {
                self.key_draft_service = key_draft_binding_after_edit(
                    self.key_draft_service.as_deref(),
                    self.draft(cx).credential_key().ok().as_deref(),
                    !self.key.read(cx).value().is_empty(),
                );
                self.saved_key_revision = None;
                self.saved_key_service = None;
            }
            if base_changed {
                self.refresh_key_presence(cx);
            }
            self.queue_save(false, cx);
        }
    }

    fn commit_edit(&mut self, cx: &mut Context<'_, Self>) {
        self.observe_current_edits(cx);
        self.bind_fresh_key_to_current_service(cx);
        if self.save_pending && !self.save_ready {
            self.save_ready = true;
            self.status = Self::label("正在自动保存…", "Saving automatically…").into();
            self.start_auto_save(cx);
            cx.notify();
        }
    }

    fn bind_fresh_key_to_current_service(&mut self, cx: &App) {
        self.key_draft_service = key_draft_binding_after_edit(
            self.key_draft_service.as_deref(),
            self.draft(cx).credential_key().ok().as_deref(),
            !self.key.read(cx).value().is_empty(),
        );
    }

    fn queue_save(&mut self, immediate: bool, cx: &mut Context<'_, Self>) {
        if self.load_failed || self.busy {
            return;
        }
        self.dismissed = false;
        let was_enabled = self.config.enabled;
        self.suspend_for_edit(cx);
        if !was_enabled {
            let revision = pause_runtime();
            if let Some(cancel) = &self.operation_cancel {
                cancel.store(revision, Ordering::Release);
            }
        }
        self.epoch = self.epoch.wrapping_add(1);
        self.save_generation = self.save_generation.wrapping_add(1);
        self.save_pending = true;
        self.save_ready = immediate;
        self.status = if immediate {
            Self::label("正在自动保存…", "Saving automatically…")
        } else {
            Self::label("完成编辑后自动保存…", "Saving when editing is finished…")
        }
        .into();
        if immediate {
            self.start_auto_save(cx);
        }
        cx.notify();
    }

    /// Pause before any I/O. A blocked settings file must never block input or
    /// leave terminal requests running with a configuration being edited.
    fn suspend_for_edit(&mut self, cx: &mut Context<'_, Self>) {
        if !self.config.enabled {
            return;
        }
        let revision = pause_runtime();
        let previous = self.config.clone();
        self.config.enabled = false;
        self.changed();
        let disabled = self.config.clone();
        let operation = cx
            .background_executor()
            .spawn(async move {
                disabled
                    .save_if_unchanged(&previous, revision)
                    .map_err(|error| error.is::<ConfigChanged>())
            })
            .shared();
        self.pause_write = Some(operation.clone());
        self.status = Self::label("AI 已暂停，正在应用更改…", "AI is paused while changes are applied…").into();
        let epoch = self.epoch;
        cx.spawn(async move |this, cx| {
            if let Err(changed) = operation.await {
                let _ = this.update(cx, |this, cx| {
                    if this.epoch == epoch {
                        this.save_error(changed, cx);
                    }
                });
            }
        })
        .detach();
        cx.notify();
    }

    fn save_error(&mut self, changed: bool, cx: &mut Context<'_, Self>) {
        if changed {
            self.load_failed = true;
            self.set_busy(false, cx);
            self.status = Self::label(
                "配置已在其他操作中更改；未覆盖新配置，请重新打开设置。",
                "Settings changed during this operation. The newer configuration was kept. Reopen settings.",
            )
            .into();
            cx.notify();
        } else {
            self.persistence_error(cx);
        }
    }

    fn persistence_error(&mut self, cx: &mut Context<'_, Self>) {
        self.config.enabled = false;
        self.persistence_failed = true;
        self.changed();
        self.status = Self::label(
            "配置保存失败，AI 已暂停；输入已保留。继续编辑会再次尝试。",
            "Could not save settings. AI is paused and your input was kept. Editing will try again.",
        )
        .into();
        cx.notify();
    }

    fn credential_error(error: CredentialError) -> &'static str {
        match error {
            CredentialError::TimedOut => Self::label(
                "密钥存储未及时响应；输入已保留，操作结束后可重试。",
                "Key storage did not respond in time. Your input was kept; retry when it finishes.",
            ),
            CredentialError::Busy => Self::label(
                "凭据操作正在完成，请稍后重试；AI 保持关闭。",
                "A credential operation is still finishing. Retry shortly; AI remains off.",
            ),
            CredentialError::Unavailable | CredentialError::InvalidService => Self::label(
                "当前凭据不可用；AI 保持关闭。可重新输入密钥。",
                "Credentials are currently unavailable; AI remains off. You can enter the key again.",
            ),
        }
    }

    fn cancel_probe(&mut self, cx: &mut Context<'_, Self>) {
        self.test_after_save = false;
        if self.probe_task.take().is_some() {
            if let Some(abort) = self.probe_abort.take() {
                abort.abort();
            }
            self.set_busy(false, cx);
        }
        self.probe_status.clear();
        self.probe_kind = ProbeKind::Idle;
    }

    fn probe_error(kind: ClientErrorKind) -> &'static str {
        match kind {
            ClientErrorKind::Authentication | ClientErrorKind::InvalidCredential => Self::label(
                "Key 无效或已失效，请检查当前服务商的 Key。",
                "The key is invalid or expired. Check the key for this provider.",
            ),
            ClientErrorKind::PaymentRequired => {
                Self::label("账户额度不足（402）。", "Insufficient account credit (402).")
            },
            ClientErrorKind::RateLimited => Self::label(
                "请求被限流（429），请稍后重试。",
                "Rate limited (429). Try again shortly.",
            ),
            ClientErrorKind::Overloaded => {
                Self::label("服务暂时繁忙，请稍后重试。", "The service is busy. Try again shortly.")
            },
            ClientErrorKind::Timeout => Self::label(
                "连接超时，请检查网络或稍后重试。",
                "Connection timed out. Check your network or retry.",
            ),
            ClientErrorKind::Transport => Self::label(
                "无法连接，请检查网络、代理及 HTTPS 地址。",
                "Could not connect. Check your network, proxy and HTTPS address.",
            ),
            ClientErrorKind::Rejected => Self::label(
                "服务拒绝请求，请检查模型权限和接口地址。",
                "The service rejected the request. Check model access and the endpoint.",
            ),
            ClientErrorKind::InvalidRequest => Self::label(
                "配置无效，请检查地址和 Jev 模型。",
                "Invalid settings. Check the address and Jev model.",
            ),
            ClientErrorKind::InvalidResponse | ClientErrorKind::ResponseTooLarge => Self::label(
                "服务返回了不兼容的响应，请检查是否支持 System One 接口。",
                "The response is incompatible. Check that the service supports System One.",
            ),
        }
    }

    fn test_connection(&mut self, cx: &mut Context<'_, Self>) {
        if self.busy || self.load_failed {
            return;
        }
        self.commit_edit(cx);
        if self.save_running || self.save_pending {
            self.test_after_save = true;
            self.probe_status = Self::label(
                "配置应用后测试连接…",
                "Testing the connection after settings are applied…",
            )
            .into();
            self.probe_kind = ProbeKind::Testing;
            cx.notify();
            return;
        }
        if self.key.read(cx).rejected {
            self.probe_status =
                Self::label("请先修正未接受的 Key 输入。", "Correct the rejected key input first.").into();
            self.probe_kind = ProbeKind::Failure;
            cx.notify();
            return;
        }
        let profile = self.draft(cx);
        // The explicit test sends only a fixed public example and never
        // persists this draft or enables automatic requests.
        let profile = match profile.validate() {
            Ok(profile) => profile,
            Err(_) => {
                self.probe_status = Self::probe_error(ClientErrorKind::InvalidRequest).into();
                self.probe_kind = ProbeKind::Failure;
                cx.notify();
                return;
            },
        };
        let secret = if key_draft_matches(self.key_draft_service.as_deref(), &profile.credential_service) {
            self.key.read(cx).value().as_bytes().to_vec()
        } else {
            Vec::new()
        };
        let credential = if secret.is_empty() {
            credentials::read(&profile.credential_service, cx)
        } else {
            gpui::Task::ready(Ok(Some(secret)))
        };
        self.epoch = self.epoch.wrapping_add(1);
        let epoch = self.epoch;
        self.set_busy(true, cx);
        self.probe_status = Self::label("正在测试连接…", "Testing connection…").into();
        self.probe_kind = ProbeKind::Testing;
        self.probe_task = Some(cx.spawn(async move |this, cx| {
            let result = match credential.await {
                Ok(Some(secret)) if !secret.is_empty() => {
                    // Only the Tokio worker polls reqwest. GPUI waits on a
                    // budget-free channel and can keep processing input.
                    let (tx, rx) = futures::channel::oneshot::channel();
                    let worker = tokio::spawn(async move {
                        let result = match JevClient::new() {
                            Ok(client) => client.probe(&profile, &secret).await.map_err(|error| error.kind),
                            Err(_) => Err(ClientErrorKind::Transport),
                        };
                        let _ = tx.send(result);
                    });
                    let abort = worker.abort_handle();
                    if this
                        .update(cx, |this, _| this.probe_abort = Some(abort.clone()))
                        .is_err()
                    {
                        abort.abort();
                        return;
                    }
                    rx.await
                        .unwrap_or(Err(ClientErrorKind::Transport))
                        .map_err(Self::probe_error)
                },
                Ok(_) => Err(Self::label(
                    "尚未配置 Key，请先输入后再测试。",
                    "No key is configured. Enter a key to test.",
                )),
                Err(error) => Err(Self::credential_error(error)),
            };
            let _ = this.update(cx, |this, cx| {
                if this.epoch != epoch {
                    return;
                }
                this.probe_abort = None;
                this.probe_task = None;
                this.set_busy(false, cx);
                let (kind, message) = match result {
                    Ok(()) => (
                        ProbeKind::Success,
                        Self::label("连接成功，Key 和模型可用。", "Connected. The key and model work."),
                    ),
                    Err(message) => (ProbeKind::Failure, message),
                };
                this.probe_kind = kind;
                this.probe_status = message.into();
            });
        }));
    }

    fn fail_queued_test(&mut self, message: &'static str) {
        if self.test_after_save {
            self.test_after_save = false;
            self.probe_kind = ProbeKind::Failure;
            self.probe_status = message.into();
        }
    }

    fn start_auto_save(&mut self, cx: &mut Context<'_, Self>) {
        if self.save_running || !self.save_pending || !self.save_ready || self.load_failed || self.busy {
            return;
        }
        self.bind_fresh_key_to_current_service(cx);
        let desired_enabled = self.enabled;
        let draft = self.draft(cx);
        let key = self.key.read(cx);
        let draft_service = draft.credential_key().ok();
        let secret = if draft_service
            .as_deref()
            .is_some_and(|service| key_draft_matches(self.key_draft_service.as_deref(), service))
        {
            key.value().as_bytes().to_vec()
        } else {
            Vec::new()
        };
        let key_revision = key.edit_revision();
        let saved_key_revision = self.saved_key_revision;
        let saved_key_service = self.saved_key_service.clone();
        if desired_enabled {
            if key.rejected || !secret.iter().all(u8::is_ascii_graphic) {
                self.save_pending = false;
                self.save_ready = false;
                let message = Self::label(
                    "请检查 Key：仅支持可见 ASCII 字符，不能含空白。",
                    "Check the key: use visible ASCII characters without whitespace.",
                );
                self.status = message.into();
                self.fail_queued_test(message);
                cx.notify();
                return;
            }
            if draft.validate().is_err() {
                self.save_pending = false;
                self.save_ready = false;
                let message = Self::label(
                    "请填写有效的 HTTPS 地址和 Jev 模型；AI 暂未启用。",
                    "Enter a valid HTTPS address and Jev model; AI is not yet enabled.",
                );
                self.status = message.into();
                self.fail_queued_test(message);
                cx.notify();
                return;
            }
        }
        self.save_running = true;
        self.save_pending = false;
        self.save_ready = false;
        let generation = self.save_generation;
        let pause = self.pause_write.take();
        let cancel = Arc::new(AtomicU64::new(0));
        self.operation_cancel = Some(cancel.clone());
        self.status = Self::label("正在自动保存…", "Saving automatically…").into();
        cx.notify();
        let executor = cx.background_executor().clone();
        let keep_alive = cx.entity();
        cx.spawn(async move |this, cx| {
            let _keep_alive = keep_alive;
            let outcome = async {
                if let Some(pause) = pause {
                    // Even a superseded pause must finish before we read its
                    // result from the settings store.
                    if let Err(changed) = pause.await
                        && !changed
                    {
                        return AutoSaveOutcome::Failed {
                            staged: None,
                            message: Self::label(
                                "暂停 AI 时无法保存配置；输入已保留。继续编辑会再次尝试。",
                                "Could not persist the AI pause; your input was kept. Editing will try again.",
                            ),
                            conflict: false,
                        };
                    }
                }
                let baseline = match executor.spawn(async { AiConfig::load_wait() }).await {
                    Ok(config) => config,
                    Err(_) => {
                        return AutoSaveOutcome::Failed {
                            staged: None,
                            message: Self::label(
                                "无法读取当前配置；AI 已暂停。请重新打开设置。",
                                "Could not read current settings; AI is paused. Reopen settings.",
                            ),
                            conflict: true,
                        };
                    },
                };
                let current = this
                    .update(cx, |this, _| this.save_generation == generation)
                    .unwrap_or(false);
                if !current {
                    return AutoSaveOutcome::Stale;
                }
                let recognized = this
                    .update(cx, |this, _| this.recognizes_baseline(&baseline))
                    .unwrap_or(false);
                if !recognized && desired_enabled {
                    return AutoSaveOutcome::Failed {
                        staged: None,
                        message: Self::label(
                            "配置已在其他操作中更改；AI 已暂停。请重新打开设置。",
                            "Settings changed elsewhere; AI is paused. Reopen settings.",
                        ),
                        conflict: true,
                    };
                }
                let revision = runtime_revision();
                let mut staged = baseline.clone();
                staged.enabled = false;
                if !desired_enabled
                    && recognized
                    && baseline.profiles.iter().any(|profile| profile.id == draft.id)
                {
                    staged.active_profile_id = Some(draft.id.clone());
                }
                let (profile, service) = if desired_enabled {
                    let service = match draft.credential_key() {
                        Ok(service) => service,
                        Err(_) => return AutoSaveOutcome::Stale,
                    };
                    let mut profile = draft.clone();
                    profile.id = baseline
                        .profiles
                        .iter()
                        .find(|old| old.credential_key().ok().as_deref() == Some(service.as_str()))
                        .map_or_else(|| uuid::Uuid::new_v4().to_string(), |old| old.id.clone());
                    if staged.upsert_profile(profile.clone()).is_err() {
                        return AutoSaveOutcome::Invalid(Self::label(
                                "配置无效或已达 16 个配置上限；AI 暂未启用。",
                                "Invalid settings or the 16-profile limit was reached; AI is not yet enabled.",
                            ));
                    }
                    staged.active_profile_id = Some(profile.id.clone());
                    (Some(profile), Some(service))
                } else {
                    (None, None)
                };
                if desired_enabled {
                    let valid = executor
                        .spawn(async { fastab_engine::public_ai_baseline_ok(&fastab_engine::default_specs_dir()) })
                        .await;
                    if !valid {
                        return AutoSaveOutcome::Invalid(Self::label(
                                "补全规格校验未通过，无法启用 AI；请重新安装完整应用。",
                                "Completion specs failed validation. Reinstall the complete app to enable AI.",
                            ));
                    }
                }
                if !this
                    .update(cx, |this, _| {
                        if this.save_generation != generation {
                            return false;
                        }
                        this.remember_write(&staged);
                        true
                    })
                    .unwrap_or(false)
                {
                    return AutoSaveOutcome::Stale;
                }
                let writing = staged.clone();
                let expected = baseline.clone();
                let staged_revision = match executor
                    .spawn(async move { writing.save_if_unchanged(&expected, revision) })
                    .await
                {
                    Ok(revision) => revision,
                    Err(error) => {
                        return AutoSaveOutcome::Failed {
                            staged: None,
                            message: Self::label(
                                "配置保存失败，AI 已暂停；输入已保留。继续编辑会再次尝试。",
                                "Could not save settings; AI is paused and your input was kept. Editing will try again.",
                            ),
                            conflict: error.is::<ConfigChanged>(),
                        };
                    },
                };
                if !desired_enabled {
                    return AutoSaveOutcome::Disabled(staged, !recognized);
                }
                if !this
                    .update(cx, |this, _| this.save_generation == generation)
                    .unwrap_or(false)
                {
                    return AutoSaveOutcome::Stale;
                }
                let service = service.expect("enabled profile has a credential service");
                let profile = profile.expect("enabled profile exists");
                let key_is_saved = saved_key_revision == Some(key_revision)
                    && saved_key_service.as_deref() == Some(service.as_str());
                let replacing = !secret.is_empty() && !key_is_saved;
                let mut attempt = 0u8;
                let credential = loop {
                    let task = this
                        .update(cx, |this, cx| {
                            if this.save_generation != generation {
                                return None;
                            }
                            if replacing {
                                let write = credentials::write(&service, secret.clone(), cx);
                                Some(cx.spawn(async move |_, _| write.await.map(|_| true)))
                            } else {
                                let read = credentials::read(&service, cx);
                                Some(cx.spawn(async move |_, _| {
                                    read.await.map(|value| value.is_some_and(|value| !value.is_empty()))
                                }))
                            }
                        })
                        .ok()
                        .flatten();
                    let Some(task) = task else {
                        return AutoSaveOutcome::Stale;
                    };
                    match task.await {
                        Err(CredentialError::Busy) if attempt < 4 => {
                            attempt += 1;
                            executor.timer(Duration::from_millis(350 * u64::from(attempt))).await;
                        },
                        result => break result,
                    }
                };
                match credential {
                    Ok(true) => {},
                    Ok(false) => return AutoSaveOutcome::NeedsKey(staged, service),
                    Err(error) => {
                        return AutoSaveOutcome::Failed {
                            staged: Some(staged),
                            message: Self::credential_error(error),
                            conflict: false,
                        };
                    },
                }
                if !this
                    .update(cx, |this, _| this.save_generation == generation)
                    .unwrap_or(false)
                {
                    return AutoSaveOutcome::Stale;
                }
                let mut enabled = staged.clone();
                enabled.enabled = true;
                if !this
                    .update(cx, |this, _| {
                        if this.save_generation != generation {
                            return false;
                        }
                        this.remember_write(&enabled);
                        true
                    })
                    .unwrap_or(false)
                {
                    return AutoSaveOutcome::Stale;
                }
                let writing = enabled.clone();
                let expected = staged.clone();
                match executor
                    .spawn(async move { writing.save_if_unchanged(&expected, staged_revision) })
                    .await
                {
                    Ok(_) => AutoSaveOutcome::Enabled {
                        config: enabled,
                        profile,
                        service,
                        key_revision,
                        secret,
                    },
                    Err(error) => AutoSaveOutcome::Failed {
                        staged: Some(staged),
                        message: Self::label(
                            "配置保存失败，AI 已暂停；输入已保留。继续编辑会再次尝试。",
                            "Could not save settings; AI is paused and your input was kept. Editing will try again.",
                        ),
                        conflict: error.is::<ConfigChanged>(),
                    },
                }
            }
            .await;
            if let AutoSaveOutcome::Enabled { config, .. } = &outcome {
                let revision = cancel.load(Ordering::Acquire);
                if revision != 0 {
                    let expected = config.clone();
                    let mut disabled = expected.clone();
                    disabled.enabled = false;
                    executor
                        .spawn(async move {
                            let _ = disabled.save_if_unchanged(&expected, revision);
                        })
                        .await;
                }
            }
            let _ = this.update(cx, |this, cx| this.finish_auto_save(generation, outcome, cx));
        })
        .detach();
    }

    fn finish_auto_save(&mut self, generation: u64, outcome: AutoSaveOutcome, cx: &mut Context<'_, Self>) {
        let current = self.save_generation == generation;
        let completed_enabled = current && matches!(&outcome, AutoSaveOutcome::Enabled { .. });
        let queued_test_failure = match &outcome {
            AutoSaveOutcome::NeedsKey(..) => Self::label(
                "尚未配置 Key，连接测试未开始。",
                "No key is configured; the connection test did not start.",
            ),
            AutoSaveOutcome::Invalid(message) => *message,
            AutoSaveOutcome::Failed { message, conflict, .. } => {
                if *conflict {
                    Self::label(
                        "配置已在其他操作中更改，连接测试未开始。",
                        "Settings changed elsewhere; the connection test did not start.",
                    )
                } else {
                    *message
                }
            },
            AutoSaveOutcome::Disabled(..) => Self::label(
                "AI 已关闭，连接测试未开始。",
                "AI is off; the connection test did not start.",
            ),
            AutoSaveOutcome::Enabled { .. } | AutoSaveOutcome::Stale => Self::label(
                "配置未能应用，连接测试未开始。",
                "Settings were not applied; the connection test did not start.",
            ),
        };
        let completed = self.save_generation == generation
            && matches!(
                &outcome,
                AutoSaveOutcome::Disabled(..) | AutoSaveOutcome::Enabled { .. } | AutoSaveOutcome::NeedsKey(..)
            );
        self.save_running = false;
        self.operation_cancel = None;
        if self.save_generation == generation {
            match outcome {
                AutoSaveOutcome::Disabled(config, drifted) => {
                    self.config = config;
                    self.own_writes.clear();
                    self.persistence_failed = false;
                    self.load_failed = drifted;
                    self.status = if drifted {
                        Self::label(
                            "AI 已关闭，但配置曾在其他操作中更改；请重新打开设置。",
                            "AI is off, but settings changed elsewhere. Reopen settings.",
                        )
                        .into()
                    } else {
                        String::new()
                    };
                    self.changed();
                    if drifted {
                        self.set_busy(false, cx);
                    }
                },
                AutoSaveOutcome::Enabled {
                    config,
                    profile,
                    service,
                    key_revision,
                    secret,
                } => {
                    self.config = config;
                    self.own_writes.clear();
                    self.profile = profile;
                    self.persistence_failed = false;
                    if !secret.is_empty()
                        && self.key.read(cx).edit_revision() == key_revision
                        && self.key.read(cx).value().as_bytes() == secret
                        && key_draft_matches(self.key_draft_service.as_deref(), &service)
                    {
                        // Keep the masked draft while this field may still have
                        // focus: clearing it mid-typing would turn the next
                        // characters into a new replacement key. Navigation
                        // clears it to the stored-key placeholder instead.
                        self.saved_key_revision = Some(key_revision);
                        self.saved_key_service = Some(service.clone());
                    }
                    self.record_key_presence(&service, true, cx);
                    self.status.clear();
                    self.changed();
                },
                AutoSaveOutcome::NeedsKey(config, service) => {
                    self.config = config;
                    self.own_writes.clear();
                    self.persistence_failed = false;
                    self.record_key_presence(&service, false, cx);
                    self.status = Self::label(
                        "请输入 API Key；填写后会自动启用 AI。",
                        "Enter an API key; AI will turn on automatically afterward.",
                    )
                    .into();
                    self.changed();
                },
                AutoSaveOutcome::Invalid(message) => {
                    self.status = message.into();
                },
                AutoSaveOutcome::Failed {
                    staged,
                    message,
                    conflict,
                } => {
                    if let Some(config) = staged {
                        self.config = config;
                    }
                    self.config.enabled = false;
                    self.persistence_failed = true;
                    self.status = if conflict {
                        Self::label(
                            "配置已在其他操作中更改；AI 已暂停。请重新打开设置。",
                            "Settings changed elsewhere; AI is paused. Reopen settings.",
                        )
                    } else {
                        message
                    }
                    .into();
                    if conflict {
                        self.load_failed = true;
                        self.set_busy(false, cx);
                    }
                    self.changed();
                },
                AutoSaveOutcome::Stale => {},
            }
        }
        if self.save_pending && self.save_ready {
            self.start_auto_save(cx);
        }
        if self.dismissed && completed && !self.save_running && !self.save_pending {
            self.clear_saved_key_draft(cx);
        }
        let config_usable = self.config.enabled
            && !self.load_failed
            && self
                .config
                .active_profile()
                .is_some_and(|profile| profile.validate().is_ok());
        match queued_probe_disposition(
            self.test_after_save,
            !self.save_running && !self.save_pending && !self.busy,
            completed_enabled && config_usable,
        ) {
            QueuedProbeDisposition::Run => {
                self.test_after_save = false;
                self.test_connection(cx);
            },
            QueuedProbeDisposition::Fail => self.fail_queued_test(queued_test_failure),
            QueuedProbeDisposition::None | QueuedProbeDisposition::Wait => {},
        }
        cx.notify();
    }

    fn delete_profile(&mut self, profile: Profile, cx: &mut Context<'_, Self>) {
        if self.busy || self.load_failed || self.save_running || self.save_pending {
            return;
        }
        let Ok(service) = profile.credential_key() else {
            return;
        };
        self.suspend_for_edit(cx);
        self.enabled = false;
        self.discard_draft(cx);
        let pause = self.pause_write.take();
        let expected = self.config.clone();
        let expected_revision = runtime_revision();
        let mut disabled = self.config.clone();
        disabled.enabled = false;
        let epoch = self.epoch;
        self.set_busy(true, cx);
        self.status = Self::label("正在删除…", "Deleting…").into();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let expected_revision = match pause {
                Some(pause) => match pause.await {
                    Ok(revision) => revision,
                    Err(changed) => {
                        let _ = this.update(cx, |this, cx| {
                            this.set_busy(false, cx);
                            this.save_error(changed, cx);
                        });
                        return;
                    },
                },
                None => expected_revision,
            };
            let staged = disabled.clone();
            let result = executor
                .spawn(async move { staged.save_if_unchanged(&expected, expected_revision) })
                .await;
            let operation = this
                .update(cx, |this, cx| {
                    if this.epoch != epoch {
                        this.set_busy(false, cx);
                        return None;
                    }
                    let revision = match result {
                        Ok(revision) => revision,
                        Err(error) => {
                            this.set_busy(false, cx);
                            this.save_error(error.is::<ConfigChanged>(), cx);
                            return None;
                        },
                    };
                    this.config = disabled.clone();
                    this.changed();
                    this.invalidate_key_presence(&service, cx);
                    Some((credentials::delete(&service, cx), revision))
                })
                .ok()
                .flatten();
            let Some((operation, revision)) = operation else {
                return;
            };
            let result = operation.await;
            let proceed = this
                .update(cx, |this, cx| {
                    if this.epoch != epoch {
                        this.set_busy(false, cx);
                        return false;
                    }
                    if let Err(error) = result {
                        this.set_busy(false, cx);
                        this.status = Self::credential_error(error).into();
                        return false;
                    }
                    this.record_key_presence(&service, false, cx);
                    true
                })
                .unwrap_or(false);
            if !proceed {
                return;
            }
            let mut updated = disabled.clone();
            updated.remove_profile(&profile.id);
            let saving = updated.clone();
            let result = executor
                .spawn(async move { saving.save_if_unchanged(&disabled, revision) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.set_busy(false, cx);
                if this.epoch != epoch {
                    return;
                }
                if let Err(error) = result {
                    this.save_error(error.is::<ConfigChanged>(), cx);
                    return;
                }
                this.config = updated;
                this.persistence_failed = false;
                if this.profile.id == profile.id {
                    let next = this
                        .config
                        .active_profile()
                        .cloned()
                        .or_else(|| this.config.profiles.first().cloned())
                        .unwrap_or_else(Profile::typesafe);
                    this.select(next, cx);
                    this.record_key_presence(&service, false, cx);
                }
                this.changed();
                this.status = Self::label("已删除 Key 和配置。", "Key and profile deleted.").into();
            });
        })
        .detach();
    }

    fn button(
        &mut self,
        id: String,
        label: String,
        cx: &mut Context<'_, Self>,
        action: impl Fn(&mut Self, &mut Context<'_, Self>) + 'static,
    ) -> gpui::AnyElement {
        let chrome = Chrome::current();
        let deleting = id.starts_with("jev-delete-");
        let disabled = self.busy || self.load_failed || (deleting && (self.save_running || self.save_pending));
        let selected = matches!(
            (id.as_str(), self.profile.provider),
            ("jev-typesafe", Provider::TypeSafe)
                | ("jev-openrouter", Provider::OpenRouter)
                | ("jev-custom", Provider::CustomSystemOne)
        );
        let focus = self
            .buttons
            .entry(id.clone())
            .or_insert_with(|| cx.focus_handle())
            .clone()
            .tab_stop(!disabled);
        let action = Rc::new(action);
        let click_action = action.clone();
        let click_entity = cx.entity();
        let key_entity = cx.entity();
        let click_focus = focus.clone();
        div()
            .id(gpui::SharedString::from(id))
            .track_focus(&focus)
            .tab_stop(!disabled)
            .when(disabled, |button| button.opacity(0.45))
            .px(px(10.))
            .py(px(6.))
            .rounded_md()
            .border_1()
            .border_color(rgb(chrome.separator))
            .text_size(px(12.))
            .text_color(rgb(chrome.text))
            .when(selected, |button| {
                button
                    .bg(rgb(chrome.selection))
                    .border_color(rgb(chrome.accent))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
            })
            .when(!disabled, |button| button.cursor_pointer())
            .hover(|style| style.bg(rgb(chrome.selection)))
            .focus(|style| style.border_color(rgb(chrome.accent)))
            .child(label)
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                if !disabled {
                    click_focus.focus(window);
                    click_entity.update(cx, |this, cx| {
                        if !this.busy && !this.load_failed && (!deleting || (!this.save_running && !this.save_pending))
                        {
                            click_action(this, cx);
                        }
                    });
                }
                cx.stop_propagation();
            })
            .on_key_down(move |event, _, cx| {
                if !disabled && matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    key_entity.update(cx, |this, cx| {
                        if !this.busy && !this.load_failed && (!deleting || (!this.save_running && !this.save_pending))
                        {
                            action(this, cx);
                        }
                    });
                    cx.stop_propagation();
                }
            })
            .into_any_element()
    }

    fn enable_toggle(&mut self, cx: &mut Context<'_, Self>) -> gpui::AnyElement {
        let chrome = Chrome::current();
        let disabled = self.busy || self.load_failed;
        let checked = self.enabled;
        let focus = self
            .buttons
            .entry("jev-enable".into())
            .or_insert_with(|| cx.focus_handle())
            .clone()
            .tab_stop(!disabled);
        let click_focus = focus.clone();
        let key_entity = cx.entity();
        let click_entity = cx.entity();
        div()
            .id("jev-enable")
            .track_focus(&focus)
            .tab_stop(!disabled)
            .border_1()
            .rounded(px(11.))
            .border_color(rgb(chrome.card))
            .when(disabled, |toggle| toggle.opacity(0.45))
            .focus(|style| style.border_color(rgb(chrome.accent)))
            .child(super::toggle("jev-enable-switch".into(), checked, chrome, move |cx| {
                click_entity.update(cx, |this, cx| this.toggle_enabled(cx));
            }))
            .on_mouse_down(MouseButton::Left, move |_, window, _| {
                if !disabled {
                    click_focus.focus(window);
                }
            })
            .on_key_down(move |event, _, cx| {
                if !disabled && matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    key_entity.update(cx, |this, cx| this.toggle_enabled(cx));
                    cx.stop_propagation();
                }
            })
            .into_any_element()
    }

    fn toggle_enabled(&mut self, cx: &mut Context<'_, Self>) {
        if self.busy || self.load_failed {
            return;
        }
        self.enabled = !self.enabled;
        self.test_after_save = false;
        self.probe_status.clear();
        self.probe_kind = ProbeKind::Idle;
        self.queue_save(true, cx);
    }
}

impl Drop for AiSettings {
    fn drop(&mut self) {
        if self.dismissal_revision.is_none() {
            if let Some(cancel) = self.operation_cancel.take() {
                let revision = pause_runtime();
                cancel.store(revision, Ordering::Release);
            }
        }
        if let Some(abort) = self.probe_abort.take() {
            abort.abort();
        }
    }
}

impl Render for AiSettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        self.dismissed = false;
        self.buttons.retain(|id, _| {
            let profile_id = id
                .strip_prefix("jev-select-")
                .or_else(|| id.strip_prefix("jev-delete-"));
            profile_id.is_none_or(|id| self.config.profiles.iter().any(|profile| profile.id == id))
        });
        let chrome = Chrome::current();
        let hint = |text: String| div().text_size(px(12.)).text_color(rgb(chrome.muted)).child(text);
        let state = if self.persistence_failed {
            Self::label("AI 已暂停 · 自动保存失败", "AI paused · automatic save failed")
        } else if self.save_pending || self.save_running {
            Self::label("正在自动保存", "Saving automatically")
        } else if self.config.enabled {
            Self::label("AI 推荐运行中", "AI recommendations are on")
        } else if self.enabled {
            Self::label("AI 尚未启用", "AI is not yet enabled")
        } else {
            Self::label("AI 推荐未启用", "AI recommendations are off")
        };
        let enable = self.enable_toggle(cx);
        let enable_body = div()
            .w_full()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .text_size(px(13.))
            .child(super::row(
                Self::label("启用 AI 推荐", "Enable AI recommendations"),
                Some(Self::label(
                    "让 Jev 从本地候选中推荐一项，沿用原有按键采纳。",
                    "Let Jev recommend a local completion. Accept it with your usual keys.",
                )),
                chrome,
                true,
                enable,
            ));
        let mut cards = div().w_full().min_w(px(0.)).flex().flex_col().child(super::card(
            Self::label("启用 AI", "Enable AI"),
            chrome,
            enable_body,
        ));
        if self.enabled {
            let draft = self.draft(cx);
            let custom = self.profile.provider == Provider::CustomSystemOne;
            let mut providers = div().flex().flex_wrap().gap(px(6.));
            for (id, name, provider) in [
                ("typesafe", "TypeSafe", Provider::TypeSafe),
                ("openrouter", "OpenRouter", Provider::OpenRouter),
                ("custom", Self::label("自定义", "Custom"), Provider::CustomSystemOne),
            ] {
                let label = if self.profile.provider == provider {
                    format!("✓ {name}")
                } else {
                    name.into()
                };
                providers = providers.child(self.button(format!("jev-{id}"), label, cx, move |this, cx| {
                    this.provider(provider, cx);
                }));
            }
            let test = self.button(
                "jev-test".into(),
                Self::label("测试连接", "Test connection").into(),
                cx,
                |this, cx| this.test_connection(cx),
            );
            let key_hint = match self.key_present {
                Some(true) => Self::label(
                    "已存 Key；留空继续使用，输入新 Key 后会自动替换。",
                    "A key is stored. Leave blank to keep it, or enter a replacement for automatic saving.",
                ),
                Some(false) => Self::label(
                    "尚未配置 Key；输入后会自动保存。",
                    "No key is configured. Enter one to save it automatically.",
                ),
                None => Self::label(
                    "正在检查已保存的 Key；留空可使用已有 Key。",
                    "Checking for a saved key. Leave blank to use an existing key.",
                ),
            };
            let mut basic = div()
                .p(px(16.))
                .flex()
                .flex_col()
                .gap(px(10.))
                .text_size(px(13.))
                .child(hint(Self::label(
                    "启用后会发送当前输入、Git 分支、近期命令和候选，用于推荐。",
                    "When enabled, current input, Git branch, recent commands and candidates are sent for recommendations.",
                ).into()))
                .child(Self::label("服务商", "Provider"))
                .child(providers);
            if custom {
                basic = basic.child("Base URL").child(self.base.clone()).child(hint(
                    Self::label(
                        "HTTPS 基地址，不含 /v1/systemone。",
                        "HTTPS base address, without /v1/systemone.",
                    )
                    .into(),
                ));
            }
            basic = basic
                .child("API Key")
                .child(self.key.clone())
                .child(hint(key_hint.into()))
                .child(hint(Self::label("Key 在本机数据库中明文保存，文件仅当前用户可读写。", "Keys are stored as plaintext locally; the database file is readable and writable only by your user account.").into()))
                .when(self.key.read(cx).rejected, |basic| basic.child(hint(Self::label("此次输入未接受：Key 不能含空白或换行，最多 4096 字符。原内容已保留。", "Input rejected: keys cannot contain whitespace or line breaks (maximum 4096 characters). The previous value was kept.").into())))
                .child(div().flex().items_center().flex_wrap().gap(px(10.)).child(test)
                    .child(hint(Self::label("仅发送固定示例。", "Sends only a fixed example.").into())))
                .when(!self.probe_status.is_empty(), |basic| {
                    basic.child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(self.probe_kind.color(chrome)))
                            .child(self.probe_status.clone()),
                    )
                });
            cards = cards.child(super::card(Self::label("基本设置", "Basic settings"), chrome, basic));
            let processors = match self.profile.provider {
                Provider::TypeSafe => Self::label(
                    "由 TypeSafe 在美国处理，不承诺零保留。",
                    "Processed by TypeSafe in the US; zero retention is not guaranteed.",
                ),
                Provider::OpenRouter => Self::label(
                    "经 OpenRouter 与 TypeSafe 处理，不承诺零保留。",
                    "Processed by OpenRouter and TypeSafe; zero retention is not guaranteed.",
                ),
                Provider::CustomSystemOne => Self::label(
                    "由自定义地址及其上游处理，请确认其数据政策。",
                    "Processed by the custom service and its upstream providers. Review their data policies.",
                ),
            };
            let destination = normalize_base_url(&draft.base_url).map_or_else(
                |_| Self::label("地址无效", "Invalid address").into(),
                |(_, endpoint)| endpoint.to_string(),
            );
            let mut detail = div().flex().flex_col().gap(px(10.)).p(px(16.));
            detail = detail
                .child(Self::label("Jev 模型", "Jev model"))
                .child(self.model.clone());
            detail = detail.child(hint(format!("{} {destination}", Self::label("请求地址：", "Endpoint:"))))
                .child(hint(Self::label("预设地址固定。需使用其他 System One 服务时选择自定义。", "Preset addresses are fixed. Choose Custom for another System One service.").into()))
                .child(hint(Self::label("请求包含当前输入、Git 分支、当前目录最近最多 10 条命令，以及公开命令路径、前缀、shell 和候选说明。历史总长最多 2 KB，跳过含明显凭据的命令；不发送环境变量或目录字段。", "Requests include current input, Git branch, up to 10 recent commands from this directory, plus public command paths, prefixes, shell and candidate descriptions. History is capped at 2 KB; commands with obvious credentials are omitted. Environment variables and directory fields are excluded.").into()))
                .child(hint(processors.into()));
            let mut policies = div().flex().flex_wrap().gap(px(6.));
            if !custom {
                policies = policies
                    .child(self.button(
                        "jev-typesafe-privacy".into(),
                        Self::label("TypeSafe 隐私政策", "TypeSafe privacy").into(),
                        cx,
                        |_, cx| cx.open_url("https://typesafe.ai/legal/privacy-policy"),
                    ))
                    .child(self.button(
                        "jev-typesafe-terms".into(),
                        Self::label("服务条款", "Terms").into(),
                        cx,
                        |_, cx| cx.open_url("https://typesafe.ai/legal/mca"),
                    ));
            }
            if self.profile.provider == Provider::OpenRouter {
                policies = policies.child(self.button(
                    "jev-openrouter-privacy".into(),
                    Self::label("OpenRouter 数据政策", "OpenRouter data policy").into(),
                    cx,
                    |_, cx| cx.open_url("https://openrouter.ai/docs/guides/privacy/data-collection"),
                ));
            }
            detail = detail.child(policies);
            if !self.config.profiles.is_empty() {
                detail = detail.child(Self::label("配置与 Key 管理", "Profile and key management"));
            }
            for profile in self.config.profiles.clone() {
                let select_profile = profile.clone();
                let label = format!(
                    "{} · {}",
                    match profile.provider {
                        Provider::TypeSafe => "TypeSafe",
                        Provider::OpenRouter => "OpenRouter",
                        Provider::CustomSystemOne => "Custom",
                    },
                    profile.base_url
                );
                let select = self.button(format!("jev-select-{}", profile.id), label, cx, move |this, cx| {
                    this.select(select_profile.clone(), cx);
                });
                let delete = self.button(
                    format!("jev-delete-{}", profile.id),
                    Self::label("删除", "Delete").into(),
                    cx,
                    move |this, cx| this.delete_profile(profile.clone(), cx),
                );
                detail = detail.child(div().flex().flex_wrap().gap(px(6.)).child(select).child(delete));
            }
            let advanced_body = div().flex().flex_col().text_size(px(13.)).child(detail);
            cards = cards.child(super::card(
                Self::label("高级设置", "Advanced settings"),
                chrome,
                advanced_body,
            ));
        }
        cards = cards.child(
            div()
                .w_full()
                .min_w(px(0.))
                .px(px(16.))
                .pb(px(16.))
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(hint(state.into()))
                .when(!self.status.is_empty(), |footer| {
                    footer.child(hint(self.status.clone()))
                }),
        );
        cards
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_draft_tracks_normalized_credential_service() {
        let mut profile = Profile {
            id: "custom-test".into(),
            provider: Provider::CustomSystemOne,
            base_url: "https://example.com/one".into(),
            model: "jev-1.13.0".into(),
            data_policy_version: DATA_POLICY_VERSION,
        };
        let old_service = profile.credential_key().unwrap();
        profile.base_url = "https://example.com/one/".into();
        let equivalent_service = profile.credential_key().unwrap();
        assert_eq!(old_service, equivalent_service);
        assert_eq!(
            key_draft_transition(Some(&old_service), Some(&equivalent_service), true),
            KeyDraftTransition::Keep
        );

        profile.base_url = "https://example.com/two".into();
        let new_service = profile.credential_key().unwrap();
        assert_ne!(old_service, new_service);
        assert_eq!(
            key_draft_transition(Some(&old_service), None, true),
            KeyDraftTransition::Keep
        );
        assert_eq!(
            key_draft_transition(Some(&old_service), Some(&new_service), true),
            KeyDraftTransition::Clear
        );
        assert!(!key_draft_matches(Some(&old_service), &new_service));
        assert!(!key_draft_matches(None, &new_service));
        assert_eq!(
            key_draft_transition(None, Some(&new_service), true),
            KeyDraftTransition::Keep
        );
        assert_eq!(key_draft_binding_after_edit(None, None, true), None);
        assert_eq!(
            key_draft_binding_after_edit(None, Some(&new_service), true),
            Some(new_service.clone())
        );
        assert_eq!(
            key_draft_binding_after_edit(Some(&old_service), None, true),
            Some(old_service.clone())
        );
        assert_eq!(
            key_draft_binding_after_edit(Some(&old_service), Some(&new_service), true),
            Some(old_service.clone())
        );
        assert_eq!(key_draft_binding_after_edit(Some(&old_service), None, false), None);
        assert!(key_draft_matches(Some(&new_service), &new_service));
    }

    #[test]
    fn queued_probe_runs_only_after_current_usable_enabled_save() {
        assert_eq!(
            queued_probe_disposition(true, false, false),
            QueuedProbeDisposition::Wait
        );
        assert_eq!(queued_probe_disposition(true, true, true), QueuedProbeDisposition::Run);
        for (enabled_save, usable_config) in [(false, true), (true, false), (false, false)] {
            assert_eq!(
                queued_probe_disposition(true, true, enabled_save && usable_config),
                QueuedProbeDisposition::Fail
            );
        }
        assert_eq!(
            queued_probe_disposition(false, true, true),
            QueuedProbeDisposition::None
        );
    }
}
