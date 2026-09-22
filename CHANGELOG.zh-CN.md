# Changelog

## Unreleased

## v0.0.3

- 修复：显示语言选项使用静态 id，设置窗口可以编译
- 变更：应用图标改为 Claude 陶土到桃色的渐变，不再是一块纯色深底
- 变更：应用图标加入 Icon Composer 的 Mark 组，macOS 26 会在提示符和列表标记下方施加系统阴影
- 变更：DMG 磁盘图标与 Dock 图标使用同一份应用图标
- 变更：机器上装有 Xcode 26 及以上时，图标编译改用对应的 actool，并走实际的应用路径而不是版本符号链接
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
