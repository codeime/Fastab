# loadSpec 按进入时机加载，查找不再深拷贝规格

日期：2026-09-29。对照代码：本仓库 `crates/fastab_engine`，以及 `/Users/guobing/Desktop/my/amazon-q-developer-cli` 的 `packages/autocomplete-parser`。

两件事。第一，把静态 `loadSpec` 从「读入父文件时整棵接上」改成 q-cli 的进入时机：只有已经走完的那个子命令或参数才读它指向的文件。第二，查找在列出建议和走进子命令时不再深拷贝规格树。q-cli 拿着规格上的引用往下走，排序的是建议行。省的都是内存。有子文件的命令都走第一条路径，量得出来的几乎只有 `gcloud`。第二条去掉的是每次补全的临时副本。

## 目标

光标停在 `gcloud` 第一层时，进程里只有 `gcloud.json` 的存根（名字和说明）。走进 `gcloud compute ` 才解析 `gcloud/compute.json`。`sql` 等没走进去的分组留在磁盘上。

已经在 `gcloud compute` 里面时，仍解析整份 `compute.json`。这一份不拆。

没有子文件的命令（`git` 等）补全结果与现在相同。

列出建议和走进子命令时，不再为了排序或下降把 `Spec` / `OptionSpec` 深拷贝一份。建议的名字、顺序、优先级、隐藏项和公共 AI 标记与现在相同。

这一项不改下面那张基线表。`gcloud compute` 仍会解析整份 `compute.json`，进程值仍由那次解析峰值决定。深拷贝去掉之后，同一次补全不再多一份子树字符串；桌面进程的系统分配器本来就会退回这份临时页，`ftab` 的 mimalloc 也不再因为这次复制抬高水位。水位的上限仍是解析峰值。

## 已测量的基线

引擎进程，含 hook 目录，底约 12 MB：

| 光标 | 现在 | 按进入时机只放该有的文件 |
| --- | ---: | ---: |
| `gcloud` 第一层 | 67 MB | 12 MB |
| `gcloud sql` | 67 MB | 14 MB |
| `gcloud compute` | 67 MB | 65 MB |

753 个命令里会再拉子文件的有 15 个，多出来的文件 15.1 MB，其中 13.8 MB 是 `gcloud`。其余每个不超过 0.4 MB。

`compute.json` 里 10415 个选项只有 1206 个唯一，选项正文去重后 0.46 MB；整个 `gcloud` 去重后 1.51 MB。桌面进程用系统分配器，解析峰值会退（本会话 126 MB 回到 71 MB）。`ftab` 用 mimalloc，峰值页不退，所以上表的 67 MB 含解析峰值。目标审查要同时看「子文件有没有被读」和「桌面常驻是否只留下走进去的那一份」。

## 不变量

- 注册表 LRU 仍是 48。子文件占同一组名额，不另开缓存，不改成 q-cli 那种无上限 `specCache`。
- hook 缓存 512、generate LRU 32、历史条数上限不动。
- 不预热 `MOST_USED_SPECS`。q-cli 在 `packages/autocomplete/src/main.tsx` 里启动后预热约 32 个根规格，这里不加。
- 不拆 `compute.json`，不改 typed hook 懒解析，不把 `ftab` 的分配器换成系统分配器。
- `fastab_util` 不引入 AppKit；IME 依赖栈和 AX 所有权不动。
- 正在输入的那个词不触发加载。查找的 `limit` 已经停在这个词之前，与 q-cli「最后一个 token 不 `updateState`」相同，保持这个边界。
- 父节点列表上的名字和说明来自存根。不为了补说明去读子文件。`gcloud.json` 的存根本身带说明。
- 字符串 `loadSpec` 替换当前节点时保留父节点的 `names`，与现在的 `replace_spec_with_loaded` 相同（`chezmoi git`、`pass grep` 仍按命令行上的写法找到节点）。
- 子文件缺失或解析失败时保留存根，这次补全继续。不让一个分组失败把整份父规格丢掉。
- 动态 `jsLoadSpec` / `generateSpec` 仍在走进节点之后调用，不跟静态路径混在一起。
- 内联 `LoadSpec::Inline` 已经在文件里，读文件时展开，不延后。
- 建议行的顺序、`dependsOn` 带来的优先级 75、隐藏项和公共 AI 标记与改前一致。
- 补全不写脏注册表里的规格。生成规格、动态 `jsLoadSpec` 和延后的路径替换得到一个新的 `Arc`，父节点缓存里的那个 `Arc` 保持原指针和原内容。

## q-cli 里要照做的行为

`packages/autocomplete-parser/src/parseArguments.ts` 的 `updateStateForLoadSpec`：

- 根规格自己的 `loadSpec` 在开 walk 时解析一次。
- 子命令的 `loadSpec` 只在这个词已经走完（后面还有 token，含末尾空格）时读取那一个位置。
- 参数上的 `loadSpec`、`isCommand`、`isScript`、`isModule` 同样只在该参数词走完之后读。
- 测试 `cmd loadSpec ` 只多调用一次 `loadSubcommandCached`。递归用例每深入一层多读一份，不读兄弟。

