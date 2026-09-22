# Fastab 内存优化实施计划

日期：2026-09-22。源码审查基线：`145f45da`。

六项源码优化已实施并通过独立静态 Review；图标回收和 HookCache 字节预算按前置审查结论保留原实现。具体改动、发现与修正、阻塞依据记录如下，各节“确认问题”描述的是改动前的源码基线。未执行编译或任何运行验证，不能据此保证零回归或量化实际收益。本计划承接 [memory-goals.md](memory-goals.md)，不重复实现已经落地的 typed hook 懒解析、spec/历史共享、PTY 行缓存上限和线程数收缩。

## 范围和执行约束

- 覆盖六项可实施优化，以及图标回收、HookCache 字节预算两个前置条件受限的事项。
- 不运行本地或远程测试、构建、lint、格式化检查、压测、内存采样或真实终端/UI 验证；不触发 CI。
- 不为本计划新增测试文件。已有测试可以作为行为契约阅读，不把未执行的测试记为通过。
- 每一步都做静态 Review，子代理统一使用 GPT-6 Astra，不使用 Luna。实施者和该步 reviewer 分开。
- 保留无关工作区内容和用户数据，不操作已安装应用、IME、权限、shell 配置；不自动提交、推送或安装。
- 图标与 HookCache 纳入计划，但以保持现有行为为前提。前置条件不能满足时，记录具体阻塞结论，不强行落地。
- 静态 Review 不能证明零回归、运行时内存下降或性能无退化；这些结论统一标为“未运行验证”。

## 不变量

- PTY `max_scroll_limit = 1`，保留滚出视口一行的 prompt 读取能力。
- Registry LRU 48、hook 缓存 512、generate LRU 32、历史数量及索引上限不变。
- `fastab_util` 不引入 AppKit；IME 不引入桌面依赖栈；AX 所有权规则不变。
- 补全内容、排序、插入元数据、历史顺序及正常终端输入语义保持不变。
- 不拆 sidecar，不削弱 snapshot 全树校验，不调整引擎栈或超时 attempt 的整体架构。
- 不以缩小打包文件、删除依赖或降低虚拟地址预留冒充实际常驻内存优化。

## 每步关闭规则

流程：确认最小方案 → 实施独立补丁 → 独立静态 Review → 修正 → 复审 → 更新本文件状态。

Reviewer 必须阅读实际 diff 及受影响调用链，记录：

1. 审查文件和基线、发现的问题及修正结果。
2. 该步行为不变量是否得到源码支持；已有测试契约是否仍成立于代码逻辑。
3. 分配/所有权/队列或资源生命周期的前后变化。
4. 未运行验证的边界和仍不能排除的风险。

有阻塞发现时不关闭该步，也不进入依赖它的实现。后续改动触及已审逻辑时，原 Review 失效，必须重新审查。不能用一句“Review 通过”替代上述记录。

状态使用“待实施”“实施中”“待 Review”“静态 Review 通过，未运行验证”“阻塞”。计划文档的 Review 通过不代表任一实现已经完成。

## 执行顺序

| 步骤 | 事项 | 当前状态 | 主要收益类型 |
| --- | --- | --- | --- |
| 0 | 固定范围、源码基线和行为契约 | 计划文档静态 Review 通过 | 控制改动范围 |
| 1 | Registry 失效 Weak 清理 | 静态 Review 通过，未运行验证 | 长期遗留分配 |
| 2 | 大粘贴后的空缓冲回收 | 静态 Review 通过，未运行验证 | 单终端容量高水位 |
| 3 | `execute_full` 有界输出收集 | 静态 Review 通过，未运行验证 | 大输出峰值 |
| 4 | 历史参数索引构建后去重 | 静态 Review 通过，未运行验证 | 重复字符串常驻量 |
| 5 | 缩窄后回收行容量 | 静态 Review 通过，未运行验证 | 每终端行容量高水位 |
| 6A | PTY 连接与消息处理方案 | 已修订，独立静态 Review 通过 | 先明确异常路径契约 |
| 6B | PTY 有界 outbox 与重连恢复 | 两路独立静态 Review 通过，未运行验证 | 离线和慢连接积压 |
| 7A | 图标缓存与 GPUI 回收前置审查 | 已完成，独立静态 Review 确认阻塞依据 | 确认可安全实施的边界 |
| 7B | GPUI 回收机制修补 | 阻塞：依赖删除正确性及绘制资源寿命未闭合 | 消除主动回收的正确性风险 |
| 7C | 应用图标生命周期管理 | 阻塞：7B 前置条件未满足，保持原实现 | 解码缓存和图形资源保留 |
| 8 | HookCache 字节预算决策 | 审查完成；实施阻塞于行为保持要求 | 策略可行性，非直接实施承诺 |
| 9 | 最终集成 Review 和交付记录 | 实现集成及最终文档静态 Review 通过，未运行验证 | 完整性与证据一致性 |

