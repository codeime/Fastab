# 补全资源生命周期与响应性改进计划

日期：2026-09-30。代码基线：`1f8d6f0b`（v0.0.5）。

状态：S0–S9 的 2026-09-30 实施已关闭；2026-10-08 安装版反馈触发的追加修复见末节。本文保存各轮实际证据，末节的显示状态契约覆盖早期“隐藏不结束资源使用”的约定；未取得证据的项目不得标为已关闭。

## 目标与范围

覆盖六个方向：输入结束后的资源释放、过期补全的协作取消、加载时的临时分配、缓存体积和上下文正确性、AX 光标失败路径、同一进程内的持续回放。每项先核对当前代码，再实施有证据支持的改进。

成功标准是补全语义保持正确、最新输入不会被可取消的旧工作长期拖住、明确结束的输入能启动 10 秒释放规则（原 25 秒，选择依据见 S8b），并能用重复运行的场景观察结果、延迟和资源。不能仅凭单元测试推断安装版体验，也不能把结构体估算字节当作 `phys_footprint`。

## 研究结果与调用链

### 1. 明确的输入结束没有通知引擎

`overlay.rs::complete` 对空白缓冲、禁用、光标位于词内等情况共用提前返回；整词退格和大段输入变化也会隐藏并跳过请求。`hide` / `dismiss` 只更新 UI generation，worker 不知道注册表已不再使用。`Engine::complete` 才会更新 `Registry::idle_since`，因此清空输入本身没有开始释放宽限。

必须区分“空缓冲/命令已执行”和“暂时不显示”：Esc、焦点暂离、光标放到词内、整词退格以及用户停下来思考，都不能等同于结束输入。多个终端共享一个 Engine，本次不改为每标签常驻一份注册表。

实现方向：独立的 session-aware `EndInput` 控制消息；空缓冲和带会话身份的 pre-exec 事件进入此路径，只作用于当前输入所有者。正常完成接口继续兼容 CLI。结束只开始宽限，不立即摘树、不伪造 Complete、不生成建议。

### 2. 旧结果会被丢弃，旧计算未必停止

`overlay.rs` 的 generation 校验阻止过期结果显示，但 detached completion future 仍等待。`worker.rs` 在尝试前检查 reply 取消，监督线程随后在 `recv_timeout` 等 attempt 交回 Engine；期间新 Complete 入队，旧工作结束后才进行合并。

取消不能被普通 hook 错误吞掉：`generate.rs` 会记录 generator 已运行；hook cache 会存储返回结果；typed evaluator 的 Try 可以处理一般错误。仅把子进程输出变为空串可能污染下一次补全。

实现方向：每次尝试绑定 sticky 取消令牌；新有效请求或相同所有者的 EndInput 使旧尝试过期。在 typed 求值、子进程轮询、hook 分发和结果/缓存提交边界检查；取消结果不写入 hook 缓存，不把不完整生成器状态提交，不更新正常完成的 idle 触碰。正常交回同一 Engine，watchdog 仍负责不可协作的挂起。控制消息（接受记录、清缓存、结束、诊断）不能被最新请求合并丢掉。

#### 取消的状态提交边界

- typed Try 遇到取消必须向外传播，不进入 catch；native adapter 即使内部捕获 exec 错误返回空数组，返回边界也用 sticky 状态拒绝结果。取消和 timeout/panic 分开计数；最终状态交接明确完成与取消谁先赢，正常 Engine 交回不能误判 watchdog 故障。
- hook cache 在 run 前与写入前检查取消，取消后不写 rows/stdout/spec；此前完整写入的有效条目保留。
- 本次 GeneratorSession 及 pending 状态取消后丢弃，下次重新运行，不逐请求复制整份 session；不能让半次运行留下 ran=true/needs_run=false。
- ensure_frecency 先在局部加载，确认未取消才发布 frecency、loaded 和 source；历史索引按行检查，不插入取消产生的部分索引。S3 完成后 S5 可在 history.rs 接入统一取消接口，接入后重审该边界。
- 尝试前保存路径级 idle 状态，不额外保留强 Arc。取消不调用普通 note_idle_after_complete；恢复仍驻留路径的原活动/待释放状态，不复活已淘汰树。本次新加载且未建立活动资格的树从取消结束开始宽限。不得让取消遗留无期限的新树，也不得取消原有 deadline。回归必须覆盖旧 deadline 被临时触碰后取消、首次加载后取消且再无请求。
- 完整校验成功的 Registry/NativeHooks 换代可以保留，不回滚有效新代际，不发布半代际；同步文件读取/JSON 解析仍受原 watchdog 兜底，不承诺立即抢占。

### 3. 加载仍有可避免的临时分配

`snapshot.rs::capture_tree` / `capture_tree_path` 对每个文件 `read_to_end` 后计算完整 SHA，只有 sidecar 字节需要保留。非 sidecar 用固定 64 KiB 缓冲分块哈希，按 EOF 读完全部内容，不按 metadata 长度截断；处理短读和 Interrupted 重试，传播其他读取错误。保留目录 FD、禁止跟随链接、manifest 摘要和后续读取复核。

`ir.rs::hash_option` 通过 `serde_json::to_vec` 得到临时 JSON；文件内去重和 Registry 的选项池都需要 fingerprint。必做项是直接将相同序列化字节写给哈希器，保留碰撞后的结构相等比较。fingerprint 仅用于进程内，不要求新旧数值相等；禁止逐 chunk 调用带长度前缀的 Hash::hash，测试对照一次性 Hasher::write(serialized_bytes)，序列化失败不得新增 panic。

单遍 Arc memo 是条件项：先用真实 compute fixture 多次独立测量重复 hash 成本；收益超过本机噪声且无解析峰值或尾延迟回退才加入。memo 必须在处理完子项后记录，以 Weak 校验身份，遍历结束销毁；禁止裸地址跨遍历复用或强 Arc memo。无足够收益则保留测量记录而不加 memo。

### 4. 缓存容量、过期和上下文是不同问题

`hook_cache.rs` 三类 map 各限 512 项，达到容量后整表清空；已有 `allocated_bytes()` 只供内部估算。TTL 来自本次读取的 policy，相同 key 可以由不同读取策略访问，因此不能按插入时 TTL 擅自统一清扫。

本轮为各 map 提供 entries、payload 字节估算、hit/miss、过期移除和容量清空计数。诊断只返回数值，不返回缓存键、命令、路径、环境或历史正文；只有主动读取诊断才计算体积，不放在按键热路径。根据持续回放记录再决定是否值得改变淘汰策略，本轮不凭空设字节预算。

`history.rs::HistoryStore::index_for` 当前只按根命令命中，但 `build_index` 依赖 aliases 和 shell。runtime 仅在历史行 Arc 改变时换 store，改别名而历史未变会继续复用旧索引。将解析上下文加入失效契约：上下文变化时清理既有 32 格缓存，构建在锁外进行，写回时重新核对上下文；如已变化，仍返回本次构建给本请求，但不写入另一个上下文的缓存。aliases/shell 按原始值比较，不用未校验碰撞的 hash 代替；不新增无限的“每上下文一份缓存”。

