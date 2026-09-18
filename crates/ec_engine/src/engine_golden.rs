//! Engine-level golden covering the six user-visible dimensions.
//!
//! Compare with `testdata/engine-golden/expected.json`. Rewrite that file
//! with `EC_ENGINE_GOLDEN_UPDATE=1 cargo test -p ec_engine engine_golden`.
//!
//! Document vs code, code wins:
//! - [`crate::runtime::CompleteResult`] has no `files` / `diagnostics` fields.
//!   Timeout / invoke outcomes are read from [`crate::hook_backend::last_diagnostic`].
//! - `executeCommand` timeout is `CommandError::TimedOut` → typed/adapter
//!   error, usually [`crate::hook_types::HookDiagnostic::InvokeError`].
//!   [`crate::hook_types::HookDiagnostic::Timeout`] is a wall-clock abort.
//! - SWR after TTL expiry refetches; it does not serve the stale rows.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::process::mock::{self, ExecRule};
use crate::rank::Frecency;
use crate::runtime::{CompleteRequest, Engine};

const CWD_TOKEN: &str = "$CWD";
const CWD_B_TOKEN: &str = "$CWD_B";

#[derive(Debug, Deserialize)]
pub(crate) struct EngineGoldenFile {
    pub(crate) cases: Vec<EngineGoldenCase>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct EngineGoldenCase {
    pub(crate) name: String,
    dimension: String,
    #[serde(default)]
    request: CompleteRequest,
    #[serde(default)]
    exec: Vec<GoldenExec>,
    #[serde(default)]
    settings: Map<String, Value>,
    #[serde(default)]
    history: Vec<(String, u64)>,
    #[serde(default, rename = "sleepMs")]
    sleep_ms: Option<u64>,
    #[serde(default, rename = "secondRequest")]
    second_request: Option<CompleteRequest>,
    #[serde(default, rename = "secondExec")]
    second_exec: Option<Vec<GoldenExec>>,
    #[serde(default, rename = "expectCalls")]
    expect_calls: Option<usize>,
    #[serde(default, rename = "expectSecondCalls")]
    expect_second_calls: Option<usize>,
    #[serde(default)]
    diagnostic: Option<String>,
    #[serde(default, rename = "firstResult")]
    first_result: Option<Value>,
    result: Value,
}

#[derive(Debug, Clone, Deserialize)]
struct GoldenExec {
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default)]
    stdout: String,
    #[serde(default)]
    stderr: String,
    #[serde(default)]
    status: i32,
    #[serde(default, rename = "delayMs")]
    delay_ms: Option<u64>,
}

pub(crate) fn golden_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/engine-golden")
}

fn updating() -> bool {
    matches!(std::env::var("EC_ENGINE_GOLDEN_UPDATE").as_deref(), Ok("1"))
}

fn rewrite_path(value: &str, cwd: &str, cwd_b: &str) -> String {
    value.replace(CWD_B_TOKEN, cwd_b).replace(CWD_TOKEN, cwd)
}

fn resolve_request(mut request: CompleteRequest, cwd: &str, cwd_b: &str) -> CompleteRequest {
    request.cwd = rewrite_path(&request.cwd, cwd, cwd_b);
    if request.cwd.is_empty() {
        request.cwd = cwd.to_string();
    }
    request
}

fn resolve_exec(rules: &[GoldenExec], cwd: &str, cwd_b: &str) -> Vec<ExecRule> {
    rules
        .iter()
        .map(|rule| ExecRule {
            command: rule.command.as_ref().map(|command| rewrite_path(command, cwd, cwd_b)),
            args: rule
                .args
                .as_ref()
                .map(|args| args.iter().map(|arg| rewrite_path(arg, cwd, cwd_b)).collect()),
            stdout: rewrite_path(&rule.stdout, cwd, cwd_b),
            stderr: rule.stderr.clone(),
            status: rule.status,
            delay_ms: rule.delay_ms,
        })
        .collect()
}

fn install_settings(case: &EngineGoldenCase) -> fig_settings::settings::SettingsOverrideGuard {
    let mut pairs: Vec<(String, Value)> = vec![
        ("autocomplete.hideAutoExecuteSuggestion".into(), Value::Bool(true)),
        ("autocomplete.disableForCommands".into(), Value::Array(Vec::new())),
    ];
    for (key, value) in &case.settings {
        if let Some(existing) = pairs.iter_mut().find(|(name, _)| name == key) {
            existing.1 = value.clone();
        } else {
            pairs.push((key.clone(), value.clone()));
        }
    }
    let refs: Vec<(&str, Value)> = pairs.iter().map(|(key, value)| (key.as_str(), value.clone())).collect();
    fig_settings::settings::install_override(fig_settings::settings::Settings::from_slice(&refs))
}

fn serialize_result(result: &crate::runtime::CompleteResult) -> Value {
    serde_json::to_value(result).expect("serialize CompleteResult")
}

/// The latest hook outcome on this thread's backend. `CompleteResult` does
/// not carry diagnostics.
fn diagnostic_label() -> Option<String> {
    crate::hook_backend::last_diagnostic().map(|record| format!("{:?}", record.outcome))
}

fn has_second_step(case: &EngineGoldenCase) -> bool {
    case.second_request.is_some()
        || case.second_exec.is_some()
        || case.sleep_ms.is_some()
        || case.expect_second_calls.is_some()
        || case.first_result.is_some()
}

