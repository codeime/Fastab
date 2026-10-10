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

- 不改 `fterm` 一行 scrollback、两 worker、行缓存上限或 `fastab_util`/IME 的依赖边界。
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

## 2026-10-08：原生视图被保留时的关窗释放

**新增现场。** 安装版 `a343180a` 的另一个冷启动进程（PID 62233）实测为 **23.2 MiB**，补全引擎尚未初始化。打开并关闭设置后，`GPUIWindow` 已消失，但两个 `GPUIView` 和 `CAMetalLayer` 仍在；引擎计数始终为零。引用图确认 `NSNotificationCenter → block → strong capture GPUIView`。它指向 AppKit 的视图通知机制，尚不能确定具体通知名或通知未解除的原因，不能直接给系统 observer 添加注销代码。

这与上一节 PID 38134 的无窗口/无 layer 现场不同。取样工具额外激活过设置，本次诊断进程升至约 95 MiB，不能把它当作用户原先 40 多 MiB 的等价复现，也不能由该数推导修复后的固定内存目标。

**释放缺口。** `GPUIView` 的 ivar 持有 `Arc<Mutex<MacWindowState>>`，而后者持有完整的 renderer。原来的 `MacWindow::drop` 调用空的 `MetalRenderer::destroy()`，实际释放要等到视图 `dealloc`。前一轮缓冲池修复也在 renderer 的 `Drop` 上，因而无法覆盖“窗口已关闭、视图仍被系统持有”的路径。字体和系统缓存的应用级生命周期是另一项成本，不与这条缺口混为一谈。

**实现与验收范围。**

1. 以 GPUI 逻辑窗口销毁作为图形资源释放边界，关闭后即使原生视图仍存活，也不能继续持有 renderer 和绘图 surface；先标记关闭并取出资源，再在窗口状态锁外调用可能重入的 AppKit 清理。
2. 已排队的绘制、尺寸、焦点、输入和拖拽回调遇到关闭状态应终止；回调执行期间发生关闭时，不能在返回后把 handler/callback 或 display link 写回旧窗口。
3. 已提交的 GPU 命令保持资源有效直到执行完毕，旧 generation 的 buffer 不重新进入共享池；其他活窗口和重新打开的设置继续正常绘制。补全窗口的 `orderOut` 隐藏不等于销毁。
4. 验证主动保留已关闭的原生视图这一场景，做同配置旧版/修复版对照，以 layer 解绑、真实 GPU 对象与 surface 回收为主要证据，footprint 作为辅助。再做多窗口压力、产品回归和独立 GPT-6 Astra ultra review/fix。原生探针不替代新安装包的真实终端验收。

**已实施与 review/fix。** `MacWindowState.renderer` 改为可取出的资源；`MacWindow::drop` 先标记关闭、取出 renderer、display link 和所有回调，再解锁、停止帧回调、清除 first responder、拆除 layer 并移出视图，立即释放 renderer。关闭状态会阻止迟到回调访问原生窗口或恢复 handler；拖拽循环在窗口关闭、状态弱引用失效时退出。独立审查发现“原生回调内部关窗，拆除视图后可能过早释放接收者”的风险，修为将空视图保留到延迟 native close 结束，GPU 资源仍在当前关闭流程释放。最终 GPT-6 Astra ultra 复审没有剩余可操作发现。

**真实原生对照。** 临时探针调用生产设置 open/close，显式保活每个已关闭的 `NSView`，但不持有其 window/layer；另一个补全窗口持续绘制。旧版与修复版都使用同一 debug 构建配置和 `MTL_DEBUG_LAYER=1`，每轮关闭并刷新存活窗口后等待 2 秒再取样，3 轮累计请求 180 次补全窗口刷新。

| 关闭设置并继续保活旧视图 | 旧版 footprint | 修复版 footprint |
| --- | --- | --- |
| 第 1 轮 | 71.0 MiB | 40.6 MiB |
| 第 2 轮 | 95.4 MiB | 41.3 MiB |
| 第 3 轮 | 122.4 MiB | 41.5 MiB |

第 3 轮两组都仍有 4 个 `GPUIView`（3 个故意保留的旧设置视图和 1 个活补全视图）。旧版有 **4 个 CAMetalLayer、79.2 MiB IOSurface resident**；修复版只有活补全窗口的 **1 个 CAMetalLayer、5808 KiB IOSurface resident**，与该探针补全窗口的基线相同。旧版的关闭断言失败 6 次，修复版为 0；存活补全窗口和重新打开的设置均保持有效绘图层。这是关闭后的图形资源回收证据，**不是完整安装版空闲 20 MiB 的承诺**。

**其余验证。**

- 4 项真实 Metal 测试通过。新增共享事件控制已提交命令，关闭 renderer 时 GPU 尚未完成；放行后验证真实写入结果与旧 buffer 不回池。失败时 RAII 与独立 500ms CPU 截止机制会放行事件，避免测试自身卡住 GPU；截止机制抢先放行会使测试失败。
- 4 轮原生 `displayLayer:` 回调内关闭自身窗口通过，交替使用外部保活/不保活接收者。每轮确认 GPUI handle 移除、下一帧闭包捕获释放、该帧不再执行；保活视图再收到绘制和 backing-properties 回调后也没有恢复 Metal layer。该探针不声称覆盖 IME 输入 handler 内关闭。
- 生产入口恢复后，`fastab_desktop` **213**、`fastab_gpui` **109** 项测试通过。验证使用前述临时副本，保留原工作区不相关未跟踪内容；没有替换已安装应用或重启输入法。
- 两个产品 crate 的 `cargo clippy --locked --all-targets -- -D warnings`、workspace `cargo fmt --all --check` 和 `git diff --check` 通过。
- 证据：`/tmp/fastab-retained-{baseline,fixed}-evidence/`、`/tmp/fastab-retained-callback-evidence/run.log`、`/tmp/fastab-retained-metal-tests.log`、`/tmp/fastab-retained-production-tests.log`。探针源为 `/tmp/fastab-settings-retained-view-probe.rs`、`/tmp/fastab-settings-callback-close-probe.rs`。
## 2026-10-08：关联资源生命周期的复核与修复

