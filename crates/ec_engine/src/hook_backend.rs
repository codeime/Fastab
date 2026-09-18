//! Native hook backend: typed IR + named adapters.
//!
//! Runtime JavaScript is gone (T4.1). A miss records a [`HookDiagnostic`]
//! and returns empty — there is no QuickJS fallback.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value as JsonValue;

use crate::hook_types::{HookContext, HookDiagnostic, HookDiagnosticRecord, ScriptCommand, ShellContext};
use crate::ir::Spec;
use crate::process::{self, CommandError};
use crate::runtime::Suggestion;
use crate::snapshot::DirectorySnapshot;
use crate::typed_hook::{
    TypedExecRequest, TypedExecResult, TypedHookCatalog, TypedHookContext, TypedHookIr, evaluate_typed_alias,
    evaluate_typed_custom, evaluate_typed_filter_template_suggestions, evaluate_typed_generate_spec,
    evaluate_typed_get_query_term, evaluate_typed_load_spec, evaluate_typed_post_process, evaluate_typed_script,
    evaluate_typed_trigger, lookup_typed_hook, parse_typed_hook_catalog_bytes, suggestions_from_typed_json,
};

const TYPED_HOOKS_FILE: &str = "typed-hooks.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookBackend {
    Native,
}

#[derive(Debug, Clone)]
struct HookMeta {
    field: String,
    body_sha256: String,
}

pub struct NativeHooks {
    catalog: Option<TypedHookCatalog>,
    hooks: HashMap<String, HookMeta>,
}

impl NativeHooks {
    #[cfg(test)]
    pub(crate) fn from_catalog(catalog: crate::typed_hook::TypedHookCatalog) -> Self {
        let mut hooks = HashMap::new();
        for (id, entry) in catalog.hooks.iter() {
            hooks.insert(
                id.clone(),
                HookMeta {
                    field: entry.source_field.clone(),
                    body_sha256: entry.function_body_sha256.clone(),
                },
            );
        }
        for (id, entry) in catalog.adapters.iter() {
            hooks.insert(
                id.clone(),
                HookMeta {
                    field: entry.source_field.clone(),
                    body_sha256: entry.function_body_sha256.clone(),
                },
            );
        }
        Self {
            catalog: Some(catalog),
            hooks,
        }
    }

    pub fn load(specs_dir: &Path, snapshot: Option<&DirectorySnapshot>) -> Self {
        let typed_bytes = read_sidecar(specs_dir, snapshot, TYPED_HOOKS_FILE);
        let catalog = typed_bytes
            .as_deref()
            .and_then(|bytes| match parse_typed_hook_catalog_bytes(bytes) {
                Ok(catalog) => Some(catalog),
                Err(error) => {
                    tracing::warn!(%error, "typed hook catalog rejected");
                    None
                },
            });
        let mut hooks = HashMap::new();
        if let Some(catalog) = &catalog {
            for (id, entry) in catalog.hooks.iter() {
                hooks.insert(
                    id.clone(),
                    HookMeta {
                        field: entry.source_field.clone(),
                        body_sha256: entry.function_body_sha256.clone(),
                    },
                );
            }
            for (id, entry) in catalog.adapters.iter() {
                hooks.insert(
                    id.clone(),
                    HookMeta {
                        field: entry.source_field.clone(),
                        body_sha256: entry.function_body_sha256.clone(),
                    },
                );
            }
        }
        Self { catalog, hooks }
    }
}

fn read_sidecar(specs_dir: &Path, snapshot: Option<&DirectorySnapshot>, relative: &str) -> Option<Vec<u8>> {
    if let Some(snapshot) = snapshot {
        return snapshot.read_optional_file(Path::new(relative)).ok().flatten();
    }
    let path = specs_dir.join(relative);
    path.is_file().then(|| std::fs::read(path).ok()).flatten()
}