不要照做的：无上限 `specCache`，`preloadSpecs()`，以及 `getStaticSuggestions` 的 `memoizeOne`。最后这个会把上一份静态建议行留在内存里，这里不留。

要照做的只有下降和列表本身：`packages/autocomplete/src/suggestions/index.ts` 排序建议对象，不克隆规格节点，选项的 `priority` 写在建议行上。包在外面的 `memoizeOne` 不照做。

## 现状

`load_spec_file_inner` 解析完一份 JSON 后调用 `resolve_spec_references`，再递归每个子命令和参数。`LoadSpec::Path` 在这里就被读进来并替换节点。`command_map_loads_versioned_alias_and_hides_nested_implementation_files` 要求 `Registry::get("docker")` 之后 `compose` 下面已经有 `up`。

查找走完一个子命令时（`lookup.rs` 里 `index < limit` 且 `find_subcommand` 命中）只调用 `apply_js_load_spec`。静态路径当时已经被接进树里，所以这里不再读盘。参数路径则依赖读文件时填好的 `resolved_spec`。

历史用 `annotate_history_command`，`limit` 是整行长度，没有「正在输入的词」。一行历史若进入了某个子命令，应该读那一份，不读兄弟。这和 q-cli 历史解析一致。

`public_ai::static_path_node` 把仍带 `load_spec` 的节点排除在字面公共路径之外。现在提前解析还会打上 `ai_resolved_reference`。延后之后，没走进去的存根仍带 `LoadSpec::Path`，结果应仍被排除。走进去并替换之后，替换结果要带上与现在相同的 `ai_resolved_reference`，避免公共 AI 资格变宽。

查找另有三处深拷贝，都在 `lookup.rs`：

- 走完一个子命令时 `current = Arc::new(next.clone())`。`next` 是父节点 `subcommands` 里的 `Spec`，`clone` 会复制这棵子树的名字、说明和嵌套子命令。选项本体是 `Arc`，不在这份副本里再摊开。
- 列出子命令时 `current.subcommands.clone()`，列出附加建议时 `additional_suggestions.clone()`，只为了按名字排序。
- 列出选项时 `collect_option_suggestions` 对每个 `OptionSpec` 做 `option.clone()`，把 `option_priority` 写进副本再排序。`gcloud compute` 有 10415 个选项槽，约 1206 份去重后的本体，每次列出选项都会把共享本体重新摊开。

持久选项和已经出现在命令行上的旗标也是拥有的 `OptionSpec`。它们的数量是沿途合并的持久旗标加上本词之前输入过的旗标，不是整份选项表。这两处保持现状。

第 1 步到第 3 步继续用现在的方式持有当前节点，可以继续克隆。去掉深拷贝是第 4 步和第 5 步，避免和第 1 步同时改所有权。

## 每步关闭规则

顺序固定：实现 → 独立 review → fix → 对这次 fix 再 review。review 没通过不能进入下一步。fix 本身再出问题就继续 fix，然后重新 review，不把上一次 review 当作对 fix 的通过。

全部步骤关闭之后，再按本文「目标」和「不变量」做一次总 review → fix → 对 fix 再 review。总 review 不通过不算做完。

每步 review 要看实际 diff 和受影响调用链，并记下：

1. 审了哪些文件，对照的基线是什么。
2. 这一步的行为是否仍由代码和测试撑住。补全结果、历史槽位、公共 AI 资格有没有被悄悄放宽。
3. 谁持有解析后的 `Spec`，LRU 名额算在哪，失败和环路时存根还在不在。
4. 还没跑到的边界。

review 和实现分开做。不能用一句「review 通过」代替上面四条。后续改动碰到已经审过的逻辑，原 review 作废，重审。

状态用：待实施、实施中、待 Review、Review 未通过、已关闭。

## 执行顺序

| 步骤 | 事项 | 状态 |
| --- | --- | --- |
| 0 | 冻结契约和基线 | 已关闭（本文） |
| 1 | 子命令路径改到走进去才读，并计入现有 LRU | 已关闭 |
| 2 | 参数路径同样延后 | 已关闭 |
| 3 | 失败、环路、名字、持久选项、版本规格、公共 AI、历史 | 已关闭 |
| 4 | 列表排序不再深拷贝子命令、附加建议和选项 | 已关闭 |
| 5 | 下降时与子命令共享 `Arc`，不再克隆子树 | 已关闭 |
| 6 | 按目标总 review，并量一次内存 | 已关闭 |

## 1. 子命令路径改到走进去才读

**代码：** `crates/fastab_engine/src/ir.rs` 的 `resolve_spec_references`、`load_spec_file_inner`；`crates/fastab_engine/src/lookup.rs` 里走完子命令、调用 `apply_js_load_spec` 的那一段。

**做法：**