## 1. Registry 失效 Weak 清理

**代码入口：** [ir.rs](../crates/fastab_engine/src/ir.rs) 的 `intern_one_option`、`evict_oldest_spec`、`ensure_loaded`。

**确认问题：** 去重池仅清理再次访问到的 hash 桶。spec 驱逐后，死 Weak 可继续保留 Arc backing allocation、桶和索引；不是完整 spec 字符串树仍然存活。

**最小改动：**

- 增加私有清理函数，移除强引用计数为零的 Weak，再移除空桶。
- 在 `evict_oldest_spec` 返回、临时 `old` 强引用释放后扫描全池；命中已有 spec 的热路径不扫描。
- 外部调用者、其他 spec、pinned/path specs 或其他 Registry 仍持有的 option 必须保留。
- 首轮不对 HashMap 反复 `shrink_to_fit`，不改变缓存数量和活对象共享关系。

**本步 Review：** 核对清理发生时的强引用生命周期、外部延迟释放、两个 spec 共用 option、Registry 克隆；确认只移除不可恢复的 Weak，并检查扫描不进入高频命中路径。`upgrade() == None` 只能证明对象失活，不能单独证明内存已归还系统。

## 2. 大粘贴后的空缓冲回收

**代码入口：** [readbuf.rs](../crates/fastab_term/src/input/readbuf.rs)、[input/mod.rs](../crates/fastab_term/src/input/mod.rs) 的 `ReadBuffer`、`InputParser`。

**确认问题：** 粘贴结束后仅 `truncate`，常驻 parser 继续持有最大容量。这是高水位保留，不应直接称为泄漏。

**最小改动：**

- 在一次 parse/process 完成后的维护点，仅对已经为空且容量超过明确阈值的缓冲换回小 Vec。
- 实施时记录阈值与选择依据；避免正常小输入触发回收。阈值不解释为已经测得的最优值。
- 本次实现选择空缓冲容量超过 64 KiB 时恢复初始 16 字节容量，保留常规小输入复用；该阈值为保守策略，未运行性能验证。
- 不在同批粘贴起始标记刚被消费时提前释放；不动未完成粘贴或后续未消费字节。
- 不改为流式粘贴，不截断，不引入长度限制，保留 raw callback 和事件顺序。

**本步 Review：** 对照完整/分块粘贴、结束标记跨块、结束后紧跟按键或第二段粘贴、UTF-8 与原始字节路径；阅读现有 `partial_bracketed_paste`、`large_paste`、`large_partial_bracketed_paste` 契约。确认回收点在本批处理末尾，小输入没有新的持续分配路径。

## 3. `execute_full` 有界输出收集

**代码入口：** [process.rs](../crates/fastab_engine/src/process.rs) 的 `execute_full`、`wait_child_output`。

**确认问题：** 当前完整收集 stdout/stderr 后才分别截为 256 KiB，执行期间的内存峰值未受该上限控制。

**最小改动：**

- Unix full 路径使用双管道非阻塞收集，每条只保存前 256 KiB 原始字节；超出部分继续排空并丢弃。
- 每轮 drain 有工作量上限，保证另一条管道、退出检查和 deadline 不被持续输出饿死。
- 仍先截原始字节，再做 `from_utf8_lossy`。不把限制改成字符数或最终 String 字节数。
- 非零退出仍返回 `Ok`，信号退出状态保持 `-1`；等待退出状态与两条管道 EOF，保留 full 路径的超时语义。
- 超时/错误路径关闭本方管道、清理原进程组并回收 leader，避免留下长期 reader 线程。不承诺杀掉已逃离原进程组的子孙。
- 保留非 Unix 路径；不直接复用普通 execute 的“满额即杀”逻辑。