### 5. xterm 光标查询缺少整次失败预算

`PlatformWindowImpl::get_x_term_cursor_elem` 先沿缓存路径搜索，失败后从窗口根递归；每个 AX 调用各有 250ms 超时，整次搜索没有总预算。`refresh_window_position` 的 xterm 路径查询期间还持有 focused-window mutex。Otty 路径已经有共享 250ms 预算、失败退避和身份检查，可复用这些原则。

实现方向：缓存叶验证、至多一次根搜索以及最终 frame 读取共用一个 250ms 截止时间；每次远程调用以剩余时间设置 timeout。失败在具体窗口上退避 750ms，窗口/面板焦点变化重置。查询在 focused-window mutex 外进行，回写与发事件核对 PID、window ID 和 epoch；失败明确清除旧 caret，不能继续用旧坐标拦截按键。保留 CF Copy/Create 所有权规则和真实光标要求。

补充调用链复核发现：xterm 面板焦点切换目前未递增 caret_epoch；移动/缩放被降为无身份的 RequestCaretPositionUpdate；send_terminal_caret 发送时重新采样身份。S7 同时修正这些边界：几何事件携带来源 app/window、后台事件不重置当前退避；接受当前焦点/几何变化后在一致的状态锁边界推进 epoch；查询和发送使用开始时捕获的 identity/epoch，旧成功与旧失败都不能改新状态。窗口层级读取也移出 focused-window 锁。预算覆盖 helper 的 role/focused/classes 每次属性读取，不能在日志 Debug 中隐式再查询 AX。

### 6. 现有回放无法覆盖长驻状态

`scripts/replay-sessions.sh` 对每条输入启动一次 `ftab engine complete`。它是冒烟检查，不能验证同一 Engine 的缓存、取消、空闲释放和跨请求状态。

新增独立 Rust 回放 example，单进程持有一个 EngineClient，读取 JSONL 场景。支持串行 complete、快速 burst、明确 end-input、等待和只读资源快照；请求可带不同会话身份。输出请求耗时分位数、成功/取消/失败数量和诊断快照。macOS 资源采样使用本进程的 `phys_footprint`，不操作已安装桌面或真实终端。

小型确定性 fixture 在 CI 验证控制流和建议语义；真实 bundle 场景用于观察冷/热加载和释放水位，不建立跨机器固定 MB/毫秒断言。慢脚本测试必须真的启动并取消子进程，依靠同步信号而非碰运气的短 sleep。此工具不等价于已安装 GPUI/AX 端到端验收，后者保留单独的手动场景。

## 不变量与范围边界

- 不改 `fastabterm` 一行 scrollback、两 worker、行缓存上限或 `fastab_util`/IME 的依赖边界。
- 保留 Registry 48、hook 各 512、generate 32、history index 32 的容量上限与既有排序/插入/AI provenance 语义。
- 保留宽限释放的语义，默认时长由 S8b 评估后调整；保留 pinned 规格、同路径多 Arc 释放、历史值独立持有、模板不保留运行期 bundle 树。
- 本文明确扩展 `spec-idle-release-plan.md` 中“仅后续普通补全开始宽限”的边界：来自当前所有者的明确 EndInput 也可开始宽限。隐藏、发呆和 history-only 请求仍不代替结束输入。
- 不拆 compute/typed-hooks，不改 IR 字段布局，不削弱全树 SHA，不引入启动预热或新的运行时 JS。
- 取消是协作式；文件系统和未设检查点的本地操作不保证立即抢占，原 watchdog 保留。取消不能通过频繁重建 Engine 实现。
- 不安装或重启桌面、IME、终端，不运行 install.sh，不改本轮无关未跟踪文件。本请求包含实现与验证，不包含新的提交、推送、发版。
- 原工作区存在无 manifest 的 `crates/fig_input_method`，Cargo 验证使用隔离目录，不能删除该目录或把它混入改动。

## 实施顺序与验收

依赖顺序为 S1→S2→S4→S5→S6→S8→S8b；S3 的 history 上下文修复与 S2/S4 无文件或语义依赖，独立实施并在 S5 前关闭。S7 只依赖 S0，可与 S2–S4 独立并行，必须在 S6 改动共享事件文件之前完成。每步顺序固定：实现 → 独立 review → fix → 对 fix 再 review → 定向验证 → 关闭。后续修改触及已关闭逻辑，重开该步。review 必须读实际 diff 和调用者，记录问题及修复，不能只填“通过”。

| 步骤 | 文件/责任 | 实现与验收 | 状态 |
| --- | --- | --- | --- |
| S0 | 本文 | 两位 Astra 复核研究与设计，解决接口/顺序/语义歧义后冻结计划 | 已关闭 |
| S1 | engine/snapshot.rs | 非 sidecar 分块完整 SHA；大于缓冲块的文件摘要一致；sidecar 原字节保留；generation/改写/symlink 既有回归通过 | 已关闭 |
| S2 | engine/ir.rs | 无临时 JSON Vec 的 fingerprint；真实 fixture 测量后决定单遍 Weak memo；相等/不等/碰撞保护、共享指针和建议顺序回归 | 已关闭 |
| S3 | engine/history.rs | aliases/shell 上下文失效；相同历史改 alias 后冷建与热缓存一致；上下文交错构建不能写错缓存；32 上限保留 | 已关闭 |
| S4 | engine/hook_cache.rs、diagnostics.rs、runtime::rebind_hosts | 数值统计；hit/miss/expired/cap-clear/replacement 行为与计数测试；读取方 TTL 不变；未改变淘汰策略 | 已关闭 |
| S5 | engine/worker.rs、runtime.rs、generate.rs、hook_backend.rs、process.rs、typed_hook | sticky 协作取消与只读诊断；真实慢脚本取消、typed Try 不吞取消、不缓存空值、不提交不完整 session、复用 Engine、控制消息保序 | 已关闭 |
| S6 | desktop/overlay.rs、gpui_host.rs、pre-exec 发送处；engine worker/runtime/registry | session-aware complete/end-input；空缓冲和当前 session pre-exec 启动宽限；后台 session 不影响当前输入；连续空输入不续期；旧 Complete 不能在 EndInput 后恢复活动标记 | 已关闭 |
| S6b | worker.rs、remote IPC 生命周期、desktop 通知、Registry Replace | 队列公平性、断开通知、提交与活动 owner 分离、Replace 孤树；真实回归及交叉复核 | 已关闭 |
| S7 | macos-utils/window_server、desktop/platform/macos.rs 与事件定义 | 共享查询预算、窗口级退避、锁外 AX、过期结果丢弃、失败清 caret；预算/焦点/身份/退避回归；真实 AX 体验单列 | 已关闭 |
| S8 | engine/examples、回放场景/脚本、CI | 一进程持续回放，串行/快速输入/慢脚本/清空/切session/空闲再入；正确性断言与数值报告；运行小场景及真实 bundle 场景 | 已关闭 |
| S8b | engine/ir.rs、宽限测试、契约文档与本文 | 对比 5/10/25 秒及必要补充值；按真实重载成本选择并调整默认宽限，记录理由、回访成本与剩余不确定性；独立 review/fix | 已关闭 |
| S9 | 全部变更与本文 | 两位 Astra 按跨模块边界整体 review；修复再审；engine/macOS/desktop 合适测试、Clippy、fmt、diff 检查；文档填写实际结果与局限 | 已关闭 |