- 读文件时只处理当前节点自己的 `LoadSpec::Path`（对应 q-cli 开头对根对象 `loadSpec` 的那一次），以及内联对象。不再递归子命令上的路径。
- 子命令节点保持 `LoadSpec::Path`。
- 查找在 `index < limit` 且命中子命令时，若该节点是路径，按相对路径读那一份文件，用 `replace_spec_with_loaded` 得到当前节点，然后再跑现有的 `apply_generate_spec` 和 `apply_js_load_spec`。
- 读入的子文件自己的子命令路径同样不展开。
- 按路径放进现有 48 格 LRU。父规格仍是存根，不把子树拼回父节点的缓存副本。下一次按键命中缓存，不重复解析。
- 改 `command_map_loads_versioned_alias_and_hides_nested_implementation_files`：`get("docker")` 之后 `compose` 仍是存根，下面没有 `up`。另加补全测试：只有存根时 `gcloud ` 仍列出 `compute`；提供 `compute.json` 后 `gcloud compute ` 列出其中的子命令；这次不把 `sql.json` 放进已加载集合。

**关闭：** 这一步的测试通过，review、fix、对 fix 的 review 都记下。`git` 一类无子文件命令的现有补全测试不改预期。

### 第 1 步记录

Review 先看到两条问题，修完再审过。

1. 审了 `crates/fastab_engine/src/ir.rs` 的解析、`load_referenced_spec`、LRU 和 snapshot 换代，以及 `lookup.rs` 走进子命令的那一段。基线是改前「读父文件时把子路径接进树」。
2. `gcloud ` 和 `gcloud comp` 不读子文件；`gcloud compute ` 只读 `compute.json`，`sql` 不进缓存。父存根不被写回。列出子命令时 `load_spec` 还在的行不会标成公共 AI；走进去之后打上 `ai_resolved_reference`。历史槽只在替换后的参数上记，名字仍是命令行写法。`git` 补全预期没改。
3. 子文件的 `Arc` 在 `loaded` 里，占 48 格里的一格，并用相对路径记在 `load_spec_cache`。不把子文件的名字放进命令表。命令表里已有的同一文件和那个命令共用一格。钉住的 dev overlay 不拿来冒充这份文件，改从 snapshot 读进路径缓存。缺失或解析失败时 walker 留存根。换代之后，树上还有没校验过的 `LoadSpec::Path` 就不缓存这次读到的父规格；没有这种路径、字节又相同的文件仍可加载。
4. 目录 inode 不变、只改子文件字节：父规格仍会缓存，SHA 要等走进那个子文件才核对。发布走的是换目录。参数路径在这一步仍是读父文件时展开。`generate()` 不带 registry，跟不了存根；生产补全会传入。版本文件仍在单独的 `path_specs`，不进这 48 格。走进去时仍会克隆整份已加载规格，第 5 步再去。已经加载过的命令被 overlay 换掉后，旧 `Arc` 留在 `loaded` 里，到下一次驱逐才丢掉。

Fix：`load_referenced_spec` 原先只把引用拼成 `引用.json`。命令表键优先，和 `resolve_reference_path` 一致，所以 `alias` → `nested/alias.json` 不会去读旁边的 `alias.json`。`command_name_load_spec_follows_the_mapped_file` 用一份 decoy 文件和 overlay 钉住。换代门放在 `load_snapshot_file_inner`：稳定代次不读那些子文件。

对这次 fix 再看：解析后的相对路径才是缓存键；共用命令格时不写入路径缓存；钉住的名字返回的是 bundle 文件，不是 overlay。两个命令共用一个文件时，按路径反查命中哪一个名字取决于表的迭代顺序，当前包里没有这种条目。`cargo clippy -p fastab_engine -- -D warnings` 通过。第 1 步关闭。参数路径仍急切；第 2 步改到它时，这一段要重审。

## 2. 参数路径同样延后

**代码：** `resolve_arg_spec`，以及 `lookup.rs` 的 `next_spec_after_arg`。

**做法：**

- 文件加载时不再把参数上的 `LoadSpec::Path` 填进 `resolved_spec`。内联对象仍在加载时展开。
- `next_spec_after_arg` 在参数词已经走完时再按路径读取，规则与第 1 步同一套缓存。
- `isCommand`、`isScript`、`isModule` 仍走 `registry.get_arc`，不提前读别的根规格。
- 测试：参数词还在输入时不读该路径；词走完后读且只读那一份。现有 `isCommand` 重启解析的测试预期不变。

**关闭：** 同第 1 步。第 1 步已审逻辑若被碰到，第 1 步 review 作废并重审。

### 第 2 步记录

第 1 步里「参数路径仍在读父文件时展开」被这一步替换，那一句作废。重审的范围是 `resolve_arg_spec`、`resolve_snapshot_arg_spec` 和 `next_spec_after_arg`，加上第 1 步的加载入口有没有被改坏。