本轮覆盖上一轮审计确认的五条路径，不以进程总占用下降多少作为单一验收标准；分别检查对象引用、缓存边界、纹理索引和迟到任务。安装版与 IME 不参与探针，所有原生实验在临时验证副本运行。

1. **帧刷新订阅。** 原实现每次重建窗口 display link 都遗留一个 `CVDisplayLink` 和一个 dispatch source，之前三轮开关设置的采样从 3 个增加到 9 个。直接删除 `mem::forget` 并不安全：上游 [Zed #60696](https://github.com/zed-industries/zed/pull/60696) 记录了 `CVDisplayLinkStop` 与系统后台线程退出之间的竞争。采用每显示器复用一个进程生命周期时钟、窗口单独订阅的方案；退订后取消并释放窗口 source，创建失败、启动失败和重复停止都必须闭环。验收重点是反复开关后的对象数稳定，以及没有窗口订阅时停止计时；不是承诺系统时钟对象零常驻。
2. **动态文件图标。** 旧 64 项限制只约束路径到 PNG 的缓存，GPUI `App.loading_assets` 仍保留不同 PNG 内容的解码结果。原生探针加载 96 份不同 PNG 后，关闭窗口并删除全部源图标，仍有 96 个解码图像；显式移除 asset 后降至 0。改用补全模块直接持有的有界解码缓存，避免落入全局 asset cache；相同内容共用图像，并保护当前建议批次。淘汰时必须先更新绘制内容，再清理旧图像的 atlas 条目；隐藏路径也须覆盖。
3. **Metal 图集删除。** 两个 key 共用纹理时，原 `remove(A)` 减少计数却保留 A 的索引；再删除 B 后，A 指向已释放的槽位。修为先移除映射，且每个 key 只减少一次计数。验证共享纹理、重复删除、重建和真实纹理内容；不立即复用单个 tile 区域，避免覆盖 GPU 在途读取。此前产品没有主动调用 `drop_image`，这属于图标回收启用前必须修正的底层缺陷。
4. **原生文本查询。** `NSTextInputClient` 返回的 attributed substring 使用 +0 约定，原 `alloc/init` 少了 autorelease；其可选 `actualRange` 输出也可能为空。修复释放、空指针处理和输出赋值，并清理相同模式的显示器编号与 tabbing identifier 字符串。Foundation 测试直接读取 UTF-16 范围，并比较 autorelease pool 排空前后的引用计数；不访问真实用户输入。`bounds_for_range` 接口没有调整范围输出；firstRect 在调用方请求 `actualRange` 时，先用既有文本查询取得规范化范围，再查询几何位置并回填相同范围，避免越界或 UTF-16 中间边界被截断后仍回填原请求；未扩展公共接口。
5. **权限引导旧任务。** 关闭 A 后立即打开 B，旧 timer 仅检查一个 active 布尔值，会误认为自己仍有效。等待窗口与周期定位任务都携带会话 epoch，旧任务不得续订；Grant 按钮只通过 `repair()` 启动引导，去掉第二个排队启动。测试只操作独立生命周期状态，不打开系统设置、不修改 TCC。

每项实现后做定向回归，再以真实设置窗口压力、Metal API validation、产品测试和独立 GPT-6 Astra ultra 审查收尾。图标缓存问题和小型 native 对象泄漏不等于首次打开设置的全部内存差额；字体、系统框架及分配器仍可能保留稳定的应用级开销。

**实施与 review/fix。** 上述五项已落地。动态图标路径与内容各限 64 项；PNG 输入最多 4 MiB、1024×1024，解码分配预算 8 MiB，保留像素最长边 64。不能把 NSImage 的 32 点大小当成 PNG 像素尺寸保证。隐藏 10 秒后清理动态图集，只保留仍可由 Tab 恢复的有界建议行；清空行后全部解码图像释放。

原生图标探针补出了静态审查遗漏的重入问题：typed `WindowHandle::update` 会先借出列表根实体，再调用 `Window::draw` 会二次借用它。生产回收路径现改为 `AnyWindowHandle::update`，只借窗口，重建 scene 完成后再删除 atlas。独立 Astra 复审重新核对了根实体、图标缓存实体、建议状态与窗口锁的借用边界。

**已验证的运行证据。**

- 原生图标探针实际使用生产缓存和 overlay，12 批 × 8 张不同 PNG：存活解码对象从 8 增至 64 后保持 64；清空行、隐藏并等待真实 10 秒任务后为 0；再次显示 8 张并收到原生帧回调。`MTL_DEBUG_LAYER=1`，`RESULT failures=0`，日志 `/tmp/fastab-file-icon-runtime-fixed.log`。
- 10 轮原生 Settings 开关，主动保留每轮关闭后的 NSView，另一个补全窗口持续刷新。各采样点 `CVDisplayLink` 总数恒为 2、`CAMetalLayer` 恒为 1、IOSurface resident 恒为 5808 KiB；释放保活引用后 `GPUIView` 回到 1。该测试保留了活跃补全窗口，不能当作无窗口空闲占用。
- 为区分应用订阅与系统 source，第二轮只在临时验证副本为 source 注册/finalizer 加计数：共注册 **23** 个，**23** 个均执行 native finalizer。每次关设置后剩 **1** 个补全窗口订阅，最后关闭补全窗口后为 **0**。证据 `/tmp/fastab-resource-ten-cycle-instrumented-evidence/summary.json`，`RESOURCE_RESULT success=true`；计数代码未写入产品。
- 全进程 dispatch source 仍从基线 67 变为释放保活视图后的 83，因此没有把该总数写成“所有系统对象都已回收”。已证明 Fastab 帧订阅完全释放，其余变化不由这组计数归因。带计数的 debug 探针，第 1/5/10 轮关窗 footprint 约为 40.1/40.8/41.8 MiB，不承诺安装版回到 20 MiB。
- 7 项真实 Metal 测试通过，覆盖 renderer 缓冲池、共享纹理删除、96 轮纹理槽复用，以及已提交 GPU 读取跨 atlas 删除/替换后仍返回旧像素；另外 5 项 GCD/registry 生命周期和 2 项 Foundation 测试通过。
- 产品回归首轮 desktop 216、fastab_gpui 109、macos-utils 34 项，共 359 项通过；三个 crate 的 all-targets Clippy `-D warnings` 通过。扩大到上游其他 macOS 测试时，未修改的 clipboard 测试因沙箱不可访问 AppKit 服务而中止；未扩大权限重跑会修改剪贴板的无关测试，针对本轮改动的验证单独完成。

**最终复审。** 图标重绘借用与 firstRect 范围回填均经过修复后的独立 Astra 复核，没有剩余可操作发现。图标 4 项回归、Foundation 2 项回归再次通过；最终产品 359 项回归、Clippy、格式检查及正式入口构建通过。验证副本的 Rust 源码、manifest 与 lockfile 已逐字节核对，生产二进制不含临时探针入口或 source 计数标记。最终日志为 `/tmp/fastab-resource-final-tests.log`、`/tmp/fastab-resource-final-clippy.log`、`/tmp/fastab-resource-final-fmt.log`、`/tmp/fastab-resource-production-build.log`。

以上仍不替代新安装包在真实终端、外屏切换/拔插、睡眠唤醒、输入法组合输入及权限引导 UI 上的验收。本节验收完成时尚未安装或发布；随后用户授权按内容提交、推送并更新同一 `v0.0.5` 标签触发重打包。版本号保持不变，标签更新以远端旧引用的精确 lease 保护。


## 2026-10-08：安装后残留与 Tab 路径的再次深入检查

**现场证据。** 安装版 PID 33758 的 `phys_footprint` 为 46.5 MiB、峰值 54.5 MiB，AX 权限为 true；当前输入为空。`ftab _ dump-state engine` 的规格文件数、idle 文件数与规格估算字节均为 0，仅一条 216-byte 脚本输出和约 6.6 KiB 历史数据。原生采样仅有一个 `GPUIView`、一个 `CAMetalLayer` 和一个 `CVDisplayLink`；IOAccelerator graphics resident 为 9168 KiB，IOSurface resident 为 1600 KiB。因此这次占用不能归因于已关闭 Settings 的渲染器累积，也不能当成规格未释放。证据为 `/tmp/fastab-installed-33758-heap.txt`、`/tmp/fastab-installed-33758-vmmap.txt`。安装包仅报告 0.0.5，构建 hash/date 均为空，不能仅凭版本号证明某次同版本重打包的 SHA。

**历史补全首次加载缺口。** `history_only` 会为了 fuzzy 策略读取规格；原实现只清除 touched，worker 又不把历史请求设为活动资源所有者。因此冷 Engine 第一次只查询历史时，新树既没有 deadline，随后的 EndInput 也不能启动回收。独立真实 bundle 回放中，100 ms 测试宽限后仍有两份缓存、零份 idle 和 null deadline。修复复用 idle checkpoint：已有路径的 active/idle 状态和旧时间原样恢复，仅给此次新加载路径记返回时间；仍不接管普通输入 owner。

修复后，默认 10 秒真实 `gcloud compute` 回放的 cached/idle 为 2/2、deadline 约 9999 ms；EndInput 后等 11 秒归零，独立 debug 进程 footprint 54.2→26.7 MiB。跨会话回放为 A 普通 compute 2/0 → B 历史 git 3/1 → End B 后 2/0 → End A 后 0/0，证明 B 不会释放 A 的活动树。两次均只有一次 Engine 初始化，无失败、watchdog 或 panic；不是安装版占用承诺。证据 `/tmp/fastab-history-only-default-grace-fixed.log`、`/tmp/fastab-history-only-owner-fixed.log`。新增小规格真实加载、Weak 引用释放与 worker 定时回归，引擎全量 482 通过、2 项大 fixture 忽略。

**Tab 行为与协议。** 现场无 `autocomplete.keybindings` 覆盖。旧默认 `insertCommonPrefix` 在候选没有更长公共前缀时只 shake；这是既有行为，未证明属于本轮回收回归。新默认使用已有 `insertCommonPrefixOrInsertSelected`：可扩展时先补公共前缀，否则接受选中的普通候选；多候选中的执行型条目保留保护。独立审查发现桌面 `DEFAULT_OVERLAY_BINDINGS` 会通过 `override_actions=true` 覆盖终端初始映射，因此两端同时修改；既有用户覆盖仍排在默认之后。默认一致性与覆盖顺序纳入现有回归。

另一个可复现的条件缺陷是实际收到 CSI-u `ESC[9u` 时，解析结果为 `Char('\t')`，真实 KeyInterceptor 无法匹配 Tab。仅规范化功能码 9/13/27/127 为 Tab/Enter/Escape/Backspace，保留修饰键、原始字节及其它控制字符的既有区别；CSI-u 模式仍使用原有重编码转发，不能声称所有模式逐字透传。parser→interceptor 回归在旧代码上明确失败，新代码通过。现场 CSI-u 配置未开启，因此没有把此缺陷认定为用户此次现象的根因。


**隐藏窗口资源回收。** 原先 `orderOut` 只隐藏窗口，renderer、图集、路径纹理和共享实例缓冲仍常驻。现在先立即 park，隐藏满 10 秒后作废排队的原生定位请求、通过 `AnyWindowHandle` 移除窗口并清空 handle；保留同一个 OverlayState 和当前行引用的小图，下一次显示再创建。显示/新结果取消旧 timer，随后隐藏重新获得完整宽限；没有动态图标也启动回收。独立复审又发现隐藏态残留的 `current_arg` 会让 caret/relayout 重建窗口却不再计时，已将这些创建入口限制为 visible，隐藏时仍更新 last_position。虚拟时钟覆盖期限重置、小图归属和隐藏参数提示不重建。连续隐藏通知的延期缺口在 2026-10-09 节修复。

同一原生 Controller 探针对照均启用 `MTL_DEBUG_LAYER=1`；主动持有旧 NSView，覆盖无动态图标、保留图片的行、设置同时打开、真实帧回调、隐藏后的 caret 更新、Tab 恢复及 `insertCommonPrefixOrInsertSelected` 到 Figterm InsertText。两版最终均 `RESULT failures=0`。

| 隐藏 11 秒后的状态 | 修改前 | 修改后 |
| --- | --- | --- |
| 无设置窗口的 footprint | 26.58 MiB | 17.38 MiB |
| 无设置窗口的 Metal layer / IOSurface resident | 1 / 1120 KiB | 0 / 0 KiB |
| 设置仍打开时的 footprint | 61.64 MiB | 55.41 MiB |
| 设置仍打开时的 Metal layer | 2 | 1，设置继续收到真实帧回调 |

原生同步重建阶段约 17.5–18.4 ms，修改前重用约 0.8–2.1 ms；这不是端到端按键延迟。最终关闭所有窗口并释放探针保活引用，两版 GPUIView / CAMetalLayer 都为 0；footprint 约 26.55 / 27.05 MiB，仍有应用级字体、框架和分配器开销。探针是 debug 独立进程，不能承诺当前安装版必回到 20 MiB。首次探针基线因设置打开时序漏到一次帧回调而未通过；修正为先稳定 overlay、单独打开/聚焦 Settings 后，两版使用完全相同的动作及断言重跑通过，没有放宽断言。证据 `/tmp/fastab-idle-overlay-{baseline2,fixed2}-evidence/`、`/tmp/fastab-deep-idle-window-comparison.json`。

**最终验证范围。** 引擎 482、desktop 218、GPUI 109、term/settings 77，共 886 项通过；原有 3 项忽略测试未计入。engine/term/settings 生产目标 Clippy `-D warnings` 通过，desktop/GPUI/term/settings 的 all-targets Clippy 通过，格式及 diff 检查通过。扩大 engine all-targets 时有 4 个既有测试辅助代码 lint（大枚举、Option<Option>、默认值后赋值、已打开安全文件的 read_to_end），本轮未修改这些无关辅助实现。根 workspace 的无关未跟踪 `crates/fig_input_method` 缺少 manifest，验证在源码一致的临时副本中完成；首次引擎运行因副本的路径/脚本符号链接使 4 个基线测试失败，补齐真实文件并使用规范化路径后全量通过。验证过程中磁盘不足，只清理了可重建的 Cargo debug incremental cache；未删除源码或未跟踪工作。正式 main、manifest 与 lockfile 已恢复，生产入口重新构建，探针和临时计数未进入产品。本轮未安装、提交或推送。

**追加审查修复：Tab 回退的执行保护。** 后续只读复审发现，直接将默认 Tab 绑定到已有回退动作，会绕过多候选 `auto-execute` / `special` 的公共前缀保护：显示执行建议时，`git status` 等精确匹配的置顶行带有 `\n`，回退接受会执行当前命令。现场当前隐藏执行建议，掩盖了这个默认配置下的回归。修复仅限制公共前缀失败后的回退接受，判断当前选中行；普通行仍可回退，拒绝的动作行保持列表和选择并提供 shake 反馈。保留此前单候选 `Full` 接受、显式 Enter 和 execute 的行为，没有修改通用插入函数。

新增 headless GPUI Controller → FigtermCommand 回归，实际检查插入消息，覆盖两种动作类型、非零选择索引、同一列表随后按 Enter、单候选只插入一次、混排普通候选、前缀扩展与回退、空列表和显式 execute。测试不连接用户 PTY，也不写用户接受记录。旧 handler 明确红测，修复后通过；desktop 全量更新为 219 项通过，desktop all-targets Clippy `-D warnings`、格式及 diff 检查通过，测试源码与工作区逐字节一致。Astra 独立复审未发现剩余可操作问题。证据为 `/tmp/fastab-tab-action-guard-{red,green,desktop-tests,clippy}.log`。本次未重跑无改动的引擎/GPUI 全量或原生内存探针，也未安装、提交或推送。

## 2026-10-09：隔夜空闲的日志常驻与重复隐藏延期

**安装版证据。** 用户确认是主进程，输入已清空或命令已执行完，设置已关闭。PID 94109 的物理 footprint 在约五分钟采样中维持 48.7 MiB，峰值 82.3 MiB。已将安装二进制 SHA-256 与本次 `v0.0.5` Release DMG 对照，并核对运行进程 Mach-O UUID，确认来自 `c7709fae` 的成功打包；不是只凭相同版本号推断。取证未重启、替换或安装应用。

本次 `GPUIView`、`GPUIPanel`、`GPUIWindow`、`CAMetalLayer` 数量均为 0；规格缓存为 0，历史约 6.8 KiB，脚本输出共 389 bytes，失败/watchdog/panic 均为 0。当前窗口与规格已经释放。live heap 约 24.7 MiB，malloc dirty/swap 碎片约 12.8 MiB，两者不能混同为可立即回收的泄漏；不能据此承诺完整桌面进程回到 20 MiB。证据：`/tmp/fastab-overnight-94109-{heap,vmmap,footprint,sample}.txt`。

**修复一：减少空日志队列的固定占用。** 两块各 4,096,000 bytes 的存活分配，经引用链归属到文件和 stdout 的 `tracing-appender` 队列。锁定的 `tracing-appender 0.2.5` 默认每队列 128,000 槽，`crossbeam-channel 0.5.15` 在创建时初始化全部槽位，arm64 每槽 32 bytes。双队列即使无日志也保留 7.8125 MiB，不是隔夜不断增长的对象。

共享 `non_blocking_writer` 将每路容量设为 4,096，双队列固定槽位变为 256 KiB，静态差值为 **7.5625 MiB**。选择 4,096 保留一定突发余量；继续使用 `lossy(true)`，保留双输出及各自 WorkerGuard 的退出刷新。慢输出或日志突发时会更早丢弃日志，不能切成阻塞模式而卡住 UI/PTY。容量限制消息数量，不能冒充 payload 总字节上限。公共日志库的修改也影响启用日志的 CLI 和 PTY。

此前独立探针对比两路默认 128,000 与 1,024 容量，三轮 footprint 差值为 7.781–7.797 MiB，证明固定槽位的影响；该探针早于禁止本地构建的要求，且 **1,024 的测量不是本次 4,096 产品补丁的安装后实测**。证据：`/tmp/fastab-log-queue-footprint-probe-results.txt`。新增阻塞 sink 的真实队列回归：首条取出后阻塞消费，4,097 次写入应及时返回并恰好丢弃 1 条；发生超时也先释放 sink，再回收生产线程。

**修复二：连续隐藏不能无限延期。** `complete_buffer_inner` 在重复输入去重前处理空输入并 dismiss；PTY 的异步提示符重绘可能每隔不到 10 秒重复通知。原 `schedule_idle` 每次重启计时，使隐藏窗口长期留存。这个条件缺陷不能解释本次原生窗口对象已经为 0 的现场，但需要单独修复。

已有 idle task 时继续使用原截止时间；真正 show 或新结果 batch 仍取消旧 task，随后隐藏获得新的 10 秒宽限。到期先核对 generation，再取出并 detach 当前 task，避免取消正在执行的回调；之后清理隐藏窗口和不再被候选行引用的图标。即使 state 消失或已经 visible 而提前返回，也不会留下占用任务槽的完成句柄。虚拟时间回归覆盖第 6/9 秒重复隐藏仍在第 10 秒回收、取消后重开、正常到期后的第二轮、visible 与 state 消失两种提前返回。

**本轮验证边界。** 按用户要求先删除项目 `target`（约 43 GB）、`build`、`proto/dist`、`website/dist`、`bundle/specs-ir` 和此前的临时验证副本；磁盘可用空间从约 6.3 GiB 回到 47 GiB。保留源码、依赖存储及无关未跟踪工作。`AGENTS.md` 与 `CLAUDE.md` 记录禁止本地构建；本轮新增回归未编译、未执行，之前章节的测试通过数不能用于本补丁。仅做源码/依赖审查、直接 rustfmt 检查和 diff 检查；编译、可执行回归与新安装包的实际 footprint 验收留给远端 CI 和后续安装验证。

后续远端验证运行 `cargo test -p fastab_log`、`cargo test -p fastab_desktop overlay::file_icons::tests` 及相关目标的严格 Clippy。安装新包后复测主进程 `phys_footprint`、两个大日志分配是否消失，以及空提示符每隔不到 10 秒重绘时窗口是否仍在最初期限后销毁；同时验证重新显示能够取消旧期限并正常恢复候选。新增虚拟时钟测试使用空窗口槽验证共享回收任务与图像引用，不代替原生窗口验收。

**静态复审结果。** 日志专项复审及 GPT-6 Astra ultra 整体独立复审未发现可行动的 P0–P2 问题；已核对真实调用链、锁定依赖的队列/Task 语义和测试超时退出路径。直接 `rustfmt --check` 与 `git diff --check` 通过，缓存目录保持不存在。此结论不扩大为编译或安装验收通过。

## 2026-10-09 追加：隐藏竞态与其他常驻路径

上述现场已经没有原生窗口，后续代码审计发现的五条路径不能都归因于当时的 48.7 MiB。现场 hook 数据只有 389 bytes，旧接受偏好索引只有 659 bytes，磁盘上的 AI 配置未启用；后三项主要消除其他输入、长期使用或慢挂载下的增长条件。

### 1. 缺失光标时的迟到 Show / Tab

`clear_caret_position` 已隐藏窗口并开始宽限，但同会话已经在队列中的显式 Show 或多候选 Tab 仍可将 `visible` 设为 true。`ensure_window` 随后取消 idle task，而布局因没有光标只能 park；布局重试也要求有光标，于是留下不可见却不再回收的窗口。

`show_kept_items` 先检查光标；缺失时继续隐藏，解除显式显示抑制并记为 `WaitingForCaret`，保持原始释放期限。有效光标到达后走已有的一次性重新补全；单候选 Tab 的接受分支和多候选执行保护不变。回归源码驱动实际 Controller 与 EngineClient：第 6/9 秒重复光标失效及 Tab，第 10 秒仍回收空缓冲区，期间不提交引擎请求，有效光标连续到达两次仅完成一次补全。headless 测试不创建原生窗口；窗口销毁仍需安装版验收。

### 2. Hook 缓存的字节预算与空闲释放

建议、脚本 stdout 和生成规格三张缓存原先各限 512 项，但没有字节上限；脚本单次最多 256 KiB，单张缓存理论上可保留约 128 MiB。TTL 由读取者传入，过期项只在下次访问相同 key 时删除，`EndInput` 原先只让规格进入空闲，不能保证 hook 结果释放。

每张 map 保留 512 项上限，并增加 **4 MiB 估算 payload** 预算。计费包含 key、值结构和拥有的缓冲区；String 与建议 Vec 计入容量，生成规格复用已有树估算，未包含全部 HashMap 桶、allocator 开销和规格字符串备用容量，不能称为进程或精确 heap 上限。超大结果正常返回但不存入缓存；替换先移除旧值，不能在拒绝超大新值后继续命中旧值。预算溢出沿用原先整张清空策略，空缓存释放 map 容量，累计诊断计数保留。

Engine 独立记录 hook 的 active/idle 状态，与规格共用 10 秒宽限，worker 等待两者的最早期限，即使没有规格文件 deadline 也能回收。正常成功请求获得活跃归属；`EndInput` 只为首次空闲计时，重复结束不延期。history-only、失败和取消不获得活跃归属，也不延长已有非空缓存的期限；首次无主请求留下缓存时，从该请求返回开始获得完整宽限。保留既有会话 owner 规则，其他终端的 history 或被取消提交不能吞掉原 owner 的结束通知。TTL 查询语义不变，空闲释放后下次按需重算。

回归源码覆盖三类超大结果、替换与字节总账、String 备用容量、清空 map 容量、无规格期限时的 worker 唤醒、首次取消留下部分缓存、跨会话 history/取消与已开始的期限。

### 3. 旧接受偏好索引

旧 `AcceptanceIndex` 按 root command / name 永久累积，并在启动加载及每次补全快照中整体持有。保留原 JSON 结构和 root 隔离，限制为 **2,048 项、256 KiB 估算字节**，root 最多 256 bytes、name 最多 1,024 bytes（trim 后按 UTF-8 字节计算）。计费为保守的 map/string 元数据和 JSON 转义后长度，包含时间戳与标点余量；这是逻辑存储预算，不是精确 heap 测量。按目录和参数位置学习的 scoped 索引保持独立。

加载仍需通过现有 state API 读取旧 JSON，但裁剪过程消费旧 map，不再克隆整份无界索引；候选集合最多保留 2,048 个唯一 key，优先较新记录。trim 后的重复 key 必须先合并最高 timestamp 再计数，避免大量空白变体挤掉其他有效偏好。重建后释放旧 map 容量，仅在裁剪或规范化改变数据时 best-effort 写回迁移。预算不足的新条目先预检，不能先删掉部分旧项再发现仍放不下；旧 timestamp 不覆盖或驱逐较新项。`record_at` 对有效旧事件的 true 返回值仍保留：前台会先录入同一个事件，后台重放依赖这个返回值持久化，不能为了省一次空写而漏掉全部正常保存。

回归源码覆盖计数上限、字节上限与 JSON 转义、UTF-8 字段边界、root 隔离、旧事件重放、拒绝大旧项前不破坏索引、旧 JSON 迁移、规范化重复 key，以及再次加载无需重复迁移。

### 4. AI 仓库文件系统探测

原 `tokio::fs` 探测虽受 200 ms 外层 timeout 限制，取消等待无法停止已经提交的 blocking I/O；限制 Tokio worker 数量也不限制其排队任务。增加独立的单许可 gate，在提交前 `try_acquire_owned`，将许可移入实际 `spawn_blocking` 闭包，直到 canonicalize、目录检查及父目录 `.git` 查询实际返回才释放。忙时本次上下文标记为不完整，不继续排队；不能终止一个已经卡住的系统调用，但它至多占一个探测槽。

取消与超时回归源码使用可控阻塞闭包，验证取消等待后许可仍被持有、随后请求不会启动，实际工作结束后恢复。测试用独立 semaphore；使用全局 gate 的已有集成测试串行隔离，避免互相触发忙状态。

### 5. 空候选与解析字符串的备用容量

`OverlayState::dismiss` 清空内容但保留 Vec / String 容量；一次大历史列表结束后仍可常驻几 MiB。隐藏回收任务到期且确认 state 隐藏后，释放空 `items` 和四个空解析字符串的 capacity。非空的保留候选及上下文继续支持 Tab 恢复，不在每次输入时 shrink。虚拟时间回归检查 9 秒时仍保留、10 秒归还空容量，以及非空行、图标和参数提示仍保留。

### 暂时隐藏 AI 设置

`AI_SETTINGS_VISIBLE = false` 隐藏侧栏入口，原 AI/Jev 设置路径回到外观页；AI Entity 改为可选并延迟创建，隐藏时打开设置不启动该页面的凭据读取或保存任务。关闭、切页和权限页的调用均兼容空 Entity。已保存的配置、启用状态、凭据与同意记录保持原值；这是设置入口变更，不是关闭已启用的运行时 AI。

### 追加修改的验证边界

本轮继续遵守禁止本地构建：只做源码及依赖审查、直接 rustfmt 和 diff 检查，新增回归仅为源码，未编译或执行。远端 CI 需要运行 engine、desktop、GPUI、log 的相关回归及严格 Clippy；安装后再验证隐藏/恢复、Tab、设置路由和真实 footprint。现有安装版不能验收这些未打包改动，也不能把早期章节的测试结果当成本轮通过记录。

**追加静态复审结果。** GPT-6 Astra ultra 分工实现并交叉审查，主代理核对实际 diff。复审修正了两处问题：旧偏好迁移在去重前限数会误删其他有效项；一个 worker 回归 fixture 缺少 `splitOn` 却断言产生候选。修正后独立复核未发现剩余可确认的 P0–P2。13 个修改 Rust 文件的直接 `rustfmt --check` 与 `git diff --check` 通过，五个已清理的构建缓存目录仍不存在；没有进行本地编译、测试、安装、提交或推送。首载历史 JSON 的临时峰值及一个不可取消的系统 I/O 不在这些内存预算的硬保证内。

## 2026-10-09 追加：无窗口时的应用级 UI 缓存

**基线与目标。** 用户要求继续减少空闲内存。安装版 PID 9411 的 `phys_footprint` 为 43,222,144 bytes（41.22 MiB），后续采样约 41.3 MiB；live heap 约 17.6 MiB，malloc 的 dirty+swap 碎片约 12.1 MiB。规格和三类 hook 缓存均为 0，历史为 7,116 bytes，GPUI 窗口与 `CAMetalLayer` 对象为 0。安装二进制与 `2d569013` 发布包的 SHA-256 相同，运行进程 Mach-O UUID 也与磁盘文件一致。旧两块 4,096,000-byte 日志队列分配已消失；这不是此前补丁未安装。证据保存在 `/tmp/fastab-idle-9411-{footprint,vmmap,heap,sample}.txt`。

本轮目标是回收无窗口时可重建的应用级缓存，不把全部碎片当作可归还页，也不承诺进程降至 20 MiB。内置 42 个 PNG 源共 73,614 bytes，解码像素合计 194,276 bytes；尺寸已经不超过 64×64，额外缩图没有收益。公开 `malloc_zone_pressure_relief` 在此前系统版本没有建立收益，本轮不加入未经本机新实验验证的强制回收调用。

**实现步骤。**

1. GPUI 提供 `App::release_idle_caches`，以内部窗口槽位集合为空为前提。正在执行 `window.update` 时，窗口临时取出但槽位仍在，必须拒绝回收，不能仅统计当前可取出的窗口对象。
2. 释放应用级 `loading_assets` 的任务/结果引用及 map 容量；外部消费者仍可完成已借出的任务，旧任务完成不回填缓存。下次绘制按原流程异步加载，不把图像解码搬到前台同步路径。
3. 释放字体度量、字形边界、换行器及字体 run 临时缓冲。保留字体标识及平台字体表，避免旧 `FontId` 和布局失效；临时池使用代际隔离，旧借用者晚归还时释放，不能重新填满已回收的池。
4. 主程序注册最后窗口关闭观察者，通过 `cx.defer` 等待旧 Window/场景真正析构后再次核对窗口集合，再回收 GPUI 缓存和内置图标的复制源。设置窗口仍开着时不执行；补全窗口沿用原有隐藏 10 秒后销毁的期限，不额外增加一轮等待。回收与重新打开之间若有新窗口插入，由最终门控拒绝清理。
5. 返回实际回收的条目数与临时缓冲容量，作为调试计数；不把这些数值称作释放的物理内存。

**验证要求。** 在现有 `fastab_gpui` 工作区测试中覆盖：资产引用释放和重新加载、旧异步结果晚到不覆盖新缓存、活窗口及 update 中的槽位阻止回收、最后窗口关闭后的自动延迟回收、旧换行器跨代继续使用/归还及字体标识稳定。不能仅测试独立的模拟 cache。提交后的远端 CI 负责实际编译和执行；本机仍禁止构建。安装候选包后需按相同的设置打开/关闭、补全输入/清空流程对比 10 秒与 60 秒的 footprint、live heap、缓存计数及首次恢复延迟，本轮源码修改尚不能证明具体 MiB 降幅。

### 引擎持有的 hook 描述目录

追加审计发现 `Engine.native → NativeHooks.catalog → RuntimeTypedHookCatalog → SharedDescriptor` 在规格与三类结果缓存归零后仍保留。每个 descriptor 同时拥有 raw JSON 及首次求值后初始化的 `OnceLock<TypedHookIr>`；它不是进程全局的静态 OnceLock，但 Engine 长期存在，因此目录也不会自动卸载。安装版侧车大小为 4,760,313 bytes，3,136 个 typed ID 在现有加载器内已按相同 raw bytes 去重为 459 个 descriptor，去重正文为 583,660 bytes，另有 565 个 adapter 绑定。不能把整个文件大小当作实际常驻 heap，已解析 IR 的大小还取决于使用过哪些 hook。

保留原来的目录格式和解析方式，把 `Engine.native` 改为可卸载的 owner。目录复用 hook 结果的活跃输入归属，但有独立空闲期限：即使没有规格或结果缓存，worker 也会在首次结束后的 10 秒到期唤醒并释放目录。重复结束、history-only 和取消请求不推迟已有期限；首次构造或刷新后尚无活跃请求的目录也必须获得期限，避免请求在早期取消检查返回后永久常驻。

后续正常补全在 attempt 内、规格代际刷新之后重新加载，仍通过 `Registry.snapshot()` 验证侧车字节，保留 digest 不匹配时拒绝旧代数据的行为。缺失/拒绝的侧车与尚未加载的 owner 分开表达，避免每次按键都重试坏目录。正常活跃会话阻止其他会话的 history/cancel 触发回收；watchdog 已丢弃的旧 attempt 保留自己的 Arc 到工作结束，不能强行清掉它仍在使用的数据。

`ftab _ dump-state engine` 新增 `hook_catalog`：是否尝试加载、是否成功加载、typed/adapter 条目、去重 descriptor 及已解析 descriptor 数。诊断只读 `OnceLock` 状态，不触发解析，旧 JSON 缺失新字段时使用默认值。重载会读取并解析侧车，仍需远端与安装后的耗时及 footprint 对照；释放 owner 不保证分配器立即归还相同字节数。

**本轮静态验证结果。** GPT-6 Astra ultra 与 GPT-6.1 Sol 分工实现并独立交叉审查，主代理核对实际 diff。复审修正了两条提前取消漏口：首次构造目录、以及代际刷新重新绑定目录后，请求都可能在取得活跃归属之前返回；现在这两处均建立空闲期限。修正后源码复核未发现剩余可确认的 P0–P2 问题。新增 7 项引擎回归及 4 项 GPUI 回归源码；12 个修改或新增 Rust 文件的直接 `rustfmt --check` 与 `git diff --check` 通过，已清理的构建缓存目录仍不存在。本轮没有本地编译或执行测试，也未安装、提交或推送；实际内存降幅及首次恢复延迟仍待远端 CI 和新包安装后验证，41.22 MiB 是修改前的安装版基线。

## 2026-10-10 追加：长期后台对象与按需分配

### 安装版现场

上一轮改动已经随 `588e93af` 发布，CI 和 ARM DMG 工作流均成功。当前安装版主进程 PID 36454 从 10 月 9 日 16:25 起连续运行约 19 小时，二进制 SHA-256 为 `3548c1da2d8ea00c3b7a6bec6d34855e0811a81fb5f7f060cf0c6fe7e2d55bdc`，Mach-O UUID 为 `0A7CE535-298A-3B2E-9ACC-88AA67ACEF3D`。本轮未重启、替换或注入进程。

11:11 的 `phys_footprint` 为 42,534,016 bytes（40.56 MiB），峰值 91.3 MiB，live heap 15,261,824 bytes（14.55 MiB），malloc dirty+swap 碎片约 15.2 MiB。规格、三类 hook 结果、hook 描述目录全部为 0；历史 163 行、7,116 bytes。167 次提交无失败、watchdog 或 panic，堆中没有 GPUIWindow、GPUIView、GPUIPanel 或 CAMetalLayer。上一轮的目录卸载在真实使用后生效；这不是缓存仍保留数十 MiB 的现场。与前一进程的 41.22 MiB / 17.6 MiB live heap 不是受控 A/B，不据此给出固定收益。

证据为 `/tmp/fastab-idle-36454-{footprint,vmmap,heap,sample}.txt`。`leaks --noContent --autoreleasePools` 另存于 `/tmp/fastab-idle-36454-pools.txt`，仅统计对象类型和持有关系，不保存对象内容。

### 实施范围与依据

1. **同步 Objective-C 临时对象的释放边界。** 堆中有 1,055 个 NSRunningApplication；autorelease pool 引用主线程的 122 个、两个 ec-tokio 线程的 468 / 454 个。工具归因于 pool 的独占内存总计约 468 KiB（含 pool 本身），不能解释全部 40.56 MiB，但它是随后台查询积累的真实持有路径。为返回 Rust 自有值的 AppKit 查询建立同步局部 pool；桌面启动 prelude 在主线程 `block_on` 返回时排空，再启动 GPUI。不能让 pool 跨可迁移线程的 async future，也不能把整个 NSApplication 运行期包成一次释放周期。
2. **按进程移除已退出应用的 AXObserver。** 观察者按 PID + bundle ID 建键，旧终止处理却等待同 bundle 所有实例都退出才清理。修复为按通知的具体身份移除，保留其他仍运行实例；这是另一个条件性增长路径，不把当前 NSRunningApplication 数量归因于 AXObserver。
3. **文件数据库连接池按需增长、空闲收缩。** r2d2 0.8.10 的默认 `min_idle=None` 等同 `max_size`，现有 `max_size=4` 仍会预建并补满 4 个连接。文件池保留最大 4、checkout 3 秒、WAL 和 busy timeout，只把空闲目标改为 1、idle timeout 改为 60 秒。reaper 每 30 秒检查，只关闭已归还的连接；全闲约 60–90 秒后回到目标，符合超时的最后一个连接也可能关闭并重建。内存数据库 mock 与维护线程数量不改。SQLite 缓存按需分配，不能把配置上限乘连接数当作实测收益。
4. **路径纹理只在场景需要时创建。** MetalRenderer 原本每次更新 drawable 尺寸都分配整窗 BGRA8 中间纹理及 4x MSAA 纹理，Settings 的普通 div/text 场景不使用它们。改为有效尺寸下首次出现路径时分配，resize 丢弃旧尺寸、后续需要时再建；相同尺寸不重建，保留 4x MSAA 和异步 GPU 资源寿命。820×640、2x 缩放下两张纹理名义容量约 40 MiB，这是可避免的使用期纹理分配，不是当前 idle malloc 碎片的归因或承诺降幅。

当前历史仅约 7 KiB，不卸载整套历史以换取反复读取成本；不缩减数据库维护线程、不改变字体 ID 表、不引入未经验证的 allocator 强制回收。

### 验证要求

对同步查询的 owned 返回值、同 bundle 多实例的观察者移除、临时文件数据库的连接扩缩/在途事务/持久数据增加真实行为回归。应用枚举回归在查询线程退出后使用快照，验证值的所有权，不将它当作已测得 pool 内对象归零；pool 的实际增长趋势仍需安装后检查。数据库回收遵循依赖库真实 reaper，测试不能用自写模拟器代替。Metal 回归需实际设备，按 `vendor/gpui/FASTAB_PATCHES.md` 的显式硬件测试流程执行；现有 workspace CI 不会自动运行 vendored GPUI 的 ignored 测试。

继续禁止本地编译、测试构建、安装和生成 IR；本轮可做源码复核、直接 rustfmt 与 diff 检查。后续远端 CI 和安装验收应同时观察：多次后台焦点切换后 NSRunningApplication/pool 是否持续增长，文件池在空闲后能否收缩，打开/关闭 Settings 的峰值与空闲 footprint，及有路径场景在 resize 后是否正常绘制。释放引用、减少纹理请求和物理页归还必须分别记录。

**本轮复核结果。** GPT-6 Astra ultra 与 GPT-6.1 Sol 独立复核实际 diff，未发现剩余可确认的 P0–P2。复核修正了一处 Metal 回归的证明盲点：编码描述对象原先也能持有旧纹理；现在编码使用内层 autoreleasepool，并在 resize 和打开 GPU gate 前排空，避免它掩盖在途命令的资源寿命问题。5 个修改 Rust 文件的直接 `rustfmt --check`、`git diff --check` 通过，5 个构建缓存目录仍不存在。

验证边界：两条文件数据库回归、调整后的应用枚举回归和真实 AXObserver 引用计数回归尚未编译或运行；后者直接检查精确移除及 sibling 保留，不代替真实系统终止通知验收。新增两条 Metal 测试是需要显式执行的硬件测试，普通 workspace CI 不会执行。当前仅完成源码及锁定依赖审查，没有本地构建、提交、推送或安装；40.56 MiB 仍是本次改动前的现场基线。
