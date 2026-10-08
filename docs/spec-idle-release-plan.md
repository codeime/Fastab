# 不用的规格树先放下，再用时重新解析

日期：2026-09-30。对照代码：`crates/fastab_engine` 的 `ir.rs`、`lookup.rs`、`history.rs`、`runtime.rs`、`worker.rs`。

**v0.0.5 修订已关闭；后续资源生命周期改进见 [完整计划](completion-resource-lifecycle-plan.md)。** 当前默认宽限为 10 秒，并接入带会话身份的输入结束与断开通知、资源所有者区分、队列公平性和显式 Replace 孤树清理。2026-10-08 追加修复将隐藏、定位失败和焦点切换也作为结束资源使用的边界，具体实现及验证见完整计划末节。下面「目标」到「每步关闭规则」为当前契约；历史关闭记录保留当时证据，旧限制不再覆盖当前契约。

人走进 `gcloud compute ` 时，解析整份 `compute.json` 曾带来大约 50 MB 的进程 `phys_footprint` 增量；这不是结构体净分配量。引擎基线曾约 12 MB，`ftab` 曾量到约 65 MB，这些是历史观测。这份文件不拆，结构体也不改。人不会一直停在里面。这份计划只加一条出口：编辑缓冲的查找不再用到某份已解析文件、再过一段宽限，就把这棵树从注册表放下。下一次查找沿现在的加载路径重新解析。进程、输入法、补全窗口都不挂起。

## 目标

光标还在一份已解析文件里时，树留着。这里的「在」指最近一次补全的编辑缓冲查找用到了它，例如缓冲是 `gcloud compute instances `。

之后某次实际执行的普通补全，其编辑缓冲查找不再用到它（例如改去 `git `、或只停在 `gcloud ` 第一层），宽限结束就把对应的 `Arc<Spec>` 从注册表摘掉。选项池里是弱引用，最后一个强引用放下后选项正文一起走。桌面进程用系统分配器，这些页可以退回。`ftab` 用 mimalloc，水位不退，不用它的进程值证明常驻下降。

除后续普通补全外，当前输入所有者的 `EndInput` 也会开始宽限：桌面在空缓冲、pre-exec、终端连接移除，以及隐藏、禁用、定位失败、焦点或会话切换时结束对应资源使用。重复结束不延长期限，旧会话结束不取消新会话；活动规格归属只由成功普通补全更新，取消的新提交不吞掉旧活动会话的结束通知。真正输入结束或焦点变更还清除可重试的旧输入；普通隐藏可保留已复制的候选字符串。临时缺少 caret 时只保留最新输入，收到有效 caret 后补全一次；Esc 后需要显式 Show 或新的输入。提示仍在显示时单纯停止打字不会结束资源使用。

下一次编辑缓冲又走进去，走现有的 `ensure_loaded` / `load_referenced_spec` / `load_relative_spec`，不另写加载器。建议的名字、顺序、优先级、隐藏项、`should_add_space`、`args_hint` 和公共 AI 标记与放下之前那次相同。

历史索引里已经抄出去的参数值留着。历史行走进子文件时可以暂时把文件读进来，但这次读入不算「编辑缓冲在用」。它和「人已经离开这份文件」走同一条宽限：从这次补全返回起满 10 秒才放下。`gcloud ` 第一层为了给历史填槽而解析的 `compute.json`，会在菜单上再留这 10 秒。

仅历史补全（`history_only`）为查询 fuzzy 策略而首次读入的规格，同样从该次返回开始 10 秒宽限。请求前已经缓存的文件保持原 active/idle 状态和既有时间，不接管普通补全的 session owner；即使冷启动后只做过历史补全、随后清空输入，也能独立到期回收。

## 已定的行为

宽限常量 `SPEC_IDLE_GRACE`，10 秒。切去看一条别的命令再回来，这棵树还在。计时用 `Instant`，测试注入 `now`，不在测试里睡 10 秒。

普通 idle 回收等满这 10 秒；LRU 容量淘汰、显式 Replace 清理孤树、ClearCaches/代际重载等显式失效路径不受此宽限约束。没有「仅因这次查找已经不用它，返回前就摘掉」的路径。

一份文件的状态只有这两种：

- 编辑缓冲用过：这次缓冲查找碰到它，就把「待释放」清掉。没有后续补全时，监督线程的超时也不会放它。人停在 `gcloud compute ` 上思考，树一直在。
- 待释放：某次走了编辑缓冲的补全没有碰到它，或当前所有者明确结束输入。从这次尝试返回起算，满 10 秒才放下。10 秒内又走进去，清掉待释放，树还是原来的 `Arc`，不重新解析。只被历史读进来、缓冲查找没碰到的文件，也进这一态，同样等满 10 秒。

一次补全用一份触碰集记下这次缓冲查找拿到的路径，缓存命中、命令表和路径表共用的那一格、版本文件命中都算。查找结束时：

- 在触碰集里：`idle_since` 写成 `None`。
- 不在触碰集里、且 `idle_since` 还是 `None`：写成这次的时刻，树先留着。
- `idle_since` 已经有值：不改写。之后在 `git` 里继续打字，不会把这 10 秒重新算起。

没走编辑缓冲的请求不更新这些时刻。只查历史就提前返回时，人可能还停在 `gcloud compute ` 上，这次不能给它开始计时。

多个终端共用一个引擎。会话身份通过 `EngineClient::complete_for_session` / `end_input` 传递，不加进 `CompleteRequest`。另一个标签上的补全若没有走进这份文件，会给它开始宽限；宽限内原来的标签再打字则续上，宽限过后原来的标签再打字会重新解析。

历史索引按根命令缓存，槽位里是 `String`。索引还在时，之后的 `gcloud ` 不会为了历史再读 `compute.json`。历史行集合换了新的 `Arc`、索引重做时，允许这次尝试内部出现解析峰值；峰值过去后仍等满 10 秒才放下。

监督线程在 `rx.recv()` 上改为等到下一份待释放文件到期。队列优先仅指 `recv_timeout` 等待阶段：已有任务会直接返回，不必等满宽限。尝试成功交回 `Engine` 后仍先执行一次 `release_idle`，再处理下一份排队任务；若旧宽限在尝试中到期，排队中的回访可能需要重新解析。这是已接受的顺序边界，本轮不改。连续处理 64 个非 attempt 消息后也提供一次到期释放机会，避免持续控制消息饿死 timer；已取得的有效 Complete 队首仍优先，不重排控制消息。放下只发生在监督线程重新取得 `Engine` 所有权之后。超时或 panic 丢掉的那次尝试仍占着旧引擎直到该线程自己结束，监督线程不从外面拆它；下一次请求从模板重建。

`registry_template` 来自 `Engine::load_registry`，可能包含本地开发目录预先解析并 pinned 的树；它不包含从运行中引擎回写的、按需解析的 bundle 树。放下只改尝试交回的那份 `Engine` 的注册表，不把运行时按需解析的 bundle `Arc` 写回模板。

## 放下时动哪些表

和 `evict_oldest_spec` 摘掉同一棵树的方式相同，收成一个按 `Arc::ptr_eq` 摘除的函数，两处都调它。漏掉一张表会把树留在另一张里，或者让下一次查找拿到半截索引。