pub(crate) fn run_case(
    specs_dir: &Path,
    cwd: &str,
    cwd_b: &str,
    case: &EngineGoldenCase,
) -> (Value, Option<Value>, Option<String>, usize, usize) {
    let _settings = install_settings(case);
    let _mock = mock::install(resolve_exec(&case.exec, cwd, cwd_b));
    let frecency = Frecency::from_commands(case.history.clone());
    let mut engine = Engine::new_with_frecency(specs_dir.to_path_buf(), frecency).expect("engine golden specs");
    let first_request = resolve_request(case.request.clone(), cwd, cwd_b);
    let first = engine.complete(first_request).unwrap_or_else(|error| {
        panic!("complete {} first request: {error:#}", case.name);
    });
    let first_calls = mock::calls().len();
    let first_json = serialize_result(&first);

    if !has_second_step(case) {
        return (first_json, None, diagnostic_label(), first_calls, 0);
    }

    if let Some(sleep_ms) = case.sleep_ms {
        std::thread::sleep(Duration::from_millis(sleep_ms));
    }
    if let Some(second_exec) = &case.second_exec {
        mock::replace_rules(resolve_exec(second_exec, cwd, cwd_b));
    }
    let second_request = resolve_request(
        case.second_request.clone().unwrap_or_else(|| case.request.clone()),
        cwd,
        cwd_b,
    );
    let second = engine.complete(second_request).unwrap_or_else(|error| {
        panic!("complete {} second request: {error:#}", case.name);
    });
    let second_calls = mock::calls().len().saturating_sub(first_calls);
    (
        serialize_result(&second),
        Some(first_json),
        diagnostic_label(),
        first_calls,
        second_calls,
    )
}

fn count_dimension(cases: &[EngineGoldenCase], dimension: &str) -> usize {
    cases.iter().filter(|case| case.dimension == dimension).count()
}

pub(crate) fn engine_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|error| error.into_inner())
}

#[test]
fn engine_golden_six_dimensions() {
    let _lock = engine_lock();
    let root = golden_root();
    let specs_dir = root.join("specs");
    let cwd = root.join("cwd");
    let cwd_b = root.join("cwd-b");
    let expected_path = root.join("expected.json");
    let cwd = cwd.to_string_lossy().into_owned();
    let cwd_b = cwd_b.to_string_lossy().into_owned();

    let original = fs::read_to_string(&expected_path).expect("engine-golden/expected.json");
    let mut file_value: Value = serde_json::from_str(&original).expect("engine golden JSON");
    let golden: EngineGoldenFile = serde_json::from_value(file_value.clone()).expect("engine golden cases");
    assert!(
        golden.cases.len() >= 40,
        "engine golden must have ≥40 cases, got {}",
        golden.cases.len()
    );
    assert!(
        count_dimension(&golden.cases, "candidates") >= 18,
        "candidates dimension needs 6 buffers × 3 cases"
    );
    assert!(
        count_dimension(&golden.cases, "insertion") >= 6,
        "insertion dimension needs ≥6 cases"
    );
    assert!(
        count_dimension(&golden.cases, "ranking") >= 6,
        "ranking dimension needs ≥6 cases"
    );
    assert!(
        count_dimension(&golden.cases, "cache") >= 4,
        "cache dimension needs ≥4 cases"
    );
    assert!(
        count_dimension(&golden.cases, "shell") >= 3,
        "shell dimension needs ≥3 cases"
    );
    assert!(
        count_dimension(&golden.cases, "timeout") >= 2,
        "timeout dimension needs ≥2 cases"
    );

    let mut seen = BTreeMap::<String, usize>::new();
    let update = updating();
    for (index, case) in golden.cases.iter().enumerate() {
        *seen.entry(case.dimension.clone()).or_insert(0) += 1;
        let (actual, first_actual, diagnostic, first_calls, second_calls) = run_case(&specs_dir, &cwd, &cwd_b, case);

        if let Some(expected_calls) = case.expect_calls {
            assert_eq!(first_calls, expected_calls, "case {} expectCalls", case.name);
        }
        if let Some(expected_second) = case.expect_second_calls {
            assert_eq!(second_calls, expected_second, "case {} expectSecondCalls", case.name);
        }
        if let Some(expected_diagnostic) = &case.diagnostic {
            assert_eq!(
                diagnostic.as_deref(),
                Some(expected_diagnostic.as_str()),
                "case {} diagnostic",
                case.name
            );
        }
        if !update {
            if let (Some(expected_first), Some(actual_first)) = (&case.first_result, &first_actual) {
                assert_eq!(actual_first, expected_first, "case {} firstResult", case.name);
            }
            assert_eq!(actual, case.result, "case {} result", case.name);
        } else if let Some(cases) = file_value.get_mut("cases").and_then(Value::as_array_mut) {
            let slot = cases.get_mut(index).expect("case slot");
            slot["result"] = actual;
            if let Some(first_actual) = first_actual {
                slot["firstResult"] = first_actual;
            }
            if let Some(diagnostic) = diagnostic {
                slot["diagnostic"] = Value::String(diagnostic);
            }
            slot["expectCalls"] = Value::from(first_calls);
            if has_second_step(case) {
                slot["expectSecondCalls"] = Value::from(second_calls);
            }
        }
    }

    assert_eq!(seen.len(), 6, "expected six dimensions, got {seen:?}");

    if update {
        let rendered = serde_json::to_string_pretty(&file_value).expect("write engine golden");
        fs::write(&expected_path, format!("{rendered}\n")).expect("overwrite engine golden");
    }
}
