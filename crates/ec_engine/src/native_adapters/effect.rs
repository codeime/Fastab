//! Shared exec/context helpers for T2.5 leftover effect adapters.

use serde_json::{Map, Value as JsonValue};

use crate::hook_types::HookContext;

#[cfg(test)]
use crate::hook_baseline::{ExecRule, Expected};
#[cfg(test)]
use serde_json::json;

use super::eval::{AdapterError, AdapterResult, js_index_of, js_slice, js_to_string, throw};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AdapterExecRequest {
    pub command: String,
    pub args: Vec<JsonValue>,
    pub cwd: Option<String>,
    pub env: Option<JsonValue>,
    pub timeout: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AdapterExecResult {
    pub stdout: String,
    pub stderr: String,
    pub status: i64,
}

pub(crate) type AdapterExec<'a> = dyn Fn(AdapterExecRequest) -> Result<AdapterExecResult, AdapterError> + 'a;

/// A JS `Map` with string keys. `Map.prototype.keys` / `values` enumerate
/// in insertion order and `set` on an existing key keeps its position; a
/// `HashMap` here would hand the ranker a different candidate order on
/// every run, and `BTreeMap` would sort what the hook never sorted. Plain
/// objects (`Object.keys` on a `JSON.parse` result) go through
/// `serde_json::Map`, which the workspace builds with `preserve_order`.
#[derive(Debug, Default)]
pub(super) struct JsMap<V> {
    entries: Vec<(String, V)>,
}

impl<V> JsMap<V> {
    pub(super) fn new() -> Self {
        Self { entries: Vec::new() }
    }

    fn position(&self, key: &str) -> Option<usize> {
        self.entries.iter().position(|(existing, _)| existing == key)
    }

    pub(super) fn insert(&mut self, key: String, value: V) {
        match self.position(&key) {
            Some(index) => self.entries[index].1 = value,
            None => self.entries.push((key, value)),
        }
    }

    pub(super) fn get(&self, key: &str) -> Option<&V> {
        self.position(key).map(|index| &self.entries[index].1)
    }

    pub(super) fn entry_or_default(&mut self, key: String) -> &mut V
    where
        V: Default,
    {
        let index = match self.position(&key) {
            Some(index) => index,
            None => {
                self.entries.push((key, V::default()));
                self.entries.len() - 1
            },
        };
        &mut self.entries[index].1
    }

    pub(super) fn keys(&self) -> impl Iterator<Item = &String> {
        self.entries.iter().map(|(key, _)| key)
    }

    pub(super) fn entries(&self) -> impl Iterator<Item = (&String, &V)> {
        self.entries.iter().map(|(key, value)| (key, value))
    }

    pub(super) fn into_values(self) -> Vec<V> {
        self.entries.into_iter().map(|(_, value)| value).collect()
    }
}

pub(super) fn last_token(tokens: &[String]) -> String {
    tokens.last().cloned().unwrap_or_default()
}

pub(super) fn env_var(context: &HookContext, name: &str) -> String {
    context.environment_variables.get(name).cloned().unwrap_or_default()
}

pub(super) fn exec_object(
    exec: &AdapterExec<'_>,
    command: impl Into<String>,
    args: impl IntoIterator<Item = impl Into<String>>,
) -> Result<AdapterExecResult, AdapterError> {
    exec(AdapterExecRequest {
        command: command.into(),
        args: args.into_iter().map(|arg| JsonValue::String(arg.into())).collect(),
        cwd: None,
        env: None,
        timeout: None,
    })
}

#[cfg(test)]
fn exec_descriptor(
    command: &str,
    args: &JsonValue,
    cwd: Option<&str>,
    env: Option<&JsonValue>,
    timeout: Option<i64>,
) -> JsonValue {
    json!({
        "command": command,
        "args": args,
        "cwd": cwd,
        "env": env,
        "timeout": timeout,
    })
}

#[cfg(test)]
pub(super) fn mock_exec_from_rules(
    rules: &[ExecRule],
) -> impl Fn(AdapterExecRequest) -> Result<AdapterExecResult, AdapterError> + '_ {
    move |request| {
        let request_args = JsonValue::Array(request.args.clone());
        let want = exec_descriptor(
            &request.command,
            &request_args,
            request.cwd.as_deref(),
            request.env.as_ref(),
            request.timeout,
        );
        for rule in rules {
            let command = rule.command.as_deref().unwrap_or("");
            if command.is_empty() {
                continue;
            }
            let args = JsonValue::Array(
                rule.args
                    .clone()
                    .unwrap_or_default()
                    .into_iter()
                    .map(JsonValue::String)
                    .collect(),
            );
            let got = exec_descriptor(command, &args, None, None, None);
            if want == got {
                return Ok(AdapterExecResult {
                    stdout: rule.stdout.clone().unwrap_or_default(),
                    stderr: rule.stderr.clone().unwrap_or_default(),
                    status: rule.status.unwrap_or(0),
                });
            }
        }
        Err(throw("UnmockedCommand"))
    }
}