1. 审了上述三个函数，以及 `load_referenced_spec` 和换代门。基线是 q-cli：参数词走完才读那一份；`isCommand` / `isScript` / `isModule` 仍只在词走完后 `get_arc`。
2. `tool value` 不读 `positional-target`；`tool value ` 读它，不读 `profile-target`。`--profile value`、`--profile=value`、`-pvalue` 在词还没走完时不读，走完后只读 profile 那一份。`isCommand`、`isModule`、内联参数、`loadSpec` 优先于 `isCommand`、`--` 之后的参数路径，这些测试预期没改。子命令那组测试（`gcloud`、换代、命令名映射）仍然通过。公共 AI 在参数切换时仍因 `crossed_loaded_spec` 退出。历史里 `sudo curl` 仍记在内层规格下。
3. 参数上的路径不写入 `resolved_spec`，也不写回父节点。走完后用第 1 步同一个 `load_referenced_spec`：命令文件共用那一格，嵌套文件占自己的一格。加载失败时这次补全留在原节点，该词记成普通参数值。内联对象仍在读文件时展开。换代且参数上还有未读的 `LoadSpec::Path` 时，父规格不进缓存（`changed_argument_load_spec_does_not_cache_parent`）。环路 `a → b → a` 按词各读一份，不会在一次加载里递归。
4. `generate()` 仍不带 registry，跟不了路径。选项本体上的 `loadSpec`（不是选项参数）本来就不会在读文件时展开，walker 也不跟。两个命令共用一个文件时的名字选择仍和第 1 步一样。

没有需要再改的行为。`cargo test -p fastab_engine --lib` 里 `ir::`、`lookup::`、`history::`、`public_ai::` 和换代用例通过。`cargo clippy -p fastab_engine -- -D warnings` 通过。第 2 步关闭。

## 3. 失败、环路和相邻行为

**代码：** 第 1、2 步的加载入口，`replace_spec_with_loaded`，`history.rs` 的 `annotate_history_command` 调用，`public_ai.rs` 的 `static_path_node`。

**做法：**

- 子文件缺失、读失败或 JSON 损坏：保留存根，补全继续，记一条 warn。
- 环路（A 的路径指向 B，B 再指回 A）停止跟随，保留当前存根。加载栈不能在延后之后失效。
- 替换时保留父节点 `names`。持久选项和解析指令仍用走进去之后现有的合并。
- 版本规格仍只加载解析到的那一个版本文件；文件内部的子路径遵守第 1 步。
- 没走进去的存根因为还带着 `LoadSpec::Path`，不能变成公共 AI 的字面路径。替换后的节点保持现在的 `ai_resolved_reference` 含义。
- 历史行 `limit` 为全长。只进入 `gcloud sql` 的历史不加载 `compute.json`。槽位仍按走进去之后的规格记，`sudo curl <url>` 仍记在 `curl` 下。

**关闭：** 上述每条都有测试。review、fix、对 fix 的 review 都记下。

### 第 3 步记录

加载函数沿用第 1、2 步。这一步补测试。公共 AI 的第一版夹具没过：菜单上只有一条不带 `loadSpec` 的子命令，`validate_provenance` 在合格行少于 2 时会清掉整个 context，看不出存根是被排除的还是被这条门槛清掉的。夹具改成 `config` 和 `auth` 两条静态子命令，`compute` 里也放 `instances` 和 `disks`。这是夹具，不是加载行为。

1. 审了 `ir.rs` 的失败、根环路、子命令环路和版本文件测试，`lookup.rs` 里走进子命令之后的名字、持久选项、解析指令和公共 AI，以及 `history.rs` 的全行标注。对照的是第 1、2 步已经关闭的进入时机，加上本节列出的失败、环路、名字、版本、公共 AI 和历史。`load_referenced_spec`、`replace_spec_with_loaded`、`next_spec_after_arg`、`static_path_node` 和 `annotate_history_command` 这次没有改。
2. 缺失子文件留存根，walker 仍列出存根自己的 `still-here`，父规格仍在缓存，warn 是一条 `loadSpec target missing`。损坏 JSON 同样留存根，warn 是一条 `loadSpec target failed`，父规格不被丢掉。根上的 `A → B → A` 在加载栈停住：`wrapper` 的名字还在，说明来自已经解析完的 B，`leaf` 在，A 和 B 不进命令表。子命令环路按词各读一份，再走进去命中同一份缓存 `Arc`，父存根不写回。`child` 的名字留在命令行写法上，说明和子命令来自加载后的文件。持久选项把父级 `--global` 和文件里的 `--inner` 合在一起。子文件自己有 `parserDirectives` 时整份换上，没写的字段不从父级补；子文件没有该字段时沿用父级，所以 `keep value ` 之后不再列出选项。版本检测选中 `8.0.0` 时不解析损坏的 `8.6.0`，其中的 `old` 保持路径存根，走进去才读 `heroku/old.json`。`gcloud ` 给 `config` 和 `auth` 打公共标记，不给 `compute` 和 `sql`。`gcloud compute ` 的 context 为空，`instances` 和 `disks` 都没有标记。历史行 `gcloud sql prod` 把 `prod` 记在 `sql` 槽上；存根没有参数，所以这个值是替换之后的参数记的。`compute.json` 不进缓存。`sudo curl <url>` 仍在 `curl` 下。
3. 失败不往 `loaded` 里加节点，父存根的 `Arc` 还在原来的命令格。环路再进入时路径缓存是同一个 `Arc`；walker 仍持有自己的克隆，第 5 步再去。版本文件在 `path_specs`，不占 48 格；只有走进去的子路径占一格。`ai_resolved_reference` 只打在这次 walk 的克隆上，缓存文件和父存根都不是。持久选项仍是 walk 上的拥有副本，不写回注册表。
4. 文件在但读不了（例如权限）走和损坏 JSON 相同的 `Err` 分支，这次没有单独造一个权限错误。参数路径的缺失和损坏用同一个 `load_referenced_spec`，留在原节点的行为在第 2 步和 `load_spec_cycle_and_missing_reference_are_safe`。选项本体上的 `loadSpec` 仍不跟随。公共 AI 少于两条合格行时 context 会被清掉；只有存根的菜单因此没有 context，夹具用两条静态子命令避开这个门槛。目录 inode 不变、只改未读子文件字节，仍要等走进那个文件才对 SHA。

