# Fastab 接入 Jev：可行性与实施计划

日期：2026-09-22。研究对象：本轮内存优化完成后的 Fastab 工作区。用户已明确选择 **Fastab 补全功能**，不是编码 Agent 的开发工具。

## 结论与推荐范围

**已接入默认关闭的“AI 候选推荐”实验代码，Jev 功能尚未进行编译或运行验收。** 按用户后续确认，设置启用后，在有效本地最终候选到达并稳定 250 ms 时自动请求 Jev；继续输入会取消旧请求。Jev 在有限、可追溯的候选集合中选择，将所选已有候选移到正常提示列表第一位，保留其余候选的相对顺序，用户通过原有按键或点击插入。网络请求不进入本地补全关键路径。

设置页纳入 **API Key 填写/替换/删除、TypeSafe 与 OpenRouter 服务商选择、预置 Base URL 和模型，以及高级自定义 System One 地址**。OpenRouter 已有官方 Jev 兼容接口，不需要改用聊天模型；选择预设时无需手工填写地址。自定义地址必须实现相同协议，不能承诺任意 OpenAI-compatible 服务都可用。

技术可行不等于收益已证明：对于 `git ch` 这类已有精确匹配、历史接受记录的输入，本地排序可能已经足够好。若不发送私人上下文，Jev 可获得的信息更少；推荐收益是否值得增加网络成本，需要以后用明确授权的样例评估。本次未做真实 API 调用、性能或质量验证，不建议直接默认开启。

本轮已写入 Jev 产品代码，并用 GPT-6 Astra 分阶段交叉静态 Review。未对 Jev 功能运行测试、构建、lint、格式检查、采样或 CI；没有安装 SDK/插件、操作 Keychain、读取密钥或上传终端数据。这里的验证边界仅指 Jev，不包含另行获授权并已执行的内存优化测试。此前工作区改动保留；静态 Review 不构成运行、视觉或接口验收。

## 1. 推文与官方能力的适配