一个路径可以同时有版本树、命令树或多个命令别名对应的不同 `Arc`。空闲释放按路径枚举全部非 pinned 的树，按指针去重后逐棵摘除，不能只选第一棵。满员淘汰仍只摘目标 `Arc`；摘除后只清掉已无可释放树的路径标记，同路径兄弟仍保留原来的 deadline。只有 pinned 树或已经没有任何树的路径不产生 deadline，无主标记随后清理。命中 pinned 命令不记录其旧 bundle 路径的触碰，避免给同路径旧 bundle 树错误续期。这里的路径所有者由 `load_spec_cache`、`path_specs` 和 `specs` + `files` 定位；若显式 Replace 覆盖旧 bundle 的全部命令名，且 `specs`、`load_spec_cache`、`path_specs`、`pinned` 均不再持有被替换的旧树，立即移除仅剩的 `loaded` 引用并在临时 Arc 释放后剪 Weak；这属于显式替换清理，不缩短普通空闲宽限。仍有别名/引用入口或同路径兄弟的树和原 deadline 保持。

- `loaded`：摘掉这一格，48 的上限不改。
- `specs`：摘掉指向同一 `Arc` 的命令名。`files` 和 `names` 留着，下次按路径再读。
- `load_spec_cache`：摘掉同一 `Arc`。`gcloud.json` 的存根是另一棵树，不跟着 `gcloud/compute.json` 走。
- `path_specs`：版本文件不在 48 格里，用同一条空闲规则放下。现有版本文件最大约 200 KB，不是上述约 50 MB 进程足迹增量的主要来源；不放的话这条表没有别的出口。
- `pinned`：不放。开发目录盖住的规格继续不被 LRU 和空闲规则摘掉。
- `option_pool`：这次摘除的临时 `Arc` 先丢掉，再 `prune_dead_options`。和满 48 格时的顺序相同。
- `version_cache`：留下已探测的版本文本，不为了放下再执行一遍版本命令。
- hook 缓存 512、generate LRU、`GeneratorSession`、历史槽位索引、typed hook 目录：不动。它们不持有 `Arc<Spec>`。

`insert` 进来、没有文件路径的规格不参与空闲释放。

## 谁算用过

缓冲查找碰到文件时记下路径。记下的点就是现在会把树留在缓存里的三个入口：

- `Registry::ensure_loaded`（`get` / `get_arc`）
- `Registry::load_referenced_spec`
- `Registry::load_relative_spec`（版本文件）

`lookup::walk_spec` 走编辑缓冲时这些入口记一笔。`history::build_index` 整段构建期间关掉这笔记录，再调用 `annotate_history_command`。LRU 的 `touch_loaded` 仍在，空闲记录和「挤掉最旧一格」是两套时间。

公共 AI、排序、持久选项、存根上的 `loadSpec`、失败留存根、环路，都不改。放下的是缓存格，不是磁盘上的文件，也不是父存根。

## 不变量

沿用 `docs/memory-goals.md` 和 `docs/lazy-loadspec-plan.md` 里已经冻结的项，这里不重开：

- `fastabterm` 的 `max_scroll_limit` 仍是 1。
- `fastab_util` 不链 AppKit。输入法不拉 `fastab_ipc` / tokio / prost。
- AX 的 `Copy*` / `Create*` 仍走 create rule。
- Registry LRU 仍是 48，hook 缓存 512，generate LRU 32，历史条数上限不改。
- 不拆 `compute.json`，不改 `OptionSpec` / `ArgSpec` / `Spec` 的字段布局，不把子命令收成字符串区。
- 不换 `ftab` 的 mimalloc，不去掉 `ftab` 的 AppKit 链接。
- 不提高 LRU，不在启动时预热常用规格。
- 不把持久选项和已经输入的旗标改成引用。
- 不削弱目录快照的全树 SHA，不拆 `typed-hooks.json`。
- 补全不 `make_mut` 注册表或父缓存里的 `Arc`。
- 不杀、不重启正在运行的桌面进程、输入法和终端。不跑 `scripts/install.sh`。

## 不做

- 人还停在该文件里、且没有新的补全把查找带离它或明确 EndInput 时，不因发呆放下。要覆盖这种情况就要在没有新请求时认定「缓冲已经不用了」，引擎看不到终端后来有没有改缓冲。
- 会话号只用于所有者和输入结束顺序，不按标签各留一份触碰集。
- 不休眠整个进程。输入法的连接绑在进程上，套接字和补全窗口也在这个进程里。
- 不用 `ftab` 的 footprint 证明页已退回。mimalloc 留着水位。
- 不把历史槽位索引和 hook 缓存一并清掉。清掉会让每次回到 `gcloud ` 都重走历史并重新解析大文件。
- 不在「这次查找已经不用它」的同一次返回里摘掉 `Arc`。宽限未满时 `release_idle` 是空操作。
- 不在尝试线程内部放下当前这次查找还握着的 `Arc`。

## 每步关闭规则

顺序固定：实现 → 独立 review → fix → 对这次 fix 再 review。review 没通过不能进入下一步。fix 本身再出问题就继续 fix，然后重新 review，不把上一次 review 当作对 fix 的通过。

全部步骤关闭之后，再按本文「目标」和「不变量」做一次总 review → fix → 对 fix 再 review。总 review 不通过不算做完。

每步 review 要看实际 diff 和受影响调用链，并记下：

1. 审了哪些文件，对照的基线是什么。
2. 这一步的行为是否仍由代码和测试撑住。补全结果、历史槽位、公共 AI 资格有没有被悄悄放宽。
3. 谁持有解析后的 `Spec`，LRU 名额算在哪，空闲释放和满员淘汰有没有摘错表。
4. 还没跑到的边界。

review 和实现分开做。不能用一句「review 通过」代替上面四条。后续改动碰到已经审过的逻辑，原 review 作废，重审。

状态用：待实施、实施中、待 Review、Review 未通过、已关闭。

## v0.0.5 历史修复关闭记录（2026-09-30，覆盖步骤 1–5）