对这次夹具修正再看：生产代码没有改动。`gcloud ` 的 context 还在，两条静态子命令有标记，两个存根没有，`compute.json` 和 `sql.json` 都不在缓存。`gcloud config ` 的 `list` 和 `get` 有标记。`gcloud compute ` 有两条子命令，context 仍为空，两条都没有标记。walk 克隆的 `ai_resolved_reference` 为真，缓存文件和父存根为假，`sql` 仍未加载。`cargo test -p fastab_engine --lib` 里 `ir::`、`lookup::`、`history::`、`public_ai` 共 151 个通过。`cargo clippy -p fastab_engine -- -D warnings` 通过。第 1、2 步的 review 仍然有效。第 3 步关闭。

## 4. 列表排序不再深拷贝

**代码：** `crates/fastab_engine/src/lookup.rs` 里列出子命令、附加建议和 `collect_option_suggestions` 的三段。

**做法：**

- 子命令和附加建议按下标排序，再按这个顺序交给 `collect_named`。不为了排序 `clone` 出 `Spec` 或 `SuggestionSeed`。
- 选项不再 `option.clone()`。优先级仍用 `option_priority` 计算，写到建议行的 `priority` 上，不写回 `OptionSpec`。
- 排序键仍是 `cmp_named_names`：主名忽略大小写，相同再用字节序。`dependsOn` 未满足时的优先级 75 保持。
- 隐藏项、过滤和公共 AI 标记仍在 `collect_named`。先按名字排好再过滤，被滤掉的行不改变其余行的相对顺序。
- 持久选项和 `passed_options` 的拥有副本留在 walk 里。

**关闭：** 现有子命令顺序、选项顺序和 `dependsOn` 优先级测试的预期不改。review 看 diff：这三段里没有为了排序而 `clone` 出 `Spec`、`SuggestionSeed` 或 `OptionSpec`。review、fix、对 fix 的 review 都记下。

### 第 4 步记录

1. 审了 `lookup.rs` 里列出子命令、附加建议和 `collect_option_suggestions` 的三段，以及 `collect_named`、`cmp_named_names`、`option_priority`。对照的是本节：子命令和附加建议按下标排，选项不再为了写优先级而克隆，排序键仍是 `cmp_named_names`，隐藏项和公共 AI 仍在 `collect_named`，先排名字再过滤，持久选项的拥有副本留在 walk。`load_referenced_spec`、`replace_spec_with_loaded`、`next_spec_after_arg`，以及走进子命令时的 `next.clone()`，这次没有改。
2. 子命令和附加建议经 `sorted_named_refs` 把引用交给 `collect_named`。选项把当前节点的 `Arc` 和持久选项的引用放进同一个 `Vec`，按名字排，再去掉互斥和已达重复上限的行；隐藏项和查询过滤仍在 `collect_named`。`dependsOn` 未满足时 `option_priority` 得到 75，写在建议行的 `priority` 上。`sortopt ` 的收集顺序是 `same`（说明 `second`）、`same`（说明 `first`）、`sub-alpha`、`sub-Beta`、`sub-beta`，附加建议和选项同样先忽略大小写再按字节序。持久选项 `--delta` 落在 `--beta` 和 `--one` 之间；去掉 `--one` 后，`--delta` 仍在 `--beta` 后面。精确输入 `sub-zulu` 和 `--zulu` 仍露出隐藏项。`tool -o ` 里 `--needed` 的建议优先级是 75，缓存里该选项的 `priority` 仍是 40，根规格和选项 `Arc` 的指针不变，`--alpha` 仍是 40。原有分类顺序、`dependsOn` 75、隐藏别名和公共 AI 测试的预期没改，都通过。
3. 列表持有的是当前节点里子命令、附加建议和选项的引用，加上 walk 上已经拥有的持久选项引用。这三段没有为排序克隆 `Spec`、`SuggestionSeed` 或 `OptionSpec`。建议行自己的名字和说明字符串是 `collect_named` 原来就复制的。缓存里的命令 `Arc` 还在原来的 LRU 格。75 写在建议行上，`OptionSpec` 保持读入时的优先级。下降时的 `next.clone()` 和参数路径的 `(*loaded).clone()` 仍在，第 5 步再去。持久选项和 `passed_options` 的 `option.clone()` 仍在 walk 上。
4. 主名为空的项按空字符串比较，这次没有单独夹具。短选项链 `option_chain_suggestions` 本来就把 `option_priority` 写在建议行上，这次没改。`lookup::complete` 返回的是收集顺序。引擎 `complete` 后面的排序和去重没改；去重比较名字、插入值、显示名和参数提示，不看说明。这一步不改 12/14/65 那张表，没有重测内存。

