//! T3.3 session replay: Native vs Js `CompleteResult` on recorded buffers.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::dual_path_compare::complete_result_diffs;
use crate::engine_golden;
use crate::hook_backend::HookBackend;
use crate::runtime::{CompleteRequest, Engine};
use crate::worker::default_specs_dir;

const SESSION_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/dual-path/sessions");
const REPOS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/dual-path/repos");

#[derive(Debug, Deserialize)]
struct SessionRow {
    buffer: String,
    cwd: String,
}

fn session_path(name: &str) -> PathBuf {
    Path::new(SESSION_DIR).join(format!("{name}.jsonl"))
}

fn load_session(name: &str) -> Vec<SessionRow> {
    let path = session_path(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line).unwrap_or_else(|error| panic!("{}:{}: {error}", path.display(), index + 1))
        })
        .collect()
}

fn ensure_git_fixture(root: &Path) {
    if root.join(".git").is_dir() {
        return;
    }
    let status = Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(root)
        .status()
        .expect("git init");
    assert!(status.success(), "git init {root:?}");
}

fn repo_cwd(key: &str) -> String {
    let root = Path::new(REPOS_DIR).join(key);
    if key == "git" {
        ensure_git_fixture(&root);
    }
    root.display().to_string()
}

fn compare_session(name: &str) {
    let rows = load_session(name);
    assert!(
        rows.len() >= 100,
        "{name} has {} buffers; T3.3 requires ≥ 100",
        rows.len()
    );
    let specs_dir = default_specs_dir();
    assert!(
        specs_dir.join("typed-hooks.json").is_file(),
        "T3.3 needs bundle/specs-ir/typed-hooks.json"
    );
    let mut native_engine = Engine::new(specs_dir.clone()).expect("native engine");
    let mut js_engine = Engine::new(specs_dir).expect("js engine");
    let mut diffs = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let cwd = repo_cwd(&row.cwd);
        let request = CompleteRequest {
            buffer: row.buffer.clone(),
            cwd,
            include_history: false,
            ..CompleteRequest::default()
        };
        let native = native_engine
            .complete(CompleteRequest {
                backend_override: Some(HookBackend::Native),
                ..request.clone()
            })
            .unwrap_or_else(|error| panic!("{name}:{} native: {error}", index + 1));
        let js = js_engine
            .complete(CompleteRequest {
                backend_override: Some(HookBackend::Js),
                ..request
            })
            .unwrap_or_else(|error| panic!("{name}:{} js: {error}", index + 1));
        let found = complete_result_diffs(&native, &js);
        if !found.is_empty() {
            diffs.push(serde_json::json!({
                "session": name,
                "line": index + 1,
                "buffer": row.buffer,
                "diffs": found,
            }));
        }
    }
    if !diffs.is_empty() {
        panic!(
            "T3.3 session {name} Native vs Js diffs: {} / {} buffers\n{}",
            diffs.len(),
            rows.len(),
            serde_json::to_string_pretty(&diffs).unwrap_or_default()
        );
    }
}

#[test]
fn dual_path_sessions_have_five_repos_and_keystroke_prefix_steps() {
    for name in ["git", "npm", "docker", "kubectl", "cargo"] {
        let rows = load_session(name);
        assert!(rows.len() >= 100, "{name} {}", rows.len());
        assert!(rows.iter().all(|row| row.cwd == name));
    }
}

#[test]
fn dual_path_sessions_native_match_js() {
    let _lock = engine_golden::engine_lock();
    for name in ["git", "npm", "docker", "kubectl", "cargo"] {
        compare_session(name);
    }
}
