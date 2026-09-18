//! Native vs QuickJS dual-path compare (T3.2).
//!
//! Test-only. The product default stays [`crate::hook_backend::HookBackend::Js`]
//! until T3.4. Failures write `target/dual-path-report.json`.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::{Map, Number, Value};

use crate::engine_golden;
use crate::hook_backend::{self, HookBackend, NativeHooks};
use crate::hook_baseline::{self, Baseline, BaselineCase, Expected};
use crate::hook_types::{HookContext, ScriptCommand, ShellContext};
use crate::js_host::JsHost;
use crate::process::mock;
use crate::runtime::Suggestion;
use crate::typed_hook::suggestions_from_typed_json;
use crate::worker::default_specs_dir;

/// Allowed Native/Js JSON rewrites applied before the byte compare.
/// Each entry names a real interop quirk, not a dropped hook.
const DUAL_PATH_NORMALISATIONS: &[DualPathNormalisation] = &[
    DualPathNormalisation {
        id: "js-negative-zero",
        reason: "QuickJS serializes IEEE -0 as -0; serde_json / Rust i64 emit 0. Same numeric value.",
    },
    DualPathNormalisation {
        id: "json-number-canonical",
        reason: "JS Number is always f64 (1.0); typed IR often emits integers. Canonicalize finite whole numbers to i64.",
    },
    DualPathNormalisation {
        id: "timeout-empty-result",
        reason: "A timed-out hook returns None / empty CompleteResult on both backends. JS records Timeout or PromiseRejected; Native records Timeout or InvokeError. Dual-path compares the Option / CompleteResult, not HookDiagnostic.",
    },
];

struct DualPathNormalisation {
    id: &'static str,
    reason: &'static str,
}

#[derive(Debug, Serialize)]
struct DualPathReport {
    version: u64,
    kind: &'static str,
    normalisations: Vec<NormalisationDoc>,
    compared: usize,
    diffs: Vec<DualPathDiff>,
}