**本步 Review：** 分别推演 stdout/stderr 同时大输出、UTF-8 边界、非法字节、非零/信号退出、EOF 后 child 存活、leader 退出但子孙持管道、无限输出和错误清理。普通 execute 现有测试不等于 full 路径覆盖。本轮不运行真实子进程，运行时死锁、时序和峰值结论保留为未验证。

## 4. 历史参数索引构建后去重

**代码入口：** [history.rs](../crates/fastab_engine/src/history.rs) 的 `build_index`、`arg_values`。

**确认问题：** 派生索引逐次保存重复参数，查询才反向去重；原始历史 Arc 共享没有消除这些派生副本。

**最小改动：**

- 保持历史遍历、tokenize、alias expansion、wrapper walk 和禁用 hooks 的顺序不变。
- 仅在索引构建完毕后，按 slot 保留各值的最后一次出现，剩余值保持相对顺序，并回收多余 Vec 容量。
- `A,B,A` 压缩为 `B,A`，查询反向遍历仍得到 `A,B`。
- 查询端继续按 slot 顺序处理并跨 slot 去重；不顺带把查询集合改成跨局部 Arc 的借用集合。
- 不混入原有 alias/shell 缓存键问题的修复，也不扩大缓存作用域。

**本步 Review：** 推演重复值穿插、同值跨 slot、同一行多命令、alias、wrapper、变长参数；确认结果顺序与原算法一致、所有权安全。短命去重集合会产生构建期分配，不将常驻压缩声称为消除了构建峰值。

## 5. 缩窄后回收行容量

**代码入口：** [row.rs](../crates/alacritty_terminal/src/grid/row.rs)、[resize.rs](../crates/alacritty_terminal/src/grid/resize.rs) 的 `Row::shrink`、`shrink_columns`。

**确认问题：** `split_off` 不收缩原 Vec，部分最终存活行会保留旧宽度容量；并非所有旧行都一直保留。

**最小改动：**

- 不修改拆行和 reflow 算法，不对中间拆分对象立即 shrink。
- 在缩列、重排及截断完成后，仅对最终存活且容量明显超过长度的行回收。
- 使用有滞回的阈值并记录依据，避免连续窗口拖动时反复重新分配。
- 本次选择容量超过行长 2 倍时，请求收缩到行长加 25% 余量；触发与保留比例分开以减少边界来回 resize 的分配，比例未经过运行测量。
- 保留主/备用 grid、行内容、`occ`、flags、cursor、`input_needs_wrap` 和 scrollback 规则。

**本步 Review：** 对照空行、长命令、宽字符 spacer、组合字符、reflow 开关及备用屏恢复路径；阅读现有 `shrink_reflow*`、`grow_reflow*` 和备用屏光标契约。确认维护点只在 resize 完成后发生，不进入按键路径。连续拖动的性能及实际 footprint 保留为未验证。

## 6. PTY 离线队列与连接恢复

**代码入口：** [ipc.rs](../crates/fastab_term/src/ipc.rs)、[ipc/outbox.rs](../crates/fastab_term/src/ipc/outbox.rs)、[main.rs](../crates/fastab_term/src/main.rs)、[event_handler.rs](../crates/fastab_term/src/event_handler.rs)、[message.rs](../crates/fastab_term/src/message.rs)、[interceptor.rs](../crates/fastab_term/src/interceptor.rs)。

**确认问题：** outgoing 无界；连接失败、握手等待、慢写入时可积压。读侧断开不必然立刻停止 writer，但 writer send 失败退出后仍存在无消费者阶段。既有 shell 不受“启动时桌面未运行则不注入”规则保护。

### 6A. 先审查协议方案

- 定义 `Disconnected → Connecting → Handshaking → Ready` 与连接 generation。
- 列出共享队列中的 EditBuffer、Prompt、PreExec、PostExec、InterceptedKey、Response、Pong 的顺序要求。
- 在线保持 FIFO，不跨控制消息合并编辑状态。
- 明确无法同时保证无限量消息不丢、有界内存和生产者永不阻塞；满载必须使连接明确失效，不能静默丢控制消息后仍视作健康。
- 为握手、写入、半连接退出、停止信号、队列拒绝和旧回复分别定义状态转换。

**本步 Review：** 先审状态转移与消息分类，确认正常输入契约不变、异常路径可收敛、重连数据来源明确。未解决的顺序或按键重放问题阻塞 6B。