1. **范围与独立复核。** 整体工作区基线为 `HEAD a3a471e5`；本轮增量以 `/tmp/fastab-idle-fix-baseline-path` 指向的修复前快照为准，改动限于 `ir.rs`、`runtime.rs` 和本文。两位 Astra 交叉复核非本人实现：一位审注册表增量、加载入口、LRU、pin、所有权和 worker 调用顺序，并复核最后的 ghost deadline 断言；另一位审 runtime 与文档，确认 `Weak` 前显式放下强引用、history-only 真正走入 child 及下一普通请求隔离、完整建议比较与具体非空断言。未发现剩余阻断项。`history.rs`、`lookup.rs`、`public_ai.rs`、`worker.rs` 和桌面 overlay 本轮只按相关调用链核对，没有改动。步骤 3 因 deadline 筛选和顺序说明受影响一并重审。
2. **行为与执行证据。** 步骤 1 覆盖同路径普通树与版本树全部释放、pin 与 bundle 共存、多别名、LRU 摘掉 `load_spec_cache` 后 `path_specs` 兄弟保留原期限；步骤 2 证明子树最后强引用消失后历史槽位仍读到 `value`，history-only 不改已有时刻、不启动活动树期限，其触碰不污染下一普通补全；步骤 3 验证更早的无主 ghost 标记不产生期限，25 秒宽限和尝试交回后释放顺序不变；步骤 4 用完整 `Vec<Suggestion>` 比较重载前后，保留名字、优先级、隐藏项、提示、插入值和公共 AI 资格断言。首轮 `cargo test --locked -p fastab_engine --lib` 为 **443 通过、0 失败、1 忽略，26.26 秒**（`/tmp/fastab-idle-fix-tests.log`）。之后仅调整 LRU 测试夹具以真正复现旧问题、并向原 deadline 测试补 ghost 断言，没有再改生产代码；最终 `cargo test --locked -p fastab_engine --lib ir::tests` 为 **61 通过、0 失败**，过滤器也匹配到部分 `spec_pair` 测试（`/tmp/fastab-idle-fix-ir-tests.log`）。最终 `cargo clippy --locked -p fastab_engine -- -D warnings`、`rustfmt --check` 和 `git diff --check` 通过。步骤 5 的最终忽略测试另跑一次：**1 通过、443 过滤，3.30 秒**，`rows=73 allocated_bytes=2762496 footprint_menu=16466448 footprint_held=57000536 footprint_released=15974976 returned=41025560`（`/tmp/fastab-idle-fix-footprint.log`）；释放后足迹回到菜单基线附近，字节数是这次测试进程观测，不是结构体净分配量，也不是已安装桌面的测量。
3. **持有者、名额与释放。** 命令树仍由 `specs` + `loaded` 持有，引用树由 `load_spec_cache` + `loaded` 持有，各占 48 格 LRU 的一格；版本树在 `path_specs`，不占 LRU；overlay 在 `pinned` + `specs`。新的路径枚举器只借用现有 `Arc`，逐棵排除 pin；释放前按指针去重收集，按原函数摘四张表，消费收集向量后才剪选项弱引用。LRU 先摘目标再清无主路径，仍只以 `lru_slot` 判断淘汰成功，同路径兄弟保留 deadline。命中 pinned 命令不续期 bundle。无主和仅 pinned 路径不触发定时器。历史值仍是字符串；hook、生成器、版本文本缓存及父存根保留，建议和公共 AI 契约没有放宽。模板允许包含本地 pinned 树，不回写运行时按需解析的 bundle 树。
4. **验证环境与剩余边界。** 原工作区 `crates/fig_input_method` 残留目录缺少 manifest，阻断 workspace 加载，未删除或改动它。验证在 `/private/var/folders/j9/5bqndst12xs3cs3mp44lfw1w54nfyn/T/fastab-idle-fix-verify-4per3jg2` 完成：仅排除此残留目录，其他非 engine crate 软链，engine、scripts、bundle 复制，使用 Node 22.23.1；最终 `ir.rs`、`history.rs`、`runtime.rs`、`worker.rs` 与工作区逐字节一致。footprint 在沙箱外仅采样自身测试进程；没有运行安装脚本或操作桌面、IME、终端。全套测试后只有上述测试改动，最终证据为定向复验而非再次跑全套。空提示符与整词退格可能被 overlay 跳过；队列优先仅在等待阶段，尝试交回后的释放仍先于排队回访；Replace 后只剩 `loaded` 的旧树仍依赖 LRU。这些是当前接受的边界。未新增长尝试跨期限与排队回访的端到端测试、版本文件经 `Engine::complete` 重载结果测试或 generateSpec/jsLoadSpec 空闲测试；忽略测试的 footprint 差值只记录、不作跨机器阈值断言。

## 执行顺序

| 步骤 | 事项 | 状态 |
| --- | --- | --- |
| 0 | 冻结契约 | 已关闭（本文） |
| 1 | 注册表按路径放下空闲树 | 已关闭（本轮复核） |
| 2 | 只有编辑缓冲的查找才续期，历史索引不算 | 已关闭（本轮复核） |
| 3 | 监督线程在宽限到期时放下 | 已关闭（本轮复核） |
| 4 | 整次补全的结果在放下再加载之后保持不变 | 已关闭（本轮复核） |
| 5 | 按目标总 review，并量一次内存 | 已关闭（本轮复核） |

## 1. 注册表按路径放下空闲树

**代码：** `crates/fastab_engine/src/ir.rs`。`evict_oldest_spec` 和这次的空闲释放共用一个按 `Arc::ptr_eq` 从 `specs`、`load_spec_cache`、`path_specs`、`loaded` 摘掉同一棵树的函数。

**做法：**

- 给有路径的缓存项记 `idle_since: Option<Instant>`。键是该文件的相对路径，和 `load_spec_cache` / `path_specs` 用的路径同一套。
- `release_idle(&mut self, now: Instant, grace: Duration)`：`idle_since` 已有值且 `now.duration_since(since) >= grace` 的路径，枚举全部非 pinned `Arc`、按指针去重，再逐棵调用摘除函数。`pinned` 里 `ptr_eq` 的不摘。没有路径的 `insert` 规格不摘。
- 摘除函数先从各表拿走 `Arc`，函数返回后调用方再丢掉这个临时值，然后 `prune_dead_options`。不能在临时 `Arc` 还活着时剪选项池。满员淘汰摘除后再检查路径所有者，同路径尚有可释放兄弟时保留原标记。
- 缓冲查找用过的路径把 `idle_since` 写成 `None`。这一步先做 API 和表的一致性，第 2 步再接到查找上。测试可以直接调用记下和释放。

**测试：**

- 两份小文件都在缓存里。给其中一份记下待释放并把 `now` 推过宽限，只有这一份从 `loaded`、`specs`、`load_spec_cache` 消失。另一份的 `Arc::ptr_eq` 仍在。48 的常量断言不动。
- 父存根和子文件是两棵 `Arc`。放下子文件后父存根还在，父节点上的 `LoadSpec::Path` 还在。
- `pinned` 即使 `idle_since` 过期也不消失。
- 放下后选项池里对应 `Weak` 的 `strong_count` 为 0，`prune_dead_options` 把空桶去掉。
- 放下后再 `load_referenced_spec`，得到新的 `Arc`，`ptr_eq` 为假，子命令名字和放下前相同。
- 版本文件在 `path_specs` 里过期后消失，`version_cache` 里的版本文本还在。
- 宽限未到不放。`idle_since` 为 `None` 不放。

**这一步 review 还要看：** 摘除函数是否把 `evict_oldest_spec` 原来的「路径只在 `load_spec_cache`、不在 `specs`」也算一次成功摘除。命令表和路径表共用一格的文件，摘掉之后两张表都没有这个 `Arc`。

**关闭记录：**

1. 审了 `crates/fastab_engine/src/ir.rs` 相对 `d5506b4d` 的未提交 diff，对照的是本节。读了 `evict_oldest_spec`、`unlink_cached_arc`、`forget_idle_arc`、`arc_for_path`、`set_idle_since`、`release_idle`、`prune_dead_options`、三个加载入口、`insert_loaded`、`overlay_spec`、`idle_path`，以及 `files` 如何存相对路径。注释里「下一步才会接上」的句子删掉之后又审了一次，逻辑没动，`allow(dead_code)` 还在：`set_idle_since`、`release_idle`、`SPEC_IDLE_GRACE` 要等后面的步骤从生产代码调用。
2. 行为由本文件 11 个测试撑住，另加满员淘汰忘掉标记、以及另一棵树还握着的选项体。`cargo test -p fastab_engine --lib -- ir::` 52 个通过；非测试 lib 的 `cargo clippy -p fastab_engine -- -D warnings` 通过。补全组装、历史槽位、公共 AI 都没改。`MAX_CACHED_SPECS` 仍是 48。宽限 25 秒，测试注入 `Instant`：刚好 25 秒放，24 秒不放，`None` 再过一小时也不放。同一轮只写标记，不摘树。
3. `Arc<Spec>` 仍在原来的表里。命令文件在 `specs` + `loaded`，占一格。嵌套 `loadSpec` 在 `load_spec_cache` + `loaded`，占一格，不进 `specs`。版本文件只在 `path_specs`，不占 48 格。overlay 在 `pinned` + `specs`，不进 `loaded`。`unlink_cached_arc` 按 `Arc::ptr_eq` 从这四张表一起摘。满员淘汰只把 `specs` 或 `load_spec_cache` 变短当成成功，先忘掉这条路径的空闲标记再摘。空闲释放把 `path_specs` 变短也当成放下，临时 `Arc` 丢掉之后才 `prune_dead_options`。`files`、`names`、`version_cache` 不动。父存根是另一棵 `Arc`。
4. 还没跑到的边界：`set_idle_since` 不把命令名解成 `files` 里的真实路径，`heroku` 的文件是 `heroku/8.6.0.json`，调用方要传解析后的相对路径。同一路径上版本 `Arc` 和命令 `Arc` 同时在时，`arc_for_path` 先返回版本那棵。Replace 之后旧 bundle 可能还占着一个 `loaded` 槽。多个名字共用路径时 `files` 的迭代顺序不定，pin 的指针不会被摘掉。这些留给后面的步骤，不在这一步改。

