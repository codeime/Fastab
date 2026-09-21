# Changelog

## Unreleased

- docs: how to uninstall Easy Complete before installing Fastab

## v0.0.1

First Fastab release. Versioning starts here; earlier Easy Complete tags are retired.

Fastab is a macOS terminal autocomplete app. Completions follow the caret in a native overlay.

- Overlay and settings are native GPUI views. Completions do not enter a WebView, and the app ships no JavaScript runtime: specs compile to typed IR or named Rust adapters.
- Product identity is Fastab only: `Fastab.app`, bundle `app.fastab`, CLI `ftab`, PTY `fastabterm`, URL scheme `fastab://`.
- Fastab can sit beside Easy Complete, Fig, or Amazon Q. Install, uninstall, IME, HIToolbox, and data dirs are Fastab-only. The same terminal session cannot run two completion desktops; `ftab init` stands down when another PTY already wraps the shell.
- Telemetry UI is hidden and reporting stays unconfigured. Completions stay on-device.
- The Apple Silicon DMG is ad-hoc unsigned. After copying to `/Applications`, clear Gatekeeper quarantine with `xattr -dr com.apple.quarantine "/Applications/Fastab.app"`. Accessibility is Fastab's own TCC identity (`app.fastab`); grant it from Fastab Settings.