已读取用户给出的[推文](https://x.com/3three_AI/status/2102252548921114776)。它是十个 Jev 项目的汇总，提供应用方向，不是 Fastab 的接入规范；星数和演示速度不作为本项目收益证据。

| 方向 | 与 Fastab 的关系 | 本计划取舍 |
| --- | --- | --- |
| 对有限选项做选择/评分 | 可以映射到现有补全候选 | 首版用 Choice 选出一个已有候选移首；保留其余相对顺序 |
| 自由生成新命令、参数值或解释 | Jev 不提供自由文本生成 | 不承诺自然语言生成整条命令，不替换 spec/generator |
| 浏览器、桌面自动操作 | 推文中的应用把模型决策映射成操作 | 不引入自动点击、自动执行 shell 或新系统权限 |
| Agent 上下文压缩、代码审查 | 属于开发工作流 | 不纳入产品依赖，也不视为 Fastab 内存优化 |

[fast-jev-compaction](https://github.com/tamaratran/fast-jev-compaction#readme) 面向 Claude Code 会话压缩，[jev-review](https://github.com/devagrawal09/jev-review#readme) 面向代码审查，[TypeSafe skills](https://github.com/typesafe-ai/skills#readme) 是开发指导。产品可直接调用 HTTP，无需把这些项目或 Node/Python sidecar 装入 `.app`。

官方公开契约与本计划选择：

- `POST https://api.typesafe.ai/v1/systemone`，Bearer 鉴权，JSON 包含 `state`、`model`、`questions`，响应有 `answers` 和 `usage`。desktop 已声明 workspace 中既有的 reqwest 与 sha2，不引入 SDK。[HTTP API](https://docs.typesafe.ai/api)
- Choice 返回选项、各选项概率及 confidence，最多 255 个选项；首版远小于此限制，并增加 `keep_local` 作为“不改变”的选项。问题 ID 不参与模型推理，候选含义必须明确写在 instructions/criteria 中。[Choice](https://docs.typesafe.ai/primitives/choice)
- Score 是 2–10 级量表上的分数，可为小数，不天然是 0–1；Noul 是 yes 的概率，没有独立 confidence。首版不同时叠加多套评分规则，避免未经校准的权重和阈值。[Score](https://docs.typesafe.ai/primitives/score)、[Noul](https://docs.typesafe.ai/primitives/noul)
- confidence 描述概率分布的集中程度，不是准确率保证；类型受约束也不代表判断不会错。任何结果都不能扩大候选集合或授权命令执行。[Confidence](https://docs.typesafe.ai/confidence)
- 当前文档列出 `jev-1.13.0`，应固定经评估的版本；不依赖会移动的 `jev-latest`/`jev-preview`。公开 token/速率上限可能调整，不能代替客户端自己的字节、并发和请求预算。[Models](https://docs.typesafe.ai/models)
- OpenRouter 官方提供 System One 兼容入口，使用 OpenRouter Key；本计划增加第二个服务商预设，沿用受限候选协议，按服务商处理模型 ID、响应扩展及错误。具体地址和版本见第 6 节。[OpenRouter TypeSafe 接入说明](https://openrouter.ai/docs/guides/community/typesafe-sdk)

## 2. 当前源码支持与接入位置

| 现有入口 | 已核对事实 | 设计影响 |
| --- | --- | --- |
| [runtime.rs](../crates/fastab_engine/src/runtime.rs)、[rank.rs](../crates/fastab_engine/src/rank.rs) | 本地合并历史，执行匹配、优先级及接受记录排序；Suggestion 有插入元数据和危险标志 | 保留原算法；不能让模型改 `insert_value`、转义、空格、删除范围或执行标志 |
| [worker.rs](../crates/fastab_engine/src/worker.rs) | worker 执行同步补全并合并等待中的请求 | 不在 worker 或 rank 中等待 HTTP |
| [desktop/overlay.rs](../crates/fastab_desktop/src/overlay.rs) 的 `apply_completion` | 已检查 generation/session；最终结果随后转为 UI 行 | 在有效最终结果处建立候选快照；`pending_generators` 的中间结果首版跳过 |
| [GPUI overlay.rs](../crates/fastab_gpui/src/overlay.rs) 的 `set_suggestions_with_match_term` | 只有用户已改变选中项时才按身份保持选择，否则刷新会选第 0 项 | 未导航时才允许 AI 移首；导航后固定当前显示顺序，拒绝晚到推荐 |
| [list.rs](../crates/fastab_gpui/src/list.rs)、desktop `insert_item` | 显示身份与真实插入内容并不相同；现有路径负责插入和接受记录 | 使用本地不可变候选 ID 映射，不按名称查找或让模型返回插入字符串 |
| [settings_ui.rs](../crates/fastab_desktop/src/settings_ui.rs)、[gpui_host.rs](../crates/fastab_desktop/src/gpui_host.rs) | 有原生设置页、事件刷新路径；`ReloadCredentials` 名称不代表已有 Jev 密钥系统 | 增加专用 AI 配置及失效事件，不用设置刷新伪造接受或执行 |
| [settings/lib.rs](../crates/fastab_settings/src/lib.rs) | 设置写普通 JSON | 只存开关、模式、版本等非秘密配置，不存 API key |

```mermaid
flowchart LR
  E[本地补全引擎] --> L[原有补全列表立即显示]
  E --> S[合格候选快照]
  S --> U[已启用且输入稳定 250 ms]
  U --> J[desktop 后台 HTTPS]
  J --> V[响应与快照校验]
  V --> P[所选已有候选移首并标记 AI]
  P --> A[用户按原有按键或点击插入]
  A --> I[原有插入路径]
```

失败、取消、过期或没有合格候选时，仅结束 AI 流程，原列表继续使用。关闭 AI 时不得创建请求、候选缓存或常驻额外 worker。

## 3. 首版行为契约

1. 设置默认关闭；选择服务商、配置该服务商自己的 API Key 并确认数据范围、保存启用后，输入过程中自动推荐。只有已显示的最终本地结果具有至少 2 个合格候选，且 250 ms 内输入和快照保持有效才发起请求。不占用已有 Tab/Enter/Escape 快捷键，也不抢终端焦点。保存时不进行连接测试；保存启用后当前仍有效的输入也可进入正常推荐流程。
2. 候选仍由本地引擎产生。校验后的 Choice 仅将一个已有候选移到首位并默认选中，其他候选相对顺序保持不变；`keep_local` 胜出时提示“保留当前排序”，不改变列表。用户已导航时不再请求或应用晚到推荐。
3. 采纳沿用普通按键或候选点击，操作前重新核对设置及当前上下文，再调用现有插入逻辑；共同前缀 Tab 和用户显式的 `insertSelectedAndExecute` 语义保留。AI 本身不触发插入、回车或自动执行。拒绝含换行、控制字符、`is_dangerous`、`auto-execute`、`special` 或无法确认插入语义的 AI 候选；危险标志在转成 UI 行之前检查。
4. 用户导航或数字选择时取消在途推理，保留当前显示顺序及所选行；继续输入、改变输入光标、接受补全、隐藏、切换会话/终端、cwd/环境、候选修订或设置失效时，恢复本地排序并清除旧 AI 标记。接受时先取得当前候选，再恢复排序，避免插入旧首项；已触发的插入不撤销或重放。
5. 本地 alphabetical/精确匹配排序算法和历史接受记录不变；AI 只在显示层临时移首。共同前缀 Tab、onlyShowOnTab、粘贴/历史唤起隐藏规则、原 `···` loading owner 和脚本超时保持不变。
6. AI 行保留原候选说明，不生成解释；不能仅因为 AI 曾推荐就写入接受记录，仍以现有插入成功路径为准。
7. Jev 首行使用参考图对应的星芒 SVG，与 history 图标共用 GPUI 图片机制；背景使用主题 accent，按背景亮度选择固定黑白两种图像缓存，不按任意主题色或尺寸新增缓存项。实际视觉效果尚未运行确认。

## 4. 候选身份、来源与外发数据

`Suggestion.kind` 不能可靠地区分公共 bundled spec、历史、文件、动态 generator 或自定义 spec；字段名看起来像 command/option，不代表内容可外发。实现新增 `public_ai.rs`，从实际 Registry 根对象和静态候选创建点传递来源；新增字段均使用 `serde(skip)`，默认不请求也不序列化来源信息。

### 4.1 首版允许范围

- 只允许来自已确认公共 bundled 规格的静态命令、子命令和选项，保持本地筛选后的候选集合。来源信息必须从 registry/规格加载、静态候选生成一路传播，不能在 UI 根据 kind 或文件路径猜测。
- `EC_SPECS_DIR` 覆盖、自定义/版本化来源、generateSpec/loadSpec 动态结果、hooks、history、路径/文件名、分支名/容器名等动态资源，以及来源混合或未知的候选，默认不入 AI 快照。合并去重时采用保守来源策略，不能因和静态候选同名就被标成公共。
- “来自 bundled”本身也不足以证明公开：构建来源需对应已知公开包及固定版本/内容标识。不能证明时标 Unknown 并跳过；不得为了扩大覆盖率默默放宽。
- `is_dangerous`/隐藏项/执行项等硬过滤先于 AI；模型不能把被过滤项加回来。首版候选不足 2 项则不请求。

当前来源锚固定于公共 `@chen86860/autocomplete-specs@3.1.0` 对应内容。实现先沿用已有全量 snapshot 校验，再要求 source tree、manifest、IR tree 三个摘要匹配；对应源 manifest 已与公开仓库固定提交 `bd2887ca38ce646e5db4db55003897a356511db4` 核对。IR 是本地构建产物，摘要是需人工维护的准入锚，不是任意本地 manifest 的自证。升级包或重新生成不同 IR 后默认拒绝，需重新审查更新。

| 锚 | SHA-256 |
| --- | --- |
| source tree | `69d4bdd9fa05ee698c4e72ea677dabb6db0623fccc89e844509cf55ca69501a7` |
| manifest | `efea68eddad4549ad1fd96a06ced085556efda70ff042ff63dcceca5c364d65f` |
| IR tree | `fec0a6a2ed769a1c47d8f2c63d8e60f08d5003b01822676fef0137bdc3df4ebb` |

**覆盖边界：** 只接受光标在末尾、最长 256 字节的简单 ASCII 命令路径，最多 8 段；已消费的每段必须是公共静态命令/子命令，当前前缀须匹配公共候选。首 token 的 PATH 搜索、自由参数、alias 展开、动态 loadSpec/generateSpec 路径都跳过。当前 `git`、`cargo`、`rustup` 根规格存在 generateSpec，因此不能把 `git ch` 当作已支持的演示；部分 `docker`、`go`、`brew`、`kubectl` 纯静态路径可进入准入判断，但其实际请求和效果未验证。

### 4.2 专用外发 DTO

只构造一个独立 DTO，最多包括：规范化 shell 类型、来自公共规格的命令/子命令路径、可匹配到公共候选的当前 token 前缀、候选临时 ID、公共名称和截短的公共说明。每个字段都经过来源白名单；无法解析或含未知自由参数的输入首版跳过，不能用正则“已脱敏”代替来源证明。

不得序列化 `CompleteRequest`、`CompleteResult` 或 `ClickInsert` 整体。以下均不外发：原始完整 buffer、cwd/主机/会话标识、环境变量、aliases、终端历史、接受历史、文件内容、动态名称、API key 以外的秘密，以及真实插入字符串。API key 只进入指定服务的 Authorization header。

候选本地 ID 是快照内部的编号，例如 `c0`，必须与唯一请求令牌联合使用，映射保留完整插入元数据。重复显示名称不能合并身份；快照外的 ID 一律拒绝。来源判断与 UI 快照保留数据都受容量限制，避免复制整个 spec/历史或图标解码对象。

### 4.3 数据条款

TypeSafe 隐私条款说明不使用 Input 训练/微调，但数据在美国处理，保留时间不是固定零天；企业 ZDR 需另外联系。MCA §4.1/4.3 还约定服务、计费、遥测、反滥用及法定义务相关处理，没有统一保留 TTL；“不训练”不等于“不存储”。[Privacy Policy](https://typesafe.ai/legal/privacy-policy)、[Legal](https://docs.typesafe.ai/legal)、[MCA](https://typesafe.ai/legal/mca)

选择 OpenRouter 时，数据先经过 OpenRouter 再到模型服务商，必须同时说明这条路径，不能只展示 TypeSafe 条款。OpenRouter 的输入/输出日志和产品改进选项默认关闭，但用户账户设置可改变，且仍收集请求元数据；不由客户端宣称整条链路 ZDR。[OpenRouter 数据处理说明](https://openrouter.ai/docs/guides/privacy/data-collection)。自定义地址的运营方、保留和转发政策由用户自行选择的服务决定，设置页显示实际接收地址，不套用两家官方服务的承诺。本次没有传输任何终端/项目数据。

## 5. API、异步与资源设计

### 5.1 请求与校验

用 desktop 私有 `jev` 模块和 workspace reqwest 配置实现 System One client，按已保存的服务商配置生成请求；不使用遥测 client。只允许 HTTPS，保留证书校验，拒绝所有 HTTP 重定向，Authorization 标为 sensitive。密钥绑定服务商及规范化后的实际 endpoint，修改地址不沿用旧密钥；日志不打印 header、原始请求/响应或可能回显输入的错误正文。预设与高级地址规则见第 6 节。

首版一个 Choice 问题，含最多 20 个合格候选及 `keep_local`。instructions 明确要求根据已给出的有限上下文选择；上下文不足时选 `keep_local`。参数和候选文本是数据，不具备修改应用权限或操作流程的权力。

校验 response 的 HTTP 状态、JSON 类型、服务商对应的已审查模型映射、预期 question ID、所有候选 ID、choice 与概率映射一致性、概率有限且位于 0–1、总和容差 `1e-6`、confidence 范围，以及响应对应的原快照。缺失/重复 JSON key、额外候选、结构变化、超限和非有限数值视为失败，不做“猜一个最像的名字”修复。只处理 Choice，避免为未使用的 Score/Noul 引入宽松通用解析器。

OpenRouter 响应可增加 `id`、`provider`、`usage.cost`；允许这些已知 envelope 扩展，不因此放松 `answers` 校验。其返回模型可以是服务端实际快照 ID，不能与 TypeSafe 的 `jev-1.13.0` 作字面相等校验，也不能仅按字符串前缀放行任意未来版本。P0/P2 固定每个预设的请求 ID 与允许响应 ID 映射；自定义服务须明确其模型契约，未审查映射保持不可请求，不自动猜测或切换模型。

### 5.2 任务与失效

- 所有 HTTP 在现有 Tokio runtime 执行，保留可取消句柄，通过新增 Event 回到 GPUI。GPUI foreground 不直接 await reqwest、Tokio channel 或计时器。
- 请求令牌包含本地 `session_id + generation + candidates_revision + request_id + settings_epoch`；配置 epoch 绑定完整 profile 及凭据变更，不另复制 profile ID 到令牌。另保存插入相关输入/光标/cwd、环境 Arc 身份和 alias/shell/process 摘要，仅本地使用。
- 发起和返回核对令牌及快照；移首前用原索引核对完整插入元数据，普通按键/点击前同步检查设置及上下文。导航后保留上下文守卫，不能只比较名称或现有 selection_identity。
- 输入/导航/隐藏/会话/设置/密钥变化立即使令牌失效并取消任务；晚到成功结果丢弃。同配置 epoch 的晚到鉴权/余额/限流错误仍使后续请求停止或冷却，旧 epoch 不影响新配置。取消客户端请求不等于撤销服务端已产生的计费。
- 网络 loading 和错误信息显示在 AI 状态区，不能占用本地补全 loading latch，也不重复统计本地补全展示或触发生成器。

### 5.3 首版资源参数（已写入代码，未实测）

| 项目 | 首版建议 | 目的 |
| --- | --- | --- |
| 自动触发 | 开启后最终本地结果稳定 250 ms | 继续输入取消旧任务，跳过 pending、隐藏和历史模式 |
| 在途/排队 | desktop 全局最多 1 个请求，0 个排队项 | 替代时先取消并结束旧任务再启动；防多终端堆积 |
| 候选 | 最多 20 个 + `keep_local`；说明按 UTF-8 边界截短到 256 字节 | 限制快照和传输大小 |
| 请求/响应 | 完整请求最多 16 KiB；响应最多 64 KiB | 只接受 identity 编码和 JSON，关闭透明解压，增量限额 |
| 总 deadline | 2 秒，覆盖一次请求的连接、发送和读取 | 显式功能超时退出；本地列表从不等待 |
| 频率 | 最多 20 次/分钟，按整个 desktop 计算 | 与供应商动态额度分离；超限不入队 |
| 结果缓存 | 首版无跨请求/持久缓存，只保留当前有效快照、移首位置及上下文守卫 | 关闭、失效和采纳后释放并恢复本地排序 |
| 重试 | 单次请求不自动重试 | 避免按键/会话变化后继续消费旧工作 |

401 停止当前配置并清内存 Key；402 停止当前配置并提示账户额度；422 归类为请求拒绝，保留本地结果且不读取错误正文。429/529/503 进入冷却：支持 `Retry-After` 秒数/HTTP 日期及 `retry-after-ms`，无效或缺失默认 30 秒，限定 1 秒至 24 小时。冷却结束不定时补发，下次有效输入才能再次请求；桌面总频率窗口也不因切配置清空。TypeSafe 官方列出相应错误类型，客户端同时归一化 OpenRouter 余额错误，未知错误不冒充 Key 错误。[HTTP API](https://docs.typesafe.ai/api)

这不是内存优化：新增 HTTPS、凭据和快照可能增加 desktop 内存。约束新增驻留资源，同时不向每个 `fastabterm` 或 IME 加入模型/HTTP 依赖，不抵消已完成的 PTY 有界队列改动。

## 6. 设置与密钥

### 6.1 用户配置入口

在原生设置的 Behavior/行为页增加“AI 候选推荐”区块，未启用时仍可配置；包括以下控件，提供中文/英文文案：

| 控件 | 行为 |
| --- | --- |
| 启用开关 | 默认关闭；配置有效、凭据可用且数据范围已确认才可启用；关闭即时取消请求、恢复本地排序并清 AI 标记，无需重开终端 |
| 服务商 | TypeSafe、OpenRouter、自定义 System One；预设切换自动展示对应地址和模型，不把上一家的 Key 复制过去 |
| API Key | 遮蔽输入，支持粘贴、保存/替换、删除；显示“未配置/保存中/已配置/当前不可用”，不回显已存密钥；保存成功不等于鉴权验证通过 |
| Base URL | 官方预设自动填入；高级设置允许自定义，修改预设地址后转为独立自定义配置；显示最终请求 URL，避免路径拼错 |
| 模型 | 按服务商预设固定版本；自定义配置填写模型 ID，但启用受已审查协议/响应映射约束，不提供任意聊天模型选择器 |
| 数据说明与配置状态 | 显示实际接收方、外发范围及条款链接；不显示原始 buffer、密钥或服务端回显；保存/删除错误留在当前配置区 |

只填 Key 的正常流程：选择 TypeSafe 或 OpenRouter → 填该平台 Key → 阅读并确认数据范围 → 打开启用开关 → 保存。也可先保持关闭只保存密钥。地址编辑放在高级区，普通 OpenRouter 用户不必手填。首版不增加自动连接检测；若以后提供“检查连接”，必须由用户主动点击、只发固定公开样例并说明可能计费，不读取当前终端。

原 `settings_ui.rs` 以按钮、开关和选择菜单为主，没有可直接复用的文本/密码输入组件。P3 已基于 GPUI `EntityInputHandler` 增加单行输入，处理光标、选区、粘贴、UTF-16 范围和设置页 Tab 焦点遍历；密码模式限制复制/剪切及系统文本查询，不只是绘制圆点。不声称具备尚未确认的系统 Secure Input。API Key 不预填已存值；保存、切换配置、离开页面、关闭窗口时清空密钥草稿。第一次编辑启用中的配置会保存关闭状态，之后逐字符只改组件草稿，不逐字符写 JSON 或操作 Keychain。

### 6.2 服务商地址与模型

| 服务商 | Base URL（不含 `/v1/systemone`） | 最终 POST URL | 请求模型 / 凭据 |
| --- | --- | --- | --- |
| TypeSafe | `https://api.typesafe.ai` | `https://api.typesafe.ai/v1/systemone` | `jev-1.13.0` / TypeSafe Key |
| OpenRouter | `https://openrouter.ai/api` | `https://openrouter.ai/api/v1/systemone` | `typesafe/jev-1.13` / OpenRouter Key |
| 自定义 System One | 用户填写 HTTPS 基址，可有路径前缀 | 规范化基址后追加一次 `/v1/systemone` | 用户服务约定的 Jev ID / 此地址专属 Key；兼容性待确认 |

OpenRouter 官方文档确认请求/响应沿用 TypeSafe 格式，但 model 返回实际服务快照，其示例为 `typesafe/jev-1.13-20260917`；是否可把该日期 ID 直接作为请求模型需另行确认，不把响应示例当作可调用 ID 证明。首版不使用 latest/preview，也不使用社区示例的 alpha 路由。这里的 Base URL 不是聊天接口常见的 `/api/v1`，不能拼接 `/chat/completions`。无需调用模型列表：TypeSafe SDK 的列表解析与 OpenRouter 列表格式不兼容。[官方兼容说明](https://openrouter.ai/docs/guides/community/typesafe-sdk)

URL 规则：解析后只接受 HTTPS，拒绝 userinfo、query、fragment、无效主机及不受支持的端口/路径形式；不允许 Key 放 URL。保留合法路径前缀，规范化尾部斜杠及默认端口，再追加固定 endpoint；若用户填入完整 `/v1/systemone` 或常见误填 `/api/v1`，提示正确基址，不静默重复拼接或猜路由。规范化结果统一用于显示、Keychain 绑定和实际发送。自定义服务必须支持 System One，HTTPS 地址正确不代表协议兼容。

### 6.3 配置状态与凭据隔离

实际非秘密设置存入单个 `autocomplete.ai.config`：默认 `enabled=false`、当前 `active_profile_id`、最多 16 个 profile，各自保存 `id/provider/base_url/model/data_policy_version`。首版只有 System One 自动推荐，不引入无实际分支的 mode/protocol 配置。每个配置一个 Key，不自动轮换账号。provider、规范化 endpoint、模型、数据策略或凭据变化增加 `settings_epoch`，取消在途任务并恢复本地排序；编辑草稿与已保存配置分离，开始编辑即暂停旧配置，保存才按新草稿决定启用。只改模型不强制重新填写同一 endpoint 的 Key。自定义地址仅开放已审查 Jev 模型映射，未知模型可存为关闭配置但不能请求。

优先复用已锁定 GPUI 0.2.2 的 `App::write_credentials/read_credentials/delete_credentials`：本机依赖源码已确认 macOS 实现使用 Security/Keychain，调用在 GPUI 后台 executor 执行，无需新增 Node/Python 或另一套密钥库依赖。

必须保留以下实现细节：

- 使用应用专属逻辑服务键，例如 `app.fastab.ai.jev.v1.<provider>.<endpoint_digest>`，digest 来自规范化后的完整 endpoint（包含路径/有效端口，不包含 Key）。GPUI 查询/更新/删除只按传入 server 字符串和类别，不按 username 隔离，不能直接复用公共 API 域名或通过 username 假装隔离账号。TypeSafe、OpenRouter、自定义地址的 Key 分开存取，不自动回退读取另一配置的 Key。
- 凭据操作串行化；保存成功才显示“已配置”。读取的 `Ok(None)` 可能是不存在，也可能是用户取消系统访问，统一显示“当前凭据不可用”，不反复触发系统授权。
- 旧系统 Keychain 操作尚未结束时切换配置，新读取可能因 Busy 保持停发，不自动排队或重试；需等旧操作结束后在设置中明确重存。runtime 暂无专门的凭据读取状态回传，不能声称这一路径会自动恢复或始终有可见提示。
- 关闭先取消请求并清内存凭据，但不等于删除所有服务商的 Keychain 项。删除按钮明确作用于当前配置，并等待 Keychain 删除结果；失败时明确报告，不能声称已删除。GPUI 删除“未找到”也可能报错，不能假设接口幂等。
- 持有并 await GPUI 凭据 Task。取消任务不保证中断已经开始的同步 Keychain 调用；用设置 epoch 防止晚到读取重新启用功能，串行处理保存/删除防覆盖。
- 密钥输入使用遮蔽控件，不进入普通设置 JSON、剪贴板自动拷贝、shell 环境、日志或诊断导出；普通 `Vec<u8>` 没有自动清零保证，不声称做到绝对内存擦除。
- 切换/修改地址前使旧配置失效，要求目标配置自己的凭据及数据范围确认；旧地址 Key 不复制、不发送到新地址。保留旧 Key 时仍须提供该配置的删除入口，不产生 UI 无法管理的孤立凭据。普通非秘密配置和 Keychain 不具备跨存储事务：Key 保存/配置保存任一步失败都不启用新配置，并保留可重试、可删除的准确状态。
- AI 配置保存返回错误时，feature wrapper 设置进程内禁用闩并将全局内存中的 enabled 改为 false；文件监听重载不能解除闩，下一次成功显式保存才可解除。设置 UI 同时广播失效。不能把错误后的内存停发称为磁盘关闭成功；底层通用 JSON 文件保存仍非原子事务，也没有新增磁盘读回/持久性验收。
- 每次进入实际配置写入前同步递增进程 revision；自动请求、返回和采纳同时检查 revision 与最新配置，避免 UI 的关闭通知尚在事件队列时继续使用旧状态。重复通知不重复启动同一次 Keychain 读取。

## 7. 分阶段实施与逐步 Review

每阶段流程：最小改动 → 不同实施者的 GPT-6 Astra 静态 Review → 修正 → 复审 → 更新记录。Jev 功能本轮仅做静态审查，不能把发布验收标成通过。P0–P5 为本轮代码及静态审查范围，P6 保持未执行。

| 阶段 | 文件/责任边界 | 产出和必须通过的 Review |
| --- | --- | --- |
| P0：冻结范围 | 本文档、产品交互约定 | 已按后续要求改为开启后自动推荐、所选已有候选移首、普通按键/点击采纳、AI 零自动执行；公开静态候选范围及默认关闭不变 |
| P1：来源与快照 | engine `public_ai.rs`、`runtime.rs`、`ir.rs`、`lookup.rs`、`spec_pair.rs`、`fig_spec.rs`、`lib.rs` | 已实现；Astra engine 实施、Astra image 独立静态通过；审查合并、alias、override、动态 spec、危险标志及 serde 跳过字段。现有请求构造处补默认字段，没有新增测试 |
| P2：协议客户端 | desktop `jev/{mod,client,types,policy,config}.rs`、Cargo.toml/Cargo.lock | 已实现；Astra image 实施、Astra engine 独立审查；发现配置写盘失败后全局内存仍可能开启，已加禁用闩和同步 revision，静态复审通过 |
| P3：设置入口与凭据 | desktop `jev/credentials.rs`、`settings_ui.rs`、`settings_ui/{ai,input}.rs` | 已实现；Astra terminal 实施、Astra image 独立审查；首轮发现关闭/编辑未即时失效、保存失败广播及政策链接缺失，已修复；晚到操作增加同内容保存 revision 检查，静态复审通过 |
| P4：推荐移首 | desktop `overlay/ai.rs`、`overlay.rs`、`event.rs`、`gpui_host.rs`、`remote_ipc/mod.rs`；GPUI `ai.rs`、`overlay.rs`、`list.rs`、`icons.rs`、`icons/ai.svg`、`lib.rs` | 已实现；主代理与 Astra terminal 分工实施并交叉审查，Astra engine 复审首位链；修复过期限流/鉴权政策、按键前过期排序恢复及取消顺序，静态复审通过；自动请求、首位候选及主题 SVG 图标已接线 |
| P5：集成与退出路径 | 上述完整 diff、本文档 | 已完成三路 Astra 分工静态复审：配置同步失效、重复通知、公开元数据到 HTTP 映射、采纳与取消链；修复配置加载和后台任务开始前的 revision 竞态；文档与实现一致性审查通过。未改 PTY、IME、proto 或原本地排序算法 |
| P6：后续效果与运行验收 | 另行获得验证授权后再确定环境及数据 | 先非敏感固定样例验证协议，再验证真实产品交互、目标网络和效果；当前约束下不执行，结果保持“未验证” |

P1 无法证明数据来源时，不进入真实 API 接入；P2 某服务商协议或模型映射未闭合时，该配置不开放请求；P3 无法证明密钥输入与目标地址隔离时，停止启用该配置。P4 无法证明已有候选移首保持插入身份、导航选择及当前上下文时，禁止应用 AI 排序，停止进入 P5，修正并复审后继续；不能以“模型置信度高”放行。首版只移动所选一项，不按整份概率表重排。P6 未完成前，最多作为默认关闭的实验实现，不宣布生产收益或改成默认启用。

## 8. 后续验收清单与退出条件

以下是将来验证的内容，不是本轮已执行结果，不新增凑数的测试文件。

- **契约与数据：** ID/概率缺失和越界、重复 key、模型变化、超大 body、UTF-8 截断、401/422/429/529、timeout/cancel；来源 Unknown、动态名称、环境/alias/历史/private cwd 不得进入外发 DTO。
- **行为：** 本地排序算法及共同前缀 Tab/onlyShowOnTab 保持不变；合格响应只移首一项，其余相对顺序保留；导航后固定显示顺序并拒绝晚到响应；输入、接受、隐藏、切窗口/会话、更新候选或关闭 AI 后恢复本地排序；按键和点击不得因取消顺序选错行，危险和自动执行项不得被 AI 推荐。
- **配置与凭据：** API Key 输入/粘贴/密码遮蔽/焦点/草稿释放；TypeSafe/OpenRouter 切换、基址拼接、自定义地址、模型映射与 envelope 扩展；保存/删除/取消访问/Keychain 错误及连续操作；地址变化不携带旧 Key，配置与凭据部分失败不启用；晚到读取不能重新开启功能；日志和诊断没有 key 或终端内容。
- **资源：** 多终端、连续输入、防抖、慢网络和断网时仍只有 1 个在途请求且无积压；取消/失效释放快照；原本地首屏不等待网络；真实驻留内存和延迟另行量测。
- **效果：** 用用户同意的公开静态补全任务对比本地首项与 AI 推荐的准确性、无建议比例、采纳所需操作和端到端延迟。不能拿供应商通用 benchmark 代替终端补全效果。若没有可观测改善，保持关闭并停止扩大数据范围。

失败或用户撤销时，用开关恢复纯本地路径，取消任务、恢复本地排序、清 AI 标记并释放快照；回滚无需修改 shell、重启 IME 或清除本地补全历史。密钥删除须单独确认实际结果，不因代码回滚就宣称 Keychain 内容消失。

## 9. 成本、未确认事项和研究证据

TypeSafe 当前标价为输入 $0.042/百万 tokens、输出免费。仅作算例：若每次实际计费输入 2,000 tokens，则一次约 $0.000084，10,000 次约 $0.84；这是算术估算，不是本项目实测，也不包含将来价格、税费或其他服务费用。OpenRouter 模型卡当前列出同一 token 单价，但结算与附加费用以其账户和实际账单为准，自定义地址另行确定。[Models](https://docs.typesafe.ai/models)、[OpenRouter 模型卡](https://openrouter.ai/typesafe/jev-1.13/api)

厂商发布文章给出的 70–500 ms 主要来自美国西海岸附近测试，不是本机、国内网络或服务 SLA；本计划不以此保证体验。[官方发布说明](https://typesafe.ai/blog/introducing-system-one-models-and-jev)

本轮没有取得可核对的 TypeSafe OpenAPI JSON；接口事实来自官方 HTTP/primitive 文档、固定版本 SDK 源码和 OpenRouter 官方兼容文档。未确认两平台实际账户准入/额度、目标网络、真实错误体、运行时返回的模型 ID、自定义服务兼容性、问题数与字节硬上限，以及本项目的最佳 confidence 阈值。首版不自行声称“无限问题”或设置一个未经评估的置信度阈值来自动决定操作。

| 研究/Review | 状态 |
| --- | --- |
| 原推文、TypeSafe/OpenRouter 官方接口与模型、数据条款研究 | 已完成静态研究；未调用鉴权推理 API |
| 本项目补全、UI 选择、任务/设置与 GPUI Keychain 静态研究 | 已完成 |
| 方案文档的独立架构与接口 Review | 两路 GPT-6 Astra 静态复审通过；已修正 P4 门槛，并复审 API Key 入口、Base URL/OpenRouter 扩展和凭据隔离；不代表实现或运行验收 |
| Jev 产品代码 | P1–P5 已写入并完成逐阶段独立静态复审；未编译和运行 |
| SDK/插件安装、实际配置和密钥操作 | 未执行 |
| Jev 本地/远程测试、构建及运行/质量验证 | 未执行 |