## 2. 只有编辑缓冲的查找才续期，历史索引不算

**代码：** `ir.rs` 的三个加载入口；`lookup.rs` 的 `walk_spec` / `annotate_history_command`；`history.rs` 的 `build_index`。

**做法：**

- 注册表上加一个只在单次查找期间有效的开关，默认记录空闲触碰。`build_index` 在调用 `annotate_history_command` 之前关掉，构建结束恢复。用 `defer` 式的守卫，中途返回也要恢复。
- 编辑缓冲的 `walk_spec` 保持开关开着。三个加载入口在开关开着且这次确实拿到了 `Arc` 时，把该路径的 `idle_since` 清掉。
- 开关关掉时，加载入口仍把树放进原来的缓存，仍 `touch_loaded`，只是不把路径放进这次的触碰集，也不把 `idle_since` 清掉。
- `Engine::complete` 在结果组装完、查找上的 `Arc` 已经结束借用之后，按上面的三条更新 `idle_since`。这次调用不因为「刚刚不再使用」而摘掉树。摘掉只走 `release_idle`，并且 `now` 距 `idle_since` 已经满 25 秒。

**测试：**

- 历史行含 `tool child value`，当前缓冲是 `tool `。索引构建会把 `child` 的文件读进缓存。`complete` 返回后子文件仍在 `load_spec_cache`，`idle_since` 是这次的时刻。把 `now` 推过 25 秒再 `release_idle`，子文件才消失。历史槽位里仍能取出 `value`。宽限内再完成一次 `tool `，子文件仍在，已有的时刻不被改写；索引命中，不再读盘。
- 当前缓冲是 `tool child `。返回后子文件在缓存里，`idle_since` 为 `None`。
- 先 `tool child `，再 `git status`。子文件的 `idle_since` 变成这次 git 补全的时刻，宽限未到时 `Arc` 还在。
- 父存根的公共 AI 标记、子文件未进入时的存根行为，用现有夹具再跑，不改断言。

**这一步 review 还要看：** `without_hooks` 和空闲开关是两件事。历史跳过钩子，同时也不续期。缓冲查找里的 `generateSpec` / `jsLoadSpec` 换了新 `Arc` 时，续期记在新树上还是旧路径上要写清楚：路径仍是那份文件，续期按路径，不按指针。新 `Arc` 若没有放回注册表，不给它单独留一份缓存。

**关闭记录：**

1. 审了 `ir.rs`、`history.rs`、`runtime.rs` 相对 `d5506b4d` 的未提交 diff（`HEAD` 是 `a3a471e5`，这三份文件在那次版本号提交里没有再改）。对照的是「已定的行为」「谁算用过」、本节，以及第 1 步关闭记录。`lookup.rs` 读了 `walk_spec`、`next_spec_after_arg`、`root_spec_for_command`、`effective_fuzzy_for_tokens`。`public_ai.rs`、`worker.rs`、`hook_backend.rs` 的 `without_hooks` 没有被这一步改到。`cargo test -p fastab_engine --lib` 432 通过；非测试 lib 的 `cargo clippy -p fastab_engine -- -D warnings` 通过。第 1 步的 `unlink_cached_arc`、`release_idle`、`evict_oldest_spec`、`forget_idle_arc` 这次没有改行为。
2. 三个加载入口只在拿到 `Arc` 之后把解析后的相对路径放进 `idle_touched`（`ensure_loaded` 用 `files` 里的路径，`load_referenced_spec` / `load_relative_spec` 用解析后的路径，失败不记）。`build_index` 整段暂停记录，缓冲查找在这段之外加载自己的根命令。`complete_with_thread_session` 开头清掉上一请求的触碰，只有走完编辑缓冲才写 `idle_since`；两处 `history_only` 返回都不写。这次扫描不摘 `Arc`。测试锁住：暂停（含嵌套）不算在用、已有时刻不被改写、pin 的标记被扫掉且 `release_idle` 不换掉那棵 `Arc`、历史子文件和 `sudo` 前缀起算并留下、再补全 `tool ` 不改时刻、24 秒留 / 满 25 秒才放、历史槽位在放下后仍是 `value`。补全组装、排序、公共 AI 资格不在这次 diff 里。`history_only` 仍直接排除公共 AI。`ai_resolved_reference` 仍只由原来的路径 `loadSpec` 走进去时设置。
3. `Arc<Spec>` 仍在原来的表里。命令文件在 `specs` + `loaded`，占 48 格里的一格。嵌套 `loadSpec` 在 `load_spec_cache` + `loaded`，占一格，不进 `specs`。版本文件只在 `path_specs`，不占 48 格。overlay 在 `pinned` + `specs`，不进 `loaded`；扫描删掉它的标记。`generateSpec` / `jsLoadSpec` 换成的是查找上的新 `Arc`，不写回注册表，不另占一格；续期记在加载时的路径上。历史槽位里是 `String`。这一步的扫描只写 `idle_since`。满员淘汰仍先忘掉标记再按 `ptr_eq` 摘四张表。空闲摘除仍只在 `release_idle`。`files`、`names`、`version_cache`、hook 缓存、`GeneratorSession`、typed-hooks 没动。`MAX_CACHED_SPECS` 仍是 48。`CompleteRequest` 没有会话号。审的时候 `allow(dead_code)` 还在 `SPEC_IDLE_GRACE`、`set_idle_since`、`release_idle`、`idle_path`、`UnlinkedSpec.path_specs` 上。监督线程接上后，`SPEC_IDLE_GRACE`、`release_idle`、`UnlinkedSpec.path_specs` 的 allow 去掉了；`set_idle_since` 和 `idle_path` 仍只给测试用。
4. 还没跑到的边界：宽限内从 `git` 走回 `tool child `，代码会把已有的 `Some` 写成 `None` 并命中同一棵缓存 `Arc`，没有测试走这条「清掉待释放」。`history_only` 先走进子文件、下一次再做别的命令时，开头的清空会丢掉那次触碰；现有测试只断言当次标记不变。版本文件经 `Engine::complete` 续期没有引擎级测试。`generateSpec` / `jsLoadSpec` 不进注册表，没有空闲测试。克隆发生在暂停期间时标志不共享，测试是在暂停结束之后克隆的。守卫在 unwind 时恢复，没有 `catch_unwind`。同一路径上版本 `Arc` 和命令 `Arc` 并存时，`arc_for_path` 仍先返回版本那棵。只查历史时宽限不会开始也不会续上，只剩 48 格 LRU。监督线程归第 3 步。

## 3. 监督线程在宽限到期时放下

**代码：** `crates/fastab_engine/src/worker.rs` 的监督循环。`Registry::release_idle` 已在第 1 步。

**做法：**

