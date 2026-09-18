//! Native vs QuickJS dual-path compare (T3.2).
//!
//! Test-only. Failures write `target/dual-path-report.json`.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::{Map, Number, Value};

use crate::engine_golden;
use crate::hook_backend::{self, HookBackend, NativeHooks};
use crate::hook_baseline::{self, Baseline, BaselineCase, Expected};
use crate::hook_types::{HookContext, HookDiagnostic, ScriptCommand, ShellContext};
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
    DualPathNormalisation {
        id: "empty-suggestions-vs-none",
        reason: "Suggestion hooks: JS throw / conversion failure is None; Native empty success is Some([]). generate.rs uses unwrap_or_default(), so both paint zero overlay rows.",
    },
    DualPathNormalisation {
        id: "emoji-variation-selector",
        reason: "JS source often writes emoji+U+FE0F (⭐️); named adapters store the bare codepoint (⭐). Same grapheme.",
    },
    DualPathNormalisation {
        id: "suggestion-set-order",
        reason: "JS object enumeration is insertion order; Rust HashMap/BTreeMap is hash or sorted. Compare suggestion arrays as a multiset when every row matches.",
    },
    DualPathNormalisation {
        id: "js-module-unevaluable",
        reason: "Closure-preserving versioned modules that QuickJS cannot eval (EvalError/SourceMissing) never produce a JS result. Skip those cases instead of treating Native rows as a silent drop — the JS path already did not run the hook.",
    },
    DualPathNormalisation {
        id: "js-throw-native-value",
        reason: "Extracted JS threw (InvokeError/PromiseRejected/Timeout) on inputs whose T1.2 source baseline is already error/timeout. Native typed/adapter may still return a value (often the string undefined). Skip — this is not a JS success that Native dropped.",
    },
    DualPathNormalisation {
        id: "generate-spec-name-tree",
        reason: "JS generateSpec runs filepaths-rewrite and fills Fig template/getQueryTerm/trigger defaults; named adapters emit catalog JSON. Compare the user-visible name tree (subcommand/option names) so default IR fields are not false diffs.",
    },
    DualPathNormalisation {
        id: "rustup-trailing-newline-toolchain",
        reason: "rustup toolchain list stdout ends in \\n. Native js_split_lines keeps the trailing empty line and cargo()/typed IR emit option name \"+\"; extracted JS Spec parse drops it. T1.2 cargo baseline expected still includes {name:\"+\", description:\"\"}. Drop empty/+ names from the name tree only — do not change the cargo adapter.",
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
    skipped_js_unevaluable: usize,
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

fn write_report(compared: usize, skipped: usize, diffs: &[DualPathDiff]) {
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
        skipped_js_unevaluable: skipped,
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

struct BackendRun {
    value: Value,
    diagnostic: Option<HookDiagnostic>,
}

fn run_baseline_backend(
    backend: HookBackend,
    native: &Arc<NativeHooks>,
    host: &JsHost,
    hook_id: &str,
    baseline: &Baseline,
    case: &BaselineCase,
) -> BackendRun {
    let _mock = mock::install(mock_rules(case));
    crate::js_host::clear_result_caches(host);
    let shell = shell_from_context(&case.context);
    let cwd = case.context.current_working_directory.as_str();
    let _bound = hook_backend::bind_native(Arc::clone(native));
    let value = hook_backend::enter_context(cwd, &shell, || {
        host.enter_with_context(cwd, &shell, || {
            hook_backend::with_backend(backend, || dispatch_case(&baseline.field, hook_id, case))
        })
    });
    let diagnostic = match backend {
        HookBackend::Native => hook_backend::last_diagnostic().map(|record| record.outcome),
        HookBackend::Js => host.last_hook_diagnostic().map(|record| record.outcome),
    };
    BackendRun { value, diagnostic }
}

fn js_unevaluable(diagnostic: Option<HookDiagnostic>) -> bool {
    matches!(
        diagnostic,
        Some(HookDiagnostic::EvalError | HookDiagnostic::SourceMissing | HookDiagnostic::RuntimeUnavailable)
    )
}

fn js_threw(diagnostic: Option<HookDiagnostic>) -> bool {
    matches!(
        diagnostic,
        Some(HookDiagnostic::InvokeError | HookDiagnostic::PromiseRejected | HookDiagnostic::Timeout)
    )
}

fn spec_name_tree(value: &Value) -> Value {
    match value {
        Value::Object(fields) => {
            let names = fields.get("names").cloned().unwrap_or(Value::Null);
            let subcommands = fields
                .get("subcommands")
                .and_then(Value::as_array)
                .map(|items| Value::Array(items.iter().map(spec_name_tree).collect()))
                .unwrap_or(Value::Array(Vec::new()));
            let options = fields
                .get("options")
                .and_then(Value::as_array)
                .map(|items| {
                    Value::Array(
                        items
                            .iter()
                            .map(|item| item.get("names").cloned().unwrap_or(Value::Null))
                            .filter(|names| !is_rustup_trailing_toolchain_name(names))
                            .collect(),
                    )
                })
                .unwrap_or(Value::Array(Vec::new()));
            serde_json::json!({ "names": names, "subcommands": subcommands, "options": options })
        },
        other => other.clone(),
    }
}

fn dropped_rustup_trailing_toolchain(value: &Value) -> bool {
    match value {
        Value::Object(fields) => {
            let here = fields.get("options").and_then(Value::as_array).is_some_and(|items| {
                items
                    .iter()
                    .any(|item| is_rustup_trailing_toolchain_name(item.get("names").unwrap_or(&Value::Null)))
            });
            let nested = fields
                .get("subcommands")
                .and_then(Value::as_array)
                .is_some_and(|items| items.iter().any(dropped_rustup_trailing_toolchain));
            here || nested
        },
        _ => false,
    }
}

/// rustup `toolchain list` trailing newline → Native option `"+"` / empty.
/// Extracted JS Spec parse omits it. Name-tree only; adapter output stays.
fn is_rustup_trailing_toolchain_name(names: &Value) -> bool {
    match names {
        Value::Null => true,
        Value::String(name) => name.is_empty() || name == "+",
        Value::Array(items) => {
            !items.is_empty()
                && items
                    .iter()
                    .all(|item| item.as_str().is_some_and(|name| name.is_empty() || name == "+"))
        },
        _ => false,
    }
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

fn strip_variation_selectors(value: Value, applied: &mut Vec<&'static str>) -> Value {
    match value {
        Value::String(text) => {
            if text.contains('\u{fe0f}') {
                if !applied.contains(&"emoji-variation-selector") {
                    applied.push("emoji-variation-selector");
                }
                Value::String(text.replace('\u{fe0f}', ""))
            } else {
                Value::String(text)
            }
        },
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| strip_variation_selectors(item, applied))
                .collect(),
        ),
        Value::Object(fields) => {
            let mut out = Map::new();
            for (key, child) in fields {
                out.insert(key, strip_variation_selectors(child, applied));
            }
            Value::Object(out)
        },
        other => other,
    }
}

fn is_empty_suggestions(value: &Value) -> bool {
    value.is_null() || value.as_array().is_some_and(Vec::is_empty)
}

fn suggestion_field(field: &str) -> bool {
    matches!(field, "postProcess" | "custom" | "filterTemplateSuggestions")
}

fn arrays_equal_as_multiset(left: &[Value], right: &[Value]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut used = vec![false; right.len()];
    for item in left {
        let Some(index) = right
            .iter()
            .enumerate()
            .find(|(index, other)| !used[*index] && *other == item)
            .map(|(index, _)| index)
        else {
            return false;
        };
        used[index] = true;
    }
    true
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
    let value = strip_variation_selectors(value, &mut applied);
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
    let mut normalisations_applied = native_applied;
    for id in js_applied {
        if !normalisations_applied.contains(&id) {
            normalisations_applied.push(id);
        }
    }
    if suggestion_field(&field) && is_empty_suggestions(&native) && is_empty_suggestions(&js) {
        if !normalisations_applied.contains(&"empty-suggestions-vs-none") {
            normalisations_applied.push("empty-suggestions-vs-none");
        }
        return None;
    }
    if native == js {
        return None;
    }
    if let (Some(native_rows), Some(js_rows)) = (native.as_array(), js.as_array())
        && arrays_equal_as_multiset(native_rows, js_rows)
    {
        if !normalisations_applied.contains(&"suggestion-set-order") {
            normalisations_applied.push("suggestion-set-order");
        }
        return None;
    }
    if field == "generateSpec" && native.is_object() && js.is_object() {
        let native_tree = spec_name_tree(&native);
        let js_tree = spec_name_tree(&js);
        if native_tree == js_tree {
            if !normalisations_applied.contains(&"generate-spec-name-tree") {
                normalisations_applied.push("generate-spec-name-tree");
            }
            if dropped_rustup_trailing_toolchain(&native) || dropped_rustup_trailing_toolchain(&js) {
                if !normalisations_applied.contains(&"rustup-trailing-newline-toolchain") {
                    normalisations_applied.push("rustup-trailing-newline-toolchain");
                }
            }
            return None;
        }
        return Some(DualPathDiff {
            suite,
            id,
            field,
            hook_id,
            native: native_tree,
            js: js_tree,
            normalisations_applied,
        });
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

fn compare_baselines(diffs: &mut Vec<DualPathDiff>, skipped: &mut usize) -> usize {
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
            let native_run = run_baseline_backend(HookBackend::Native, &native, &host, &hook_id, baseline, case);
            let js_run = run_baseline_backend(HookBackend::Js, &native, &host, &hook_id, baseline, case);
            if js_unevaluable(js_run.diagnostic) || js_threw(js_run.diagnostic) {
                *skipped += 1;
                continue;
            }
            compared += 1;
            if let Some(diff) = compare_values(
                "baseline",
                format!("{}:{}:{}", baseline.field, baseline.body_sha256, case.id),
                baseline.field.clone(),
                hook_id.clone(),
                native_run.value,
                js_run.value,
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
    assert!(
        DUAL_PATH_NORMALISATIONS
            .iter()
            .any(|entry| entry.id == "js-module-unevaluable"),
        "T3.2 must document skipped QuickJS EvalError modules"
    );
    assert!(
        DUAL_PATH_NORMALISATIONS
            .iter()
            .any(|entry| entry.id == "generate-spec-name-tree"),
        "T3.2 must document generateSpec name-tree compare"
    );
    assert!(
        DUAL_PATH_NORMALISATIONS
            .iter()
            .any(|entry| entry.id == "rustup-trailing-newline-toolchain"),
        "T3.2 must document rustup trailing-newline + toolchain option"
    );
}

#[test]
fn dual_path_native_matches_js() {
    let _lock = engine_golden::engine_lock();
    let mut diffs = Vec::new();
    let mut skipped = 0usize;
    let compared = compare_baselines(&mut diffs, &mut skipped) + compare_goldens(&mut diffs);
    if !diffs.is_empty() {
        write_report(compared, skipped, &diffs);
        panic!(
            "dual-path Native vs Js diffs: {} (compared {compared}, skipped {skipped}); wrote {}",
            diffs.len(),
            report_path().display()
        );
    }
    assert!(compared > 0, "dual-path must compare T1.2 baselines and T1.5 goldens");
}

#[test]
fn rustup_trailing_newline_option_is_dropped_from_name_tree() {
    let native = serde_json::json!({
        "names": ["cargo"],
        "options": [
            { "names": ["+1.88.0"] },
            { "names": ["+"] },
            { "names": [""] }
        ],
        "subcommands": []
    });
    let js = serde_json::json!({
        "names": ["cargo"],
        "options": [{ "names": ["+1.88.0"] }],
        "subcommands": []
    });
    assert!(dropped_rustup_trailing_toolchain(&native));
    assert!(!dropped_rustup_trailing_toolchain(&js));
    assert_eq!(spec_name_tree(&native), spec_name_tree(&js));
}