#[cfg(test)]
pub(super) fn expected_from_field(field: &str, result: AdapterResult) -> Expected {
    match result {
        Err(error) => Expected::Error {
            value: error.js_class.unwrap_or("Error").to_string(),
        },
        Ok(value) => match field {
            "custom" => super::eval::normalize_expected(value),
            "alias" => match value.as_str() {
                Some(text) => Expected::String { value: text.to_owned() },
                None => Expected::Error {
                    value: "typed alias result must be a string".into(),
                },
            },
            "loadSpec" | "generateSpec" => Expected::Spec { value },
            _ => super::eval::normalize_expected(value),
        },
    }
}

fn suggestion_from_name(name: impl Into<String>) -> JsonValue {
    let mut object = Map::new();
    object.insert("name".into(), JsonValue::String(name.into()));
    JsonValue::Object(object)
}

fn as_suggestion(item: &JsonValue) -> JsonValue {
    match item {
        JsonValue::String(name) => suggestion_from_name(name),
        other => other.clone(),
    }
}

fn with_insert_suffix(suffix: &str, items: &[JsonValue]) -> Vec<JsonValue> {
    if suffix.is_empty() {
        return items.iter().map(as_suggestion).collect();
    }
    items
        .iter()
        .map(|item| {
            let mut object = match as_suggestion(item) {
                JsonValue::Object(object) => object,
                other => return other,
            };
            if object.get("insertValue").and_then(JsonValue::as_str).is_none() {
                let name = js_to_string(object.get("name"));
                object.insert("insertValue".into(), JsonValue::String(format!("{name}{suffix}")));
            }
            JsonValue::Object(object)
        })
        .collect()
}