对这次修正再看：clippy 要求去掉 `sorted_named_refs` 上多余的生命周期，函数体没变。`--Persist` 按名字本来就在选项末尾，看不出持久选项有没有参加同一轮排序；改成 `--delta` 之后它落在 `--beta` 和 `--one` 中间，去掉 `--one` 后还在原位。两条同名子命令按文件顺序留下说明 `second` 然后 `first`。`cargo test -p fastab_engine --lib` 里 `ir::`、`lookup::`、`history::`、`public_ai` 共 153 个通过。`cargo clippy -p fastab_engine -- -D warnings` 通过。第 1 到第 3 步的 review 仍然有效。第 4 步关闭。

## 5. 下降时与子命令共享 Arc

**代码：** `crates/fastab_engine/src/ir.rs` 里 `Spec.subcommands` 的类型和反序列化；`lookup.rs` 里 `current = Arc::new(next.clone())`，以及 `parent = current.clone()` 已是 `Arc` 克隆的那些位置。

**做法：**

- `subcommands` 改成 `Vec<Arc<Spec>>`。反序列化时每个子命令包进 `Arc`，和选项的共享方式相同。`find_subcommand` 要能拿到这个 `Arc`。
- 无改写钩子时，下降执行 `current = Arc::clone(child)`。不再 `Arc::new(next.clone())`。
- `generateSpec`、`jsLoadSpec` 和第 1 步的路径替换仍是 `*current =` 一个新 `Arc`。不用 `Arc::make_mut` 去改父规格缓存里的节点，也不把替换结果写回 `subcommands` 里原来的那个 `Arc`。
- 这样 `Spec` 的克隆只复制当前节点自己的名字、说明、参数和选项 `Arc`，子命令只增加引用计数。先找出克隆之后还改子节点的调用；那些调用改成替换 `Arc`，或显式做一份只给该调用使用的副本。
- `shrink_to_fit` 和 `allocated_bytes` 对子命令 `Arc` 只计一次，规则与选项相同。

**测试：**

- 没有 `generateSpec`、`jsLoadSpec`、路径 `loadSpec` 的下降，例如 `git checkout`：walk 得到的 `spec` 与注册表里那个 `checkout` 是同一个 `Arc`。
- 有上述替换的节点：walk 得到新 `Arc`；注册表里原来的 `Arc` 指针和内容都不变。
- 现有补全、历史槽位和公共 AI 测试的预期不改。

**关闭：** 同第 1 步。这一步会碰到第 1 步到第 3 步的下降和替换，那些 review 作废并重审。

### 第 5 步记录

Review 先看到一处证明不够，补上之后再审过。