thread_local! {
    static SKIP_HOOKS: Cell<bool> = const { Cell::new(false) };
    static OVERRIDE: Cell<Option<HookBackend>> = const { Cell::new(None) };
    static NATIVE: RefCell<Option<Arc<NativeHooks>>> = const { RefCell::new(None) };
    static CWD: RefCell<Option<String>> = const { RefCell::new(None) };
    static SHELL: RefCell<Option<ShellContext>> = const { RefCell::new(None) };
    static LAST_DIAGNOSTIC: RefCell<Option<HookDiagnosticRecord>> = const { RefCell::new(None) };
}

pub fn current() -> HookBackend {
    if let Some(backend) = OVERRIDE.get() {
        return backend;
    }
    current_from_settings(&fig_settings::settings::Settings::new())
}

/// Force one backend for the duration of `f`.
#[allow(dead_code)]
pub fn with_backend<R>(backend: HookBackend, f: impl FnOnce() -> R) -> R {
    OVERRIDE.with(|cell| {
        let previous = cell.replace(Some(backend));
        let result = f();
        cell.set(previous);
        result
    })
}

pub fn current_from_settings(settings: &fig_settings::settings::Settings) -> HookBackend {
    let _ = settings;
    HookBackend::Native
}

#[cfg(test)]
fn parse_backend_name(value: Option<&str>) -> Option<HookBackend> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "native" => Some(HookBackend::Native),
        _ => None,
    }
}

pub fn bind_native(native: Arc<NativeHooks>) -> BoundNative {
    NATIVE.with(|cell| {
        let previous = cell.replace(Some(native));
        BoundNative { previous }
    })
}

pub struct BoundNative {
    previous: Option<Arc<NativeHooks>>,
}

impl Drop for BoundNative {
    fn drop(&mut self) {
        NATIVE.with(|cell| {
            *cell.borrow_mut() = self.previous.take();
        });
    }
}

pub fn enter_context<R>(cwd: &str, shell: &ShellContext, f: impl FnOnce() -> R) -> R {
    CWD.with(|cell| {
        let previous_cwd = cell.replace(Some(cwd.to_string()));
        let result = SHELL.with(|shell_cell| {
            let previous_shell = shell_cell.replace(Some(shell.clone()));
            let result = f();
            *shell_cell.borrow_mut() = previous_shell;
            result
        });
        *cell.borrow_mut() = previous_cwd;
        result
    })
}

pub fn without_hooks<R>(f: impl FnOnce() -> R) -> R {
    SKIP_HOOKS.with(|cell| {
        let previous = cell.replace(true);
        let result = f();
        cell.set(previous);
        result
    })
}

#[allow(dead_code)]
pub fn last_diagnostic() -> Option<HookDiagnosticRecord> {
    LAST_DIAGNOSTIC.with(|cell| cell.borrow().clone())
}

fn record(hook_id: &str, outcome: HookDiagnostic) {
    LAST_DIAGNOSTIC.with(|cell| {
        *cell.borrow_mut() = Some(HookDiagnosticRecord {
            hook_id: hook_id.to_string(),
            outcome,
        });
    });
}

fn hooks_skipped() -> bool {
    SKIP_HOOKS.get()
}

fn native() -> Option<Arc<NativeHooks>> {
    NATIVE.with(|cell| cell.borrow().clone())
}

fn session_shell() -> ShellContext {
    SHELL.with(|cell| cell.borrow().clone().unwrap_or_default())
}

pub fn current_cwd() -> Option<String> {
    CWD.with(|cell| cell.borrow().clone())
}

pub fn current_shell() -> ShellContext {
    session_shell()
}

fn finish_option<T>(hook_id: &str, value: Option<T>, empty: impl Fn(&T) -> bool) -> Option<T> {
    match value {
        Some(value) if empty(&value) => {
            record(hook_id, HookDiagnostic::EmptyResult);
            Some(value)
        },
        Some(value) => {
            record(hook_id, HookDiagnostic::Success);
            Some(value)
        },
        None => {
            record(hook_id, HookDiagnostic::EmptyResult);
            None
        },
    }
}

fn missing(hook_id: &str) -> Option<std::convert::Infallible> {
    record(hook_id, HookDiagnostic::SourceMissing);
    None
}