fn names_of(item: &JsonValue) -> Vec<String> {
    match item.get("name") {
        Some(JsonValue::String(name)) => vec![name.clone()],
        Some(JsonValue::Array(names)) => names
            .iter()
            .filter_map(JsonValue::as_str)
            .map(ToOwned::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

fn filter_used(used: &[String], items: Vec<JsonValue>) -> Vec<JsonValue> {
    let used: std::collections::HashSet<&str> = used.iter().map(String::as_str).collect();
    items
        .into_iter()
        .filter(|item| {
            let names = names_of(item);
            if names.is_empty() {
                return true;
            }
            names.iter().all(|name| !used.contains(name.as_str()))
        })
        .collect()
}

pub(super) fn key_value(
    token: &str,
    separator: &str,
    keys: &[JsonValue],
    values: &[JsonValue],
    insert_separator: bool,
) -> AdapterResult {
    let choosing_keys = !token.contains(separator);
    let list = if choosing_keys { keys } else { values };
    let suffix = if choosing_keys && insert_separator {
        separator
    } else {
        ""
    };
    Ok(JsonValue::Array(with_insert_suffix(suffix, list)))
}

fn last_index_of(value: &str, needle: &str) -> i64 {
    if needle.is_empty() {
        return value.encode_utf16().count() as i64;
    }
    match value.rfind(needle) {
        Some(index) => value[..index].encode_utf16().count() as i64,
        None => -1,
    }
}

fn last_index_of_any(value: &str, needles: &[&str]) -> i64 {
    needles
        .iter()
        .map(|needle| last_index_of(value, needle))
        .max()
        .unwrap_or(-1)
}

/// `keyValueList`'s side switch: keys unless the last separator-or-delimiter
/// in the token is the separator. The factory only evaluates the side it
/// chose, so callers with an effectful side compute this first.
pub(super) fn key_value_list_chooses_keys(token: &str, separator: &str, delimiter: &str) -> bool {
    let last = last_index_of_any(token, &[separator, delimiter]);
    last < 0 || {
        let start = utf16_slice_start(token, last);
        !token[start..].starts_with(separator)
    }
}

pub(super) fn key_value_list(
    token: &str,
    separator: &str,
    delimiter: &str,
    keys: &[JsonValue],
    values: &[JsonValue],
    insert_separator: bool,
    insert_delimiter: bool,
    allow_repeated_keys: bool,
    allow_repeated_values: bool,
) -> AdapterResult {
    let choosing_keys = key_value_list_chooses_keys(token, separator, delimiter);
    let list = if choosing_keys { keys } else { values };
    let suffix = if choosing_keys {
        if insert_separator { separator } else { "" }
    } else if insert_delimiter {
        delimiter
    } else {
        ""
    };
    let rows = with_insert_suffix(suffix, list);
    // Both `used` lists keep the spec's `indexOf` arithmetic when a part has
    // no separator: `m.slice(0, -1)` drops the part's last character and
    // `m.slice(-1 + separator.length)` keeps the whole part, so a bare `x`
    // in `x,a:` hides the value `x` exactly as the JS did.
    if choosing_keys {
        if allow_repeated_keys {
            return Ok(JsonValue::Array(rows));
        }
        let used: Vec<String> = token
            .split(delimiter)
            .map(|part| js_slice(part, 0, Some(js_index_of(part, separator))))
            .collect();
        return Ok(JsonValue::Array(filter_used(&used, rows)));
    }
    if allow_repeated_values {
        return Ok(JsonValue::Array(rows));
    }
    let used: Vec<String> = token
        .split(delimiter)
        .map(|part| {
            let separator_units = separator.encode_utf16().count() as i64;
            js_slice(part, js_index_of(part, separator) + separator_units, None)
        })
        .collect();
    Ok(JsonValue::Array(filter_used(&used, rows)))
}

pub(super) fn value_list(
    token: &str,
    delimiter: &str,
    values: &[JsonValue],
    insert_delimiter: bool,
    allow_repeated: bool,
) -> AdapterResult {
    let suffix = if insert_delimiter { delimiter } else { "" };
    let rows = with_insert_suffix(suffix, values);
    if allow_repeated {
        return Ok(JsonValue::Array(rows));
    }
    let used: Vec<String> = token.split(delimiter).map(ToOwned::to_owned).collect();
    Ok(JsonValue::Array(filter_used(&used, rows)))
}

fn utf16_slice_start(value: &str, unit: i64) -> usize {
    if unit <= 0 {
        return 0;
    }
    let mut consumed = 0i64;
    for (index, ch) in value.char_indices() {
        if consumed >= unit {
            return index;
        }
        consumed += ch.len_utf16() as i64;
    }
    value.len()
}

/// The literal tables the adapters closed over in the spec source (man
/// sections, esbuild loaders, the `cargo --config` keys, …), compiled into
/// the binary. These used to be read from `CARGO_MANIFEST_DIR` at call time,
/// which is the *build* machine's checkout: an installed app whose source
/// tree had moved panicked inside the attempt thread on `man 3<TAB>`, and a
/// prebuilt `.app` on another machine never had the files at all. The
/// directory is the source of truth; `embedded_lists_match_the_directory`
/// keeps this table in step with it.
macro_rules! adapter_lists {
    ($($file:literal),* $(,)?) => {
        #[cfg(test)]
        pub(super) const ADAPTER_LIST_FILES: &[&str] = &[$($file),*];

        fn adapter_source(file: &str) -> &'static str {
            match file {
                $($file => include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/testdata/native-hooks/adapter-lists/",
                    $file
                )),)*
                other => panic!("adapter list {other} is not embedded; add it to adapter_lists! in effect.rs"),
            }
        }
    };
}