### 公共接口约定（实现前由 S0 复核）

- 引擎会话身份采用不依赖桌面 UUID crate 的小型值类型；desktop 从已有 session UUID 转换，匿名 CLI 保持兼容。
- session completion 的 sequence 分配、owner 更新、令牌取消及入队在同一个提交顺序下完成，不能等 GPUI future 首次 poll 才决定新旧；reply 使用 futures oneshot，不能恢复 tokio oneshot。
- S5 先建立 session/sequence/owner/token 的同步提交基础；S6 才增加 EndInput 转换和 desktop 接线。新请求取消此前可替代的 completion，接受记录/ClearCaches/diagnostics 不用于取消。丢弃 reply 仅取消对应 token，不能误清新请求。
- 提交 owner 与最后成功普通补全的 active session 分开；EndInput 总是携带 session 排入 FIFO，只在提交 owner 匹配时立即取消 token，worker 根据 active session 决定是否结束规格。history-only/失败/取消不转移活动规格归属。EndInput 和控制消息要形成顺序边界，不能被“先应用所有副作用、最后执行最新 Complete”反转。Complete(A)→EndInput(A) 必须取消旧 A，EndInput(A)→新 Complete(A) 必须允许重新进入；A→B 成功普通补全→迟到 EndInput(A) 不影响 B；B 取消/失败/history-only 时不能吞掉 A 的结束；重复 EndInput 不重启宽限。接受记录、ClearCaches 和诊断保留其队列屏障，不能被合并丢掉。
- 诊断在 worker 取得 Engine 所有权后执行，不抢占在途尝试，不启动一个尚未加载的 Engine；返回 EngineClientDiagnostics，engine 为 Option 表示尚无 Engine，requests 独立保存请求/取消/失败/重建计数；快照时刻是实际读取时刻，不是请求发送时刻。
- CacheMapDiagnostics 冻结字段为 entries、allocated_bytes、hits、misses、expired_removals、capacity_clears；三组为 suggestions、script_output、specs。expired 同时计 miss，替换同 key 不计 capacity clear，未启用缓存不计 lookup；计数饱和递增。字节沿现有 payload 估算，补 acceptance_scope/public_ai 字符串，不含 map 节点、字符串余量和分配器开销，共享 Arc 跨条目可能重复计量。
- hook 各 map 统计与数据共用各自的锁，无嵌套锁。同一个 Engine 的 rebind_hosts 清空现有 HookCache 数据，保留实例与累计计数；成功清缓存、失败重载、自动换代均验证。只有 Engine 重建才重置统计，并由 worker 生命周期计数区分。
- AX 旧 epoch 查询不得回写缓存、退避状态或发 None 清掉新 caret；预算覆盖远程调用，不声称消除本地处理和系统超时误差。回放成功延迟从提交开始计算，与取消/失败计数分开；慢脚本以握手确认启动后才提交替代请求。
- 日志和回放报告默认只含数量、时间、状态、资源；不输出真实命令正文、环境变量或密钥。场景输入使用明确的测试数据。

## 验证环境与执行记录

验证目录：`/var/folders/j9/5bqndst12xs3cs3mp44lfw1w54nfyn/T/fastab-resource-verify-1sab0z8v`。Engine、desktop、macos-utils 使用源文件副本，其余有效 crate 软链；bundle/scripts 用实目录保持 snapshot 的真实安全检查。最终必须逐字节核对涉及源文件与工作区一致。

基线证据：v0.0.5 的 engine 测试 443 通过、1 忽略；其后 IR 定向 61 通过；忽略的 footprint 测试单独通过。这些是前一轮证据，不冒充本轮结果。

### S0 设计 review

第一位 Astra 发现统计重绑归零与计划冲突，以及 Arc memo 缺乏测量门槛；已改为同 Engine 保留 cache 计数、memo 条件实施，并补 SHA/history/AX/回放验收。该审查者已复核这些修订，通过。第二位 Astra 要求补齐取消状态提交、idle 恢复和控制消息顺序；已加入上文具体契约，该审查者也已复核修订，无剩余设计阻断，S0 关闭。

### S1–S8 分步记录

逐步追加实际 diff 范围、review 人、发现的问题、fix、复核结论与命令/结果。没有剩余已知问题才关闭；不能把尚未执行的验证写成通过。

#### S1 关闭记录

- 实现：Astra 修改 snapshot.rs；非 sidecar 用固定 64 KiB 分块，sidecar 仍整份捕获。
- 独立 review：主代理逐段检查 Unix/nonUnix 的 open/metadata/capture/read_file 调用链、摘要格式、错误传播和栈缓冲生命周期；没有发现须修复问题，原 O_NOFOLLOW/普通文件/代际复核保持。
- 验证：隔离目录 snapshot::tests **6/6**；跨代拒绝与相同内容跨代读取两项 **2/2**。日志 `/tmp/fastab-resource-s1.log`；基线 snapshot **2/2**。格式与 diff 检查通过。
- 边界：尚未在安装版量启动峰值；真实 bundle 的资源观测归 S8，不承诺减少完整 SHA 的 I/O。

#### S2 关闭记录

- 实现：Astra 修改 ir.rs；直接 serde writer，不分配临时 JSON Vec；Registry 单次遍历用 Weak memo，初次解析不分配 memo，遍历结束销毁，完整 Eq 保护不变。
- review/fix：Astra 独立复核。最初固定小缓冲在 release 完整加载中退化到约 91–97ms，已撤回。改为无缓冲 writer 加单遍 memo，复审身份校验、子项处理顺序、弱引用释放、跨遍历重算与碰撞保护，通过。
- 同 release 二进制三路轮换：旧 Vec 加载 compute 为 **77.0–80.9ms**，流式无 memo 为 **74.6–77.1ms**，流式加 memo 为 **50.8–51.3ms**；两次遍历 hash 调用 **20,830→11,621**。驻留树估算均 **2,762,496 bytes**。Registry 首次目录加载受文件缓存影响，未混入上述 compute 对比。
- 验证：ir::tests **66/66**，1 个默认忽略的真实 fixture profile 单独通过；日志 `/tmp/fastab-resource-s2-ir.log`、`/tmp/fastab-resource-s2-final.log`。五项 fingerprint 定向回归通过。
- 限制：这些是本机完整 Registry 规格加载时间，不是桌面完整补全延迟；瞬时峰值和释放回访在 S8 继续观测，不把 resident 估算作为 phys_footprint。