- 生产宽限就是 `SPEC_IDLE_GRACE`（25 秒）。测试可以给监督循环一个更短的值，单元测试仍直接把 `now` 传进 `release_idle`，不靠睡眠。
- `rx.recv()` 改成 `recv_timeout`，超时时间是最早一份 `idle_since + grace` 距现在的时长。没有待释放文件时仍一直等。
- 超时醒来且没有新任务：对当前 `Engine` 调用 `release_idle(Instant::now(), SPEC_IDLE_GRACE)`。只剩 pinned 或无主标记时不产生 deadline。`engine` 为 `None`（上一次尝试被放弃）时走 `recv()`，不会定时唤醒。
- `recv_timeout` 收到任务时按现在的顺序处理。补全、接受记录、`ClearCaches` 都不被定时器丢掉，也不被合并逻辑误伤。`ClearCaches` 换上一份新注册表之后，旧的待释放时刻一起消失。
- 普通补全在尝试内部返回前按第 2 步盖上 `idle_since`，监督线程拿回引擎后调用 `release_idle(now, SPEC_IDLE_GRACE)`。刚盖上的时刻距现在不到 25 秒，这次不会摘掉；保留的旧标记若已到期，则先摘掉再处理下一份排队请求。之后的等待用同一条宽限。

**测试：**

- 纯函数：`idle_since` 距 `now` 24 秒不放，25 秒放。
- 监督循环用可注入的短宽限：一次补全让文件进入待释放，在没有新的 `Complete` 到达时，循环自己把它摘掉。断言发生在测试线程，不读取正在运行的桌面进程。
- 宽限等待期间塞进一次 `Complete`，这次补全先跑完，并且它若又走进该文件，文件不再待释放。
- `ClearCaches` 之后待释放集合为空。

**这一步 review 还要看：** 尝试线程在 `run_engine_attempt` 里独占 `Engine`。监督线程只在重新取得 `Engine` 所有权后调用 `release_idle`；成功路径调用在放回 `Option` 之前。`recv_timeout` 的剩余时间不会把等待阶段已经排队的补全推迟到宽限结束，但尝试返回后的释放仍先于下一份排队回访。

**关闭记录：**

1. 审了 `crates/fastab_engine/src/worker.rs` 的监督循环、`wait_for_engine_job`、尝试返回后的 `release_idle_specs`、`ClearCaches` / 接受记录分支、测试用 `InspectIdle`，以及三个新测试（`supervisor_releases_an_idle_file_without_another_completion`、`completion_during_the_grace_runs_and_clears_the_idle_mark`、`clear_caches_drops_a_pending_idle_file`）。对照读了 `ir.rs` 的 `release_idle`、`next_idle_deadline`、`UnlinkedSpec.path_specs`、`SPEC_IDLE_GRACE`、`set_idle_since` / `idle_path` 上仍在的 `allow(dead_code)`，以及 `idle_release_waits_out_the_grace_and_ignores_an_active_mark` 和 `next_idle_deadline_uses_the_earliest_pending_mark_and_skips_pins`。`runtime.rs` 读了 `release_idle_specs`、`next_idle_deadline`、`idle_file_state`、`clear_caches_and_report`、`complete_with_thread_session`。基线是本节、「已定的行为」、「放下时动哪些表」，外加第 1 步对 `release_idle` 的关闭描述。`release_idle` 的摘除条件、pin 跳过、临时 `Arc` 丢掉之后才 `prune_dead_options` 与第 1 步一致，这次只是生产路径开始调用它。`cargo test -p fastab_engine --lib` 436 通过是在 `map_err` 绑定改成 `|_disconnected|` 之前；那次改名只为过 `clippy::map_err_ignore`，随后 `cargo clippy -p fastab_engine -- -D warnings` 通过。审查看的是改名之后的代码。`history.rs` 相对 `HEAD` 只有第 2 步的 `pause_idle_touch`，这一步没有再改历史索引。
2. 行为由代码和上述测试撑住。`EngineClient::spawn` 传入 `SPEC_IDLE_GRACE`；短宽限只从 `spawn_with_idle_grace` 进监督循环，等待和 `release_idle` 用同一个值。没有待释放文件、`engine` 为 `None`、或只剩使用中的 `None` 标记时，`next_idle_deadline` 得到 `None`，循环回到 `recv()`。有最早期限时 `recv_timeout` 的剩余时间不会挡住已经在队列里的任务：补全、`RecordAcceptance`、`RecordScopedAcceptance`、`ClearCaches` 仍走原来的分支，合并逻辑只换掉更新的 `Complete`。`ClearCaches` 成功时 `clear_caches_and_report` 用 `load_registry` 换掉整份 `Registry`，旧的 `idle_since` 跟着旧表丢掉。尝试成功返回后不再调用 `note_idle_after_complete`；`complete_with_thread_session` 仍只在编辑缓冲走完后盖一次章，两处 `history_only` 提前返回仍不盖章。刚盖上的 `Some(now)` 或 `None` 短于宽限，返回后的那次 `release_idle` 不会摘。纯函数测试仍注入 `Instant`：未满宽限（含 24 秒）不放，满 25 秒放，`None` 再过一小时也不放，不睡 25 秒。监督测试睡的是注入的 200ms。三个新测试分别锁住：没有新的 `Complete` 时监督线程自己摘掉 `child`（摘之前断言还在缓存里且标记是 `Some`）；宽限 5 秒时插进来的 `tool child ` 在 1 秒内跑完，缓存还在且标记变成 `Some(None)`；`ClearCaches` 之后 `child` 不在缓存里且没有标记。补全组装、排序、历史槽位、公共 AI 资格不在这一步的监督改动里。`history_only` 仍在进入编辑缓冲之前返回。`Suggestion` / `CompleteResult` 里没有 `Arc<Spec>`。`MAX_CACHED_SPECS` 仍是 48。`CompleteRequest` 没有会话号。`release_idle` 没有按分配字节设门槛，也没有拆 `compute.json`。
3. 解析后的 `Arc<Spec>` 仍在原来的表里。命令文件在 `specs` + `loaded`，占 48 格里的一格。嵌套 `loadSpec` 在 `load_spec_cache` + `loaded`，占一格，不进 `specs`。版本文件只在 `path_specs`，不占 48 格。overlay 在 `pinned` + `specs`，不进 `loaded`。监督线程只在尝试线程已经把 `Engine` 经通道交回之后调用 `release_idle`（成功路径在写回 `Option` 的前一行；失败路径不调用）。`release_idle` 仍按 `Arc::ptr_eq` 从 `specs`、`load_spec_cache`、`path_specs`、`loaded` 一起摘，临时 `Arc` 丢掉之后才 `prune_dead_options`。pin 住的 `Some` 标记留在表上，`next_idle_deadline` 跳过它，也跳过使用中的 `None`，所以不会 `recv_timeout(0)` 空转。`Arc` 已经不在的过期标记仍会成为期限，醒来后 `release_idle` 删掉这条标记。满员淘汰仍先 `forget_idle_arc` 再按 `lru_slot` 摘，不把 `path_specs` 变短当成 LRU 成功。`files`、`names`、`version_cache`、hook 缓存、`GeneratorSession`、历史槽位没被空闲释放清掉。`registry_template` 只在 `load_registry` 或成功的 `ClearCaches` 之后更新，不从已经解析过的那份 `Engine` 抄回。放弃的尝试把 `engine` 设成 `None`，旧引擎留在尝试线程里直到该线程自己结束；下一次请求从模板重建，模板里没有解析后的树。`SPEC_IDLE_GRACE`、`release_idle`、`UnlinkedSpec.path_specs` 已从生产路径调用，没有 `allow(dead_code)`。`set_idle_since` 和 `idle_path` 仍只给测试用，`allow` 还在。
4. 还没跑到的边界：放弃尝试之后 `engine == None`，等待走 `recv()` 而不会进入超时臂，因此「超时且引擎为 `None` 时什么也不做」没有测试；同一线程上这个 `None` 分支也到不了，因为期限只在 `engine` 为 `Some` 时算出来。宽限等待期间到达的接受记录会立刻记账并且不改 `idle_since`，没有测试在等待中塞 `RecordAcceptance` / `RecordScopedAcceptance`。宽限在一次长尝试里走完时，返回后的 `release_idle` 会摘掉那个旧标记（已有的 `Some` 不会被这次盖章改写），没有测试把尝试拖过注入的宽限再断言返回时已经放下。`Arc` 已经没了的陈旧标记仍会醒一次，`next_idle_deadline` 的测试只覆盖了最早期限、`None` 和 pin，没有造一条悬空标记。第 2 步记下的「宽限内从 `git` 走回 `tool child `」由这一步的 `completion_during_the_grace_runs_and_clears_the_idle_mark` 在监督线程上盖住。

