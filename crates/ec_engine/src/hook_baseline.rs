//! Closed JSON contract for per-function-body hook output baselines.
//!
//! This module is test-only. The JavaScript validator in
//! `scripts/hook-baseline-contract.mjs` is the other copy of the same schema.
//! `deny_unknown_fields` is attached to every object: a new harness key must
//! be added on both sides before a baseline file can carry it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

const BASELINE_VERSION: u64 = 1;
const BASELINE_KIND: &str = "native-hook-baseline";
const SHA256_LEN: usize = 64;

const FIELD_CONTRACTS: &[(&str, &[&str], Option<u8>, &str)] = &[
    ("postProcess", &["string", "string-array"], None, "suggestions"),
    ("custom", &["string-array"], Some(1), "suggestions"),
    ("getQueryTerm", &["string"], None, "string"),
    ("trigger", &["string", "string"], None, "bool"),
    ("script", &["string-array"], None, "argv"),
    ("generateSpec", &["string-array"], Some(1), "spec"),
    ("filterTemplateSuggestions", &["suggestion-array"], None, "suggestions"),
    ("alias", &["string"], Some(1), "string"),
    ("loadSpec", &["string"], Some(1), "spec"),
];

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    pub version: u64,
    pub kind: String,
    pub field: String,
    #[serde(rename = "bodySha256")]
    pub body_sha256: String,
    #[serde(rename = "representativeHookId")]
    pub representative_hook_id: String,
    #[serde(rename = "hookCount")]
    pub hook_count: u64,
    pub cases: Vec<BaselineCase>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BaselineCase {
    pub id: String,
    pub args: Vec<JsonValue>,
    pub exec: Vec<ExecRule>,
    pub context: HookContext,
    #[serde(rename = "timeoutMs")]
    pub timeout_ms: u64,
    pub expected: Expected,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExecRule {
    pub command: String,
    pub args: Vec<String>,
    pub stdout: String,
    pub stderr: String,
    pub status: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HookContext {
    #[serde(rename = "currentWorkingDirectory")]
    pub current_working_directory: String,
    #[serde(rename = "currentProcess")]
    pub current_process: String,
    #[serde(rename = "sshPrefix")]
    pub ssh_prefix: String,
    #[serde(rename = "environmentVariables")]
    pub environment_variables: BTreeMap<String, String>,
    #[serde(rename = "searchTerm")]
    pub search_term: String,
    #[serde(rename = "isDangerous")]
    pub is_dangerous: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Expected {
    #[serde(rename = "suggestions")]
    Suggestions { value: Vec<FigSuggestion> },
    #[serde(rename = "string")]
    String { value: String },
    #[serde(rename = "bool")]
    Bool { value: bool },
    #[serde(rename = "argv")]
    Argv { value: ArgvValue },
    #[serde(rename = "spec")]
    Spec { value: JsonValue },
    #[serde(rename = "error")]
    Error { value: String },
    #[serde(rename = "timeout")]
    Timeout {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<JsonValue>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum ArgvValue {
    Command(String),
    Args(Vec<String>),
    Object(ArgvObject),
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArgvObject {
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum FigSuggestion {
    Name(String),
    Object(FigSuggestionObject),
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FigSuggestionObject {
    pub name: FigName,
    #[serde(default, rename = "displayName", skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, rename = "insertValue", skip_serializing_if = "Option::is_none")]
    pub insert_value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    #[serde(default, rename = "isDangerous", skip_serializing_if = "Option::is_none")]
    pub is_dangerous: Option<bool>,
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub suggestion_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<JsonValue>,
    #[serde(default, rename = "replaceValue", skip_serializing_if = "Option::is_none")]
    pub replace_value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<bool>,
    #[serde(default, rename = "shouldAddSpace", skip_serializing_if = "Option::is_none")]
    pub should_add_space: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum FigName {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug)]
pub struct BaselineError(String);

impl std::fmt::Display for BaselineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BaselineError {}

type BaselineResult<T> = Result<T, BaselineError>;

fn err(message: impl Into<String>) -> BaselineError {
    BaselineError(message.into())
}

pub fn parse_baseline(value: &JsonValue) -> BaselineResult<Baseline> {
    let baseline = serde_json::from_value::<Baseline>(value.clone())
        .map_err(|error| err(format!("hook baseline schema: {error}")))?;
    validate_baseline(&baseline)?;
    Ok(baseline)
}

pub fn parse_baseline_bytes(bytes: &[u8]) -> BaselineResult<Baseline> {
    let value: JsonValue =
        serde_json::from_slice(bytes).map_err(|error| err(format!("hook baseline JSON: {error}")))?;
    parse_baseline(&value)
}

/// Load every committed baseline under `testdata/native-hooks/baseline`.
/// The directory is empty until T1.2 writes one file per unique function body.
pub fn load_all() -> BaselineResult<Vec<Baseline>> {
    load_all_from(&default_baseline_root())
}

pub fn load_all_from(root: &Path) -> BaselineResult<Vec<Baseline>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    let fields = fs::read_dir(root).map_err(|error| err(format!("read {}: {error}", root.display())))?;
    for entry in fields {
        let entry = entry.map_err(|error| err(format!("read {}: {error}", root.display())))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let files = fs::read_dir(&path).map_err(|error| err(format!("read {}: {error}", path.display())))?;
        for file in files {
            let file = file.map_err(|error| err(format!("read {}: {error}", path.display())))?;
            let file_path = file.path();
            if file_path.extension().is_some_and(|ext| ext == "json") {
                paths.push(file_path);
            }
        }
    }
    paths.sort();
    let mut baselines = Vec::with_capacity(paths.len());
    for path in paths {
        let bytes = fs::read(&path).map_err(|error| err(format!("read {}: {error}", path.display())))?;
        let baseline = parse_baseline_bytes(&bytes).map_err(|error| err(format!("{}: {error}", path.display())))?;
        let field_dir = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .ok_or_else(|| err(format!("{} is missing a field directory", path.display())))?;
        let stem = path
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or_else(|| err(format!("{} is missing a file stem", path.display())))?;
        if field_dir != baseline.field {
            return Err(err(format!(
                "{} lives under {field_dir:?} but field is {:?}",
                path.display(),
                baseline.field
            )));
        }
        if stem != baseline.body_sha256 {
            return Err(err(format!(
                "{} name does not match bodySha256 {}",
                path.display(),
                baseline.body_sha256
            )));
        }
        baselines.push(baseline);
    }
    Ok(baselines)
}

fn default_baseline_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/native-hooks/baseline")
}

fn field_contract(field: &str) -> BaselineResult<(&'static [&'static str], Option<u8>, &'static str)> {
    FIELD_CONTRACTS
        .iter()
        .find(|(name, _, _, _)| *name == field)
        .map(|(_, params, exec_index, result_kind)| (*params, *exec_index, *result_kind))
        .ok_or_else(|| err(format!("unknown hook field {field:?}")))
}

fn validate_baseline(baseline: &Baseline) -> BaselineResult<()> {
    if baseline.version != BASELINE_VERSION {
        return Err(err(format!(
            "hook baseline version {} is unsupported",
            baseline.version
        )));
    }
    if baseline.kind != BASELINE_KIND {
        return Err(err(format!("hook baseline kind {:?} is unsupported", baseline.kind)));
    }
    let (params, _exec_index, result_kind) = field_contract(&baseline.field)?;
    if !is_sha256(&baseline.body_sha256) {
        return Err(err(
            "hook baseline bodySha256 must be a 64-character lowercase hex digest",
        ));
    }
    if baseline.representative_hook_id.is_empty() {
        return Err(err("hook baseline representativeHookId must be a non-empty string"));
    }
    if baseline.hook_count < 1 {
        return Err(err("hook baseline hookCount must be a positive integer"));
    }
    let mut case_ids = BTreeSet::new();
    for case in &baseline.cases {
        if case.id.is_empty() {
            return Err(err("hook baseline case id must be a non-empty string"));
        }
        if !case_ids.insert(&case.id) {
            return Err(err(format!("hook baseline case id {:?} is duplicated", case.id)));
        }
        if case.args.len() != params.len() {
            return Err(err(format!(
                "case {:?} has {} argument(s); {} requires {}",
                case.id,
                case.args.len(),
                baseline.field,
                params.len()
            )));
        }
        for (value, ty) in case.args.iter().zip(params.iter()) {
            validate_arg(value, ty, &case.id)?;
        }
        for rule in &case.exec {
            if rule.command.is_empty() {
                return Err(err(format!("case {:?} exec command must be non-empty", case.id)));
            }
        }
        validate_expected(&case.expected, result_kind, &case.id)?;
    }
    Ok(())
}

fn validate_arg(value: &JsonValue, ty: &str, case_id: &str) -> BaselineResult<()> {
    let ok = match ty {
        "string" => value.is_string(),
        "string-array" => value
            .as_array()
            .is_some_and(|items| items.iter().all(JsonValue::is_string)),
        "suggestion-array" => value.is_array(),
        other => return Err(err(format!("unknown argument type {other:?}"))),
    };
    if ok {
        Ok(())
    } else {
        Err(err(format!("case {case_id:?} argument does not match type {ty:?}")))
    }
}

fn validate_expected(expected: &Expected, result_kind: &str, case_id: &str) -> BaselineResult<()> {
    let kind = match expected {
        Expected::Suggestions { .. } => "suggestions",
        Expected::String { .. } => "string",
        Expected::Bool { .. } => "bool",
        Expected::Argv { .. } => "argv",
        Expected::Spec { value } => {
            if fig_names(value).is_none() {
                return Err(err(format!("case {case_id:?} spec value must have a non-empty name")));
            }
            "spec"
        },
        Expected::Error { .. } => "error",
        Expected::Timeout { value } => {
            if let Some(value) = value
                && !value.is_null()
            {
                return Err(err(format!("case {case_id:?} timeout value must be omitted or null")));
            }
            "timeout"
        },
    };
    if kind != "error" && kind != "timeout" && kind != result_kind {
        return Err(err(format!(
            "case {case_id:?} expected kind {kind:?} does not match {result_kind:?}"
        )));
    }
    Ok(())
}

fn fig_names(value: &JsonValue) -> Option<Vec<String>> {
    let object = value.as_object()?;
    names_from(object.get("name")).or_else(|| names_from(object.get("names")))
}

fn names_from(value: Option<&JsonValue>) -> Option<Vec<String>> {
    match value? {
        JsonValue::String(name) if !name.is_empty() => Some(vec![name.clone()]),
        JsonValue::Array(items) => {
            let names: Vec<String> = items
                .iter()
                .filter_map(|item| item.as_str().filter(|name| !name.is_empty()).map(ToOwned::to_owned))
                .collect();
            if names.is_empty() { None } else { Some(names) }
        },
        _ => None,
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == SHA256_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    const SAMPLE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/native-hooks/sample-baseline.json"
    ));

    fn sample_value() -> JsonValue {
        serde_json::from_slice(SAMPLE).expect("sample baseline JSON")
    }

    #[test]
    fn sample_baseline_parses() {
        let baseline = parse_baseline_bytes(SAMPLE).expect("sample baseline");
        assert_eq!(baseline.version, BASELINE_VERSION);
        assert_eq!(baseline.kind, BASELINE_KIND);
        assert_eq!(baseline.field, "postProcess");
        assert_eq!(baseline.cases.len(), 1);
        assert_eq!(baseline.cases[0].id, "normal");
        assert_eq!(baseline.cases[0].args.len(), 2);
        match &baseline.cases[0].expected {
            Expected::Suggestions { value } => assert_eq!(value.len(), 1),
            other => panic!("expected suggestions, got {other:?}"),
        }
    }

    #[test]
    fn extra_field_is_rejected() {
        let mut value = sample_value();
        value["unexpected"] = json!(true);
        let error = parse_baseline(&value).expect_err("unknown field");
        assert!(error.0.contains("unknown field"), "{error}");

        let mut case_extra = sample_value();
        case_extra["cases"][0]["extra"] = json!(1);
        assert!(parse_baseline(&case_extra).is_err());

        let mut expected_extra = sample_value();
        expected_extra["cases"][0]["expected"]["note"] = json!("nope");
        assert!(parse_baseline(&expected_extra).is_err());
    }

    #[test]
    fn args_count_must_match_field_contract() {
        let mut value = sample_value();
        value["cases"][0]["args"] = json!(["only-stdout"]);
        let error = parse_baseline(&value).expect_err("arity");
        assert!(error.0.contains("requires 2"), "{error}");
    }

    #[test]
    fn load_all_reads_files_under_the_field_directory() {
        let root = tempfile::tempdir().expect("baseline root");
        let field_dir = root.path().join("postProcess");
        fs::create_dir_all(&field_dir).expect("field dir");
        let baseline = parse_baseline_bytes(SAMPLE).expect("sample");
        let path = field_dir.join(format!("{}.json", baseline.body_sha256));
        fs::write(&path, SAMPLE).expect("write sample");

        let loaded = load_all_from(root.path()).expect("load_all_from");
        assert_eq!(loaded, vec![baseline]);

        let empty = load_all().expect("empty committed baseline dir");
        assert!(empty.is_empty());
    }

    #[test]
    fn load_all_rejects_path_that_does_not_match_identity() {
        let root = tempfile::tempdir().expect("baseline root");
        let field_dir = root.path().join("trigger");
        fs::create_dir_all(&field_dir).expect("field dir");
        let baseline = parse_baseline_bytes(SAMPLE).expect("sample");
        fs::write(field_dir.join(format!("{}.json", baseline.body_sha256)), SAMPLE).expect("write");
        let error = load_all_from(root.path()).expect_err("field mismatch");
        assert!(error.0.contains("lives under"), "{error}");
    }
}