#### S3 关闭记录

- 实现：主代理修改 history.rs；单份 cache 保存原始 aliases/shell，变更清理 entries 和 LRU，锁外构建后只在上下文仍相同的情况下发布。
- 独立 review：Astra 检查上下文匹配、A→B→A、锁边界、32 项上限和冷/热结果；无阻断。S5 添加取消检查时重新复核发布边界。
- 验证：history::tests **8/8**，包含真实 zsh/fish alias 解析变化与迟到构建不得污染另一上下文；日志 `/tmp/fastab-resource-s3.log`。

#### S4 关闭记录

- 实现：Astra 修改 hook_cache.rs、diagnostics.rs、lib.rs 和 runtime.rs；各 map 的数据与累计统计同锁，补充 payload 字段估算，同 Engine rebind 保留 HookCache 实例。
- 独立 review：另一位 Astra 检查 TTL 由读取方决定、过期记 miss、disabled 不计 lookup、同 key 替换不清表、计数饱和、诊断不改变状态，及三类重载路径，无阻断。
- 验证：hook_cache::tests **8/8**，成功清缓存/失败重载/自动换代保持实例及累计计数回归 **1/1**；日志 `/tmp/fastab-resource-s4.log`。S5 接入取消写屏障后重审此发布边界。

#### S5 关闭记录

- 实现：两位 Astra 分别负责 worker/runtime 状态提交和 hook/process/typed 取消检查。同步提交即分配 token/sequence/owner，FIFO 跳过已取消工作，控制消息不跨队列屏障合并；保留 futures oneshot。取消复用同 Engine，完成和取消由短互斥门决定唯一赢家。
- 交叉 review/fix：历史索引前半段的上下文/LRU 更新最初缺少取消写屏障，已补门；版本探测被取消的 None 结果可能成为长期失败缓存，已改成局部探测后有条件发布。两项均补真实回归并经另一位 Astra 复核。Completed 后的 token 也禁止再次提交状态。
- 验证：最新 engine --lib **471 通过、0 失败、2 忽略**（真实大 fixture 默认忽略）；日志 `/tmp/fastab-resource-s5-tests.log`。涵盖子进程启动握手后取消、kill/reap 和丢弃部分输出，typed Try/循环传播、native fallback 拒绝、hook/history/version 缓存不污染、旧 idle deadline 恢复、新树取消后静默释放、取消后正常补全仍只初始化一次 Engine。
- S3/S4 被 S5 触及的发布边界已重审。取消仍为协作式，同步文件读取和 JSON 解析不保证立即抢占；Windows 未实机验收。

#### S6/S6b 关闭记录

- 实现：Astra 负责 Engine 的 EndInput 与队列公平性；主代理负责同步提交、空输入/pre-exec 的 session-aware 事件和 remote session removal 通知。EndInput 清本次 generator session、开始已有树的宽限；后台/重复结束不取消新 owner、不重启期限；无 Engine 不初始化。
- 独立 review：两位 Astra 分别复核 Engine 与 desktop/remote 接线。空 B 终端不能证明 A 的未提交命令已结束，按契约保留 A；单纯隐藏、焦点暂离均不伪造 EndInput。明确断开则用服务端 UUID 通知，重连不复用握手 ID，Closed 在 sessions_changed 前发出。最终复核又发现取消 B 后 End(A) 可能丢失，已分离提交/资源 owner，成功普通补全才转移资源归属；旧通知始终按 FIFO 到 worker 后判断。watchdog 和成功 ClearCaches 清空资源 owner，失败 reload 保留。A→B 取消→EndA 归零与 B 成功→迟到 EndA/historyC 不误释放均有真实交错测试，另一位 Astra 复核通过。
- 防饥饿：每连续 64 个非 attempt 消息提供一次到期释放机会；已取得的 FIFO 队首若为有效 Complete 则仍优先执行；不重排或丢弃控制消息。实际 attempt 和 timeout 后复位计数。
- Replace 健壮性：只收集实际 displaced 的旧 Arc，处理完全部别名后确认所有查找表及 pinned 均无持有，再删除 loaded 的孤树；先 drop 临时 Arc 再剪 Weak。另一位 Astra 独立复核通过，新增树/选项 Weak 真正失效与同路径版本树保留两项回归，并增强剩余别名/旧 deadline 断言。
- 验证：worker **27/27**；真实 UnixStream 两连接同握手 ID 的关闭通知回归 **1/1**，队列繁忙下真实 Registry/Weak 释放和保序 **1/1**；desktop 全套 **205/205**。回放 self-check 同进程确认 3 成功、1 取消、0 失败、只初始化一次 Engine。日志 `/tmp/fastab-resource-s6.log`、`/tmp/fastab-resource-s6b.log`。

#### S7 关闭记录

- 实现：主代理修改 UIElement 查询、WindowServer 事件、desktop platform 与 GPUI 消费路径。共享截止时间覆盖 helper 各属性、叶缓存、一次根遍历和 frame；窗口级失败退避 750ms，查询放到 focused-window 锁外，身份和 epoch 在回写及消费时再次核对。
- 独立 review：Astra 发现正常移动先发 None 会调用 hide 并取消在途补全；已去掉预先 None，几何变化只使旧查询失效，成功保留列表，实际查询失败才清 caret。补充查询中途耗尽预算的真实 closure 测试；Astra 再审通过。
- 验证：macos-utils **32/32**，desktop platform::macos **11/11**，gpui_host **3/3**。日志 `/tmp/fastab-resource-s7-tests-final.log`。使用已有 protoc 解决首次构建缺 PATH 问题，没有更改构建脚本。
- 边界：没有启动或安装应用验证真实终端 AX 体验。既有 ActiveSpaceChanged 的无身份 Hide 未在本轮改写，不宣称所有系统空间事件都已消除迟到隐藏；本步覆盖窗口/面板焦点与几何查询。

#### S8 关闭记录：持续回放与真实资源证据

