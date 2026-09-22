# Changelog

## Unreleased

## v0.0.2

- 修复：补全种类与之前的解析器一致。位置参数被吃掉之后，或已经输入 `--` 之后，不再列出子命令，因此模糊搜索不会再把 `git check m` 配成 `merge` 这类无关命令
- 修复：应用图标回到之前的深色底，绿色提示符和补全列表画在 `icon.icns` 里。土黄色渐变和分层图标目录都已去掉，macOS 26 不再只显示一块纯背景
- 修复：授予辅助功能时应用不再直接退出。拖动行上的名称标签在卡片仍在显示时被释放了
- 修复：辅助功能授权卡片里，图标与名称、标题与关闭按钮分别垂直居中，箭头会向列表方向循环移动
- 修复：显示语言选项使用静态 id，设置窗口可以编译
- 变更：DMG 不再盖自定义磁盘图标
- 变更：来自 shell 的历史补全与本地历史库使用同一段最近记录
- 变更：首次打开的权限设置页可以切换显示语言，不必等其余设置页可用
- 变更：语言切换放在窗口标题栏下方，点击会落到页面上

## v0.0.1

Fastab 的第一次发布。版本号从这里重新开始；此前的 Easy Complete tag 已全部退役。

Fastab 是一款 macOS 终端自动补全应用，补全列表以原生浮层跟随光标。

- 补全浮层和设置窗口都是原生 GPUI 视图。补全不进 WebView，应用也不带 JavaScript 运行时：spec 在构建期编译成 typed IR 或具名 Rust 适配器。
- 产品身份只属于 Fastab：`Fastab.app`、bundle `app.fastab`、CLI `ftab`、PTY `fastabterm`、URL scheme `fastab://`。
- 可以和 Easy Complete、Fig、Amazon Q 并排安装。安装、卸载、输入法、HIToolbox 和数据目录都只动 Fastab。同一终端会话不能同时跑两套补全桌面；另一个 PTY 已经包装了 shell 时，`ftab init` 会让路。
- 遥测开关已从界面隐藏，上报保持未配置。补全只在本机运行。
- Apple Silicon DMG 是未签名的 ad-hoc 构建。拷到 `/Applications` 后先清隔离属性：`xattr -dr com.apple.quarantine "/Applications/Fastab.app"`。辅助功能是 Fastab 自己的 TCC 身份（`app.fastab`），从 Fastab 设置里授予。
