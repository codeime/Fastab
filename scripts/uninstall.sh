#!/bin/bash
set -euo pipefail

# ── Fastab macOS uninstaller ───────────────────────────────────────────────
# Fastab-only. Easy Complete is a sibling product and is left installed.

APP_NAME="fastab"
APP_DISPLAY="Fastab"
BUNDLE_ID="app.fastab"
IME_BUNDLE_ID="app.fastab.inputmethod"

APP_BUNDLE="/Applications/${APP_DISPLAY}.app"
LOCAL_BIN="${HOME}/.local/bin"
LAUNCH_AGENTS="${HOME}/Library/LaunchAgents"
PLIST_PATH="${LAUNCH_AGENTS}/${BUNDLE_ID}.plist"
INPUT_METHODS_DIR="${HOME}/Library/Input Methods"
IME_SYMLINK="${INPUT_METHODS_DIR}/FastabInputMethod.app"
APP_SUPPORT="${HOME}/Library/Application Support/${APP_NAME}"
CACHE_DIR="${HOME}/Library/Caches/${APP_NAME}"
TMP_ROOT="${TMPDIR:-/tmp}"
TMP_ROOT="${TMP_ROOT%/}"

GREEN='\033[0;32m'; YELLOW='\033[0;33m'; RED='\033[0;31m'; NC='\033[0m'
info()  { echo -e "${GREEN}==>${NC} $*"; }
warn()  { echo -e "${YELLOW}==>${NC} $*"; }
error() { echo -e "${RED}==>${NC} $*" >&2; }

# ── Confirm ───────────────────────────────────────────────────────────────────
if [[ "${1:-}" != "--yes" ]]; then
  echo ""
  warn "This will completely remove ${APP_DISPLAY} and all its data."
  echo "  • /Applications/${APP_DISPLAY}.app"
  echo "  • ${IME_SYMLINK}"
  echo "  • ${PLIST_PATH}"
  echo "  • ${LOCAL_BIN}/ftab, fastabterm"
  echo "  • ${APP_SUPPORT}/"
  echo "  • ${CACHE_DIR}/"
  echo "  • ${TMP_ROOT}/ftablog"
  echo "  • Fastab shell integration lines in ~/.zshrc / ~/.bashrc / ~/.config/fish/config.fish"
  echo ""
  echo "  Easy Complete is not touched."
  echo ""
  read -r -p "Continue? [y/N] " confirm
  [[ "${confirm}" =~ ^[Yy]$ ]] || { echo "Aborted."; exit 0; }
fi

# ── 0. Telemetry (best-effort; reporting is off in this product) ─────────────
if command -v ftab &>/dev/null; then
  ftab telemetry track app_uninstalled 2>/dev/null || true
fi

# ── 1. Uninstall integrations via CLI (must run before binary is removed) ─────
info "Uninstalling input method integration..."
if command -v ftab &>/dev/null; then
  ftab integrations uninstall input-method 2>/dev/null || true
fi

info "Uninstalling shell integration..."
if command -v ftab &>/dev/null; then
  ftab integrations uninstall shell 2>/dev/null || true
fi

# ── 2. Kill running processes ─────────────────────────────────────────────────
info "Stopping processes..."
if [[ -x "${APP_BUNDLE}/Contents/MacOS/${APP_NAME}" ]]; then
  "${APP_BUNDLE}/Contents/MacOS/${APP_NAME}" --unregister-login-item 2>/dev/null || true
fi
pkill -x "${APP_NAME}"       2>/dev/null || true
pkill -f "FastabInputMethod.app/Contents/MacOS/fig_input_method" 2>/dev/null || true
pkill -f "fastabterm"        2>/dev/null || true
sleep 0.5

# ── 3. Remove login item and Fastab LaunchAgent ──────────────────────────────
info "Removing login startup entries..."
uid="$(id -u)"
launchctl bootout "gui/${uid}/${BUNDLE_ID}" 2>/dev/null || true
if [[ -f "$PLIST_PATH" ]]; then
  launchctl unload "$PLIST_PATH" 2>/dev/null || true
  rm -f "$PLIST_PATH"
fi

# ── 4. Remove IME symlink ──────────────────────────────────────────────────────
info "Removing Input Method..."
if [[ -L "$IME_SYMLINK" || -d "$IME_SYMLINK" ]]; then
  rm -rf "$IME_SYMLINK"
fi