- `examples/resource-replay.rs` 用一个 EngineClient 和一个观察线程执行 JSONL；延迟从同步提交开始，future 完成时取样，不包含之后等待 `await` 指令的时间。成功分位数与取消/失败分开；摘要不输出命令、环境、建议内容或错误正文。回放进程内固定测试设置并禁用历史加载，不写用户配置。
- `--self-check` 使用临时真实规格和 shell 子进程的 ready 文件握手，断言慢请求被替代后取消、EndInput 身份/重复事件、缓存静默归零、重载建议以及一次 Engine 初始化。CI 在现有 macOS Test 后执行；不对跨机器延迟或 MB 设硬门槛。
- macOS `--footprint` 通过 `proc_pid_rusage(RUSAGE_INFO_V4)` 读取**自身**当前/峰值 `phys_footprint`；System allocator 与 desktop 一致。最初每次 spawn `/usr/bin/footprint` 的方案出现第二轮约 72MB 的异常，改成无子进程的内核计数后未再复现；正式比较采用 native 数据，未据该采样方式下的异常结果添加强制 allocator trim；尚未证明外部采样异常的具体分配器机制。
- 真实 `gcloud compute ` 同进程回放：初始化菜单基线 **10.88–10.95 MiB**，首次加载持有 **54.06–54.36 MiB**，首次到期释放 **14.55–14.81 MiB**。Registry 从 2 个文件/估算 2,809,421 bytes 变为 **0 文件/0 bytes**；这些结构估算与物理占用分别报告。
- 8 轮独立压力回放（仅该场景注入 100ms 宽限）每轮都确认 registry 归零；第 1 轮释放后 **14.72 MiB**，第 2 轮 **19.08 MiB**，第 3–8 轮 **19.23–19.36 MiB**，再静默 12 秒仍为 **19.36 MiB**；全程一次 Engine 初始化、无 watchdog/panic/失败。这里只能证明本次 8 轮未呈持续大幅增长，不能以有限轮次证明所有场景无泄漏。
- 辅助采集的长驻回放进程 heap 显示 **1,944,736 bytes** 存活分配，vmmap 的真实足迹约 19.8 MiB；较大的 malloc dirty/fragmentation 数字不是存活 Spec 大小，也不能当作 `phys_footprint`。释放后不会回到未初始化进程的约 2 MiB：Engine 索引、sidecar、线程、代码页、其他缓存与分配器保留页仍在。
- 数据来自独立回放进程，**不是已安装 Fastab.app 的总内存**；没有安装/重启 desktop、IME 或终端，没有做真实 AX/GPUI 体验验收。真实 hook/history 负载可能有额外驻留，当前 compute 场景的三个 hook cache 和历史索引均为空。

可复现命令（有效 Cargo workspace 内，优化构建）：

```bash
cargo run --release --locked -p fastab_engine --example resource-replay -- --self-check
cargo run --release --locked -p fastab_engine --example resource-replay -- bundle/specs-ir crates/fastab_engine/testdata/resource-replay/compute.jsonl 5000 --footprint
cargo run --release --locked -p fastab_engine --example resource-replay -- bundle/specs-ir crates/fastab_engine/testdata/resource-replay/compute.jsonl 10000 --footprint
cargo run --release --locked -p fastab_engine --example resource-replay -- bundle/specs-ir crates/fastab_engine/testdata/resource-replay/compute.jsonl 25000 --footprint
cargo run --release --locked -p fastab_engine --example resource-replay -- bundle/specs-ir crates/fastab_engine/testdata/resource-replay/release-cycles.jsonl 100 --footprint
```

正式原始报告：`/tmp/fastab-resource-grace-{5000,10000,25000}-native.jsonl`、`/tmp/fastab-resource-cycle-native-report.jsonl`；临时报告用于本机追溯，输入场景已纳入仓库。

#### S8b 关闭记录：默认宽限选择 25 秒 → 10 秒

同一个优化二进制、相同 bundle 和输入，分别注入 5/10/25 秒。每组均依次执行结束输入后等待 6、11、26 秒再回访。结果如下；每格为该次实际补全时间，不是用户行为分布或大量样本的统计估计。

| 宽限 | 6 秒回访 | 11 秒回访 | 26 秒回访 |
| --- | --- | --- | --- |
| 5 秒 | 已释放，91.045ms | 已释放，89.122ms | 已释放，87.365ms |
| 10 秒 | 热缓存，2.117ms | 已释放，89.932ms | 已释放，89.082ms |
| 25 秒 | 热缓存，1.328ms | 热缓存，1.121ms | 已释放，89.424ms |

首次 compute 加载为 53.01–53.69ms，连续热请求为 0.178–0.295ms；释放后重载会同时重读根规格，因此不把约 89ms 与 S2 的单 compute 解析耗时混为一谈。

选择 **10 秒**：明确结束输入后缩短 60% 的保留时间，并为数秒内回访保留热缓存。5 秒会在本次 6 秒回访中引入约 90ms 重载；25 秒在 11 秒后仍保留首次加载的约 54MiB。此选择是基于测得成本的工程折中，没有声称已知用户停顿分布或证明全局最优。当前输入仍 active 时不按发呆时长释放。

实现修改 `SPEC_IDLE_GRACE`，默认断言同步为 10 秒，临界前测试改成 `SPEC_IDLE_GRACE - 1ms`，`EngineClientOptions` 仍允许回放注入候选值；`spec-idle-release-plan.md` 当前契约同步，v0.0.5 历史测量仍保留当时的 25 秒语义。

### 追加审计：真实内存与无法释放路径

用户要求观察释放后的真实占用及不能释放的情形。S8 同进程优化构建采集 `phys_footprint` 和峰值，重复加载/结束/释放/回访，记录已初始化菜单基线与多轮释放后水位，不用 RSS 或树体积代替进程占用。桌面和回放均未启用自定义全局分配器，但回放不包含 GPUI/AX/桌面其他常驻组件，不能冒充安装版总内存。

审计发现以下释放缺口；最终复核发现后两项并重新打开 S6b；四项均已实现并通过交叉 review 与最终测试：

- terminal 连接断开时原来只更新 Jev 上下文，不通知 Engine；无空缓冲/pre-exec 的关闭会让旧 owner 的树保持 active。新增真实 session removal 通知，以服务端新建的 UUID 定位已结束 owner，后台断开不隐藏当前终端；重连生成新 UUID，不复用客户端握手 ID。
- 控制消息/已取消请求持续填满队列时，worker 反复 continue，可能一直收不到 idle timeout。增加有界消息处理后的释放机会，同时保持队头可执行补全优先以及控制消息 FIFO。
- A 已有活动规格，切 B 提交后因隐藏/光标失败丢弃任务，取消恢复了 A 的活动树，但提交 owner 已变成 B；随后 A 执行命令或关闭，End(A) 曾被误判为旧会话而拒绝。已把“最新提交者”与“最后成功普通补全建立的活动规格所有者”分离；history-only、失败和取消不能伪造所有权。
- 公开 Registry API 在 bundle 已加载后用 Replace 覆盖所有别名，旧 Arc 可能只留在 loaded，失去路径关联，因此只能等容量淘汰。桌面当前在 fresh load 后立即 overlay，不触发这个次序；仍补齐 API 健壮性，显式 Replace 清理已失去所有查找入口的旧非 pinned 树，并保留仍有别名/同路径兄弟。

预期保留与异常必须区分：当前未结束输入、pinned 规格、本地运行期生成值、带 TTL 的 hook 缓存不属于此次 bundle 树闲置释放范围。Registry 模板只保留索引/pinned，选项池和 memo 是 Weak，history index 保存字符串；NativeHooks 的 sidecar 描述目录以及已惰性解析的 descriptor 随 Engine 常驻。树已卸载但进程仍有这些资源或分配器页不等于树泄漏，需用各缓存诊断与多轮真实水位一起判断。