## 4. 整次补全的结果在放下再加载之后保持不变

**代码：** 不改建议的组装。这一步用 `Engine::complete` 把第 1 到第 3 步串起来，缺的断言补上。生产代码只在测试暴露缺口时改，改了就重审被碰到的步骤。

**做法：**

- 同一份注册表、同一缓冲，连续做：走进大文件的补全、一次没有走进它的补全、把 `now` 推过宽限、`release_idle`、再走进去。
- 用完整 `Vec<Suggestion>` 比较两次走进去的建议，覆盖顺序和所有字段，包括公共 AI 标记、kind、icon、acceptance scope 等。保留具体名字、优先级、隐藏项、参数提示和插入值的非空断言，避免空向量相等掩盖缺失。
- 父命令第一层在子文件已放下时仍只来自存根，不把子文件读回来。
- 公共 AI：未进入的路径存根仍排除，走进去之后的替换仍按第 3 步旧夹具的规则。空闲释放不给存根打 `ai_resolved_reference`。

**测试夹具：** 用小的多文件规格，不在单元测试里解析整份 `compute.json`。真实文件的体积放到第 5 步的测量。

**这一步 review 还要看：** 重新解析会再跑 `shrink_to_fit` 和选项 intern。同一份 JSON 两次解析，建议行相同，但 `Arc` 指针不同。测试要断言指针不同，避免以后把「放下」做成清一个标志位而树还在。

**关闭记录：**

1. 审了 `crates/fastab_engine/src/runtime.rs` 第 4 步新增的 `suggestion_rows`、`quiet_settings`、`complete_listed`，以及 `reload_after_idle_keeps_suggestion_rows_and_uses_a_new_arc`、`idle_release_leaves_public_ai_stubs_excluded`。顺着 `Engine::complete` → `complete_with_thread_session` 末尾的 `note_idle_after_complete`，再到测试直接调用的 `Registry::release_idle`；加载侧核对了 `ensure_loaded` 命中时记触碰、`load_referenced_spec` 把嵌套文件放进 `load_spec_cache` + `loaded`，以及父文件解析时不跟随子路径。建议行对照 `lookup.rs` 的 `collect_named`、`args_hint`、`should_add_space`、`hidden_item_is_visible`；`rank::apply_with_acceptance` 只排序，不改 `priority` 字段。公共 AI 对照 `public_ai_excludes_unentered_load_spec_stubs_and_entered_replacements`、`public_ai::context` 和 `static_path_node`。基线是本节、「已定的行为」、「放下时动哪些表」、「谁算用过」，以及第 1–3 步已关闭的记录。`HEAD` 是 `a3a471e5`。相对这份基线，`runtime.rs` 没有删除或改写已有行；生产代码里的新增是第 2、3 步已经关闭的 `begin_idle_completion`、`note_idle_after_complete`、`release_idle_specs`。第 4 步新增的符号都在 `mod tests`。`lookup.rs`、`public_ai.rs`、`rank.rs`、`generate.rs` 不在 diff 里。没有发现为这两个测试改过的生产函数，第 1–3 步的 review 不必作废。夹具是临时目录里的小 JSON，没有解析 `bundle/specs-ir/gcloud/compute.json`。没有 `sleep`；宽限是测试读到的 `idle_mark` 加上 `SPEC_IDLE_GRACE`（25 秒）。`cargo test -p fastab_engine --lib` 438 通过、0 失败。
2. 行为仍由代码和这两个测试撑住。补全结果、历史槽位、公共 AI 资格都没有放宽。同一份注册表走完「走进子文件 → `git status` → 把 `now` 推过宽限 → `release_idle` → 再走进去」。`suggestion_rows` 用向量顺序比较名字、说明、优先级、`args_hint`、`should_add_space`、hidden、`insert_value`。非空断言和收集代码一致：可选参数 `file` 得到 `[file]`，必填 `target` 得到 `<target>`，选项的可选 `level` 得到 `[level]`；`alpha` 的 `shouldAddSpace: true` 盖过「可选参数不加空格」，`zeta` 的 `shouldAddSpace: false` 盖过「必填参数要加空格」；优先级 40/80/60/10 原样留下；空查询的行都不是 hidden，`tool child secret` 上 `secret` 仍是 hidden，优先级 10，`insert_value` 为 `secret-now`。两次走进去的行相等，`cached_load_spec` 返回的是缓存 `Arc` 的克隆，测试又握着旧 `Arc`，所以只清标记通不过指针断言。`git status` 也会给已经缓存的 `tool.json` 起算；放下后 `tool` 不在 `specs`，`git` 仍是原来的 `Arc`，再补全 `tool ` 的行与放下前相同，说明仍是存根上的 `Stub child`，`child` 不回到 `load_spec_cache`。公共 AI 在放下前后都按旧夹具：`config`/`auth` 有候选，`compute`/`sql` 没有；`gcloud comp` 不加载；`gcloud config ` 给 `list`/`get` 打上候选；`gcloud compute ` 没有 `public_ai_context`，`instances`/`disks` 没有候选。缓存文件的 `ai_resolved_reference` 为假。存根保持 `LoadSpec::Path("gcloud/compute")` 且标志为假。放下后父 `Arc` 不变，子文件是新 `Arc`，再走进去的行与放下前相同。测试末尾的 `resolve_context` 只用来看行走克隆：标志为真，且不是缓存 `Arc`；存根仍为假。这次调用可能往 `idle_touched` 记一笔，测试到此结束，没有再盖章。两个测试都关掉历史加载。`release_idle` 不清历史槽位、hook 缓存或 `GeneratorSession`。`history_only` 仍在盖章之前返回。`MAX_CACHED_SPECS` 仍是 48。`CompleteRequest` 没有会话号。
3. 解析后的 `Arc<Spec>` 仍在原来的表里。`tool.json`、`git.json`、`gcloud.json` 在 `specs` + `loaded`，各占 48 格里的一格。`child.json` 和 `gcloud/compute.json` 不进 `specs`，在 `load_spec_cache` + `loaded`，各占一格。`gcloud/sql.json` 没有被读入。这两个测试没有版本文件。父存根不是单独的缓存项，它在父文件那棵 `Arc` 的子命令上。`git status` 之后 `release_idle` 仍按 `Arc::ptr_eq` 从 `specs`、`load_spec_cache`、`path_specs`、`loaded` 一起摘掉 `tool` 和 `child`；`git` 的标记是使用中的 `None`，不摘。公共 AI 那次先回到 `gcloud `，父标记是 `None`，只摘 `gcloud/compute.json`，父 `Arc` 不变，存根不会换成另一棵树，也不会被写上 `ai_resolved_reference`。`files` 和 `names` 还在。重新解析仍经 `load_spec_file_inner` 的 `shrink_to_fit` 和选项 intern。测试握着旧的 `Spec` `Arc`，选项弱引用可以还活着；指针断言做在 `Spec` 的 `Arc` 上。`resolve_context` 的行走克隆不写回注册表，不另占一格。这两个测试没有把缓存填到 48。`registry_template` 没有从已经解析过的引擎抄回。
4. 还没跑到的边界：版本文件经 `Engine::complete` 放下再加载后的建议行，这一步没有引擎级测试。`generateSpec` / `jsLoadSpec` 换上的行走 `Arc` 仍不进注册表，没有空闲测试。这两个测试直接调用 `Registry::release_idle`，没有再走监督线程的 `recv_timeout`；那条路径仍是第 3 步的测试。旧夹具里的 `walked.spec.names` 和 `!crossed_loaded_spec` 没有再断言；无 context、候选范围、存根路径、缓存标志，以及行走克隆不是缓存 `Arc`，已经锁住。放下之后没有重放 `gcloud comp` 和 `gcloud config `；父 `Arc` 指针相同，且放下后的 `gcloud ` 仍不读 `compute`。历史槽位在放下之后仍能取出原值，仍由第 2 步撑住；这一步为了比较建议行关掉了历史。宽限内走回同一文件、未满 25 秒不放，不在这两个测试里。旧 `Arc` 还活着时，选项池的 `strong_count` 没有断言。

