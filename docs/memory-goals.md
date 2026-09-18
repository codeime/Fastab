# 内存目标

桌面进程在去掉 QuickJS 之后，常驻大头不再是 `hooks/*.js`，而是启动时整份反序列化 `typed-hooks.json`（约 4.6 MB、3136 个 typed hook）。`ecterm` / IME / AX / Registry LRU 已经收过，**本文件列出的不做项保持不动**。

## 不变量（本阶段不改）

- `ecterm` `max_scroll_limit = 1`（0 会丢掉滚出视口的 prompt）
- `fig_util` 不链 AppKit
- `fig_input_method` 不拉 `fig_ipc` / tokio / prost
- AX `Copy*` / `Create*` 走 create rule
- Registry LRU 48、hook 结果缓存 512、generate LRU 32 的数字不改
- 不为「再砍 figterm 直接依赖」开 PR（已量过约 2.8 KB）

## 本阶段目标

| ID | 目标 | 验收 |
| --- | --- | --- |
| **G1** | 启动不物化未使用的 typed hook IR | `parse_typed_hook_catalog_bytes` 之后 `parsed_descriptor_count() == 0`；只有第一次求值才 `parse_typed_hook_ir_bytes`。`LazyTypedHookIr::PartialEq` 不得 `get()`。测试钉住生产 sidecar 与「坏 descriptor 在首次使用时 fail closed」。 |
| **G2** | `NativeHooks` 不再复制一份 `HookMeta` HashMap | 适配器 SHA 从 runtime catalog 读；typed IR 从 catalog 懒解析。`into_runtime` 丢掉 `module` / `moduleSha256` / `path` / 每条 typed hook 的 `functionBodySha256`。 |
| **G3** | 打包 spec 钉在已发布最新版 | `@chen86860/autocomplete-specs` 与 npm `latest`、fork `master` 一致。当前即 **3.1.0**（2026-09-02），仓库无更新提交。 |
| **G4** | 打开 snapshot 时截下 sidecar，load 不再读第二遍 | `take_captured_file("typed-hooks.json")`；digest 仍在 open 时写入，后续 `read_file` 照常校验。 |

## 后续（明确不做）

- 按 spec 拆 `typed-hooks.json` 成多文件（DirectorySnapshot 会多 hash 877+ 个文件）
- 削弱 snapshot 全树 SHA（混代读取的安全网）
- 整份 sidecar 收成 `Arc<[u8]>` + 偏移（自定义解析，不再 per-hook `RawValue` 拷贝）
- 首次求值成功后丢掉该 hook 的 raw JSON（`Mutex<Option<RawValue>>`，热路径加锁）
- 超时 attempt 丢掉整颗 Engine 的双倍峰值（罕见路径）
- 16 MB 引擎栈（虚拟预留，evaluate 仍会深递归）