10 秒是普通空闲宽限，不是强制杀线程的回收上限：Engine 仍由在途 attempt 持有时，监督线程必须等它交回才能释放。脚本/typed 求值已覆盖协作取消，但同步文件读取、JSON 解析或不返回的系统调用不能被安全地立即抢占；watchdog 允许新 Engine 接续，旧线程若一直不返回仍可能持有旧 Engine。此边界未通过强杀线程伪装解决，异常计数用于识别它；最终回放的 watchdog/panic 均为零。

### 追加评估：25 秒宽限是否过长

用户明确要求评估后把时间改动纳入实施。新增正式步骤 S8b，在 S8 测量基础上选择并调整默认宽限；S8 增加 5/10/25 秒对照：固定真实大规格的加载—离开—静默—回访路径，测冷/热/释放后重载延迟、内存保留时间与复用次数。25 秒是既有保守值，没有当前最佳值证明；对照后选择 10 秒，S8b 记录选择和变更理由。测试场景的回访间隔不冒充真实用户行为分布。调整默认时同步计划、边界测试和契约文档，再独立复核；所有语义测试依赖常量或注入时钟，不用过时的25秒字面量。

### S9 整体 review 与最终验收

- 两位 GPT-6 Astra 按核心 worker/取消/缓存提交/所有权和 hook/AX/资源观测两条路径复核，再交叉审查对方修复；最后增量为提交 owner 与资源 owner 分离、Replace 孤树清理、10 秒常量和 native 采样。已发现问题均修复并复审，未发现剩余可行动的代码阻断。文档也同步区分普通 idle 宽限与 Replace/LRU/ClearCaches 显式失效。
- 最终 `cargo test --locked -p fastab_engine --lib`：**479 通过、0 失败、2 忽略**，24.99s。两项大 fixture 默认忽略，前述专项 profile/footprint 与持续回放提供资源证据。
- 初轮最终 desktop：**205/205**；macos-utils：**32/32**；fastab_remote_ipc：**5/5**。当轮合计 **721 项通过**；后续 Astra ultra 发现的桌面生命周期缺口及新增回归见下方追加记录。日志 `/tmp/fastab-resource-final-{engine,desktop,leaf}-tests.log`。
- 最终 replay self-check：**3 成功、1 按预期取消、0 失败**，一次 Engine 初始化、无 watchdog/panic；日志 `/tmp/fastab-resource-final-selfcheck.log`。优化 replay 构建通过。
- `cargo clippy --locked -p fastab_engine -p fastab_desktop -p macos-utils -p fastab_remote_ipc -- -D warnings` 和 replay example 单独 Clippy 均通过；新增分号/map_err 与诊断迭代告警已修复。曾扩展 `--all-targets`，遇到基线已有的测试代码告警（hook_baseline 大 enum、idle_mark 三态、旧 fixture 字段赋值、typed-hook 测试 read_to_end），未扩大范围改写这些旧测试，也不声称 all-targets gate 通过。
- 修改的 **26 个 Rust 文件**均通过 rustfmt；`git diff --check` 通过。隔离验证的 **26 个文件**（含回放与场景）逐字节等于工作区最终源码；remote IPC 通过原目录软链验证。没有删除阻断根 workspace 的无 manifest 遗留目录。
- 最终默认值（不传 grace 覆盖）与 8 轮资源复验均通过。默认报告显示 EndInput 后 deadline 9,999ms，6 秒仍有热缓存、11 秒归零；持有 **54.42 MiB → 首次释放 14.92 MiB**，后次释放 **19.22 MiB**；6 秒热回访 **1.201ms**，两次释放后回访 **92.293/87.341ms**。8 轮独立复验为首轮 **14.50 MiB**、第 2 轮 **18.91 MiB**、第 3–8 轮 **18.98–19.09 MiB**，静默 12 秒仍 **19.09 MiB**；每轮 registry 归零，两组各 8 次成功、0 失败，一次 Engine 初始化、无 watchdog/panic。日志 `/tmp/fastab-resource-final-default-native.jsonl`、`/tmp/fastab-resource-final-cycles-native.jsonl`。
- 交付边界：本轮未提交/推送/发版；未安装或启动新桌面/IME/真实终端，真实 AX 延迟、光标位置和已安装 App 总内存仍需独立运行验收。有限固定场景不能证明所有输入/所有系统上的性能上限或不存在任何泄漏。


### Astra ultra 追加审查与修复（2026-09-30，已关闭）

- 追加独立审查确认一项 P2：`OverlayController::end_input` 只隐藏窗口，保留 `last_input`。非空缓冲的终端断开后，设置修改触发 `ReloadCredentials → recomplete`，即使 remote session 已移除也会使用默认上下文重提交旧缓冲；成功后规格重新 active，已关闭连接不会再发 EndInput。pre-exec 后尚无新缓冲时同样可以重试旧命令。S6/S9 因此重开，并按下述修复重新关闭。
- 最小修复：EndInput 命中当前会话时先 `forget_last_input()`，再沿原 `hide()` 处理 generation、loading、AI 取消和拦截。保留 session UUID，确保原拦截同步能找到会话；后台会话结束和普通 hide/dismiss 不清除当前输入。下一次真实缓冲事件重新建立 `last_input`，即使文本相同也不会误触发重复输入抑制。
- 回归使用现有 `#[gpui::test]`，实际调用 OverlayController 和 EngineClient。覆盖后台 EndInput/普通 hide 后设置刷新仍可提交、当前 EndInput 后多次设置刷新不新增请求、规格到期归零，以及同 UUID/同文本的下一次真实输入恢复提交。不是源码字符串断言或复制实现的假状态测试。
- 设置 override 是 TLS，不能覆盖 Engine worker，因此用 `current_exe --exact` 独立子进程执行此用例，在启动 Engine 前设置该子进程的内存配置并关闭历史加载；不写配置文件、不改变 HOME、不污染其他并行测试。父进程同时检查子进程成功和确实通过 1 项测试，防止过滤器失配造成零测试伪通过。
- 红绿证据：旧实现两次设置刷新后 `submitted=5`，预期为 3，真实测试失败；修复后保持 **3**，释放后同文字真实输入使 `submitted/completed=4`，回归通过。日志 `/tmp/fastab-endinput-red.log`、`/tmp/fastab-endinput-green.log`。
- 最终验证：desktop 全套 **206/206**，`cargo clippy --locked -p fastab_desktop --tests -- -D warnings`、该文件 rustfmt 和 `git diff --check` 均通过。日志 `/tmp/fastab-endinput-desktop-tests.log`、`/tmp/fastab-endinput-clippy.log`。最终隔离验证的 overlay 源码逐字节等于工作区。
- 修复与回归经 GPT-6 Astra **ultra** 再次只读复核，无剩余可行动问题。此增量仅涉及 overlay 与两份契约/记录文档；未再次运行无改动的 engine/macOS/IPC 测试或内存基准，不把前轮测量冒充本轮重测。没有安装或操作真实桌面、IME、终端；未提交、推送或发版。

