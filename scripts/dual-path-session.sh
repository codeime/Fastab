#!/usr/bin/env bash
#
# Replay T3.3 session buffers through `ec engine complete` (Native only).
#
#   scripts/dual-path-session.sh
#   scripts/dual-path-session.sh tests/dual-path/sessions/git.jsonl
#
# Each session is a JSONL of `{buffer, cwd}` where `cwd` is a repo key under
# tests/dual-path/repos/{git,npm,docker,kubectl,cargo}. Runtime JS is gone.
#
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SESSIONS_DIR="$ROOT/tests/dual-path/sessions"
REPOS_DIR="$ROOT/tests/dual-path/repos"
SPECS_DIR="${EC_SPECS_DIR:-$ROOT/bundle/specs-ir}"

usage() {
    sed -n '3,11p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        -h|--help) usage 0 ;;
        *) break ;;
    esac
done

if [[ ! -f "$SPECS_DIR/index.json" ]]; then
    echo "missing specs IR at $SPECS_DIR (run node scripts/compile-spec-ir.mjs)" >&2
    exit 2
fi

setup_repos() {
    local git_dir="$REPOS_DIR/git"
    if [[ ! -d "$git_dir/.git" ]]; then
        git init -b main "$git_dir" >/dev/null
        git -C "$git_dir" add README.md
        git -C "$git_dir" -c user.email=dual-path@example.com -c user.name=dual-path \
            commit -m "dual-path fixture" >/dev/null
    fi
}

resolve_ec() {
    if [[ -n "${EC_BIN:-}" && -x "$EC_BIN" ]]; then
        printf '%s\n' "$EC_BIN"
        return
    fi
    if [[ -x "$ROOT/target/debug/ec" ]]; then
        printf '%s\n' "$ROOT/target/debug/ec"
        return
    fi
    if [[ -x "$ROOT/target/release/ec" ]]; then
        printf '%s\n' "$ROOT/target/release/ec"
        return
    fi
    echo "building ec (cargo build -p ec_cli)..." >&2
    cargo build -p ec_cli --quiet
    printf '%s\n' "$ROOT/target/debug/ec"
}

setup_repos
EC="$(resolve_ec)"

if [[ $# -gt 0 ]]; then
    files=("$@")
else
    files=("$SESSIONS_DIR"/*.jsonl)
fi

if [[ ${#files[@]} -eq 0 ]]; then
    echo "no session files" >&2
    exit 2
fi

for file in "${files[@]}"; do
    echo "==> $(basename "$file")"
    while IFS= read -r line || [[ -n "$line" ]]; do
        [[ -z "${line// }" ]] && continue
        buffer="$(node -e 'const r=JSON.parse(process.argv[1]); process.stdout.write(r.buffer)' "$line")"
        cwd_key="$(node -e 'const r=JSON.parse(process.argv[1]); process.stdout.write(r.cwd)' "$line")"
        case "$cwd_key" in
            git|npm|docker|kubectl|cargo) cwd="$REPOS_DIR/$cwd_key" ;;
            *) cwd="$cwd_key" ;;
        esac
        "$EC" engine complete --buffer "$buffer" --cwd "$cwd" --specs-dir "$SPECS_DIR" >/dev/null
    done < "$file"
done

echo "all sessions completed"