**首轮方案 Review 发现及修订：** 每个拦截键绑定作出决定时已应用的 generation；RunProcess 延期 task 在真正开始命令前再次核对 generation；引入 `RequestOrigin`，插入时间、期望缓冲及来源统一为单锁状态，拦截 flags 和 visible 分别记录 owner，退代只清失效远端来源。本地请求覆盖后的状态保留，延期插入在 Prompt 和实际 PTY 写入前两次核对。三处修订已通过独立设计复审，进入 6B；设计通过不代表实现通过。

### 6B. 再实施连接与 outbox

- 离线不保存历史 outbox；Ready 使用非阻塞 admission，同时限制消息条数和累计 payload。
- 满载、超时或任一半连接退出时，使整代失效、清理旧消息并解除 interception，随后按既有转发机制重连。
- 入队拒绝不推进“已同步 context”标记；新 generation 首次有效事件包含完整 context/env/alias，随后恢复 epoch 去重。
- 不为同步伪造 Prompt，也不在 preexec 时发会唤起补全的编辑事件。
- RunProcess 回复绑定原 generation，旧 nonce 不跨连接投递。
- 拦截消息未被接纳时回落原始输入；已经交付连接的按键不能无条件重放。
- 保留 session/secret/parent_id、本机与转发连接的契约；控制错误和异步任务生命周期不能遗漏。

**已审定的实现参数与边界：** outbox 首版使用 256 条、4 MiB 完整编码帧总量，包含 writer 正持有的一帧；编码前检查单帧长度，离线/旧代先拒绝，编码后同锁复核并提交 context 进度。握手、单帧 write+flush 采用 5 秒预算，保留原有连接/重试机制。阈值及超时为工程策略，未实测慢连接兼容性。预算不覆盖调用者构造消息、编码瞬时分配、RunProcess 原始输出或 incoming。读写使用有作用域的 futures，任一退出取消另一侧；WSL 转发 Child 由连接持有并负责回收。没有协议 ACK，admission 成功不表示 desktop 已收到；退代不重放已经接纳的键。

**服务端契约复核：** desktop 只有 EditBuffer、Prompt、PreExec、PostExec 路径把 context 应用到 session。InterceptedKey 虽带完整 payload，其处理只派发 action，因此不得推进 outbox 的 full-context/environment-epoch 标记；否则重连首事件为拦截键时，后续编辑帧可能提前变轻量并漏掉完整上下文。保留原合并契约：已有 session 的空 env 列表/alias None 表示保持，非空 env 整体替换、alias `Some("")` 可清空；新 session 没有 context 时采用整份初值。不在此次优化中修改该协议。服务端 nonce 按连接从零开始，旧回复必须留在原 generation，避免碰撞新连接 nonce。

**IPC 部分 Review 修正：** 首轮发现 WSL 转发 Child 在 kill 失败后可能无限等待，阻断 stop/重连。已给回收等待增加独立 5 秒期限，等待期间接收 stop 后只处理一次；无法确认回收时进入 Stopped，不继续重连累计 forwarder。`kill_on_drop` 只作为尽力清理，超时不记为已回收。独立复审已关闭此发现。

**生产者与状态接入 Review：** 第二路独立 reviewer 已核对逐键绑定、拒绝回落、Local/Remote owner、插入锁与 timer 全量迁移、延期插入双重检查、RunProcess 启动前复核及固定代号回复、context 标记和原 epoch 符号替换。主循环只等待 watch `changed()`，不持有 watch borrow guard 再获取 outbox 状态锁。没有剩余静态阻塞。`main.rs` 既有内联 epoch 测试随生产 `ContextProgress` API 作必要适配，未新增测试文件、未执行测试。

**本步 Review：** 沿每类生产者到发送者、接收者、断线清理和重连路径逐一审查，重点检查阻塞 send、字节记账、取消、generation 隔离、旧任务退出和重复输入。此项独立补丁，不把一行 `bounded()` 视为完成。并发时序、真实 shell 可用性保留为未运行验证；静态仍有阻塞发现则不关闭。

## 7. 文件图标：先解决回收前置问题

**代码入口：** [overlay.rs](../crates/fastab_desktop/src/overlay.rs) 的 `file_icon_png`、[icons.rs](../crates/fastab_gpui/src/icons.rs)，以及锁定的 GPUI 0.2.2 资产与 Metal atlas 实现。

### 7A. 前置审查