fn invoke_err<T>(hook_id: &str) -> Option<T> {
    record(hook_id, HookDiagnostic::InvokeError);
    None
}

/// A hook that ran out of its `autocomplete.scriptTimeout` budget is not a
/// broken hook. Keep the two apart in the diagnostic, as the QuickJS
/// interrupt handler did.
fn eval_err<T>(hook_id: &str, error: &crate::typed_hook::TypedHookError) -> Option<T> {
    if error.is_timed_out() {
        record(hook_id, HookDiagnostic::Timeout);
        return None;
    }
    invoke_err(hook_id)
}

pub fn dispatch_trigger(hook_id: &str, search_term: &str, previous: &str) -> Option<bool> {
    if hooks_skipped() {
        return None;
    }
    match current() {
        HookBackend::Native => native_trigger(hook_id, search_term, previous),
    }
}

pub fn dispatch_get_query_term(hook_id: &str, search_term: &str) -> Option<String> {
    if hooks_skipped() {
        return None;
    }
    match current() {
        HookBackend::Native => native_get_query_term(hook_id, search_term),
    }
}

pub fn dispatch_post_process(hook_id: &str, stdout: &str, tokens: &[String]) -> Option<Vec<Suggestion>> {
    if hooks_skipped() {
        return None;
    }
    match current() {
        HookBackend::Native => native_post_process(hook_id, stdout, tokens),
    }
}

pub fn dispatch_script_command(hook_id: &str, tokens: &[String]) -> Option<ScriptCommand> {
    if hooks_skipped() {
        return None;
    }
    match current() {
        HookBackend::Native => native_script(hook_id, tokens),
    }
}

pub fn dispatch_filter_template_suggestions(hook_id: &str, suggestions: &[Suggestion]) -> Option<Vec<Suggestion>> {
    if hooks_skipped() {
        return None;
    }
    match current() {
        HookBackend::Native => native_filter(hook_id, suggestions),
    }
}

pub fn dispatch_custom(
    hook_id: &str,
    tokens: &[String],
    cwd: &str,
    search_term: &str,
    timeout: Duration,
    is_dangerous: bool,
) -> Option<Vec<Suggestion>> {
    if hooks_skipped() {
        return None;
    }
    match current() {
        HookBackend::Native => native_custom(hook_id, tokens, cwd, search_term, timeout, is_dangerous),
    }
}

pub fn dispatch_alias(hook_id: &str, token: &str, cwd: &str, timeout: Duration) -> Option<String> {
    if hooks_skipped() {
        return None;
    }
    match current() {
        HookBackend::Native => native_alias(hook_id, token, cwd, timeout),
    }
}

pub fn dispatch_load_spec(hook_id: &str, token: &str, cwd: &str, timeout: Duration) -> Option<Spec> {
    if hooks_skipped() {
        return None;
    }
    match current() {
        HookBackend::Native => native_load_spec(hook_id, token, cwd, timeout),
    }
}

pub fn dispatch_generate_spec(hook_id: &str, tokens: &[String], cwd: &str, timeout: Duration) -> Option<Spec> {
    if hooks_skipped() {
        return None;
    }
    match current() {
        HookBackend::Native => native_generate_spec(hook_id, tokens, cwd, timeout),
    }
}

fn typed_entry<'a>(native: &'a NativeHooks, hook_id: &str) -> Option<&'a TypedHookIr> {
    lookup_typed_hook(native.catalog.as_ref()?, hook_id).map(|entry| &entry.descriptor)
}

fn adapter_sha<'a>(native: &'a NativeHooks, hook_id: &str, field: &str) -> Option<&'a str> {
    let meta = native.hooks.get(hook_id)?;
    (meta.field == field).then_some(meta.body_sha256.as_str())
}

fn native_trigger(hook_id: &str, search_term: &str, previous: &str) -> Option<bool> {
    let native = native()?;
    if let Some(descriptor) = typed_entry(&native, hook_id) {
        return match evaluate_typed_trigger(descriptor, search_term, previous) {
            Ok(value) => finish_option(hook_id, Some(value), |_| false),
            Err(error) => eval_err(hook_id, &error),
        };
    }
    let _ = missing(hook_id);
    None
}