# `ftab integrations uninstall input-method` already dropped our palette
# entries in-process. This fallback is only for when the CLI was already gone.
# It still filters by bundle ID rather than deleting the whole array.
info "Removing Input Method from HIToolbox..."
python3 - "$IME_BUNDLE_ID" <<'PY' 2>/dev/null || true
import subprocess, plistlib, sys
bundle_ids = set(sys.argv[1:])
domain = "com.apple.HIToolbox"
proc = subprocess.run(["defaults", "export", domain, "-"], capture_output=True)
if proc.returncode != 0:
    sys.exit(0)
data = plistlib.loads(proc.stdout)
changed = False
for key in ("AppleEnabledInputSources", "AppleSelectedInputSources"):
    sources = data.get(key)
    if not isinstance(sources, list):
        continue
    kept = [s for s in sources if s.get("Bundle ID") not in bundle_ids]
    if len(kept) != len(sources):
        data[key] = kept
        changed = True
if changed:
    subprocess.run(["defaults", "import", domain, "-"], input=plistlib.dumps(data))
PY

# ── 5. Remove app bundle ───────────────────────────────────────────────────────
info "Removing /Applications/${APP_DISPLAY}.app..."
rm -rf "$APP_BUNDLE"

# ── 6. Remove CLI symlinks ─────────────────────────────────────────────────────
info "Removing CLI symlinks..."
rm -f "${LOCAL_BIN}/ftab"
rm -f "${LOCAL_BIN}/fastabterm"

# ── 7. Fallback shell integration cleanup (in case the CLI was already removed)
# ftab integrations uninstall shell was already called in step 1.
# This fallback removes only Fastab lines.
info "Verifying shell integration removal..."

strip_shell_integration_fallback() {
  local rc_file="$1"
  [[ -f "$rc_file" ]] || return 0

  local tmp
  tmp="$(mktemp)"
  grep -Ev \
    'Fastab (pre|post) block|fastab/shell/(zshrc|zprofile|bashrc|bash_profile)\.(pre|post)\.(zsh|bash)|eval "\$\((~/.local/bin/)?ftab init |eval \((~/.local/bin/)?ftab init |\[ -x ~/.local/bin/ftab \] && eval |command -v ftab >/dev/null 2>&1 && eval ' \
    "$rc_file" > "$tmp" || true
  mv "$tmp" "$rc_file"
}

strip_shell_integration_fallback "${HOME}/.zshrc"
strip_shell_integration_fallback "${HOME}/.zprofile"
strip_shell_integration_fallback "${HOME}/.bashrc"
strip_shell_integration_fallback "${HOME}/.bash_profile"
strip_shell_integration_fallback "${HOME}/.config/fish/config.fish"

# Fish dedicated conf files are still named 00_fig_pre.fish / 99_fig_post.fish.
# Easy Complete uses the same names, so only drop a file when it is Fastab-only.
maybe_remove_fastab_fish_conf() {
  local path="$1"
  [[ -f "$path" ]] || return 0
  if grep -Eq 'ftab|fastab' "$path" && ! grep -Eq 'easy-complete|ec init' "$path"; then
    rm -f "$path"
  fi
}
maybe_remove_fastab_fish_conf "${HOME}/.config/fish/conf.d/00_fig_pre.fish"
maybe_remove_fastab_fish_conf "${HOME}/.config/fish/conf.d/99_fig_post.fish"

# ── 8. Remove application data ────────────────────────────────────────────────
info "Removing application data..."
rm -rf "$APP_SUPPORT"
rm -rf "$CACHE_DIR"

# IPC sockets / logs live under the process temp dir, not ~/.local/share.
rm -rf "${TMP_ROOT}/fastabrun" "${TMP_ROOT}/ftablog" 2>/dev/null || true
rm -rf /tmp/fastabrun /tmp/ftablog 2>/dev/null || true

# Preferences
defaults delete "$BUNDLE_ID"          2>/dev/null || true
defaults delete "$IME_BUNDLE_ID"      2>/dev/null || true

# Accessibility grant — drop the now-dead TCC entry so it doesn't linger in
# System Settings pointing at a removed binary.
tccutil reset Accessibility "$BUNDLE_ID" 2>/dev/null || true

# Keychain entries (best-effort)
security delete-generic-password -s "$BUNDLE_ID" 2>/dev/null || true

# ── Done ───────────────────────────────────────────────────────────────────────
echo ""
info "Fastab has been fully uninstalled."
echo ""
echo "  To remove the ~/.local/bin directory itself (if empty):"
echo "    rmdir ~/.local/bin 2>/dev/null"
echo ""
echo "  Reload your shell to apply PATH changes:"
echo "    exec \$SHELL"