## 5. 按目标总 review，并量一次内存

生产代码在前四步已经定下来。这一步对照整份 diff 和一次测量，不把新行为塞进这一步。

**逻辑：** 大文件的 `Arc` 在待释放到期后从四张表消失，选项弱引用归零，父存根还在，再次走进去是新 `Arc` 且建议行相同。

**进程：** 用系统分配器的进程，不用 `ftab`。`cargo test -p fastab_engine` 的测试二进制没有 mimalloc。驱动在测试或一个只链 `fastab_engine` 的小程序里：解析 `bundle/specs-ir/gcloud/compute.json`，读 `footprint -p`，调用 `release_idle` 放掉它，再读一次。安装着的桌面进程、输入法、终端不动。`ftab` 的 65 MB 水位留作对照，不要求它下降。

桌面分配器对大量小对象不一定把历史观测的约 50 MB 进程足迹增量全部退给系统。测量分别记录加载前、持有时和释放后的 `phys_footprint`。`allocated_bytes` 不含分配器元数据和字符串余量，与进程足迹不是同一指标；下降不足时如实记录，不能仅凭两者的差认定原因，不改结构体去追这个差。

**总 review：** 按本文开头的四条，覆盖 `ir.rs`、`lookup.rs`、`history.rs`、`worker.rs`，以及 `runtime.rs` 里 `complete` 返回前的那次扫描。

**关闭记录：**