fn native_get_query_term(hook_id: &str, search_term: &str) -> Option<String> {
    let native = native()?;
    if let Some(descriptor) = typed_entry(&native, hook_id) {
        return match evaluate_typed_get_query_term(descriptor, search_term) {
            Ok(value) => finish_option(hook_id, Some(value), String::is_empty),
            Err(error) => eval_err(hook_id, &error),
        };
    }
    let _ = missing(hook_id);
    None
}

fn native_post_process(hook_id: &str, stdout: &str, tokens: &[String]) -> Option<Vec<Suggestion>> {
    let native = native()?;
    // Named adapters own their body SHA (T2.4). A typed compile of the same
    // body is a fallback, not a shadow — yarn's package.json parser is the
    // case that made this order load-bearing.
    if let Some(sha) = adapter_sha(&native, hook_id, "postProcess")
        && let Some(json) = crate::native_adapters::evaluate_post_process(sha, stdout, tokens)
    {
        return json_suggestions(hook_id, json);
    }
    if let Some(descriptor) = typed_entry(&native, hook_id) {
        return match evaluate_typed_post_process(descriptor, stdout, tokens) {
            Ok(value) => finish_option(hook_id, Some(value), Vec::is_empty),
            Err(error) => eval_err(hook_id, &error),
        };
    }
    let _ = missing(hook_id);
    None
}

fn native_script(hook_id: &str, tokens: &[String]) -> Option<ScriptCommand> {
    let native = native()?;
    if let Some(descriptor) = typed_entry(&native, hook_id) {
        return match evaluate_typed_script(descriptor, tokens) {
            Ok(value) => finish_option(hook_id, Some(value), |command| command.command.is_empty()),
            Err(error) => eval_err(hook_id, &error),
        };
    }
    let _ = missing(hook_id);
    None
}

fn native_filter(hook_id: &str, suggestions: &[Suggestion]) -> Option<Vec<Suggestion>> {
    let native = native()?;
    if let Some(sha) = adapter_sha(&native, hook_id, "filterTemplateSuggestions") {
        let payload: Vec<JsonValue> = suggestions
            .iter()
            .map(|item| {
                serde_json::json!({
                    "name": item.name,
                    "description": item.description,
                    "type": item.kind,
                    "insertValue": item.insert_value,
                    "displayName": item.display_name,
                    "icon": item.icon,
                    "hidden": item.hidden,
                    "isDangerous": item.is_dangerous,
                    "priority": item.priority,
                    "shouldAddSpace": item.should_add_space,
                })
            })
            .collect();
        if let Some(json) = crate::native_adapters::evaluate_filter(sha, &payload) {
            return json_suggestions(hook_id, json);
        }
    }
    if let Some(descriptor) = typed_entry(&native, hook_id) {
        return match evaluate_typed_filter_template_suggestions(descriptor, suggestions) {
            Ok(value) => finish_option(hook_id, Some(value), Vec::is_empty),
            Err(error) => eval_err(hook_id, &error),
        };
    }
    let _ = missing(hook_id);
    None
}

fn native_custom(
    hook_id: &str,
    tokens: &[String],
    cwd: &str,
    search_term: &str,
    timeout: Duration,
    is_dangerous: bool,
) -> Option<Vec<Suggestion>> {
    let native = native()?;
    let shell = session_shell();
    let context = HookContext::from_shell(cwd, &shell, search_term, is_dangerous);
    if let Some(sha) = adapter_sha(&native, hook_id, "custom") {
        let exec = live_adapter_exec(cwd, timeout);
        if let Some(json) = crate::native_adapters::evaluate_custom(sha, tokens, &exec, &context) {
            return json_suggestions(hook_id, json);
        }
    }
    if let Some(descriptor) = typed_entry(&native, hook_id) {
        let typed_ctx = typed_context(&context);
        let exec = live_typed_exec(cwd, timeout);
        let deadline = Some(Instant::now() + timeout + Duration::from_secs(2));
        return match evaluate_typed_custom(descriptor, tokens, &typed_ctx, &exec, deadline) {
            Ok(value) => finish_option(hook_id, Some(value), Vec::is_empty),
            Err(error) => eval_err(hook_id, &error),
        };
    }
    let _ = missing(hook_id);
    None
}