1. 审了 `ir.rs` 的 `Spec.subcommands`、`find_subcommand`、`allocated_bytes` / `shared_spec_bodies`、`ShrinkSpecTree for Arc<Spec>`、`intern_spec_options` 和两处未进入子孙的解析循环；`lookup.rs` 走进子命令和 `next_spec_after_arg`；`fig_spec.rs` 的 `spec_from_fig_json`、`mark_generated_spec`、`merge_specs`；`public_ai.rs` 里 `find_subcommand` 之后的 `.as_ref()`。第 4 步列出子命令、附加建议和选项的三段没有改写。第 1 到第 3 步的下降和替换被碰到，那些 review 作废，下面按进入时机重审。对照的是本节：没有改写时 `Arc::clone`，有改写时换成新 `Arc`，不用 `make_mut` 改父缓存，子命令字节只计一次。
2. `git checkout orphan ` 的 walk 是注册表里的 `orphan`，`help_parent` 是同一个 `checkout`，命令 `Arc` 不变。`tool child ` 的路径替换得到新 `Arc`，名字仍是 `child`，说明是 `loaded`，`ai_resolved_reference` 只在这次 walk 上；存根仍是说明 `wrapper` 和 `loadSpec: child`，缓存文件的名字仍是 `child-target`，其中的 `inside` 与 walk 是同一个 `Arc`。`tool curl ` 的参数路径直接持有命令格里的 `curl`，参数上的 `resolved_spec` 仍空，`ai_resolved_reference` 仍为假，`inside` 同一个，路径缓存里没有另一份。`git ` 的 generateSpec 换了根，`changelog` 只在 walk 上并且 `ai_generated` 为真，`checkout` 与缓存是同一个并且 `ai_generated` 为假；再走 `git checkout ` 仍是那个 `checkout`，`help_parent` 是带 `changelog` 的新根。`tool child ` 的 jsLoadSpec 换了 walk，`from-hook` 只在 walk 上，`static-child` 与存根同一个，存根说明仍是 `stub`，钩子 id 还在。原有补全、历史槽位、公共 AI、失败、环路和版本测试的预期没改。`gcloud ` 和 `gcloud comp` 不读子文件，`gcloud compute ` 只读 `compute.json`。缺失和损坏 JSON 仍留存根，各 warn 一次。环路再进入时路径缓存仍是同一个 `Arc`。
3. 没有改写的子命令，walk 持有的就是 `subcommands` 里那个 `Arc`，命令 `Arc` 还在原来的 LRU 格。子命令路径替换先浅拷贝当前节点：复制名字、说明、参数和选项 `Arc`，子命令只增加引用计数，再 `Arc::new`。缓存文件在 `load_spec_cache` 里占 `loaded` 的一格；引用如果就是命令表里的文件，则共用那条命令的格，不另占。两种都不写回父存根。参数路径没有要改写的名字，walk 持有 `load_referenced_spec` 返回的那份缓存 `Arc`，不写进参数的 `resolved_spec`。`generateSpec` 和 `jsLoadSpec` 仍是 `*current =` 一个新 `Arc`。`mark_generated_spec` 的 `make_mut` 只作用在钩子刚解析出来的那棵树上；合并时把包装节点的子命令 `Arc` 克隆进新根。加载时的 `make_mut` 发生在规格放进注册表之前，当时每个子命令 `Arc` 只有一份。`shrink_to_fit` 用 `get_mut`，已经共享的子命令不克隆。`allocated_bytes` 按指针把子命令正文计一次，根不放进 `seen_specs`，内联的 `Box<Spec>` 仍按这份拥有的盒子计。持久选项和 `passed_options` 的拥有副本仍在 walk 上。
4. 子文件缺失时 walk 就是存根那个 `Arc`，现有测试钉的是存根内容，没有单独钉指针；这和没有路径时的 `Arc::clone` 是同一条分支。同一个 `Arc` 在 `subcommands` 里出现两次时，`get_mut` 缩不动里面的容量；JSON 反序列化不会把同一个 `Arc` 放进两个槽。选项本体上的 `loadSpec` 仍不跟随。参数路径再叠 `generateSpec` 时会换成新 `Arc`，这次没有单独夹具。目录 inode 不变、只改未读子文件字节，仍要等走进那个文件才对 SHA。这一步不改 12/14/65 那张表，没有重测内存。

对这次修正再看：第一版 generateSpec 测试只停在 `git `，看得到合并后的 `checkout` 指针，看不到合并之后再下降。补了 `git checkout `：walk 仍是缓存里的 `checkout`，`help_parent` 是带 `changelog` 的新根，注册表里的 `git` 仍没有 `changelog`。注释写明子命令不进选项那套 intern。`cargo test -p fastab_engine --lib` 里 `ir::`、`lookup::`、`history::`、`public_ai` 共 159 个通过。`cargo clippy -p fastab_engine -- -D warnings` 通过。第 4 步的排序三段没有改，顺序测试仍通过，那次 review 仍然有效。第 1 到第 3 步按上面第 2、3 条重审过，进入时机、LRU 格和失败行为没变。第 5 步关闭。

## 6. 按目标总 review

对照「目标」和「不变量」看整份 diff，而不是再审某一段的写法。

至少确认：

- `gcloud ` 不读子文件，`gcloud compute ` 只读 `compute.json`，`gcloud sql ` 不读 `compute.json`。
- `git` 补全与改动前一致。
- LRU 仍是 48，没有第二套缓存，没有启动预热。
- 用与基线相同的方式量一次：`gcloud` 第一层、`gcloud sql`、`gcloud compute`、`git`。第一层和 `sql` 应靠近 12 MB 和 14 MB 那一档，而不是 67 MB。`compute` 仍会解析整份文件，不要求它掉到 12 MB。桌面进程若仍是系统分配器，另看常驻是否只留下走进去的那一份，不把 `ftab` 的 mimalloc 峰值当成常驻节省。
- `git checkout` 的 walk 与注册表共享同一个子命令 `Arc`。列出 `gcloud compute` 的选项时，diff 和测试都表明没有把 `OptionSpec` 再克隆一份来写优先级。建议顺序和优先级 75 与改前一致。
- `compute` 的测量值仍停在解析峰值那一档。不把「没有再降一截」写成第 4、5 步失败。

总 review 的问题和 fix 同样遵守：fix 之后再 review，通过才把第 6 步标成已关闭。

### 第 6 步记录

对照整份 diff 和下面的测量看过。生产代码没有改。