#[derive(Debug, Serialize)]
struct NormalisationDoc {
    id: &'static str,
    reason: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct DualPathDiff {
    suite: &'static str,
    id: String,
    field: String,
    hook_id: String,
    native: Value,
    js: Value,
    normalisations_applied: Vec<&'static str>,
}

fn report_path() -> PathBuf {
    if let Ok(dir) = std::env::var("CARGO_TARGET_DIR") {
        return PathBuf::from(dir).join("dual-path-report.json");
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/dual-path-report.json")
}

fn write_report(compared: usize, diffs: &[DualPathDiff]) {
    let path = report_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let report = DualPathReport {
        version: 1,
        kind: "dual-path-report",
        normalisations: DUAL_PATH_NORMALISATIONS
            .iter()
            .map(|entry| NormalisationDoc {
                id: entry.id,
                reason: entry.reason,
            })
            .collect(),
        compared,
        diffs: diffs.to_vec(),
    };
    let rendered = serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into());
    let _ = fs::write(&path, format!("{rendered}\n"));
}

fn resolve_hook_id(native: &NativeHooks, baseline: &Baseline) -> String {
    if native.contains_hook(&baseline.representative_hook_id) {
        return baseline.representative_hook_id.clone();
    }
    native
        .hook_id_for(&baseline.field, &baseline.body_sha256)
        .unwrap_or(baseline.representative_hook_id.as_str())
        .to_string()
}

fn shell_from_context(context: &HookContext) -> ShellContext {
    ShellContext {
        current_process: context.current_process.clone(),
        environment_variables: Arc::new(
            context
                .environment_variables
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        ),
    }
}

fn mock_rules(case: &BaselineCase) -> Vec<mock::ExecRule> {
    case.exec
        .iter()
        .map(|rule| mock::ExecRule {
            command: rule.command.clone(),
            args: rule.args.clone(),
            stdout: rule.stdout.clone().unwrap_or_default(),
            stderr: rule.stderr.clone().unwrap_or_default(),
            status: i32::try_from(rule.status.unwrap_or(0)).unwrap_or(-1),
            delay_ms: rule.delay_ms,
        })
        .collect()
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn json_opt<T: Serialize>(value: Option<T>) -> Value {
    match value {
        Some(value) => serde_json::to_value(value).unwrap_or(Value::Null),
        None => Value::Null,
    }
}

#[derive(Serialize)]
struct ScriptCommandJson {
    command: String,
    args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout_ms: Option<i64>,
}

impl From<ScriptCommand> for ScriptCommandJson {
    fn from(command: ScriptCommand) -> Self {
        Self {
            command: command.command,
            args: command.args,
            timeout_ms: command.timeout_ms,
        }
    }
}

fn suggestions_arg(value: Option<&Value>) -> Vec<Suggestion> {
    let Some(value) = value else {
        return Vec::new();
    };
    suggestions_from_typed_json(value).unwrap_or_default()
}

fn dispatch_case(field: &str, hook_id: &str, case: &BaselineCase) -> Value {
    let timeout = Duration::from_millis(case.timeout_ms);
    let cwd = case.context.current_working_directory.as_str();
    let search = case.context.search_term.as_str();
    let dangerous = case.context.is_dangerous;
    match field {
        "postProcess" => {
            let stdout = case.args.first().and_then(Value::as_str).unwrap_or("");
            let tokens = string_array(case.args.get(1));
            json_opt(hook_backend::dispatch_post_process(hook_id, stdout, &tokens))
        },
        "custom" => {
            let tokens = string_array(case.args.first());
            json_opt(hook_backend::dispatch_custom(
                hook_id, &tokens, cwd, search, timeout, dangerous,
            ))
        },
        "getQueryTerm" => {
            let token = case.args.first().and_then(Value::as_str).unwrap_or("");
            json_opt(hook_backend::dispatch_get_query_term(hook_id, token))
        },
        "trigger" => {
            let new_token = case.args.first().and_then(Value::as_str).unwrap_or("");
            let old_token = case.args.get(1).and_then(Value::as_str).unwrap_or("");
            json_opt(hook_backend::dispatch_trigger(hook_id, new_token, old_token))
        },
        "script" => {
            let tokens = string_array(case.args.first());
            json_opt(hook_backend::dispatch_script_command(hook_id, &tokens).map(ScriptCommandJson::from))
        },
        "generateSpec" => {
            let tokens = string_array(case.args.first());
            json_opt(hook_backend::dispatch_generate_spec(hook_id, &tokens, cwd, timeout))
        },
        "filterTemplateSuggestions" => {
            let suggestions = suggestions_arg(case.args.first());
            json_opt(hook_backend::dispatch_filter_template_suggestions(
                hook_id,
                &suggestions,
            ))
        },
        "alias" => {
            let token = case.args.first().and_then(Value::as_str).unwrap_or("");
            json_opt(hook_backend::dispatch_alias(hook_id, token, cwd, timeout))
        },
        "loadSpec" => {
            let token = case.args.first().and_then(Value::as_str).unwrap_or("");
            json_opt(hook_backend::dispatch_load_spec(hook_id, token, cwd, timeout))
        },
        other => Value::String(format!("unknown-field:{other}")),
    }
}

fn run_baseline_backend(
    backend: HookBackend,
    native: &Arc<NativeHooks>,
    host: &JsHost,
    hook_id: &str,
    baseline: &Baseline,
    case: &BaselineCase,
) -> Value {
    let _mock = mock::install(mock_rules(case));
    crate::js_host::clear_result_caches(host);
    let shell = shell_from_context(&case.context);
    let cwd = case.context.current_working_directory.as_str();
    let _bound = hook_backend::bind_native(Arc::clone(native));
    hook_backend::enter_context(cwd, &shell, || {
        host.enter_with_context(cwd, &shell, || {
            hook_backend::with_backend(backend, || dispatch_case(&baseline.field, hook_id, case))
        })
    })
}

fn apply_timeout_empty(value: Value, timeout_case: bool, applied: &mut Vec<&'static str>) -> Value {
    if timeout_case && value.is_null() && !applied.contains(&"timeout-empty-result") {
        applied.push("timeout-empty-result");
    }
    value
}

fn is_timeout_case(case: &BaselineCase) -> bool {
    case.id == "timeout" || matches!(case.expected, Expected::Timeout { .. })
}

fn canonicalize_number(number: &Number, applied: &mut Vec<&'static str>) -> Value {
    if let Some(int) = number.as_i64() {
        return Value::from(int);
    }
    if let Some(uint) = number.as_u64() {
        return Value::from(uint);
    }
    let Some(float) = number.as_f64() else {
        return Value::Number(number.clone());
    };
    if float == 0.0 {
        if !applied.contains(&"js-negative-zero") {
            applied.push("js-negative-zero");
        }
        return Value::from(0);
    }
    if float.fract() == 0.0 && float >= i64::MIN as f64 && float <= i64::MAX as f64 {
        if !applied.contains(&"json-number-canonical") {
            applied.push("json-number-canonical");
        }
        return Value::from(float as i64);
    }
    Value::from(float)
}

fn canonicalize_json(value: Value, applied: &mut Vec<&'static str>) -> Value {
    match value {
        Value::Number(number) => canonicalize_number(&number, applied),
        Value::Array(items) => Value::Array(items.into_iter().map(|item| canonicalize_json(item, applied)).collect()),
        Value::Object(fields) => {
            let mut out = Map::new();
            for (key, child) in fields {
                out.insert(key, canonicalize_json(child, applied));
            }
            Value::Object(out)
        },
        other => other,
    }
}

fn normalised(value: Value, timeout_case: bool) -> (Value, Vec<&'static str>) {
    let mut applied = Vec::new();
    let value = apply_timeout_empty(value, timeout_case, &mut applied);
    (canonicalize_json(value, &mut applied), applied)
}

fn compare_values(
    suite: &'static str,
    id: String,
    field: String,
    hook_id: String,
    native: Value,
    js: Value,
    timeout_case: bool,
) -> Option<DualPathDiff> {
    let (native, native_applied) = normalised(native, timeout_case);
    let (js, js_applied) = normalised(js, timeout_case);
    if native == js {
        return None;
    }
    let mut normalisations_applied = native_applied;
    for id in js_applied {
        if !normalisations_applied.contains(&id) {
            normalisations_applied.push(id);
        }
    }
    Some(DualPathDiff {
        suite,
        id,
        field,
        hook_id,
        native,
        js,
        normalisations_applied,
    })
}

fn compare_baselines(diffs: &mut Vec<DualPathDiff>) -> usize {
    let specs_dir = default_specs_dir();
    assert!(
        specs_dir.join("typed-hooks.json").is_file(),
        "T3.2 needs bundle/specs-ir/typed-hooks.json"
    );
    let native = Arc::new(NativeHooks::load(&specs_dir, None));
    let host = JsHost::from_specs_dir(&specs_dir);
    let baselines = hook_baseline::load_all().expect("T1.2 baselines");
    let mut compared = 0usize;
    for baseline in &baselines {
        let hook_id = resolve_hook_id(&native, baseline);
        for case in &baseline.cases {
            let native_value = run_baseline_backend(HookBackend::Native, &native, &host, &hook_id, baseline, case);
            let js_value = run_baseline_backend(HookBackend::Js, &native, &host, &hook_id, baseline, case);
            compared += 1;
            if let Some(diff) = compare_values(
                "baseline",
                format!("{}:{}:{}", baseline.field, baseline.body_sha256, case.id),
                baseline.field.clone(),
                hook_id.clone(),
                native_value,
                js_value,
                is_timeout_case(case),
            ) {
                diffs.push(diff);
            }
        }
    }
    compared
}

fn compare_goldens(diffs: &mut Vec<DualPathDiff>) -> usize {
    let root = engine_golden::golden_root();
    let specs_dir = root.join("specs");
    assert!(
        specs_dir.join("typed-hooks.json").is_file(),
        "T3.2 needs testdata/engine-golden/specs/typed-hooks.json"
    );
    let cwd = root.join("cwd").to_string_lossy().into_owned();
    let cwd_b = root.join("cwd-b").to_string_lossy().into_owned();
    let expected_path = root.join("expected.json");
    let original = fs::read_to_string(&expected_path).expect("engine-golden/expected.json");
    let golden: engine_golden::EngineGoldenFile = serde_json::from_str(&original).expect("engine golden cases");
    let mut compared = 0usize;
    for case in &golden.cases {
        let (native_result, native_first, _, _, _) = hook_backend::with_backend(HookBackend::Native, || {
            engine_golden::run_case(&specs_dir, &cwd, &cwd_b, case)
        });
        let (js_result, js_first, _, _, _) = hook_backend::with_backend(HookBackend::Js, || {
            engine_golden::run_case(&specs_dir, &cwd, &cwd_b, case)
        });
        compared += 1;
        if let Some(diff) = compare_values(
            "engine-golden",
            case.name.clone(),
            "complete".into(),
            String::new(),
            native_result,
            js_result,
            case.name.contains("timeout") || case.name.contains("spin"),
        ) {
            diffs.push(diff);
        }
        if let (Some(native_first), Some(js_first)) = (native_first, js_first)
            && let Some(diff) = compare_values(
                "engine-golden",
                format!("{}#firstResult", case.name),
                "complete".into(),
                String::new(),
                native_first,
                js_first,
                false,
            )
        {
            diffs.push(diff);
        }
    }
    compared
}

#[test]
fn dual_path_normalisations_are_documented() {
    let mut seen = std::collections::BTreeSet::new();
    for entry in DUAL_PATH_NORMALISATIONS {
        assert!(!entry.id.is_empty(), "normalisation id must be non-empty");
        assert!(!entry.reason.is_empty(), "normalisation {} needs a reason", entry.id);
        assert!(seen.insert(entry.id), "duplicate normalisation id {}", entry.id);
    }
    assert!(
        DUAL_PATH_NORMALISATIONS
            .iter()
            .any(|entry| entry.id == "js-negative-zero"),
        "T3.2 requires the JS -0 vs Rust 0 rewrite to be listed"
    );
}

#[test]
fn dual_path_native_matches_js() {
    let _lock = engine_golden::engine_lock();
    let mut diffs = Vec::new();
    let compared = compare_baselines(&mut diffs) + compare_goldens(&mut diffs);
    if !diffs.is_empty() {
        write_report(compared, &diffs);
        panic!(
            "dual-path Native vs Js diffs: {} (compared {compared}); wrote {}",
            diffs.len(),
            report_path().display()
        );
    }
    assert!(compared > 0, "dual-path must compare T1.2 baselines and T1.5 goldens");
}