fn native_alias(hook_id: &str, token: &str, cwd: &str, timeout: Duration) -> Option<String> {
    let native = native()?;
    let context = HookContext::from_shell(cwd, &session_shell(), token, false);
    if let Some(descriptor) = typed_entry(&native, hook_id) {
        let typed_ctx = typed_context(&context);
        let exec = live_typed_exec(cwd, timeout);
        let deadline = Some(Instant::now() + timeout + Duration::from_secs(2));
        return match evaluate_typed_alias(descriptor, token, &typed_ctx, &exec, deadline) {
            Ok(value) => finish_option(hook_id, Some(value), String::is_empty),
            Err(error) => eval_err(hook_id, &error),
        };
    }
    let _ = missing(hook_id);
    None
}

fn native_load_spec(hook_id: &str, token: &str, cwd: &str, timeout: Duration) -> Option<Spec> {
    let native = native()?;
    let context = HookContext::from_shell(cwd, &session_shell(), token, false);
    if let Some(descriptor) = typed_entry(&native, hook_id) {
        let typed_ctx = typed_context(&context);
        let exec = live_typed_exec(cwd, timeout);
        let deadline = Some(Instant::now() + timeout + Duration::from_secs(2));
        return match evaluate_typed_load_spec(descriptor, token, &typed_ctx, &exec, deadline) {
            Ok(json) => spec_from_json(hook_id, json),
            Err(error) => eval_err(hook_id, &error),
        };
    }
    let _ = missing(hook_id);
    None
}

fn native_generate_spec(hook_id: &str, tokens: &[String], cwd: &str, timeout: Duration) -> Option<Spec> {
    let native = native()?;
    let context = HookContext::from_shell(cwd, &session_shell(), tokens.last().map_or("", String::as_str), false);
    if let Some(sha) = adapter_sha(&native, hook_id, "generateSpec") {
        let exec = live_adapter_exec(cwd, timeout);
        if let Some(json) = crate::native_adapters::evaluate_generate_spec(sha, tokens, &exec, &context) {
            return spec_from_json_result(hook_id, json);
        }
    }
    if let Some(descriptor) = typed_entry(&native, hook_id) {
        let typed_ctx = typed_context(&context);
        let exec = live_typed_exec(cwd, timeout);
        let deadline = Some(Instant::now() + timeout + Duration::from_secs(2));
        return match evaluate_typed_generate_spec(descriptor, tokens, &typed_ctx, &exec, deadline) {
            Ok(json) => spec_from_json(hook_id, json),
            Err(error) => eval_err(hook_id, &error),
        };
    }
    let _ = missing(hook_id);
    None
}

fn json_suggestions(hook_id: &str, result: Result<JsonValue, String>) -> Option<Vec<Suggestion>> {
    match result {
        Ok(json) => match suggestions_from_typed_json(&json) {
            Ok(value) => finish_option(hook_id, Some(value), Vec::is_empty),
            Err(_) => {
                record(hook_id, HookDiagnostic::ResultConversionError);
                None
            },
        },
        Err(_) => invoke_err(hook_id),
    }
}

fn spec_from_json_result(hook_id: &str, result: Result<JsonValue, String>) -> Option<Spec> {
    match result {
        Ok(json) => spec_from_json(hook_id, json),
        Err(_) => invoke_err(hook_id),
    }
}

fn spec_from_json(hook_id: &str, json: JsonValue) -> Option<Spec> {
    let spec = fig_spec_from_json(&json);
    match spec {
        Some(spec) => {
            record(hook_id, HookDiagnostic::Success);
            Some(spec)
        },
        None => {
            record(hook_id, HookDiagnostic::ResultConversionError);
            None
        },
    }
}

fn fig_spec_from_json(value: &JsonValue) -> Option<Spec> {
    crate::fig_spec::spec_from_fig_json(value)
}

