# Changelog

## Unreleased

## v0.0.2

- fix: display-language chips use a static option id so the settings window compiles
- change: app icon uses a Claude terracotta-to-peach gradient instead of a flat dark tile
- change: app icon ships an Icon Composer Mark group so macOS 26 can apply its system shadow under the prompt-and-list mark
- change: the DMG volume uses the same app icon as the Dock tile
- change: icon compilation uses Xcode 26+ actool when that Xcode is installed, following the real app path rather than a version symlink
- change: shell history suggestions use the same recent window as the on-disk history database
- change: the first-run permission page can switch display language before the rest of settings is available
- change: that language control sits below the window title bar so the click reaches the page

## v0.0.1

First Fastab release. Versioning starts here; earlier Easy Complete tags are retired.

Fastab is a macOS terminal autocomplete app. Completions follow the caret in a native overlay.

- Overlay and settings are native GPUI views. Completions do not enter a WebView, and the app ships no JavaScript runtime: specs compile to typed IR or named Rust adapters.
- Product identity is Fastab only: `Fastab.app`, bundle `app.fastab`, CLI `ftab`, PTY `fastabterm`, URL scheme `fastab://`.
- Fastab can sit beside Easy Complete, Fig, or Amazon Q. Install, uninstall, IME, HIToolbox, and data dirs are Fastab-only. The same terminal session cannot run two completion desktops; `ftab init` stands down when another PTY already wraps the shell.
- Telemetry UI is hidden and reporting stays unconfigured. Completions stay on-device.
- The Apple Silicon DMG is ad-hoc unsigned. After copying to `/Applications`, clear Gatekeeper quarantine with `xattr -dr com.apple.quarantine "/Applications/Fastab.app"`. Accessibility is Fastab's own TCC identity (`app.fastab`); grant it from Fastab Settings.