- 上层 64 项路径缓存不约束 GPUI 解码缓存；增长单位是实际加载的不同 PNG 内容，不是文件路径数。
- `Arc<Image>` 路径绕过可选 image cache；包一层 `image_cache` 不是该路径的修复。
- 只删除 CPU asset 会在重新解码时产生新的 RenderImage ID，旧 atlas tile 仍可能保留，不可当作安全中间方案。
- 当前 GPUI 删除逻辑涉及旧 key、重复减计数、texture 槽位复用和未归还 tile 空间，主动释放前必须先解决。

**本步 Review：** 明确 asset、解码结果和 GPU tile 的完整所有权图、可用公开 API 及阻塞项。本计划不测量真实图标数量和收益，不设虚构的安全容量或节省 MB。

### 7B. GPUI 前置修补

- 仅通过可审查、固定版本的依赖补丁处理，不修改本机 cargo registry 作为交付。
- 删除必须幂等：一次成功删除对应一次计数调整；旧 key 不指向已释放或复用槽位。
- 回收正确的 allocator allocation，保证仍有常驻图标的纹理能够复用空位。
- 明确旧帧、在途绘制、跨窗口和重绘的资源寿命。

**本步 Review：** 推演 A/B 共享 texture、重复删除 A、依次删除后重新插入、常驻 B 加反复更换 A。若资源寿命无法由源码审查闭合，标记阻塞，不接入应用主动释放。即使静态通过，也不得声称实际 Metal 绘制已验证安全。

### 7C. 应用生命周期

- 按内容 ID 集中管理图标，分离路径索引和内容记录。
- 当前可见列表仍使用的内容不能因某一路径被淘汰而释放。
- 保存 pending decode/已解码结果，避免“为了删除而重新加载”。
- 在 GPUI 主线程联动处理 asset 与 atlas，正确处理当前更新窗口和其他窗口。
- 保持现有自定义图标效果；不以超限改通用图标的产品变化绕过前置阻塞。

**本步 Review：** 检查共享内容、最后使用者、加载中淘汰、列表切换、隐藏再显示及跨窗口生命周期。当前不能保证前置问题可在无运行验证下充分收敛，因此本项允许以明确阻塞结论结束。

**本次决策：7A 审查完成；7B、7C 阻塞，未修改图标路径或依赖。** 锁定 GPUI 0.2.2 的 `src/platform/mac/metal_atlas.rs` 中，`remove` 仅在纹理计数归零时删除本次 key：A/B 共用纹理时重复删除 A 可错误减少 B 所依赖的计数，分别删除 A、B 又可能遗留指向已释放槽位的 A。tile 分配使用 atlas allocator，但删除没有对应的 deallocate。`src/elements/img.rs` 的 Image 分支进入 `src/app.rs` 的全局 `loading_assets`，路径缓存淘汰不能解除这些持有。

解除阻塞需要固定版本的完整依赖补丁，同时给出在途绘制、旧帧及跨窗口仍引用 tile 时的延迟释放机制，并明确 pending decode 与应用最后使用者的关系。本轮未闭合这些资源寿命，不能通过仅调用公开删除 API 或只删 CPU asset 保持现有绘制行为。按已确认的前置条件规则保留原实现；不把此前置审查计作图标内存已优化。

## 8. HookCache 字节预算：保持现有行为的策略决策

**代码入口：** [hook_cache.rs](../crates/fastab_engine/src/hook_cache.rs)。

**确认边界：** 已有 `allocated_bytes()` 不参与驱逐，但它是估算量，忽略部分 capacity/节点且可能重复计算共享 Arc。TTL 按此次读取者 policy 解释，不能直接按写入时 TTL 全局清理。

**处理计划：**

- 静态梳理三类缓存的值大小来源、共享关系、TTL、key 和重新执行路径；不在热路径增加全量扫描。
- 明确预算提前驱逐会使 TTL 内或永久缓存 hook 重新执行，可能改变结果、时延与副作用。
- 本次要求保持现有行为，不自行实施提前驱逐、不缓存超大项或变更 TTL 的方案。
- 若无法证明预算不改变既有执行契约，记录“行为保持要求与提前驱逐冲突”的阻塞结论，保留当前策略。
- 在不执行量测的约束下，不给出未经依据的预算值，也不把策略审查完成记作内存优化已经落地。