1. 审了相对 HEAD 的 `fig_spec.rs`、`history.rs`、`ir.rs`、`lookup.rs`、`public_ai.rs`，对照本文「目标」和「不变量」。内存仍用 `EC_SPECS_DIR` 加 `__DATA,__interpose` 挡住带 `suggestions` 的那次写出，进程还在时读 `footprint -p`。建议条数另跑完整 stdout，不从被截断的那次写出里数。基线是：只放存根的菜单 12 MB / 70 条，`gcloud sql ` 14 MB / 27 条，`gcloud compute instances ` 65 MB / 56 条，整棵急切展开停在菜单 67 MB / 70 条。仓库 `bundle/specs-ir` 里的 `gcloud.json`、`git.json`、`typed-hooks.json`、`gcloud/compute.json`、`gcloud/sql.json` 与安装包 `Contents/Resources/specs-ir` 的 SHA-256 相同。桌面进程没有换二进制，也没有重启。
2. 新的 `target/release/ftab` 和安装包里的旧 `ftab` 用同一份规格。`git ` 80 条、`git checkout ` 59 条、`gcloud sql ` 27 条、`gcloud compute ` 73 条、`gcloud compute instances ` 56 条，名字、顺序、优先级、说明、`args_hint`、`should_add_space` 和隐藏项逐项相同，stderr 都是 0。基线的 56 条是 `gcloud compute instances `；`gcloud compute ` 两边都是 73 条。`gcloud ` 仍是 70 条，名字、说明、顺序、优先级和隐藏项与旧引擎相同。其中 63 条的插入信息和急切展开不同：60 条只少了末尾空格，`docker`、`help`、`init` 还少了子文件根上的必填参数提示。存根没有子命令、也没有参数，空格按 q-cli 的 `shouldAddSpaceToItem` 看当前节点。`cheat-sheet`、`feedback`、`info`、`survey`、`version` 以及磁盘上没有的 `alpha`、`beta` 两边都不加空格。`git checkout` 与注册表共享子命令 `Arc`、选项优先级写在建议行上，仍由第 5 步和第 4 步的测试钉住。公共 AI 仍由第 3 步的夹具钉住。这一步没有改那些测试或生产代码，159 个测试是第 5 步关闭时跑的。
3. LRU 仍是 `MAX_CACHED_SPECS = 48`。`load_spec_cache` 把相对路径指到已经放进 `loaded` 的那个 `Arc`，腾出最旧一格时命令表和路径表一起摘掉。版本文件仍在 `path_specs`。命令表里的路径共用那条命令的格。没有第二套规格缓存，没有 `MOST_USED_SPECS`，没有启动预热。hook 缓存 512 和 generate LRU 32 不在这份 diff 里。不改写的下降，walk 持有子命令那个 `Arc`。路径替换浅拷贝当前节点再 `Arc::new`，子命令只加引用计数，不写回父存根。参数路径持有 `load_referenced_spec` 返回的 `Arc`。`generateSpec` 和 `jsLoadSpec` 仍换成新 `Arc`。`make_mut` 只发生在放进注册表之前，以及钩子刚解析出来的树上。列出选项是 `Vec<&OptionSpec>`，优先级由 `option_priority` 写到建议行。持久选项和 `passed_options` 仍是拥有的副本。
4. 菜单上没走进去的存根不带子文件的空格和参数提示。补这两项就要读子文件，第一层就不再只剩存根；把每条路径都当成有子命令，又会给 `version` 这类叶子多一个空格。`compute.json` 和 `sql.json` 自己没有嵌套 `loadSpec`。选项本体上的 `loadSpec` 仍不跟随。参数路径再叠 `generateSpec` 仍没有单独夹具。桌面常驻没有重测：正在跑的仍是安装包里的旧进程，`main.rs` 没有全局分配器，临时页会退。`ftab` 用 mimalloc，整棵目录在磁盘上时菜单 19 MB、`gcloud sql ` 22 MB、`git checkout ` 22 MB；目录里只放 `gcloud.json` 和 `typed-hooks.json` 时菜单 12 MB。旧引擎在同一棵整目录上的菜单是 67 MB。19 MB 和 12 MB 的差是打开目录时给每个文件做 SHA-256，只留下 `typed-hooks.json` 的字节，分配器留着最大那次临时读。子规格没有解析进去。`gcloud compute ` 是 65 MB，停在解析 `compute.json` 的峰值，和基线走进这份文件的 65 MB 同一档。第 4、5 步去掉的是同一次补全里的临时副本，水位上限仍是这次解析。

没有要改的生产代码。空格和参数提示的差别就是存根本身，子文件留在磁盘上。第 6 步关闭。

## 不做

- 拆开 `compute.json` 或改选项结构体去消那 50 MB 解析峰值。
- 更换 `ftab` 的 mimalloc。
- 去掉 `ftab` 对 AppKit 的链接。
- 提高 LRU，或在启动时预热常用规格。
- 用 `memoizeOne` 把上一份静态建议行留在内存里。
- 把解析状态做成无上限缓存。
- 把持久选项和命令行上已经出现的旗标改成引用。它们不是整份选项表。