### v0.0.5 同版本交付准备（2026-09-30）

上述“未提交、推送或发版”描述各轮审查结束时的状态。用户随后授权按内容提交、推送并重新打包 0.0.5。本次交付按规格加载性能、Engine 生命周期、桌面输入结束与 IPC、AX 光标查询、持续回放与 CI、文档及发布说明六组提交；Cargo 与网站版本均保持 0.0.5，中英文更新日志同步修改现有版本条目。推送使用远端旧引用的精确 lease，原子更新 main 与 v0.0.5；交付时另行核对新提交对应的构建状态及 DMG 资产，不以旧资产或仅已触发工作流视为打包完成。

### 安装版反馈后的追加修复（2026-10-08）

用户反馈清空输入或执行后超过 10 秒内存仍高，且 Otty 有时不出提示。本轮基线为 `5c138588`，安装版为 0.0.5（2026-09-30 构建），Otty 1.5.4，macOS 27.0。以下区分已确认的代码缺口与尚未验证的安装体验。

**研究证据。** 当前安装版通过合成 IPC 输入 `gcloud compute `，桌面 `phys_footprint` 约从 58.8 MiB 升到 107.2 MiB，输入结束后约 10 秒降到 64.6 MiB，说明释放路径能够生效。另一次真实 Otty 场景清空后仍约 104 MiB；前后 heap/vmmap 显示 live heap 仅增加约 3.1 MiB，而 IOSurface 增加约 24.5 MiB、malloc dirty/swap 碎片约增加 16.8 MiB。该轮有窗口显示变化，不能把整个物理内存增量归为规格泄漏。AXUIElement 数量约 14→24，没有重现早期的大量 AX 对象泄漏。真实进程占用与 registry 估算字节应分别观察。

**1. 把资源使用与显示状态接起来。** 原 `hide`/`dismiss` 只丢弃旧 UI 结果，普通补全仍可运行；非空但无法定位的输入可无限保持规格 active。现改为：

- 没有有效 caret 时保存最新输入，暂不提交 Engine；首次有效位置恢复一次，重复位置不重提。
- `RemoteHook::edit_buffer` 必须先入队 `GpuiOverlayBuffer`，再请求 AX/IME 刷新。旧顺序中，快速 caret 回复可能先到，随后新 session 的 buffer 将它清掉，又没有周期性定位重试，停在等待状态直到下次输入。真实 socket→protobuf→hook→事件队列回归固定新顺序，同时覆盖空白输入不请求定位及断开通知归属。
- 隐藏、Esc、禁用、插入后抑制、布局重试耗尽和会话切换结束原会话资源使用；用原有 session-aware `EndInput` 协作取消工作并启动 10 秒宽限，保留字符串候选供显式 Show。
- 临时 caret 查询失败允许自动恢复；真正窗口/面板焦点变化、窗口销毁及输入结束会清掉可重试旧输入，不能在新位置复活。窗口销毁事件携带清空时捕获的 epoch，消费端核对当前身份，避免旧事件误清新窗口。
- Tab-only 或空候选的最终结果可结束资源使用；有 `pending_generators` 的有效中间结果必须继续 debounce。独立 review 发现过早按 invisible 退休会取消唯一动态建议来源，已修正为仅终轮退休。
- 焦点失效条件在 host 与 platform 复用，覆盖 Otty AX、Terminal/iTerm、xterm；IME-only 的元素通知仍按原规则处理。

**2. 写入失败也必须关闭连接。** remote writer 已经发出 `bad_connection` 通知，但主循环没有消费。新增 select 分支进入原统一清理，删除会话、取消 pending response 并发送关闭通知。真实 Unix socket 回归保留客户端读方向连接，通过关闭服务端写半部制造写错，防止测试被 EOF 清理路径伪装成通过。

**3. 让安装版资源与 Otty 失败可诊断。** `ftab _ dump-state engine` 经 GPUI 转交现有 worker，返回纯数值的 registry/hook/history/请求统计，不创建 Engine、不读 shell 正文、不在 GPUI 前台等待 Tokio。IPC 1.5 秒超时；已取消诊断跳过快照。CLI 严格按数字 schema 解析并重编码，旧桌面误返回 shell 状态时拒绝打印；未知协议组件也拒绝。服务端错误保留可读原因。`ftab _ dump-state platform` 的 `ax_caret` 记录 Otty AX 查询阶段、固定失败类别、错误码、耗时和身份；其他 caret 路径只记录 route。读取诊断本身不重新查询 AX。保留原 PID/窗口/零长度选区/预算检查，不放宽坐标有效性规则，不增加窗口矩形兜底。

**验收范围。** 以下测试与静态审查针对源代码。未替换当前安装版，没有把缺少 Fastab overlay 的 Otty 单应用截图当作充分证据，也没有据此宣称已确定或修复 Otty AX 定位失败的具体原因。新构建安装后仍需结合上述两个诊断入口核对真实输入、光标失败阶段和 registry 归零时刻；进程总水位同时包含 UI、GPU、缓存与分配器保留页。

**Review/fix 与最终验证。** GPT-6 Astra ultra 交叉复核发现并修复了空结果 pending 误取消、普通 AX 终端面板切换遗漏、销毁窗口输入可复活、输入/光标入队次序四个问题；实际改动再次独立复核，无剩余阻断发现。

- 最终相关单测：desktop **213**、engine **480**（2 项原有忽略）、CLI **37**（3 项原有忽略）、remote IPC **6**、macos-utils **33**、local IPC **12** 通过，合计 **781**。local IPC 的旧 socket listener 测试被沙箱阻止 bind，在允许临时 Unix socket 的同一隔离工作区重跑通过。另行扩大运行的旧 CLI 集成测试中，2 项读取用户 local-state 数据库因沙箱 `Operation not permitted` 未通过；没有为它们改写用户数据库或宣称全量集成测试通过。
- GPUI 生命周期回归验证无 caret 时零提交/零初始化、有效位置恢复一次、Esc 不自动恢复、Show 可重提、会话切换和强焦点失效后旧规格归零。动态生成器用真实脚本和真实 debounce 事件验证普通模式结果到达 host；Tab-only 还验证 UI 行及 EndInput 清空。GPUI TestWindow 没有 NSWindow，因此普通模式末次 native 渲染不在该测试验收内。
- 红绿验证：临时撤销 writer 通知分支后，真实 socket 回归在 3 秒清理期限失败，恢复后通过；临时恢复过早 invisible 退休后，普通/Tab-only 两个真实 debounce 回归均失败，恢复后均通过。仅改临时副本，源工作区保持最终修复。
- 六个相关 crate 的 `cargo clippy --locked ... -- -D warnings` 通过；desktop/remote IPC/local IPC 的 `--tests` Clippy 也通过。新代码中的错误映射 lint、CLI 错误库引用及协议字段名问题均已修正。rustfmt、`git diff --check` 通过。
- 为保留未跟踪且缺 manifest 的 `crates/fig_input_method/`，Cargo 在临时验证工作区执行；所有本次变更文件逐字节与源工作区核对一致，bundle/scripts 使用真实目录以保持 provenance 检查。最终日志为 `/tmp/fastab-resource-desktop-final-tests.log`、`/tmp/fastab-resource-unit-tests.log`、`/tmp/fastab-resource-platform-ipc-tests.log`、`/tmp/fastab-resource-clippy.log`、`/tmp/fastab-resource-test-clippy.log`；红绿日志为 `/tmp/fastab-writer-{red,green}.log`、`/tmp/fastab-pending-{red,green}.log`。以上是修复完成时的验收记录。随后用户授权按内容提交、推送并重新打包 0.0.5；版本号保持不变，更新现有发布说明并以远端旧引用的精确 lease 原子推送 main 与 v0.0.5。

