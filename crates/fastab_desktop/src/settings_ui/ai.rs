use std::collections::BTreeMap;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{App, Context, Entity, FocusHandle, MouseButton, Window, div, px, rgb};

use crate::EventLoopProxy;
use crate::event::Event;
use crate::jev::config::{AiConfig, Profile, Provider, normalize_base_url, runtime_revision};
use crate::jev::credentials::{self, CredentialError};
use crate::jev::policy::DATA_POLICY_VERSION;

use super::input::Input;
use super::theme::Chrome;

pub(super) struct AiSettings {
    config: AiConfig,
    profile: Profile,
    key: Entity<Input>,
    model: Entity<Input>,
    base: Entity<Input>,
    enabled: bool,
    acknowledged_service: Option<String>,
    advanced: bool,
    busy: bool,
    load_failed: bool,
    persistence_failed: bool,
    epoch: u64,
    status: String,
    buttons: BTreeMap<String, FocusHandle>,
    proxy: EventLoopProxy,
    edit_revisions: [u64; 3],
    _input_observations: [gpui::Subscription; 3],
}

impl AiSettings {
    pub(super) fn new(proxy: EventLoopProxy, cx: &mut Context<'_, Self>) -> Self {
        let loaded = AiConfig::load();
        let load_failed = loaded.is_err();
        let config = loaded.unwrap_or_default();
        let profile = config.active_profile().cloned().unwrap_or_else(Profile::typesafe);
        let acknowledged_service = (profile.data_policy_version == DATA_POLICY_VERSION)
            .then(|| profile.credential_key().ok())
            .flatten();
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
        Self {
            enabled: config.enabled,
            config,
            profile: profile.clone(),
            key,
            model,
            base,
            acknowledged_service,
            advanced: false,
            busy: false,
            load_failed,
            persistence_failed: false,
            epoch: 0,
            status: if load_failed {
                Self::label(
                    "无法读取 AI 配置；请关闭并重新打开设置。",
                    "Could not read AI settings. Close and reopen settings.",
                )
                .into()
            } else {
                String::new()
            },
            buttons: BTreeMap::new(),
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
        profile.data_policy_version = if profile.credential_key().ok().as_ref() == self.acknowledged_service.as_ref()
            && self.acknowledged_service.is_some()
        {
            DATA_POLICY_VERSION
        } else {
            0
        };
        profile
    }

    fn select(&mut self, profile: Profile, cx: &mut Context<'_, Self>) {
        self.status.clear();
        self.suspend_for_edit(cx);
        self.clear_draft(cx);
        self.acknowledged_service = (profile.data_policy_version == DATA_POLICY_VERSION)
            .then(|| profile.credential_key().ok())
            .flatten();
        self.model.update(cx, |input, cx| input.set(profile.model.clone(), cx));
        self.base
            .update(cx, |input, cx| input.set(profile.base_url.clone(), cx));
        self.profile = profile;
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
        for field in [&self.key, &self.model, &self.base] {
            field.update(cx, |input, cx| {
                input.enabled = !busy;
                cx.notify();
            });
        }
        cx.notify();
    }

    fn changed(&self) {
        let _ = self.proxy.send_event(Event::JevSettingsChanged);
    }

    fn input_edited(&mut self, field: usize, revision: u64, cx: &mut Context<'_, Self>) {
        if self.edit_revisions[field] == revision {
            return;
        }
        self.edit_revisions[field] = revision;
        self.suspend_for_edit(cx);
        cx.notify();
    }

    fn observe_current_edits(&mut self, cx: &mut Context<'_, Self>) {
        let revisions = [
            self.key.read(cx).edit_revision(),
            self.model.read(cx).edit_revision(),
            self.base.read(cx).edit_revision(),
        ];
        if revisions != self.edit_revisions {
            self.edit_revisions = revisions;
            self.suspend_for_edit(cx);
        }
    }

    /// Only the first user edit of an enabled configuration writes the disabled
    /// state. Subsequent characters stay in the draft, including its intended
    /// enabled switch; programmatic field resets do not count as user edits.
    fn suspend_for_edit(&mut self, cx: &mut Context<'_, Self>) {
        if !self.config.enabled {
            return;
        }
        let mut disabled = self.config.clone();
        disabled.enabled = false;
        self.epoch = self.epoch.wrapping_add(1);
        let saved = self.persist(
            &disabled,
            Self::label("暂停旧配置时保存失败。", "Saving the paused configuration failed."),
            cx,
        );
        self.config = disabled;
        if saved {
            self.changed();
            self.status = Self::label(
                "旧配置已暂停；完成编辑并明确保存后才重新启用。",
                "The previous configuration is paused. Edit and explicitly save to enable again.",
            )
            .into();
        }
        cx.notify();
    }

    fn persist(&mut self, config: &AiConfig, failure: &str, cx: &mut Context<'_, Self>) -> bool {
        if config.save().is_err() {
            // Config::save latches the process off even when settings storage
            // mutated its in-memory copy before returning an I/O error.
            self.config.enabled = false;
            self.persistence_failed = true;
            self.changed();
            self.status = format!(
                "{failure} {}",
                Self::label(
                    "本次运行已停止 AI 请求；磁盘状态未确认，请重试保存。",
                    "AI requests are stopped for this run. The disk state is unconfirmed; retry saving."
                )
            );
            cx.notify();
            return false;
        }
        self.persistence_failed = false;
        true
    }

    fn credential_error(error: CredentialError) -> &'static str {
        match error {
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

    fn save(&mut self, cx: &mut Context<'_, Self>) {
        if self.busy || self.load_failed {
            return;
        }
        self.observe_current_edits(cx);
        self.suspend_for_edit(cx);
        let secret = self.key.update(cx, |input, cx| input.take(cx));
        if !secret.bytes().all(|byte| byte.is_ascii_graphic()) {
            self.status = Self::label("API Key 只能包含可见 ASCII 字符且不能含空白；草稿已清除，请重新粘贴。", "API keys must contain visible ASCII characters without whitespace. The draft was cleared; paste the key again.").into();
            cx.notify();
            return;
        }
        let mut profile = self.draft(cx);
        let desired_enabled = self.enabled;
        let service = match profile.credential_key() {
            Ok(service) => service,
            Err(_invalid_address) => {
                self.status = Self::label(
                    "Base URL 无效：预设地址固定，自定义地址须为 HTTPS 基地址。",
                    "Invalid Base URL: presets are fixed; custom profiles require an HTTPS base address.",
                )
                .into();
                cx.notify();
                return;
            },
        };
        if desired_enabled && profile.validate().is_err() {
            self.status = Self::label(
                "启用前请确认数据范围，并使用已支持的 Jev 模型。",
                "Confirm the data scope and select a supported Jev model before enabling.",
            )
            .into();
            cx.notify();
            return;
        }
        // Fail closed before Keychain work: a mismatched / missing IR pin means
        // public-static provenance cannot be proven, so enabling would only produce
        // silent no-ops at completion time.
        if desired_enabled && !fastab_engine::public_ai_baseline_ok(&fastab_engine::default_specs_dir()) {
            self.status = Self::label(
                "当前补全规格基线未通过公开静态校验，无法启用 AI 推荐。请使用匹配的 bundled specs-ir，或更新已审查的基线 pin。",
                "The completion specs baseline does not match the reviewed public pins, so AI recommendations cannot be enabled. Use a matching bundled specs-ir, or update the reviewed baseline pins.",
            )
            .into();
            cx.notify();
            return;
        }
        // Reuse an existing endpoint identity; changing the destination retains
        // the old profile so its old key remains explicitly deletable.
        profile.id = self
            .config
            .profiles
            .iter()
            .find(|old| old.credential_key().ok().as_deref() == Some(service.as_str()))
            .map_or_else(|| uuid::Uuid::new_v4().to_string(), |old| old.id.clone());
        let mut disabled = self.config.clone();
        if disabled.upsert_profile(profile.clone()).is_err() {
            self.status = Self::label(
                "配置未保存：请检查模型、地址及配置数量（最多 16 个）。",
                "Settings were not saved. Check the model, address and profile limit (16).",
            )
            .into();
            cx.notify();
            return;
        }
        disabled.active_profile_id = Some(profile.id.clone());
        disabled.enabled = false;
        if !self.persist(
            &disabled,
            Self::label(
                "无法保存配置；未修改密钥。",
                "Could not save settings; the key was not changed.",
            ),
            cx,
        ) {
            return;
        }
        self.config = disabled.clone();
        self.profile = disabled.active_profile().cloned().unwrap_or(profile);
        self.changed();
        self.epoch = self.epoch.wrapping_add(1);
        let epoch = self.epoch;
        let saved_revision = runtime_revision();
        // Explicitly disabling with no replacement key does not require a
        // Keychain read (or an operating-system authorization prompt).
        if secret.is_empty() && !desired_enabled {
            self.status = Self::label("已保存，AI 已关闭。", "Saved. AI is off.").into();
            cx.notify();
            return;
        }
        let replacing = !secret.is_empty();
        let operation = if replacing {
            let write = credentials::write(&service, secret.into_bytes(), cx);
            cx.spawn(async move |_, _| write.await)
        } else {
            let read = credentials::read(&service, cx);
            cx.spawn(async move |_, _| match read.await {
                Ok(Some(secret)) if !secret.is_empty() => Ok(()),
                Ok(_) => Err(CredentialError::Unavailable),
                Err(error) => Err(error),
            })
        };
        self.set_busy(true, cx);
        self.status = Self::label("正在处理凭据…", "Updating credentials…").into();
        cx.spawn(async move |this, cx| {
            let result = operation.await;
            let _ = this.update(cx, |this, cx| {
                this.set_busy(false, cx);
                if this.epoch != epoch {
                    return;
                }
                if let Err(error) = result {
                    this.status = Self::credential_error(error).into();
                    return;
                }
                if runtime_revision() != saved_revision || AiConfig::load().ok().as_ref() != Some(&disabled) {
                    this.status = Self::label(
                        "配置已变化；密钥操作已完成，未启用 AI。请重新打开设置。",
                        "Settings changed. The credential operation finished; AI was not enabled. Reopen settings.",
                    )
                    .into();
                    this.load_failed = true;
                    return;
                }
                let mut final_config = disabled;
                final_config.enabled = desired_enabled;
                if !this.persist(
                    &final_config,
                    Self::label(
                        "凭据操作已完成，但配置保存失败。",
                        "The credential operation finished, but saving settings failed.",
                    ),
                    cx,
                ) {
                    return;
                }
                this.config = final_config;
                this.enabled = desired_enabled;
                this.changed();
                this.status = if desired_enabled {
                    Self::label("已保存并开启自动推荐。", "Saved. Automatic recommendations are on.")
                } else {
                    Self::label("密钥已保存，AI 保持关闭。", "Key saved. AI remains off.")
                }
                .into();
            });
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
        // Disable before deleting. Failure leaves an honest, manageable profile
        // instead of an enabled configuration or an orphaned secret.
        let mut disabled = self.config.clone();
        disabled.enabled = false;
        if !self.persist(
            &disabled,
            Self::label(
                "无法保存关闭状态；未删除密钥。",
                "Could not persist the disabled state; the key was not deleted.",
            ),
            cx,
        ) {
            return;
        }
        self.config = disabled.clone();
        self.enabled = false;
        self.clear_draft(cx);
        self.changed();
        let epoch = self.epoch;
        let saved_revision = runtime_revision();
        let operation = credentials::delete(&service, cx);
        self.set_busy(true, cx);
        self.status = Self::label("正在删除凭据…", "Deleting credentials…").into();
        cx.spawn(async move |this, cx| {
            let result = operation.await;
            let _ = this.update(cx, |this, cx| {
                this.set_busy(false, cx);
                if this.epoch != epoch {
                    return;
                }
                if let Err(error) = result {
                    this.status = Self::credential_error(error).into();
                    return;
                }
                if runtime_revision() != saved_revision || AiConfig::load().ok().as_ref() != Some(&disabled) {
                    this.status = Self::label(
                        "密钥已删除，但配置已变化；请重新打开设置。",
                        "The key was deleted, but settings changed. Reopen settings.",
                    )
                    .into();
                    this.load_failed = true;
                    return;
                }
                let mut updated = disabled;
                updated.remove_profile(&profile.id);
                if !this.persist(
                    &updated,
                    Self::label(
                        "密钥已删除；配置删除失败。",
                        "Key deleted. Removing the profile failed.",
                    ),
                    cx,
                ) {
                    return;
                }
                this.config = updated;
                if this.profile.id == profile.id {
                    let next = this
                        .config
                        .active_profile()
                        .cloned()
                        .or_else(|| this.config.profiles.first().cloned())
                        .unwrap_or_else(Profile::typesafe);
                    this.select(next, cx);
                }
                this.changed();
                this.status =
                    Self::label("已删除密钥及配置，AI 已关闭。", "Key and profile deleted. AI is off.").into();
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
        let focus = self
            .buttons
            .entry(id.clone())
            .or_insert_with(|| cx.focus_handle())
            .clone();
        let action = Rc::new(action);
        let click_action = action.clone();
        let click_entity = cx.entity();
        let key_entity = cx.entity();
        let click_focus = focus.clone();
        div()
            .id(gpui::SharedString::from(id))
            .track_focus(&focus)
            .tab_stop(true)
            .px(px(10.))
            .py(px(6.))
            .rounded_md()
            .border_1()
            .border_color(rgb(chrome.separator))
            .text_size(px(12.))
            .text_color(rgb(chrome.text))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(chrome.selection)))
            .focus(|style| style.border_color(rgb(chrome.accent)))
            .child(label)
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                click_focus.focus(window);
                click_entity.update(cx, |this, cx| click_action(this, cx));
                cx.stop_propagation();
            })
            .on_key_down(move |event, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    key_entity.update(cx, |this, cx| action(this, cx));
                    cx.stop_propagation();
                }
            })
            .into_any_element()
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
        let confirmed = draft.data_policy_version == DATA_POLICY_VERSION;
        let destination = normalize_base_url(&draft.base_url).map_or_else(
            |_| Self::label("尚无有效 HTTPS 地址", "No valid HTTPS destination").into(),
            |(_, endpoint)| endpoint.to_string(),
        );
        let mut providers = div().flex().flex_wrap().gap(px(6.));
        for (id, label, provider) in [
            ("typesafe", "TypeSafe", Provider::TypeSafe),
            ("openrouter", "OpenRouter", Provider::OpenRouter),
            ("custom", "Custom System One", Provider::CustomSystemOne),
        ] {
            let label = if self.profile.provider == provider {
                format!("● {label}")
            } else {
                label.into()
            };
            providers = providers.child(self.button(format!("jev-{id}"), label, cx, move |this, cx| {
                this.provider(provider, cx);
            }));
        }
        let enable_label = if self.enabled {
            Self::label("自动推荐：开启", "Automatic recommendations: on")
        } else {
            Self::label("自动推荐：关闭", "Automatic recommendations: off")
        };
        let enable_label = if self.enabled != self.config.enabled {
            format!("{enable_label}{}", Self::label("（待保存）", " (save to apply)"))
        } else {
            enable_label.into()
        };
        let enable = self.button("jev-enable".into(), enable_label, cx, |this, cx| {
            if this.enabled {
                this.enabled = false;
                this.clear_draft(cx);
                this.suspend_for_edit(cx);
            } else if !this.busy {
                this.enabled = true;
            }
            cx.notify();
        });
        let consent_label = if confirmed {
            Self::label("☑ 已确认上述数据范围与处理方", "☑ Data scope and processors confirmed")
        } else {
            Self::label("☐ 确认上述数据范围与处理方", "☐ Confirm data scope and processors")
        };
        let consent = self.button("jev-consent".into(), consent_label.into(), cx, |this, cx| {
            if this.busy {
                return;
            }
            this.suspend_for_edit(cx);
            let service = this.draft(cx).credential_key().ok();
            this.acknowledged_service = if service == this.acknowledged_service {
                None
            } else {
                service
            };
            cx.notify();
        });
        let advanced = self.button(
            "jev-advanced".into(),
            Self::label("高级：Base URL", "Advanced: Base URL").into(),
            cx,
            |this, cx| {
                this.advanced = !this.advanced;
                cx.notify();
            },
        );
        let save = self.button(
            "jev-save".into(),
            Self::label("保存配置 / 替换密钥", "Save settings / replace key").into(),
            cx,
            |this, cx| this.save(cx),
        );
        let mut policies = div().flex().flex_wrap().gap(px(6.));
        if self.profile.provider != Provider::CustomSystemOne {
            policies = policies
                .child(self.button(
                    "jev-typesafe-privacy".into(),
                    Self::label("TypeSafe 隐私政策", "TypeSafe Privacy Policy").into(),
                    cx,
                    |_, cx| cx.open_url("https://typesafe.ai/legal/privacy-policy"),
                ))
                .child(self.button(
                    "jev-typesafe-terms".into(),
                    Self::label("TypeSafe 服务条款", "TypeSafe Terms").into(),
                    cx,
                    |_, cx| cx.open_url("https://typesafe.ai/legal/mca"),
                ));
        }
        if self.profile.provider == Provider::OpenRouter {
            policies = policies.child(self.button(
                "jev-openrouter-privacy".into(),
                Self::label("OpenRouter 数据政策", "OpenRouter Data Policy").into(),
                cx,
                |_, cx| cx.open_url("https://openrouter.ai/docs/guides/privacy/data-collection"),
            ));
        }
        let mut saved = div().flex().flex_col().gap(px(8.));
        for profile in self.config.profiles.clone() {
            let select_profile = profile.clone();
            let provider = match profile.provider {
                Provider::TypeSafe => "TypeSafe",
                Provider::OpenRouter => "OpenRouter",
                Provider::CustomSystemOne => "Custom System One",
            };
            let label = format!("{provider} · {}", profile.base_url);
            let select = self.button(format!("jev-select-{}", profile.id), label, cx, move |this, cx| {
                this.select(select_profile.clone(), cx);
            });
            let delete = self.button(
                format!("jev-delete-{}", profile.id),
                Self::label("删除密钥及配置", "Delete key and profile").into(),
                cx,
                move |this, cx| this.delete_profile(profile.clone(), cx),
            );
            saved = saved.child(div().flex().flex_wrap().gap(px(6.)).child(select).child(delete));
        }
        let processors = match self.profile.provider {
            Provider::TypeSafe => Self::label(
                "处理方：TypeSafe（美国处理）；不承诺零保留。",
                "Processor: TypeSafe (US processing); no zero-retention promise.",
            ),
            Provider::OpenRouter => Self::label(
                "处理方：OpenRouter 及 TypeSafe；数据经过两方，不承诺零保留。",
                "Processors: OpenRouter and TypeSafe; data passes through both, with no zero-retention promise.",
            ),
            Provider::CustomSystemOne => Self::label(
                "处理方：下方自定义地址的运营方及其上游；请自行确认其数据政策。",
                "Processors: the custom destination below and its upstream providers. Review their data policies.",
            ),
        };
        let body = div().p(px(16.)).flex().flex_col().gap(px(12.)).text_size(px(13.))
            .child(Self::label("开启并保存后，输入停止约 250ms 且本地候选就绪时自动请求 Jev；继续输入会取消旧推荐，忙碌或冷却时跳过。推荐候选以 AI 图标置于列表首位，沿用现有按键或点击采纳；已开始选择时不再调整顺序。", "Once enabled and saved, Jev runs automatically after about 250ms without typing when local candidates are ready. Further typing cancels the previous recommendation; requests are skipped while busy or cooling down. Its choice appears first with an AI icon and uses the usual keys or click to accept. The order stays fixed once you start selecting."))
            .child(if self.persistence_failed {
                Self::label("本次运行已停止 AI；磁盘状态未确认，请重试保存。", "AI is stopped for this run. The disk state is unconfirmed; retry saving.")
            } else if self.config.enabled {
                Self::label("已保存状态：开启。编辑立即暂停旧配置；关闭立即生效。重新启用须明确保存。", "Saved state: on. Editing immediately pauses the old profile; switching off is immediate. Save explicitly to enable again.")
            } else { Self::label("已保存状态：关闭。明确保存后才能重新启用。", "Saved state: off. Save explicitly to enable again.") })
            .when(self.load_failed, |body| body.child(Self::label("配置不可用，保存已锁定；请重新打开设置。", "Settings are unavailable and saving is locked. Reopen settings.")))
            .child(enable).child(providers)
            .child(Self::label("模型", "Model")).child(self.model.clone())
            .child(advanced)
            .when(self.advanced, |body| body.child(Self::label("填写基地址，不含 /v1/systemone；预设地址固定，修改地址请选择 Custom。", "Enter the base address without /v1/systemone. Preset addresses are fixed; choose Custom to change the destination.")).child(self.base.clone()))
            .child(format!("{} {destination}", Self::label("实际请求地址：", "Request destination:")))
            .child(processors)
            .child(policies)
            .child(Self::label("外发范围：公共命令/子命令路径、已知 token 前缀、shell 类型，以及公共候选 ID、名称和说明。不发送完整输入、当前目录、环境变量、别名、历史、文件名、动态资源或实际插入文本。", "Sent: public command/subcommand paths, known token prefixes, shell type, and public candidate IDs, names and descriptions. Full input, working directory, environment, aliases, history, filenames, dynamic resources and actual insertion text are excluded."))
            .child(consent)
            .child(Self::label("API Key（遮蔽输入）", "API Key (masked input)"))
            .child(self.key.clone())
            .child(div().text_size(px(12.)).text_color(rgb(chrome.muted)).child(Self::label("不回填已存密钥；留空保存时沿用当前地址的密钥。仅存入系统 Keychain，不写入普通设置。复制和剪切已禁用。", "Saved keys are never prefilled. Leave blank to use the key for this destination. Stored only in system Keychain, not ordinary settings. Copy and cut are disabled.")))
            .child(save)
            .child(div().text_color(rgb(chrome.muted)).child(self.status.clone()))
            .child(Self::label("已保存的配置与密钥管理", "Saved profiles and key management"))
            .child(saved);
        super::card(
            Self::label("AI 候选推荐 · Jev", "AI recommendations · Jev"),
            chrome,
            body,
        )
    }
}