1. 审了本文全文（目标、已定的行为、放下时动哪些表、谁算用过、不变量、第 1–4 步关闭记录、本节），对照 `HEAD` `a3a471e5` 的未提交 diff。diff 只有 `crates/fastab_engine/src/ir.rs`、`history.rs`、`runtime.rs`、`worker.rs`；`lookup.rs` 不在 diff 里，按调用链读过，没有改它。生产路径读了 `Registry` 的 `idle_since` / `suppress_idle_touch` / `idle_touched`、`SPEC_IDLE_GRACE`、`unlink_cached_arc`、`forget_idle_arc`、`arc_for_path`、`release_idle`、`note_idle_after_complete`、`ensure_loaded`、`load_referenced_spec`、`load_relative_spec`、`idle_path`，以及 `allocated_bytes` 上「不含分配器头和字符串余量」的注释。`history.rs` 读了 `build_index` 开头的 `pause_idle_touch`（包住整段，含行首 token 的 `get_arc`）。`runtime.rs` 读了 `complete_with_thread_session` 开头的 `begin_idle_completion`、两处 `history_only` 提前返回、返回前的 `note_idle_after_complete`，以及 `release_idle_specs`。`worker.rs` 读了监督循环、`wait_for_engine_job`、成功返回后的 `release_idle_specs`、失败臂把 `engine` 设成 `None`、`rebuild_engine` 只克隆模板、`ClearCaches`。`lookup.rs` 读了 `walk_spec` 进入子命令时的 `load_referenced_spec`（约 1080 行）、`next_spec_after_arg` 的路径 `load_referenced_spec` 和 `get_arc`（约 1340、1356 行）、`versioned_or_bundled` 的 `get_versioned_arc` / `get_arc`（约 2418、2422 行）、`effective_fuzzy_for_tokens` 的 `get_arc`（约 2530 行）；`apply_generate_spec` / `apply_js_load_spec` 只换成查找上的新 `Arc`，不写回注册表。`public_ai.rs` 的 `context` 没有被这个 diff 改到。CI 是 `.github/workflows/ci.yml` 的 `cargo test --workspace --locked`，没有 `--ignored`；仓库脚本里也没有 `--ignored` / `--include-ignored`。`gcloud` 不在 `index.json` 的 `versioned` 里，`files` 指向 `gcloud.json`；`gcloud.json` 根上没有 `loadSpec`；`gcloud/compute.json` 顶层 59 个子命令加 14 个选项，含 `instances`，根上也没有 `loadSpec`。第 5 步新符号都在 `runtime.rs` 的 `mod tests`：`phys_footprint_bytes`、`assert_gcloud_compute_stub`、`leave_gcloud_compute_and_release`、`compute_json_idle_release_footprint`。没有为这个测试改生产函数，也没有给选项池加生产访问器。第 1–4 步已经审过的生产逻辑这次没有被再改，那些 review 仍然有效。审查没有重跑足迹和测试套件；数字是实现时测得的那一次，代码路径和它一致。
2. 行为仍由代码和已有测试撑住。补全组装、历史槽位、公共 AI 资格都没有放宽。`lookup.rs`、`public_ai.rs`、`rank.rs` 不在 diff 里。`Suggestion` / `CompleteResult` 里仍然没有 `Arc<Spec>`。`history_only` 仍在 `public_ai::context` 里直接排除。存根上的 `LoadSpec::Path` 不会被空闲释放写成 `ai_resolved_reference`。历史槽位仍是 `String`；`release_idle` 不清历史索引、hook 缓存或 `GeneratorSession`。`MAX_CACHED_SPECS` 仍是 48，hook 缓存仍是 512，generate LRU 仍是 32，历史索引上限仍是 32。`CompleteRequest` 没有会话号。`Spec` / `OptionSpec` / `ArgSpec` 的字段布局不在这次 diff 里。宽限常量是 25 秒，生产 `EngineClient::spawn` 把 `SPEC_IDLE_GRACE` 传进监督循环；短宽限只从 `spawn_with_idle_grace` 进来。任何放下都等满这次传入的宽限，同一次补全返回前只盖章、不摘树。测试注入 `Instant`，不睡 25 秒。`idle_release_waits_out_the_grace_and_ignores_an_active_mark` 仍是未满（含 24 秒）不放、满 25 秒放、`None` 再过一小时也不放。已有的 `Some` 时刻不被后来的无关补全改写；`note_idle_after_complete` 见到 `Some(Some(_))` 就跳过。`build_index` 的暂停包住整段函数，包括行首 `get_arc`。两处 `history_only` 返回都在 `note_idle_after_complete` 之前。监督线程在尝试成功交回之后调用 `release_idle_specs`，不再调用 `note_idle_after_complete`。超时或 panic 把 `engine` 设成 `None`，不从外面拆那份引擎；下一次从 `registry_template` 重建，模板来自 `load_registry`，不从已经解析过的引擎抄回。`engine == None` 的超时臂在同一线程上到不了：`wait_for_engine_job` 只有 `engine` 为 `Some` 且算出期限时才 `recv_timeout`，否则是 `recv()`。这和第 3 步记下的一样。`set_idle_since` 和 `idle_path` 仍是 `#[cfg_attr(not(test), allow(dead_code))]`。`SPEC_IDLE_GRACE`、`release_idle`、`UnlinkedSpec.path_specs` 已从生产路径使用，没有这行 allow。选项池仍是私有字段；足迹测试没有为了看 `strong_count` 加访问器，归零仍由 `idle_release_drops_option_bodies_with_the_tree` 撑住。`#[ignore = "parses bundle/specs-ir/gcloud/compute.json"]` 使默认 `cargo test` 不解析这份文件。`cargo test -p fastab_engine --lib`（不带 `--ignored`）438 通过、0 失败、1 忽略，25.34 秒。忽略测试本身 3.55 秒通过，打印 `COMPUTE_IDLE rows=73 allocated_bytes=2762496 footprint_menu=16466424 footprint_held=56869440 footprint_released=15843880 returned=41025560`。`rows=73` 是 `gcloud compute ` 的建议行数，和顶层 59 个子命令加 14 个选项一致，并且断言了名字 `instances`。`footprint_menu` 在 `gcloud ` 之后、`compute.json` 解析之前。`footprint_held` 时注册表单独持有这棵树：测试先丢掉临时 `Arc` 再采样。`footprint_released` 在 `gcloud ` 起算宽限、`release_idle(marked + SPEC_IDLE_GRACE)` 摘掉该文件之后；父存根被检查过，那份父 `Arc` 在采样前已经丢掉。`returned = held - released = 41025560`。`released`（15843880）回到菜单基线（16466424）附近。树的 `allocated_bytes` 是 2762496，不含分配器头和字符串余量；进程足迹的历史「大约 50 MB」不是这个计数。这次足迹下降大于 `allocated_bytes`，也和从菜单基线抬上去的幅度一致。页退回来了。不改结构体，不提高 LRU 或缓存，不换分配器。`ftab` 的 65 MB 水位留作对照，不要求它下降。指针断言 `!Arc::ptr_eq` 在两次采样之后的另一次放下再加载上，而且测试一直握着上一棵 `previous`，再拿新的 `renewed` 比较；没有在两份 `Arc` 都丢掉之后比地址。测量那一轮的临时 `Arc` 在 `release_idle` 之前已经丢掉，那一轮不做指针比较。
3. 解析后的 `Arc<Spec>` 仍在原来的表里。命令文件在 `specs` + `loaded`，占 48 格里的一格。嵌套 `loadSpec`（`gcloud/compute.json`）在 `load_spec_cache` + `loaded`，占一格，不进 `specs`。版本文件只在 `path_specs`，不占 48 格。overlay 在 `pinned` + `specs`，不进 `loaded`。`generateSpec` / `jsLoadSpec` 以及路径替换后的行走节点是查找上的新 `Arc`，不写回注册表，不另占一格；续期记在加载时的路径上。历史槽位是 `String`。hook 缓存、`GeneratorSession`、`version_cache` 里的版本文本都不持有这份注册表 `Arc`。`compute.json` 这次测量里由注册表的 `load_spec_cache` 和 `loaded` 持有；父 `gcloud.json` 是另一棵 `Arc`，在 `specs` + `loaded`。空闲释放和满员淘汰都走 `unlink_cached_arc`，按 `Arc::ptr_eq` 从 `specs`、`load_spec_cache`、`path_specs`、`loaded` 一起摘。满员淘汰先 `forget_idle_arc`，只把 `specs` 或 `load_spec_cache` 变短当成成功，不把 `path_specs` 变短当成 LRU 成功。空闲释放把 `path_specs` 变短也当成放下；临时 `Arc` 丢掉之后才 `prune_dead_options`。pin 住的 `Arc` 不摘。`files`、`names`、`version_cache` 不动。父存根留在父文件那棵 `Arc` 的子命令上，`LoadSpec::Path` 还在。没有路径的 `insert` 不参与。`registry_template` 不从已经解析过的引擎抄回。
4. 第 1–4 步记下、现在仍然成立、这一步不要求补上的边界：`set_idle_since` 不把命令名解成 `files` 里的真实路径，调用方要传解析后的相对路径。同一路径上版本 `Arc` 和命令 `Arc` 同时在时，`arc_for_path` 仍先返回版本那棵。多个名字共用一条路径时，`files` 的迭代顺序不定；pin 的指针不会被摘掉。Replace 之后旧 bundle 可能还占着一个 `loaded` 槽。宽限内从别的命令走回同一文件会把 `Some` 写成 `None` 并命中同一棵缓存 `Arc`，监督测试 `completion_during_the_grace_runs_and_clears_the_idle_mark` 盖住了子文件这条，纯 `Engine::complete` 的 24 秒保留仍是 `leaving_a_child_starts_its_grace_and_keeps_the_arc`。`history_only` 先走进子文件时当次标记不变已有测试；下一次别的命令会在开头清掉那次触碰，没有单独再断言。版本文件经 `Engine::complete` 放下再加载后的建议行，没有引擎级测试。`generateSpec` / `jsLoadSpec` 的行走 `Arc` 不进注册表，没有空闲测试。克隆发生在暂停期间时标志不共享，现有测试是在暂停结束之后克隆的。守卫在 unwind 时恢复，没有 `catch_unwind`。只查历史时宽限不会开始也不会续上，只剩 48 格 LRU。放弃尝试之后「超时且 `engine == None`」什么也不做，没有测试；同一线程上这个分支到不了。宽限等待期间的 `RecordAcceptance` / `RecordScopedAcceptance` 会立刻记账并且不改 `idle_since`，没有测试。宽限在一次长尝试里走完时，返回后的 `release_idle` 会摘掉未被这次盖章改写的旧标记，没有把尝试拖过注入宽限的测试。`Arc` 已经不在的陈旧标记仍会让监督线程醒一次，`next_idle_deadline` 的测试没有造这条悬空标记。第 4 步的两个引擎测试直接调用 `Registry::release_idle`，不走监督线程的 `recv_timeout`。旧夹具里的 `walked.spec.names` 和 `!crossed_loaded_spec` 没有在放下之后再断言。放下之后没有重放 `gcloud comp` 和 `gcloud config `。历史槽位在放下之后仍能取出原值，仍由第 2 步撑住。旧 `Arc` 还被测试握着时，选项池的 `strong_count` 没有断言。这一步足迹测试新的边界：它直接调用 `Registry::release_idle`，不走监督线程的定时器，那条路径仍是第 3 步的测试。选项池的 `strong_count` 从 `runtime.rs` 看不见，这个测试也没有查；归零仍引用 `idle_release_drops_option_bodies_with_the_tree`。采样是一次调试进程、系统分配器、没有 mimalloc，不是安装着的桌面进程，也不是 `ftab`。字节差只 `println`，不断言，所以以后页退不干净时这个忽略测试仍会通过。测量那一轮没有对父 `Arc` 做 `ptr_eq`，父指针仍由第 4 步的公共 AI 测试锁住；这个测试只断言父命令仍在缓存里、存根路径还在、`ai_resolved_reference` 为假。第二次放下再加载才比指针，并且当时测试还握着上一棵 `Arc`。