adapter_lists!(
    "airflow_custom_0.json",
    "bun_custom_15.json",
    "cargo-config-keys.json",
    "cargo_generateSpec_0.json",
    "chezmoi-attrs-plus.json",
    "chezmoi-includes.json",
    "chezmoi_custom_24.json",
    "deno-lint-rules.json",
    "esbuild-loaders.json",
    "esbuild_custom_4.json",
    "file_custom_0.json",
    "kamal_generateSpec_0.json",
    "man-sections.json",
    "nx_generateSpec_0.json",
    "osqueryi-tables.json",
    "oxlint-categories.json",
    "php_generateSpec_0.json",
    "pnpm_generateSpec_19.json",
    "rails_generateSpec_0.json",
    "rich-styles.json",
    "task_generateSpec_0.json",
    "twilio-resources.json",
    "z_generateSpec_0.json",
);

pub(super) fn adapter_list(file: &str) -> Vec<JsonValue> {
    match adapter_json(file) {
        JsonValue::Array(items) => items,
        other => panic!("adapter list {file} is not a JSON array: {other}"),
    }
}

pub(super) fn adapter_json(file: &str) -> JsonValue {
    serde_json::from_str(adapter_source(file)).unwrap_or_else(|error| panic!("parse adapter list {file}: {error}"))
}

#[allow(dead_code)]
pub(super) fn named_strings(names: &[&str]) -> Vec<JsonValue> {
    names.iter().map(|name| suggestion_from_name(*name)).collect()
}

pub(super) fn parse_json(source: &str) -> Result<JsonValue, AdapterError> {
    serde_json::from_str(source).map_err(|_error| throw("SyntaxError"))
}

#[allow(dead_code)]
pub(super) fn index_of_sep(value: &str, needle: &str) -> i64 {
    js_index_of(value, needle)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn embedded_lists_match_the_directory() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/native-hooks/adapter-lists");
        let on_disk: BTreeSet<String> = std::fs::read_dir(&dir)
            .expect("adapter-lists directory")
            .map(|entry| entry.expect("dir entry").file_name().to_string_lossy().into_owned())
            .collect();
        let embedded: BTreeSet<String> = ADAPTER_LIST_FILES.iter().map(|file| (*file).to_owned()).collect();
        assert_eq!(
            embedded,
            on_disk,
            "adapter_lists! in effect.rs must name exactly the files under {}",
            dir.display()
        );
        for file in ADAPTER_LIST_FILES {
            // Every table parses, and the ones read as lists really are lists.
            let value = adapter_json(file);
            if !file.contains("generateSpec") {
                assert!(
                    value.is_array(),
                    "{file} is read with adapter_list and must be an array"
                );
            }
        }
    }

    #[test]
    fn a_list_is_served_from_the_binary_not_the_checkout() {
        // Would panic on a missing file if this were still a runtime read.
        let sections = adapter_list("man-sections.json");
        assert!(!sections.is_empty());
        assert!(adapter_source("man-sections.json").contains("\"name\""));
    }

    #[test]
    fn key_value_list_used_entries_follow_the_js_index_of_arithmetic() {
        let keys = [json!({"name": "a"}), json!({"name": "b"}), json!({"name": "bc"})];
        let values = [json!({"name": "x"}), json!({"name": "1"})];
        // Keys: `"bc".slice(0, -1)` is `"b"`, so the key `b` is treated as
        // used while `bc` itself is not.
        let rows = key_value_list("a:1,bc", ":", ",", &keys, &values, true, false, false, false).unwrap();
        let names: Vec<&str> = rows
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["bc"]);
        // Values: a part without a separator is `slice(0)`, the whole part,
        // so the bare `x` hides the value `x`.
        let rows = key_value_list("x,a:", ":", ",", &keys, &values, true, false, false, false).unwrap();
        let names: Vec<&str> = rows
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["1"]);
    }
}
