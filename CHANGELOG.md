# Changelog

## Unreleased

## v0.0.3

- fix: remote IPC reconnect no longer stops permanently when an in-flight outbox frame still holds budget accounting (`Busy` vs `Stopped`)
- fix: intercepted-key replay no longer blocks the PTY main loop for up to 5s; ordinary input and desktop Inserts stay ordered behind pending keys without a barrier wait
- fix: generator subprocess cleanup stays bounded after kill; unfinished children are reaped in the background instead of hanging `wait` or leaving zombies
- fix: enabling Jev AI recommendations fails closed with a clear settings status when the bundled specs-ir public baseline pins do not match
- fix: CI `cargo fmt --check` passes across the workspace
- change: terminal grid row-cache reclaim uses hysteresis so short-lived resizes do not thrash capacity
- feat: opt-in Jev completion recommendations (settings + provider profiles), promoting one existing candidate when local results are ready

## v0.0.2

- fix: completion kinds match the previous parser. Once a positional argument is consumed, or after `--`, subcommands leave the list, so fuzzy search no longer offers unrelated commands such as `merge` for `git check m`
- fix: the app icon is the previous dark tile again, with the green prompt and completion list painted into `icon.icns`. The ochre gradient and the layered icon catalog are gone, so macOS 26 no longer shows a plain background
- fix: granting Accessibility no longer quits the app. The drag row's name label was released while the card was still showing it
- fix: the Accessibility grant card centers the icon with its name and the title with the close button, and animates an arrow toward the list
- fix: display-language chips use a static option id so the settings window compiles
- change: the DMG no longer stamps a custom volume icon
- change: shell history suggestions use the same recent window as the on-disk history database
- change: the first-run permission page can switch display language before the rest of settings is available
- change: that language control sits below the window title bar so the click reaches the page
- docs: how to uninstall Easy Complete before installing Fastab

## v0.0.1

First Fastab release. Versioning starts here; earlier Easy Complete tags are retired.

Fastab is a macOS terminal autocomplete app. Completions follow the caret in a native overlay.

- Overlay and settings are native GPUI views. Completions do not enter a WebView, and the app ships no JavaScript runtime: specs compile to typed IR or named Rust adapters.
- Product identity is Fastab only: `Fastab.app`, bundle `app.fastab`, CLI `ftab`, PTY `fastabterm`, URL scheme `fastab://`.
- Fastab can sit beside Easy Complete, Fig, or Amazon Q. Install, uninstall, IME, HIToolbox, and data dirs are Fastab-only. The same terminal session cannot run two completion desktops; `ftab init` stands down when another PTY already wraps the shell.
- Telemetry UI is hidden and reporting stays unconfigured. Completions stay on-device.
- The Apple Silicon DMG is ad-hoc unsigned. After copying to `/Applications`, clear Gatekeeper quarantine with `xattr -dr com.apple.quarantine "/Applications/Fastab.app"`. Accessibility is Fastab's own TCC identity (`app.fastab`); grant it from Fastab Settings.