**本步 Review：** 核对 max-age 无 TTL、同 key 不同 policy、cwd 隔离、当前 tokens 的 postProcess 和 hook 执行次数。最终产物是可实施且行为一致的方案，或具体的阻塞决策。

**本次决策：策略审查完成；字节预算实施阻塞，未修改 HookCache。** `owned_cache_policy` 中 max-age 无 TTL 对应永不过期；`cache_get` 使用当前读取者的 policy，`cached_spec` 没有 TTL。新增字节预算在既有 512 条 cap 触发前便可能删除这些命中项，使 `generate.rs` 的 custom/脚本或 `lookup.rs` 的 generateSpec 再次运行。其结果、时延和副作用不能证明与原行为一致。脚本缓存仍须按当前 tokens 做 postProcess，也不能改成缓存最终 rows。解除阻塞需要另行接受新的缓存执行契约；本轮保持原策略，不设置虚构的预算值。

## 9. 最终集成 Review 与交付

- 审查最终完整 diff，确认改动只覆盖本计划且不变量仍成立。
- 检查各步骤修改是否互相影响，后续修正是否使前面的 Review 失效。
- 逐项填写状态、实现范围、reviewer、审查发现、修正和未验证边界。
- 分开记录“源码层面的分配/生命周期改善”和“运行时收益未测量”，不引用旧版已安装应用的占用作为新补丁收益。
- 阻塞项明确列出缺失的前置条件，不静默删除，也不计入已实施数量。
- 交付源码变更清单和更新后的本文档，不附带未执行的检查成功声明。

## Review 记录