fn typed_context(context: &HookContext) -> TypedHookContext {
    TypedHookContext {
        current_working_directory: context.current_working_directory.clone(),
        current_process: context.current_process.clone(),
        ssh_prefix: context.ssh_prefix.clone(),
        environment_variables: context
            .environment_variables
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        search_term: context.search_term.clone(),
        is_dangerous: context.is_dangerous,
    }
}

fn live_typed_exec(
    cwd: &str,
    timeout: Duration,
) -> impl Fn(TypedExecRequest) -> Result<TypedExecResult, crate::typed_hook::TypedHookError> + '_ {
    move |request| {
        let command_cwd = request.cwd.as_deref().filter(|value| !value.is_empty()).unwrap_or(cwd);
        let command_timeout = request.timeout_ms.map_or(timeout, Duration::from_millis);
        match process::execute_full(
            &request.command,
            &request.args,
            command_cwd,
            &request.env,
            command_timeout,
        ) {
            Ok(output) => Ok(TypedExecResult {
                stdout: output.stdout,
                stderr: output.stderr,
                status: i64::from(output.status),
            }),
            Err(CommandError::TimedOut) => Err(crate::typed_hook::TypedHookError::timed_out("timeout")),
            Err(CommandError::Failed) => Err(crate::typed_hook::TypedHookError::new("exec failed")),
        }
    }
}

fn live_adapter_exec(
    cwd: &str,
    timeout: Duration,
) -> impl Fn(
    crate::native_adapters::AdapterExecRequest,
) -> Result<crate::native_adapters::AdapterExecResult, crate::native_adapters::AdapterError>
+ '_ {
    move |request| {
        let command_cwd = request.cwd.as_deref().filter(|value| !value.is_empty()).unwrap_or(cwd);
        let command_timeout = request
            .timeout
            .and_then(|ms| u64::try_from(ms).ok())
            .map_or(timeout, Duration::from_millis);
        let args: Vec<String> = request
            .args
            .iter()
            .filter_map(JsonValue::as_str)
            .map(ToOwned::to_owned)
            .collect();
        let env = match request.env.as_ref() {
            Some(JsonValue::Object(fields)) => fields
                .iter()
                .filter_map(|(key, value)| value.as_str().map(|text| (key.clone(), text.to_string())))
                .collect(),
            _ => Vec::new(),
        };
        match process::execute_full(&request.command, &args, command_cwd, &env, command_timeout) {
            Ok(output) => Ok(crate::native_adapters::AdapterExecResult {
                stdout: output.stdout,
                stderr: output.stderr,
                status: i64::from(output.status),
            }),
            Err(_) => Err(crate::native_adapters::adapter_throw("Error")),
        }
    }
}

pub fn merge_generated_spec(wrapper: &Spec, generated: Spec) -> Spec {
    crate::fig_spec::merge_generated_spec(wrapper, generated)
}

pub use crate::hook_types::clean_output;

#[cfg(test)]
pub(crate) fn test_sidecar_contracts() -> JsonValue {
    serde_json::json!({
        "trigger": {"irVersion": 1, "params": ["string", "string"], "resultType": "bool"},
        "getQueryTerm": {"irVersion": 1, "params": ["string"], "resultType": "string"},
        "postProcess": {"irVersion": 1, "params": ["string", "string-array"], "resultType": "suggestion-array"},
        "script": {"irVersion": 1, "params": ["string-array"], "resultType": "string-array"},
        "filterTemplateSuggestions": {"irVersion": 1, "params": ["suggestion-array"], "resultType": "suggestion-array"},
        "custom": {"irVersion": 1, "params": ["string-array", "exec", "context"], "resultType": "suggestion-array"},
        "alias": {"irVersion": 1, "params": ["string", "exec"], "resultType": "string"},
        "loadSpec": {"irVersion": 1, "params": ["string", "exec"], "resultType": "spec"},
        "generateSpec": {"irVersion": 1, "params": ["string-array", "exec"], "resultType": "spec"}
    })
}