## 2026-10-08：设置关闭后的真实内存与原生窗口回收

**现场口径修正。** 本轮开始时，安装版 desktop PID 38134 的 `phys_footprint` 为 **41.1 MiB**（峰值 75.0 MiB），IME 为 8.5 MiB。`ftab _ dump-state engine` 返回 `engine=null`，提交数与引擎初始化数均为 0；`heap` 没有存活的 `GPUIWindow` / `CAMetalLayer`，`vmmap` 的 IOSurface resident 为 0。因此这次不是规格树仍在使用，也不是设置窗口没有销毁。前文约 19–20 MiB 是独立引擎回放进程，不能作为完整桌面应用的空闲承诺。

**检查与决策。** 设置关闭后，GPUI 的应用级 `renderer_context` 仍持有 `InstanceBufferPool`。该池每块默认 2 MiB，GPU 完成后归还，没有随窗口销毁释放的出口。字体 ID、字体表和文本缓存仍被共享布局引用，本轮不清空。现场 malloc 的约 8.2 MiB 碎片也不等于可归还字节数。

另试验公开的 `malloc_zone_pressure_relief(NULL, 0)`，但本机 macOS 27.0（26A428）的默认 zone 与 objc zone 回调直接执行 ARM64 `mov x0, #0; ret`；调用返回 0、耗时亚微秒量级。三轮原生设置窗口对照未建立可重复的回收收益，已撤销这条候选路径，没有加入无效的延迟回收任务。此结论只适用于本机已检查的 zone，不泛化到所有 macOS 或自定义分配器。

**已实施。** 固定 vendor 的 crates.io GPUI 0.2.2，保留发布包许可证和依赖，根 `[patch.crates-io]` 统一覆盖所有消费者，详见 [依赖补丁说明](../vendor/gpui/FASTAB_PATCHES.md)。

1. `MetalRenderer::drop` 取出应用级池内的闲置 buffer，在解锁后释放，保留增长后的 buffer 大小。
2. pool 和借出的 buffer 带 generation。关闭或扩容使旧 generation 失效；GPU 完成前仍持有 buffer，完成后旧 buffer 直接释放，不能把清空的池重新填回。其他活窗口可继续绘制并使用新一代缓存。
3. 真实原生窗口压力验证额外复现了 GPUI 既存的焦点死锁：`window_did_change_key_status` 持有窗口状态 mutex 调用 `resignKeyWindow`，系统同步发送 resign 通知再次取同一把锁。该阻塞发生在首个设置窗口打开、任何 renderer 销毁之前。现先用 `StrongPtr` 保活原生窗口、释放 mutex，再调用 AppKit，正常激活与绘制分支保持原样。
4. 更新 Cargo.lock 与第三方声明收集器，使本地 GPUI 仍计入 `THIRD_PARTY_NOTICES.txt`；声明正文无须改写。vendor 不属于产品 workspace members，不把上游完整测试集带入常规 CI。

**测量与边界。** 探针直接使用生产 `settings_ui::open_settings_window` / `close_settings` 和原生 GPUI；只在临时副本替换入口，不启动安装、登录项、IME 或桌面 IPC，未替换 `/Applications/Fastab.app`。同一 debug 配置启动后等待 2 秒、打开设置 4 秒，关闭后在 1/3/10/30 秒以 `proc_pid_rusage` 采集进程 footprint。没有用引擎回放替代 UI 测量。

| 原生窗口独立进程，3 轮 | 关闭后 1 秒 | 关闭后 30 秒 |
| --- | --- | --- |
| 原始 renderer，同样使用 path 依赖构建 | 25.69–26.25 MiB | 24.14–25.67 MiB |
| 回收补丁 | 21.41–21.55 MiB | 19.66–21.55 MiB |

最初 registry 与 path 的 profile 指纹不同，故补做了原始 renderer 的 path 构建对照。即便如此，各组打开窗口时的峰值和分配器自然回收仍有差异，**不把总 footprint 差全部归因于清池**。相同原生窗口场景的 `vmmap` 明确显示：关闭后 `IOAccelerator (graphics)` 从 **2432 KiB 降到 384 KiB，减少 2 MiB**。这是当前可以明确归因的回收证据；不承诺完整安装版会从 41 MiB 变成 20 MiB。

**Review/fix 与验证。** GPT-6 Astra ultra 代理分工实现、独立审阅 Metal 池与焦点回调修复。审查发现第三方声明过滤本地依赖的问题，已修正；最终静态审查没有剩余可操作发现。

- 3 项真实 Metal 测试在 `MTL_DEBUG_LAYER=1` 下通过：旧 GPU 完成不能回灌、扩容后重开可复用、另一 renderer 的在途 buffer 保持有效。测试实际提交 fill/synchronize 命令，完成回调读取结果，不是模拟计数。
- 同一原生压力探针修复前卡在第一轮；修复后完成 **12 次设置打开/关闭、360 次补全窗口刷新请求**，Metal API validation 无错误，正常退出。它验证原生窗口和渲染资源寿命，不替代真实终端输入验收。
- 生产源回归：desktop **213**、fastab_gpui **109** 项通过。探针入口已从验证副本恢复，未写入产品入口。
- 两个产品 crate 的 `cargo clippy --locked --all-targets -- -D warnings`、第三方声明 `--check` 与 `git diff --check` 通过。
- 临时证据：`/tmp/fastab-settings-{path-baseline,patched}-{1,2,3}.log`、`/tmp/fastab-settings-{baseline,patched}-vmmap.txt`、`/tmp/fastab-settings-metal-tests.log`、`/tmp/fastab-settings-stress-{sample.txt,fixed.log}`、`/tmp/fastab-allocator-pressure-relief-evidence.md`、`/tmp/fastab-settings-production-tests.log`。