| 步骤 | 实施范围/基线 | Reviewer | 发现与处理 | 结论 |
| --- | --- | --- | --- | --- |
| 0：计划文档 | `145f45da`；本文档 | 独立 GPT-6 Astra 子代理 | 无阻塞发现；核对六项优化、两项前置受限事项、每步审查规则、行为约束和未验证边界，均与前轮源码复核一致；未重新探索源码、未执行运行验证 | 计划文档静态 Review 通过；不代表实现已完成或运行收益已验证 |
| 1 | `ir.rs`：`prune_dead_options` 及驱逐后的调用 | 独立 GPT-6 Astra 子代理 | 无阻塞发现；核对外部 Arc、共享 option、versioned/pinned 持有和驱逐时机；命中热路径不扫描，48 项 LRU 不变。其他 Weak 与 HashMap 容量仍可能保留 | 静态 Review 通过，未运行验证 |
| 2 | `input/readbuf.rs`、`input/mod.rs`：整次 parse 返回后回收大空缓冲 | 独立 GPT-6 Astra 子代理 | 无阻塞发现；核对分块粘贴、`Pasting(0)`、raw callback 所有权及小输入路径；64 KiB 为策略阈值，连续大粘贴的重新分配成本未测量 | 静态 Review 通过，未运行验证 |
| 3 | `process.rs`：Unix full 路径双管道有界收集；非 Unix 路径保留 | 独立 GPT-6 Astra 子代理 | 首轮发现双 EOF 后 poll 等待期间 child 已退出却可能先被 deadline 判超时；修正为双 EOF 时先 try_wait、取得退出状态立即返回，复审关闭。核对每管道 256 KiB 原始字节、每轮 64 KiB 排空上限、非零退出、进程组清理与 leader 回收，未见新增阻塞发现 | 静态 Review 通过，未运行验证 |
| 4 | `history.rs`：构建完成后按 slot 保留最后出现项 | 独立 GPT-6 Astra 子代理 | 无阻塞发现；反向生成标记后按原序 retain，借用集合先于 String 修改销毁；查询、跨 slot 顺序、解析轨迹及 alias key 不变。新增短命集合/标记，不声称减少构建峰值 | 静态 Review 通过，未运行验证 |
| 5 | `grid/row.rs`、`grid/resize.rs`：缩列结束后回收最终存活行的过剩容量 | 独立 GPT-6 Astra 子代理 | 无阻塞发现；核对方法可见性、饱和运算、空 Vec/ZST、主/备用 grid、Storage 复位及既有 reflow 契约；只改变 capacity，不改变行内容、occ、flags、游标或 history。分配器不保证精确容量或立即归还 RSS，resize 搬移成本未测量 | 静态 Review 通过，未运行验证 |
| 6A | PTY 连接状态、outbox admission、context 与输入/控制消息方案及 desktop 处理器 | 独立 GPT-6 Astra 子代理 | 首轮发现逐键绑定、RunProcess 延期启动和本地/远端状态来源三处缺口；补充 generation-bound sender、启动前检查、RequestOrigin/单锁插入状态/按字段 owner，独立复审关闭。服务端复核另发现 InterceptedKey 不应用 context，已要求其不得推进同步标记；确认空 env/alias 与 nonce 现有语义 | 设计静态 Review 通过；6B 实现另行审查 |
| 6B：IPC/outbox | `ipc.rs`、新增私有生产模块 `ipc/outbox.rs` | 独立 GPT-6 Astra 子代理（与实施者不同） | 发现 WSL 清理无限等待，修正为固定期限且响应 stop，无法确认回收时停止 supervisor；复审关闭。核对 in-flight 记账、同锁 admission/context、旧代扣账、Notify 注册与 watch 顺序、作用域取消及本地 Tokio/prost API。末轮错误文本修改再次复核，无控制流变更 | 静态 Review 通过，未运行验证 |
| 6B：生产者与状态 | `main.rs`、`event_handler.rs`、`message.rs`、`interceptor.rs` | 另一独立 GPT-6 Astra 子代理（与实施者不同） | 无阻塞发现；核对逐键 generation、原始输入回落、按 owner 退代、插入锁/延期插入、异步启动和回复、四类有效 context hook 与 InterceptedKey 例外；无旧 epoch/锁引用遗留，既有内联测试适配真实 API，watch 无反向锁序 | 静态 Review 通过，未运行验证 |
| 7A–7C | 应用图标调用链及锁定 GPUI 0.2.2 的 img/app/metal_atlas 源码 | 独立 GPT-6 Astra 子代理 | 复核 64 项路径缓存与全局解码资产的区别；确认删除非幂等、残留 key 和缺少 allocator 回收，且旧帧/在途绘制/跨窗口寿命尚未闭合。未改本机 registry、依赖或图标实现 | 7A 完成；7B/7C 阻塞，保持现有行为 |
| 8 | `hook_cache.rs`、`generate.rs`、`lookup.rs` 的三类缓存及调用链 | 独立 GPT-6 Astra 子代理 | 复核无 TTL 缓存、按读取者 policy 判过期、current-tokens postProcess 与缓存 miss 重新执行；提前驱逐可能增加外部操作，估算字节也不等于准确硬预算。未修改缓存策略 | 策略审查完成；实施阻塞于行为保持要求 |
| 9 | 当前完整 tracked diff、新 outbox 源码与本文档 | 独立 GPT-6 Astra 子代理及主代理 | 未发现新增集成阻塞；核对 1/4 的 spec/option 生命周期与 2/5/6 的输入/行容量/来源状态交叉关系；依赖、proto、图标、HookCache 和一行 scrollback 均未改。步骤 3 与 6B 生产者独立结论由非作者 reviewer 提供；最终文档范围、状态、文件数和证据边界经独立复核一致 | 实现集成及最终文档静态 Review 通过，未运行验证 |

## 本次交付

- 已实施：失效 Weak 清理、大粘贴空缓冲回收、有界双管道输出、历史参数索引去重、缩列后行容量回收、有界 PTY outbox 与重连恢复，共六项。
- 前置审查后保留原实现：图标回收、HookCache 字节预算。具体缺失条件和后续解除阻塞要求见第 7、8 节，未计入已实施数量。
- 源码范围：12 个已有 Rust 文件及一个新增私有生产模块 `crates/fastab_term/src/ipc/outbox.rs`；本文档是本次新增的计划与交付记录。既有无关未跟踪内容保留。
- 本地及远程测试、构建、lint、格式检查、压测、采样、实际 shell/UI 行为验证和 CI 均未执行；没有编译成功、运行验收通过或节省 MB 的声明。
- 后续用户已明确授权按内容分别提交并推送；交付拆为六个源码提交及独立计划文档提交，Git 操作禁用本地 hooks，提交携带 `[skip ci]` 以保留不运行验证的约束。没有安装或操作已安装应用/IME/权限。

剩余证据边界：实际编译兼容性、双管道进程时序、连续 resize/大粘贴成本、PTY 并发与慢连接体验、WSL 回收和实际内存收益均未运行验证。若以后放开验证约束，应按各节列出的真实场景补充证据，而不是把本次静态 Review 当作运行验收。