#[cfg(test)]
pub(crate) fn test_typed_entry(id: &str, field: &str, expr: JsonValue) -> (String, JsonValue) {
    let (params, result_type) = match field {
        "trigger" => (
            serde_json::json!([{"index": 0, "type": "string"}, {"index": 1, "type": "string"}]),
            "bool",
        ),
        "getQueryTerm" => (serde_json::json!([{"index": 0, "type": "string"}]), "string"),
        "postProcess" => (
            serde_json::json!([{"index": 0, "type": "string"}, {"index": 1, "type": "string-array"}]),
            "suggestion-array",
        ),
        "script" => (
            serde_json::json!([{"index": 0, "type": "string-array"}]),
            "string-array",
        ),
        "filterTemplateSuggestions" => (
            serde_json::json!([{"index": 0, "type": "suggestion-array"}]),
            "suggestion-array",
        ),
        "custom" => (
            serde_json::json!([
                {"index": 0, "type": "string-array"},
                {"index": 1, "type": "exec"},
                {"index": 2, "type": "context"}
            ]),
            "suggestion-array",
        ),
        "alias" => (
            serde_json::json!([{"index": 0, "type": "string"}, {"index": 1, "type": "exec"}]),
            "string",
        ),
        "loadSpec" => (
            serde_json::json!([{"index": 0, "type": "string"}, {"index": 1, "type": "exec"}]),
            "spec",
        ),
        "generateSpec" => (
            serde_json::json!([{"index": 0, "type": "string-array"}, {"index": 1, "type": "exec"}]),
            "spec",
        ),
        other => panic!("unsupported test hook field {other}"),
    };
    let mut sha = String::new();
    for byte in id.as_bytes() {
        sha.push_str(&format!("{byte:02x}"));
    }
    while sha.len() < 64 {
        sha.push('0');
    }
    sha.truncate(64);
    (
        id.to_string(),
        serde_json::json!({
            "module": "test.js",
            "moduleSha256": "0".repeat(64),
            "path": "root.args[0]",
            "sourceField": field,
            "functionBodySha256": sha,
            "descriptor": {
                "version": 1,
                "kind": "typed-hook-expression",
                "sourceField": field,
                "resultType": result_type,
                "params": params,
                "expr": expr
            }
        }),
    )
}

#[cfg(test)]
pub(crate) fn test_native_hooks(entries: Vec<(String, JsonValue)>) -> NativeHooks {
    use crate::typed_hook::parse_typed_hook_catalog;
    let hooks = JsonValue::Object(entries.into_iter().collect());
    let catalog = serde_json::json!({
        "version": 1,
        "kind": "typed-hook-expressions",
        "contracts": test_sidecar_contracts(),
        "hooks": hooks
    });
    let catalog = parse_typed_hook_catalog(&catalog).expect("test typed catalog");
    NativeHooks::from_catalog(catalog)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_backend_is_native() {
        let settings = fig_settings::settings::Settings::from_slice(&[]);
        let _ = settings;
        assert_eq!(current(), HookBackend::Native);
        assert_eq!(
            current_from_settings(&fig_settings::settings::Settings::from_slice(&[])),
            HookBackend::Native
        );
        assert_eq!(parse_backend_name(Some("native")), Some(HookBackend::Native));
        assert_eq!(parse_backend_name(Some("js")), None);
    }

    #[test]
    fn override_selects_native_without_touching_process_env() {
        with_backend(HookBackend::Native, || {
            assert_eq!(current(), HookBackend::Native);
        });
    }

    #[test]
    fn native_miss_records_source_missing_and_does_not_run_js() {
        let native = Arc::new(NativeHooks {
            catalog: None,
            hooks: HashMap::new(),
        });
        let _bound = bind_native(Arc::clone(&native));
        with_backend(HookBackend::Native, || {
            assert!(dispatch_trigger("missing#trigger#0", "a", "b").is_none());
            let record = last_diagnostic().expect("native miss records a diagnostic");
            assert_eq!(record.hook_id, "missing#trigger#0");
            assert_eq!(record.outcome, HookDiagnostic::SourceMissing);
        });
    }
}
