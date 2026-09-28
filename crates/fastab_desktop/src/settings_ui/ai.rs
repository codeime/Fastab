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

use super::input::Input;
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

pub(super) struct AiSettings {
    config: AiConfig,
    profile: Profile,
    key: Entity<Input>,
    model: Entity<Input>,
    base: Entity<Input>,
    enabled: bool,
    advanced: bool,
    busy: bool,
    load_failed: bool,
    persistence_failed: bool,
    epoch: u64,
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
    _input_observations: [gpui::Subscription; 3],
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
            advanced: false,
            busy: false,
            load_failed: true,
            persistence_failed: false,
            epoch: 0,
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
            _input_observations: input_observations,
        }
    }

    fn label(zh: &'static str, en: &'static str) -> &'static str {
        if super::locale_is_zh() { zh } else { en }
    }

    /// Also invalidates every pending apply. The OS operation is deliberately
    /// allowed to finish, retaining its global credential lease until then.
    pub(super) fn clear_draft(&mut self, cx: &mut Context<'_, Self>) {
        self.observe_current_edits(cx);
        self.cancel_probe(cx);
        if self.busy {
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
        cx.notify();
    }

    fn draft(&self, cx: &App) -> Profile {
        let mut profile = self.profile.clone();
        profile.base_url = self.base.read(cx).value().to_owned();
        profile.model = self.model.read(cx).value().to_owned();
        // The explicit Enable/Save action accepts the disclosed data scope for
        // this draft. Existing persisted profiles are not rewritten on load.
        profile.data_policy_version = DATA_POLICY_VERSION;
        profile
    }

    fn select(&mut self, profile: Profile, cx: &mut Context<'_, Self>) {
        if self.busy {
            return;
        }
        self.status.clear();
        self.suspend_for_edit(cx);
        self.clear_draft(cx);
        self.model.update(cx, |input, cx| input.set(profile.model.clone(), cx));
        self.base
            .update(cx, |input, cx| input.set(profile.base_url.clone(), cx));
        self.profile = profile;
        self.refresh_key_presence(cx);
        cx.notify();
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
        } else {
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
        }
        cx.notify();
    }

    fn update_key_placeholder(&mut self, cx: &mut Context<'_, Self>) {
        let placeholder = self.key_present.filter(|present| *present).map(|_| "********".into());
        self.key.update(cx, |input, cx| input.set_placeholder(placeholder, cx));
    }

    fn refresh_key_presence(&mut self, cx: &mut Context<'_, Self>) {
        self.key_presence_generation = self.key_presence_generation.wrapping_add(1);
        self.key_presence_service = self.draft(cx).credential_key().ok();
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

    fn input_edited(&mut self, field: usize, revision: u64, cx: &mut Context<'_, Self>) {
        if self.edit_revisions[field] == revision {
            return;
        }
        self.edit_revisions[field] = revision;
        self.probe_status.clear();
        self.probe_kind = ProbeKind::Idle;
        self.suspend_for_edit(cx);
        if field == 2 {
            self.refresh_key_presence(cx);
        }
        cx.notify();
    }

    fn observe_current_edits(&mut self, cx: &mut Context<'_, Self>) {
        let revisions = [
            self.key.read(cx).edit_revision(),
            self.model.read(cx).edit_revision(),
            self.base.read(cx).edit_revision(),
        ];
        if revisions != self.edit_revisions {
            let base_changed = revisions[2] != self.edit_revisions[2];
            self.edit_revisions = revisions;
            self.probe_status.clear();
            self.probe_kind = ProbeKind::Idle;
            self.suspend_for_edit(cx);
            if base_changed {
                self.refresh_key_presence(cx);
            }
        }
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
        self.status = Self::label("已暂停；保存后应用更改。", "Paused. Save to apply changes.").into();
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
            "配置保存失败，AI 已暂停；输入已保留，请重试保存。",
            "Could not save settings. AI is paused; your input was kept. Retry saving.",
        )
        .into();
        cx.notify();
    }

    fn credential_error(error: CredentialError) -> &'static str {
        match error {
            CredentialError::TimedOut => Self::label(
                "系统钥匙串未及时响应；输入已保留，操作结束后可重试。",
                "System Keychain did not respond in time. Your input was kept; retry when it finishes.",
            ),
            CredentialError::Busy => Self::label(
                "凭据操作正在完成，请稍后重试；AI 保持关闭。",
                "A credential operation is still finishing. Retry shortly; AI remains off.",
            ),
            CredentialError::Unavailable | CredentialError::InvalidService => Self::label(
                "当前凭据不可用；AI 保持关闭。可重新保存密钥。",
                "Credentials are currently unavailable; AI remains off. You can save a new key.",
            ),
        }
    }

    fn cancel_probe(&mut self, cx: &mut Context<'_, Self>) {
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
        self.observe_current_edits(cx);
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
        let secret = self.key.read(cx).value().as_bytes().to_vec();
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

    fn save(&mut self, cx: &mut Context<'_, Self>) {
        if self.busy || self.load_failed {
            return;
        }
        self.observe_current_edits(cx);
        self.suspend_for_edit(cx);
        if self.key.read(cx).rejected || !self.key.read(cx).value().bytes().all(|byte| byte.is_ascii_graphic()) {
            self.status = Self::label(
                "请检查 Key：仅支持可见 ASCII 字符，不能含空白。",
                "Check the key: only visible ASCII characters without whitespace are accepted.",
            )
            .into();
            cx.notify();
            return;
        }
        let mut profile = self.draft(cx);
        let desired_enabled = self.enabled;
        let service = match profile.credential_key() {
            Ok(service) => service,
            Err(_) => {
                self.status = Self::label(
                    "请输入有效的 HTTPS 基地址；预设地址不可修改。",
                    "Enter a valid HTTPS base address. Preset addresses are fixed.",
                )
                .into();
                cx.notify();
                return;
            },
        };
        if desired_enabled && profile.validate().is_err() {
            self.status = Self::label(
                "启用前请检查地址和 Jev 模型。",
                "Check the address and Jev model before enabling.",
            )
            .into();
            cx.notify();
            return;
        }
        profile.id = self
            .config
            .profiles
            .iter()
            .find(|old| old.credential_key().ok().as_deref() == Some(service.as_str()))
            .map_or_else(|| uuid::Uuid::new_v4().to_string(), |old| old.id.clone());
        let mut disabled = self.config.clone();
        if disabled.upsert_profile(profile.clone()).is_err() {
            self.status = Self::label(
                "无法保存：请检查地址、模型或删除多余配置（最多 16 个）。",
                "Cannot save: check the address/model or remove unused profiles (maximum 16).",
            )
            .into();
            cx.notify();
            return;
        }
        disabled.active_profile_id = Some(profile.id.clone());
        disabled.enabled = false;
        // No main-thread file locks, writes, or specs hashing. Explicit saves
        // follow the outstanding pause write so it cannot overwrite this save.
        let pause = self.pause_write.take();
        let expected = self.config.clone();
        let expected_revision = runtime_revision();
        let secret = self.key.read(cx).value().as_bytes().to_vec();
        let replacing = !secret.is_empty();
        self.epoch = self.epoch.wrapping_add(1);
        let epoch = self.epoch;
        self.set_busy(true, cx);
        self.status = Self::label("正在保存…", "Saving…").into();
        let cancel = Arc::new(AtomicU64::new(0));
        self.operation_cancel = Some(cancel.clone());
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
                .spawn(async move {
                    if desired_enabled && !fastab_engine::public_ai_baseline_ok(&fastab_engine::default_specs_dir()) {
                        return Err(None);
                    }
                    staged.save_if_unchanged(&expected, expected_revision).map_err(Some)
                })
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
                            if error.is_none() {
                                this.status = Self::label(
                                    "补全规格校验未通过，无法启用 AI；请重新安装完整应用。",
                                    "Completion specs failed validation. Reinstall the complete app to enable AI.",
                                )
                                .into();
                            } else {
                                this.save_error(error.is_some_and(|error| error.is::<ConfigChanged>()), cx);
                            }
                            return None;
                        },
                    };
                    this.config = disabled.clone();
                    this.profile = profile;
                    this.persistence_failed = false;
                    this.changed();
                    if !replacing && !desired_enabled {
                        this.set_busy(false, cx);
                        this.status = Self::label("已保存，AI 已关闭。", "Saved. AI is off.").into();
                        return None;
                    }
                    this.status = Self::label("正在处理系统钥匙串…", "Updating system Keychain…").into();
                    let credential = if replacing {
                        this.invalidate_key_presence(&service, cx);
                        credentials::write(&service, secret, cx)
                    } else {
                        let read = credentials::read(&service, cx);
                        cx.spawn(async move |_, _| match read.await {
                            Ok(Some(secret)) if !secret.is_empty() => Ok(()),
                            Ok(_) => Err(CredentialError::Unavailable),
                            Err(error) => Err(error),
                        })
                    };
                    Some((credential, revision))
                })
                .ok()
                .flatten();
            let Some((operation, revision)) = operation else {
                return;
            };
            let credential = operation.await;
            let proceed = this
                .update(cx, |this, cx| {
                    if this.epoch != epoch {
                        this.set_busy(false, cx);
                        return false;
                    }
                    if let Err(error) = credential {
                        this.set_busy(false, cx);
                        this.status = Self::credential_error(error).into();
                        return false;
                    }
                    this.record_key_presence(&service, true, cx);
                    true
                })
                .unwrap_or(false);
            if !proceed {
                return;
            }
            let mut final_config = disabled.clone();
            final_config.enabled = desired_enabled;
            let saving = final_config.clone();
            let result = executor
                .spawn(async move { saving.save_if_unchanged(&disabled, revision) })
                .await;
            let applied = this
                .update(cx, |this, cx| {
                    this.set_busy(false, cx);
                    if this.epoch != epoch {
                        return false;
                    }
                    if let Err(error) = result {
                        this.save_error(error.is::<ConfigChanged>(), cx);
                        return true;
                    }
                    this.config = final_config.clone();
                    this.enabled = desired_enabled;
                    this.persistence_failed = false;
                    this.key.update(cx, |input, cx| {
                        input.take(cx);
                    });
                    this.changed();
                    this.status = if desired_enabled {
                        Self::label("已保存并开启 AI 推荐。", "Saved. AI recommendations are on.")
                    } else {
                        Self::label("Key 已保存，AI 已关闭。", "Key saved. AI is off.")
                    }
                    .into();
                    true
                })
                .unwrap_or(false);
            if !applied {
                // Closing can race the gap between a successful background
                // write and its UI acknowledgement. Fence that write on disk
                // too, without overwriting any newer window's saved profile.
                let revision = cancel.load(Ordering::Acquire);
                if revision != 0 {
                    let mut paused = final_config.clone();
                    paused.enabled = false;
                    executor
                        .spawn(async move {
                            let _ = paused.save_if_unchanged(&final_config, revision);
                        })
                        .await;
                }
            }
        })
        .detach();
    }

    fn delete_profile(&mut self, profile: Profile, cx: &mut Context<'_, Self>) {
        if self.busy || self.load_failed {
            return;
        }
        let Ok(service) = profile.credential_key() else {
            return;
        };
        self.suspend_for_edit(cx);
        self.enabled = false;
        self.clear_draft(cx);
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
        let disclosure = id == "jev-advanced";
        let disabled = (self.busy || self.load_failed) && !disclosure;
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
        let primary = id == "jev-save";
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
            .when(primary, |button| button.bg(rgb(chrome.accent)))
            .px(px(10.))
            .py(px(6.))
            .rounded_md()
            .border_1()
            .border_color(rgb(chrome.separator))
            .text_size(px(12.))
            .text_color(rgb(if primary { chrome.accent_text } else { chrome.text }))
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
                        if disclosure || (!this.busy && !this.load_failed) {
                            click_action(this, cx);
                        }
                    });
                }
                cx.stop_propagation();
            })
            .on_key_down(move |event, _, cx| {
                if !disabled && matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    key_entity.update(cx, |this, cx| {
                        if disclosure || (!this.busy && !this.load_failed) {
                            action(this, cx);
                        }
                    });
                    cx.stop_propagation();
                }
            })
            .into_any_element()
    }

    fn enable_checkbox(&mut self, cx: &mut Context<'_, Self>) -> gpui::AnyElement {
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
        let click_entity = cx.entity();
        let key_entity = cx.entity();
        let box_color = if checked { chrome.accent } else { chrome.card };
        div()
            .id("jev-enable")
            .track_focus(&focus)
            .tab_stop(!disabled)
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(4.))
            .py(px(4.))
            .border_1()
            .rounded(px(5.))
            .border_color(rgb(chrome.card))
            .cursor_pointer()
            .when(disabled, |checkbox| checkbox.opacity(0.45))
            .focus(|style| style.border_color(rgb(chrome.accent)).bg(rgb(chrome.selection)))
            .child(
                div()
                    .w(px(18.))
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .border_1()
                    .rounded(px(4.))
                    .border_color(rgb(if checked { chrome.accent } else { chrome.separator }))
                    .bg(rgb(box_color))
                    .text_color(rgb(chrome.accent_text))
                    .child(if checked { "✓" } else { "" }),
            )
            .child(Self::label("启用 AI 推荐", "Enable AI recommendations"))
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                if !disabled {
                    click_focus.focus(window);
                    click_entity.update(cx, |this, cx| this.toggle_enabled(cx));
                }
                cx.stop_propagation();
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
        if !self.enabled {
            self.suspend_for_edit(cx);
        }
        cx.notify();
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
        self.buttons.retain(|id, _| {
            let profile_id = id
                .strip_prefix("jev-select-")
                .or_else(|| id.strip_prefix("jev-delete-"));
            profile_id.is_none_or(|id| self.config.profiles.iter().any(|profile| profile.id == id))
        });
        let chrome = Chrome::current();
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
        let save = self.button(
            "jev-save".into(),
            Self::label("保存", "Save").into(),
            cx,
            |this, cx| this.save(cx),
        );
        let enable = self.enable_checkbox(cx);
        let advanced_label = if self.advanced {
            Self::label("▾ 收起高级设置", "▾ Hide advanced settings")
        } else {
            Self::label("▸ 高级设置与已存配置", "▸ Advanced settings and saved profiles")
        };
        let advanced = self.button("jev-advanced".into(), advanced_label.into(), cx, |this, cx| {
            this.advanced = !this.advanced;
            cx.notify();
        });
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
        let hint = |text: String| div().text_size(px(12.)).text_color(rgb(chrome.muted)).child(text);
        let state = if self.persistence_failed {
            Self::label("AI 已暂停 · 保存失败，请重试", "AI paused · save failed, please retry")
        } else if self.config.enabled {
            Self::label("AI 推荐运行中", "AI recommendations are on")
        } else {
            Self::label("AI 推荐未启用", "AI recommendations are off")
        };
        let mut body = div()
            .p(px(16.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .text_size(px(13.))
            .child(hint(
                Self::label(
                    "让 Jev 从本地候选中推荐一项，沿用原有按键采纳。",
                    "Let Jev recommend a local completion. Accept it with your usual keys.",
                )
                .into(),
            ))
            .child(Self::label("服务商", "Provider"))
            .child(providers);
        if custom {
            body = body
                .child("Base URL")
                .child(self.base.clone())
                .child(hint(
                    Self::label(
                        "HTTPS 基地址，不含 /v1/systemone。",
                        "HTTPS base address, without /v1/systemone.",
                    )
                    .into(),
                ))
                .child(Self::label("Jev 模型", "Jev model"))
                .child(self.model.clone());
        }
        let key_hint = match self.key_present {
            Some(true) => Self::label(
                "已保存 Key；留空继续使用，输入新 Key 可替换。",
                "A key is saved. Leave blank to keep using it, or enter a replacement.",
            ),
            Some(false) => Self::label("尚未保存 Key；请输入后保存。", "No key is saved. Enter one to save."),
            None => Self::label(
                "Key 保存在系统钥匙串；留空可使用已有 Key。",
                "Keys are stored in system Keychain. Leave blank to use an existing key.",
            ),
        };
        body = body.child("API Key").child(self.key.clone())
            .child(hint(key_hint.into()))
            .when(self.key.read(cx).rejected, |body| body.child(hint(Self::label("此次输入未接受：Key 不能含空白或换行，最多 4096 字符。原内容已保留。", "Input rejected: keys cannot contain whitespace or line breaks (maximum 4096 characters). The previous value was kept.").into())))
            .child(div().flex().items_center().flex_wrap().gap(px(10.)).child(test)
                .child(hint(Self::label("仅发送固定示例；不会保存或启用配置。", "Sends a fixed example; does not save or enable settings.").into())))
            .when(!self.probe_status.is_empty(), |body| {
                body.child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(self.probe_kind.color(chrome)))
                        .child(self.probe_status.clone()),
                )
            })
            .child(div().border_t_1().border_color(rgb(chrome.separator)).pt(px(12.)).child(enable))
            .child(hint(
                Self::label(
                    "启用即允许发送公开命令、前缀和候选；不发送完整输入、目录或历史。",
                    "Enabling permits public commands, prefixes and candidates to be sent; never full input, directories or history.",
                )
                .into(),
            ));
        body = body
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(save)
                    .child(hint(state.into())),
            )
            .when(!self.status.is_empty(), |body| body.child(hint(self.status.clone())))
            .child(advanced);
        if self.advanced {
            let destination = normalize_base_url(&draft.base_url).map_or_else(
                |_| Self::label("地址无效", "Invalid address").into(),
                |(_, endpoint)| endpoint.to_string(),
            );
            let mut detail = div().flex().flex_col().gap(px(10.)).pt(px(4.));
            if !custom {
                detail = detail
                    .child(Self::label("Jev 模型", "Jev model"))
                    .child(self.model.clone());
            }
            detail = detail.child(hint(format!("{} {destination}", Self::label("请求地址：", "Endpoint:"))))
                .child(hint(Self::label("预设地址固定。需使用其他 System One 服务时选择自定义。", "Preset addresses are fixed. Choose Custom for another System One service.").into()))
                .child(hint(Self::label("自动请求的完整范围：公开命令/子命令路径、已知 token 前缀、shell 类型、公开候选 ID、名称与说明。不发送环境变量、别名、动态资源或实际插入文本。", "Automatic requests include public command/subcommand paths, known token prefixes, shell type and public candidate IDs, names and descriptions. Environment variables, aliases, dynamic resources and actual insertion text are excluded.").into()))
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
            body = body.child(detail);
        }
        super::card(Self::label("AI 候选推荐", "AI recommendations"), chrome, body)
    }
}
