//! The closed, typed expression language used by native hook adapters.
//!
//! The JavaScript compiler emits this descriptor at build time.  This module
//! deliberately does not know a command name or a hook id: it accepts the
//! versioned field contracts and evaluates the expression supplied by that
//! contract.  Production sidecars still emit `trigger` only; the other field
//! contracts exist so research descriptors and T2.1 goldens share one parser.
//! Keeping the JSON boundary strict is important here.  A new operation or a
//! field with a different meaning must be rejected until both the compiler and
//! this evaluator have been updated.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

const IR_VERSION: u64 = 1;
const IR_KIND: &str = "typed-hook-expression";
const SOURCE_FIELD: &str = "trigger";
const GET_QUERY_TERM_SOURCE_FIELD: &str = "getQueryTerm";
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
const MAX_STRING_LITERAL_UNITS: usize = 32_768;
const MAX_SERIALIZED_DESCRIPTOR_BYTES: usize = 256 * 1024;
const MAX_NODES: usize = 512;
const MAX_DEPTH: usize = 24;
const TRIGGER_PARAM_COUNT: usize = 2;
const GET_QUERY_TERM_PARAM_COUNT: usize = 1;
const CATALOG_KIND: &str = "typed-hook-expressions";
const MAX_CATALOG_BYTES: usize = 64 * 1024 * 1024;
const MAX_CATALOG_HOOKS: usize = 65_536;
const MAX_HOOK_ID_BYTES: usize = 4_096;
const MAX_HOOK_PATH_BYTES: usize = 4_096;
const MAX_MODULE_BASENAME_BYTES: usize = 255;
const REFERENCE_BASELINE_VERSION: u64 = 1;
const REFERENCE_BASELINE_KIND: &str = "typed-trigger-reference";
const GET_QUERY_TERM_REFERENCE_BASELINE_KIND: &str = "typed-get-query-term-reference";
const MAX_REFERENCE_BASELINE_BYTES: usize = 64 * 1024 * 1024;
const MAX_REFERENCE_CASES: usize = 256;
const MAX_REFERENCE_CORPUS_BYTES: usize = 1_000_000;
const REFERENCE_EXEC_MARKER: &str = "$referenceExec";
const ASDF_GET_QUERY_TERM_CANDIDATE_IDS: [&str; 2] = ["asdf#getQueryTerm#7", "asdf#getQueryTerm#8"];

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum TypedValueType {
    String,
    Bool,
    Integer,
    StringArray,
    Json,
    Suggestion,
    SuggestionArray,
    StringRecord,
    Null,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TypedHookParam {
    index: u64,
    #[serde(rename = "type")]
    value_type: TypedValueType,
}

/// The expression enum is internally tagged so every operation has one
/// unambiguous JSON shape. `deny_unknown_fields` is intentionally attached to
/// the enum: adding an operation field without updating this evaluator must
/// fail closed at deserialization time.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "op", deny_unknown_fields)]
enum TypedExpr {
    #[serde(rename = "arg")]
    Arg { index: u64 },
    #[serde(rename = "string")]
    String { value: String },
    #[serde(rename = "bool")]
    Bool { value: bool },
    #[serde(rename = "integer")]
    Integer { value: i64 },
    #[serde(rename = "null")]
    Null,
    #[serde(rename = "array")]
    Array { items: Vec<TypedExpr> },
    #[serde(rename = "length")]
    Length { value: Box<TypedExpr> },
    #[serde(rename = "string-includes")]
    StringIncludes {
        value: Box<TypedExpr>,
        needle: Box<TypedExpr>,
    },
    #[serde(rename = "string-index-of")]
    StringIndexOf {
        value: Box<TypedExpr>,
        needle: Box<TypedExpr>,
    },
    #[serde(rename = "string-last-index-of")]
    StringLastIndexOf {
        value: Box<TypedExpr>,
        needle: Box<TypedExpr>,
    },
    #[serde(rename = "string-slice")]
    StringSlice {
        value: Box<TypedExpr>,
        start: Box<TypedExpr>,
    },
    #[serde(rename = "string-slice-after-first")]
    StringSliceAfterFirst {
        value: Box<TypedExpr>,
        needle: Box<TypedExpr>,
    },
    #[serde(rename = "string-substring")]
    StringSubstring {
        value: Box<TypedExpr>,
        start: Box<TypedExpr>,
        end: Box<TypedExpr>,
    },
    #[serde(rename = "string-split")]
    StringSplit {
        value: Box<TypedExpr>,
        separator: Box<TypedExpr>,
    },
    #[serde(rename = "string-trim")]
    StringTrim { value: Box<TypedExpr> },
    #[serde(rename = "string-trim-start")]
    StringTrimStart { value: Box<TypedExpr> },
    #[serde(rename = "string-trim-end")]
    StringTrimEnd { value: Box<TypedExpr> },
    #[serde(rename = "string-replace")]
    StringReplace {
        value: Box<TypedExpr>,
        needle: Box<TypedExpr>,
        replacement: Box<TypedExpr>,
    },
    #[serde(rename = "string-replace-all")]
    StringReplaceAll {
        value: Box<TypedExpr>,
        needle: Box<TypedExpr>,
        replacement: Box<TypedExpr>,
    },
    #[serde(rename = "string-starts-with")]
    StringStartsWith {
        value: Box<TypedExpr>,
        needle: Box<TypedExpr>,
    },
    #[serde(rename = "string-ends-with")]
    StringEndsWith {
        value: Box<TypedExpr>,
        needle: Box<TypedExpr>,
    },
    #[serde(rename = "string-to-lower")]
    StringToLower { value: Box<TypedExpr> },
    #[serde(rename = "string-to-upper")]
    StringToUpper { value: Box<TypedExpr> },
    #[serde(rename = "string-pad-start")]
    StringPadStart {
        value: Box<TypedExpr>,
        target: Box<TypedExpr>,
        pad: Box<TypedExpr>,
    },
    #[serde(rename = "string-pad-end")]
    StringPadEnd {
        value: Box<TypedExpr>,
        target: Box<TypedExpr>,
        pad: Box<TypedExpr>,
    },
    #[serde(rename = "string-repeat")]
    StringRepeat {
        value: Box<TypedExpr>,
        count: Box<TypedExpr>,
    },
    #[serde(rename = "string-concat")]
    StringConcat { parts: Vec<TypedExpr> },
    #[serde(rename = "string-char-at")]
    StringCharAt {
        value: Box<TypedExpr>,
        index: Box<TypedExpr>,
    },
    #[serde(rename = "string-at")]
    StringAt {
        value: Box<TypedExpr>,
        index: Box<TypedExpr>,
    },
    #[serde(rename = "array-includes")]
    ArrayIncludes {
        value: Box<TypedExpr>,
        needle: Box<TypedExpr>,
    },
    #[serde(rename = "strict-eq")]
    StrictEq {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    #[serde(rename = "strict-ne")]
    StrictNe {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    #[serde(rename = "add")]
    Add {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    #[serde(rename = "sub")]
    Sub {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    #[serde(rename = "mul")]
    Mul {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    #[serde(rename = "lt")]
    LessThan {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    #[serde(rename = "le")]
    LessThanOrEqual {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    #[serde(rename = "gt")]
    GreaterThan {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    #[serde(rename = "ge")]
    GreaterThanOrEqual {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    #[serde(rename = "not")]
    Not { value: Box<TypedExpr> },
    #[serde(rename = "nullish")]
    Nullish {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    #[serde(rename = "and")]
    And {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    #[serde(rename = "or")]
    Or {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    #[serde(rename = "if")]
    If {
        condition: Box<TypedExpr>,
        #[serde(rename = "then")]
        then_branch: Box<TypedExpr>,
        #[serde(rename = "else")]
        else_branch: Box<TypedExpr>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TypedHookIr {
    version: u64,
    kind: String,
    #[serde(rename = "sourceField")]
    source_field: String,
    #[serde(rename = "resultType")]
    result_type: TypedValueType,
    params: Vec<TypedHookParam>,
    expr: TypedExpr,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TypedHookContract {
    #[serde(rename = "irVersion")]
    ir_version: u64,
    params: Vec<TypedValueType>,
    #[serde(rename = "resultType")]
    result_type: TypedValueType,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TypedHookContracts {
    trigger: TypedHookContract,
    #[serde(default, rename = "getQueryTerm", skip_serializing_if = "Option::is_none")]
    get_query_term: Option<TypedHookContract>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TypedHookCatalogEntry {
    module: String,
    #[serde(rename = "moduleSha256")]
    module_sha256: String,
    path: String,
    #[serde(rename = "sourceField")]
    source_field: String,
    #[serde(rename = "functionBodySha256")]
    function_body_sha256: String,
    descriptor: TypedHookIr,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TypedHookCatalog {
    version: u64,
    kind: String,
    contracts: TypedHookContracts,
    hooks: BTreeMap<String, TypedHookCatalogEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TypedTriggerReferenceCase {
    id: String,
    args: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TypedTriggerReferenceBaseline {
    version: u64,
    kind: String,
    #[serde(rename = "generatorSha256")]
    generator_sha256: String,
    #[serde(rename = "harnessSha256")]
    harness_sha256: String,
    #[serde(rename = "pairSha256")]
    pair_sha256: String,
    #[serde(rename = "hookManifestSha256")]
    hook_manifest_sha256: String,
    #[serde(rename = "sidecarSha256")]
    sidecar_sha256: String,
    cases: Vec<TypedTriggerReferenceCase>,
    catalog: TypedHookCatalog,
    expected: BTreeMap<String, Vec<bool>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TypedGetQueryTermReferenceCase {
    id: String,
    args: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TypedGetQueryTermReferenceCandidate {
    source: String,
    #[serde(rename = "sourceSha256")]
    source_sha256: String,
    ir: String,
    #[serde(rename = "irSha256")]
    ir_sha256: String,
    #[serde(rename = "hookFile")]
    hook_file: String,
    #[serde(rename = "hookFileSha256")]
    hook_file_sha256: String,
    module: String,
    #[serde(rename = "moduleSha256")]
    module_sha256: String,
    path: String,
    #[serde(rename = "sourceField")]
    source_field: String,
    #[serde(rename = "functionBodySha256")]
    function_body_sha256: String,
    descriptor: TypedHookIr,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TypedGetQueryTermReferenceBaseline {
    version: u64,
    kind: String,
    #[serde(rename = "generatorSha256")]
    generator_sha256: String,
    #[serde(rename = "harnessSha256")]
    harness_sha256: String,
    #[serde(rename = "pairSha256")]
    pair_sha256: String,
    #[serde(rename = "hookManifestSha256")]
    hook_manifest_sha256: String,
    cases: Vec<TypedGetQueryTermReferenceCase>,
    candidates: BTreeMap<String, TypedGetQueryTermReferenceCandidate>,
    expected: BTreeMap<String, Vec<String>>,
}

/// A parse, schema, or evaluation failure.  Callers intentionally receive a
/// single failure type so an unsupported sidecar can remain on the legacy
/// path without exposing serde implementation details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypedHookError {
    message: String,
}

impl TypedHookError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for TypedHookError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for TypedHookError {}

type TypedHookResult<T> = Result<T, TypedHookError>;

/// A string represented as JavaScript UTF-16 code units.  Rust's `str` is
/// UTF-8, so keeping this representation through every string operation is
/// what preserves JavaScript's length, indexing, and surrogate-boundary
/// behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Utf16String(Vec<u16>);

impl Utf16String {
    fn from_str(value: &str) -> Self {
        Self(value.encode_utf16().collect())
    }

    fn from_units(value: impl Into<Vec<u16>>) -> Self {
        Self(value.into())
    }

    fn len(&self) -> usize {
        self.0.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TypedValue {
    String(Utf16String),
    Bool(bool),
    Integer(i64),
    StringArray(Vec<Utf16String>),
    Null,
}

#[derive(Debug, Default)]
struct ValidationState {
    nodes: usize,
}

/// Parse and validate a generated descriptor from a JSON value.
pub(crate) fn parse_typed_hook_ir(value: &JsonValue) -> TypedHookResult<TypedHookIr> {
    ensure_descriptor_size(value)?;
    let descriptor = serde_json::from_value::<TypedHookIr>(value.clone())
        .map_err(|error| TypedHookError::new(format!("typed hook schema: {error}")))?;
    validate_typed_hook_ir(&descriptor)?;
    Ok(descriptor)
}

/// Parse and validate a generated descriptor directly from sidecar bytes.
pub(crate) fn parse_typed_hook_ir_bytes(bytes: &[u8]) -> TypedHookResult<TypedHookIr> {
    if bytes.len() > MAX_SERIALIZED_DESCRIPTOR_BYTES {
        return Err(TypedHookError::new(format!(
            "typed hook descriptor exceeds {MAX_SERIALIZED_DESCRIPTOR_BYTES} bytes"
        )));
    }
    let descriptor = serde_json::from_slice::<TypedHookIr>(bytes)
        .map_err(|error| TypedHookError::new(format!("typed hook schema: {error}")))?;
    validate_typed_hook_ir(&descriptor)?;
    Ok(descriptor)
}

/// Parse and validate the build-time catalog of typed hook descriptors.
pub(crate) fn parse_typed_hook_catalog(value: &JsonValue) -> TypedHookResult<TypedHookCatalog> {
    ensure_catalog_size(value)?;
    let catalog = serde_json::from_value::<TypedHookCatalog>(value.clone())
        .map_err(|error| TypedHookError::new(format!("typed hook catalog schema: {error}")))?;
    validate_typed_hook_catalog(&catalog)?;
    Ok(catalog)
}

/// Parse and validate a catalog directly from the generated sidecar bytes.
pub(crate) fn parse_typed_hook_catalog_bytes(bytes: &[u8]) -> TypedHookResult<TypedHookCatalog> {
    if bytes.len() > MAX_CATALOG_BYTES {
        return Err(TypedHookError::new(format!(
            "typed hook catalog exceeds {MAX_CATALOG_BYTES} bytes"
        )));
    }
    let catalog = serde_json::from_slice::<TypedHookCatalog>(bytes)
        .map_err(|error| TypedHookError::new(format!("typed hook catalog schema: {error}")))?;
    validate_typed_hook_catalog(&catalog)?;
    Ok(catalog)
}

/// Parse and validate the immutable source/module differential baseline used
/// by the typed-hook migration tests.  This is deliberately test-only data:
/// the production engine does not load or consult this baseline.
pub(crate) fn parse_typed_trigger_reference(value: &JsonValue) -> TypedHookResult<TypedTriggerReferenceBaseline> {
    ensure_reference_baseline_size(value)?;
    let baseline = serde_json::from_value::<TypedTriggerReferenceBaseline>(value.clone())
        .map_err(|error| TypedHookError::new(format!("typed trigger reference schema: {error}")))?;
    validate_typed_trigger_reference(&baseline)?;
    Ok(baseline)
}

/// Parse and validate the checked-in typed-trigger reference baseline bytes.
pub(crate) fn parse_typed_trigger_reference_bytes(bytes: &[u8]) -> TypedHookResult<TypedTriggerReferenceBaseline> {
    if bytes.len() > MAX_REFERENCE_BASELINE_BYTES {
        return Err(TypedHookError::new(format!(
            "typed trigger reference exceeds {MAX_REFERENCE_BASELINE_BYTES} bytes"
        )));
    }
    let baseline = serde_json::from_slice::<TypedTriggerReferenceBaseline>(bytes)
        .map_err(|error| TypedHookError::new(format!("typed trigger reference schema: {error}")))?;
    validate_typed_trigger_reference(&baseline)?;
    Ok(baseline)
}

/// Parse the isolated asdf getQueryTerm differential baseline.  This is
/// research/test data only; production never loads this artifact or selects
/// this evaluator in place of QuickJS.
pub(crate) fn parse_typed_get_query_term_reference(
    value: &JsonValue,
) -> TypedHookResult<TypedGetQueryTermReferenceBaseline> {
    ensure_reference_baseline_size(value)?;
    let baseline = serde_json::from_value::<TypedGetQueryTermReferenceBaseline>(value.clone())
        .map_err(|error| TypedHookError::new(format!("typed getQueryTerm reference schema: {error}")))?;
    validate_typed_get_query_term_reference(&baseline)?;
    Ok(baseline)
}

/// Parse the checked-in asdf getQueryTerm baseline bytes.
pub(crate) fn parse_typed_get_query_term_reference_bytes(
    bytes: &[u8],
) -> TypedHookResult<TypedGetQueryTermReferenceBaseline> {
    if bytes.len() > MAX_REFERENCE_BASELINE_BYTES {
        return Err(TypedHookError::new(format!(
            "typed getQueryTerm reference exceeds {MAX_REFERENCE_BASELINE_BYTES} bytes"
        )));
    }
    let baseline = serde_json::from_slice::<TypedGetQueryTermReferenceBaseline>(bytes)
        .map_err(|error| TypedHookError::new(format!("typed getQueryTerm reference schema: {error}")))?;
    validate_typed_get_query_term_reference(&baseline)?;
    Ok(baseline)
}

fn ensure_catalog_size(value: &JsonValue) -> TypedHookResult<()> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| TypedHookError::new(format!("typed hook catalog schema: {error}")))?;
    if bytes.len() > MAX_CATALOG_BYTES {
        return Err(TypedHookError::new(format!(
            "typed hook catalog exceeds {MAX_CATALOG_BYTES} bytes"
        )));
    }
    Ok(())
}

fn ensure_reference_baseline_size(value: &JsonValue) -> TypedHookResult<()> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| TypedHookError::new(format!("typed trigger reference schema: {error}")))?;
    if bytes.len() > MAX_REFERENCE_BASELINE_BYTES {
        return Err(TypedHookError::new(format!(
            "typed trigger reference exceeds {MAX_REFERENCE_BASELINE_BYTES} bytes"
        )));
    }
    Ok(())
}

fn validate_typed_trigger_reference(baseline: &TypedTriggerReferenceBaseline) -> TypedHookResult<()> {
    if baseline.version != REFERENCE_BASELINE_VERSION {
        return Err(TypedHookError::new(format!(
            "typed trigger reference version {} is unsupported",
            baseline.version
        )));
    }
    if baseline.kind != REFERENCE_BASELINE_KIND {
        return Err(TypedHookError::new(format!(
            "typed trigger reference kind {:?} is unsupported",
            baseline.kind
        )));
    }
    for (field, value) in [
        ("generatorSha256", &baseline.generator_sha256),
        ("harnessSha256", &baseline.harness_sha256),
        ("pairSha256", &baseline.pair_sha256),
        ("hookManifestSha256", &baseline.hook_manifest_sha256),
        ("sidecarSha256", &baseline.sidecar_sha256),
    ] {
        validate_sha256(value, field)?;
    }

    validate_reference_cases(&baseline.cases)?;
    validate_typed_hook_catalog(&baseline.catalog)?;
    if baseline.catalog.hooks.is_empty() {
        return Err(TypedHookError::new(
            "typed trigger reference catalog must contain at least one hook",
        ));
    }

    let catalog_ids: BTreeSet<_> = baseline.catalog.hooks.keys().collect();
    let expected_ids: BTreeSet<_> = baseline.expected.keys().collect();
    if catalog_ids != expected_ids {
        return Err(TypedHookError::new(
            "typed trigger reference expected ids do not match catalog ids",
        ));
    }
    for id in &expected_ids {
        let expected = baseline
            .expected
            .get(*id)
            .ok_or_else(|| TypedHookError::new("typed trigger reference expected id is missing"))?;
        if expected.len() != baseline.cases.len() {
            return Err(TypedHookError::new(format!(
                "typed trigger reference expected values for {id:?} have length {}, expected {}",
                expected.len(),
                baseline.cases.len()
            )));
        }
    }

    // The baseline embeds the exact sidecar catalog.  Re-serializing it with
    // the Rust schema must reproduce the compact JSON sidecar plus its final
    // newline; otherwise the provenance digest could describe a different
    // artifact than the evaluator is testing.
    let mut catalog_bytes = serde_json::to_vec(&baseline.catalog)
        .map_err(|error| TypedHookError::new(format!("typed trigger reference catalog serialization: {error}")))?;
    catalog_bytes.push(b'\n');
    let digest = sha256_hex(&catalog_bytes);
    if digest != baseline.sidecar_sha256 {
        return Err(TypedHookError::new(
            "typed trigger reference sidecarSha256 does not match the embedded catalog",
        ));
    }
    Ok(())
}

fn validate_typed_get_query_term_reference(baseline: &TypedGetQueryTermReferenceBaseline) -> TypedHookResult<()> {
    if baseline.version != REFERENCE_BASELINE_VERSION {
        return Err(TypedHookError::new(format!(
            "typed getQueryTerm reference version {} is unsupported",
            baseline.version
        )));
    }
    if baseline.kind != GET_QUERY_TERM_REFERENCE_BASELINE_KIND {
        return Err(TypedHookError::new(format!(
            "typed getQueryTerm reference kind {:?} is unsupported",
            baseline.kind
        )));
    }
    for (field, value) in [
        ("generatorSha256", &baseline.generator_sha256),
        ("harnessSha256", &baseline.harness_sha256),
        ("pairSha256", &baseline.pair_sha256),
        ("hookManifestSha256", &baseline.hook_manifest_sha256),
    ] {
        validate_sha256(value, field)?;
    }
    if baseline.cases.is_empty() {
        return Err(TypedHookError::new(
            "typed getQueryTerm reference corpus must not be empty",
        ));
    }
    if baseline.cases.len() > MAX_REFERENCE_CASES {
        return Err(TypedHookError::new(format!(
            "typed getQueryTerm reference corpus exceeds {MAX_REFERENCE_CASES} cases"
        )));
    }
    let mut case_ids = BTreeSet::new();
    let mut corpus_bytes = 0usize;
    for case in &baseline.cases {
        validate_reference_case_id(&case.id)?;
        if !case_ids.insert(&case.id) {
            return Err(TypedHookError::new(format!(
                "typed getQueryTerm reference case id {:?} is duplicated",
                case.id
            )));
        }
        if case.args.len() != GET_QUERY_TERM_PARAM_COUNT {
            return Err(TypedHookError::new(format!(
                "typed getQueryTerm reference case {:?} must have exactly {GET_QUERY_TERM_PARAM_COUNT} argument",
                case.id
            )));
        }
        let serialized = serde_json::to_vec(case).map_err(|error| {
            TypedHookError::new(format!("typed getQueryTerm reference case serialization: {error}"))
        })?;
        if serialized
            .windows(REFERENCE_EXEC_MARKER.len())
            .any(|window| window == REFERENCE_EXEC_MARKER.as_bytes())
        {
            return Err(TypedHookError::new(
                "typed getQueryTerm reference corpus must not contain an executor marker",
            ));
        }
        corpus_bytes = corpus_bytes
            .checked_add(serialized.len())
            .ok_or_else(|| TypedHookError::new("typed getQueryTerm corpus size overflowed"))?;
    }
    if corpus_bytes > MAX_REFERENCE_CORPUS_BYTES {
        return Err(TypedHookError::new(format!(
            "typed getQueryTerm reference corpus exceeds {MAX_REFERENCE_CORPUS_BYTES} bytes"
        )));
    }

    let expected_candidate_ids: BTreeSet<_> = ASDF_GET_QUERY_TERM_CANDIDATE_IDS.into_iter().collect();
    let candidate_ids: BTreeSet<_> = baseline.candidates.keys().map(String::as_str).collect();
    if candidate_ids != expected_candidate_ids {
        return Err(TypedHookError::new(
            "typed getQueryTerm reference candidates must be exactly the two asdf hooks",
        ));
    }
    let expected_ids: BTreeSet<_> = baseline.expected.keys().map(String::as_str).collect();
    if candidate_ids != expected_ids {
        return Err(TypedHookError::new(
            "typed getQueryTerm reference expected ids do not match candidates",
        ));
    }
    for id in &expected_candidate_ids {
        let candidate = baseline
            .candidates
            .get(*id)
            .ok_or_else(|| TypedHookError::new(format!("typed getQueryTerm candidate {id:?} is missing")))?;
        validate_reference_file(&candidate.source, "source")?;
        validate_reference_file(&candidate.ir, "ir")?;
        validate_reference_file(&candidate.hook_file, "hookFile")?;
        validate_module_file(&candidate.module)?;
        validate_source_hook_path(&candidate.path)?;
        validate_sha256(&candidate.source_sha256, "sourceSha256")?;
        validate_sha256(&candidate.ir_sha256, "irSha256")?;
        validate_sha256(&candidate.hook_file_sha256, "hookFileSha256")?;
        validate_sha256(&candidate.module_sha256, "moduleSha256")?;
        validate_sha256(&candidate.function_body_sha256, "functionBodySha256")?;
        if candidate.source_field != GET_QUERY_TERM_SOURCE_FIELD {
            return Err(TypedHookError::new(format!(
                "typed getQueryTerm candidate {id:?} sourceField must be {GET_QUERY_TERM_SOURCE_FIELD:?}"
            )));
        }
        validate_typed_hook_ir(&candidate.descriptor)?;
        if candidate.descriptor.source_field != GET_QUERY_TERM_SOURCE_FIELD {
            return Err(TypedHookError::new(format!(
                "typed getQueryTerm candidate {id:?} descriptor sourceField is invalid"
            )));
        }
        let expected = baseline
            .expected
            .get(*id)
            .ok_or_else(|| TypedHookError::new(format!("typed getQueryTerm expected {id:?} is missing")))?;
        if expected.len() != baseline.cases.len() {
            return Err(TypedHookError::new(format!(
                "typed getQueryTerm expected values for {id:?} have length {}, expected {}",
                expected.len(),
                baseline.cases.len()
            )));
        }
    }
    Ok(())
}

fn validate_reference_cases(cases: &[TypedTriggerReferenceCase]) -> TypedHookResult<()> {
    if cases.is_empty() {
        return Err(TypedHookError::new("typed trigger reference corpus must not be empty"));
    }
    if cases.len() > MAX_REFERENCE_CASES {
        return Err(TypedHookError::new(format!(
            "typed trigger reference corpus exceeds {MAX_REFERENCE_CASES} cases"
        )));
    }

    let mut ids = BTreeSet::new();
    let mut corpus_bytes = 0usize;
    for case in cases {
        validate_reference_case_id(&case.id)?;
        if !ids.insert(&case.id) {
            return Err(TypedHookError::new(format!(
                "typed trigger reference case id {:?} is duplicated",
                case.id
            )));
        }
        if case.args.len() != TRIGGER_PARAM_COUNT {
            return Err(TypedHookError::new(format!(
                "typed trigger reference case {:?} must have exactly {TRIGGER_PARAM_COUNT} arguments",
                case.id
            )));
        }

        let serialized = serde_json::to_vec(case)
            .map_err(|error| TypedHookError::new(format!("typed trigger reference case serialization: {error}")))?;
        if serialized
            .windows(REFERENCE_EXEC_MARKER.len())
            .any(|window| window == REFERENCE_EXEC_MARKER.as_bytes())
        {
            return Err(TypedHookError::new(
                "typed trigger reference corpus must not contain an executor marker",
            ));
        }
        corpus_bytes = corpus_bytes
            .checked_add(serialized.len())
            .ok_or_else(|| TypedHookError::new("typed trigger reference corpus size overflowed"))?;
    }
    if corpus_bytes > MAX_REFERENCE_CORPUS_BYTES {
        return Err(TypedHookError::new(format!(
            "typed trigger reference corpus exceeds {MAX_REFERENCE_CORPUS_BYTES} bytes"
        )));
    }
    Ok(())
}

fn validate_reference_case_id(id: &str) -> TypedHookResult<()> {
    if id.is_empty() {
        return Err(TypedHookError::new("typed trigger reference case id must not be empty"));
    }
    if id.len() > MAX_HOOK_ID_BYTES {
        return Err(TypedHookError::new(format!(
            "typed trigger reference case id exceeds {MAX_HOOK_ID_BYTES} UTF-8 bytes"
        )));
    }
    if id
        .chars()
        .any(|character| character.is_control() || matches!(character, '/' | '\\'))
    {
        return Err(TypedHookError::new(format!(
            "typed trigger reference case id {id:?} contains a forbidden control or path character"
        )));
    }
    Ok(())
}

fn validate_typed_hook_catalog(catalog: &TypedHookCatalog) -> TypedHookResult<()> {
    if catalog.version != IR_VERSION {
        return Err(TypedHookError::new(format!(
            "typed hook catalog version {} is unsupported",
            catalog.version
        )));
    }
    if catalog.kind != CATALOG_KIND {
        return Err(TypedHookError::new(format!(
            "typed hook catalog kind {:?} is unsupported",
            catalog.kind
        )));
    }

    let contract = &catalog.contracts.trigger;
    if contract.ir_version != IR_VERSION
        || contract.params != [TypedValueType::String, TypedValueType::String]
        || contract.result_type != TypedValueType::Bool
    {
        return Err(TypedHookError::new(
            "typed hook catalog trigger contract does not match the native contract",
        ));
    }
    if let Some(contract) = &catalog.contracts.get_query_term
        && (contract.ir_version != IR_VERSION
            || contract.params != [TypedValueType::String]
            || contract.result_type != TypedValueType::String)
    {
        return Err(TypedHookError::new(
            "typed hook catalog getQueryTerm contract does not match the research contract",
        ));
    }
    if catalog.hooks.len() > MAX_CATALOG_HOOKS {
        return Err(TypedHookError::new(format!(
            "typed hook catalog contains more than {MAX_CATALOG_HOOKS} hooks"
        )));
    }

    for (id, entry) in &catalog.hooks {
        validate_hook_id(id)?;
        validate_module_file(&entry.module)?;
        validate_source_hook_path(&entry.path)?;
        validate_sha256(&entry.module_sha256, "moduleSha256")?;
        validate_sha256(&entry.function_body_sha256, "functionBodySha256")?;
        if entry.source_field != SOURCE_FIELD {
            return Err(TypedHookError::new(format!(
                "typed hook {id:?} sourceField must be {SOURCE_FIELD:?}"
            )));
        }
        validate_typed_hook_ir(&entry.descriptor)?;
        let descriptor_value = serde_json::to_value(&entry.descriptor)
            .map_err(|error| TypedHookError::new(format!("typed hook descriptor schema: {error}")))?;
        ensure_descriptor_size(&descriptor_value)?;
    }
    Ok(())
}

fn validate_hook_id(id: &str) -> TypedHookResult<()> {
    if id.is_empty() {
        return Err(TypedHookError::new("typed hook id must not be empty"));
    }
    if id.len() > MAX_HOOK_ID_BYTES {
        return Err(TypedHookError::new(format!(
            "typed hook id exceeds {MAX_HOOK_ID_BYTES} UTF-8 bytes"
        )));
    }
    if id.contains(['\0', '\\']) {
        return Err(TypedHookError::new(
            "typed hook id contains a forbidden NUL or backslash",
        ));
    }
    Ok(())
}

fn validate_module_file(value: &str) -> TypedHookResult<()> {
    if value.is_empty()
        || value.len() > MAX_MODULE_BASENAME_BYTES
        || value.contains(['/', '\\', '\0'])
        || !value.ends_with(".js")
        || value == ".js"
        || value == "..js"
    {
        return Err(TypedHookError::new(format!(
            "invalid typed hook module basename {value:?}"
        )));
    }
    Ok(())
}

fn validate_reference_file(value: &str, field: &str) -> TypedHookResult<()> {
    if value.is_empty()
        || value.len() > MAX_HOOK_PATH_BYTES
        || value.starts_with('/')
        || value.contains(['\\', '\0'])
        || value
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return Err(TypedHookError::new(format!(
            "typed getQueryTerm {field} must be a safe repository-relative file"
        )));
    }
    Ok(())
}

fn validate_source_hook_path(value: &str) -> TypedHookResult<()> {
    if value.len() > MAX_HOOK_PATH_BYTES || !is_safe_source_hook_path(value) {
        return Err(TypedHookError::new(format!("invalid typed hook source path {value:?}")));
    }
    Ok(())
}

fn is_safe_source_hook_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    if !bytes.starts_with(b"root") {
        return false;
    }
    let mut cursor = 4;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'.' => {
                cursor += 1;
                if cursor == bytes.len() || !is_identifier_start(bytes[cursor]) {
                    return false;
                }
                cursor += 1;
                while cursor < bytes.len() && is_identifier_continue(bytes[cursor]) {
                    cursor += 1;
                }
            },
            b'[' => {
                cursor += 1;
                let start = cursor;
                while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
                    cursor += 1;
                }
                if cursor == start || cursor == bytes.len() || bytes[cursor] != b']' {
                    return false;
                }
                cursor += 1;
            },
            _ => return false,
        }
    }
    true
}

fn is_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte == b'$'
}

fn is_identifier_continue(byte: u8) -> bool {
    is_identifier_start(byte) || byte.is_ascii_digit()
}

fn validate_sha256(value: &str, field: &str) -> TypedHookResult<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(TypedHookError::new(format!(
            "typed hook {field} must be a lowercase SHA-256 digest"
        )));
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;

        write!(output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

/// Find one catalog entry by its compiler-assigned hook id.
pub(crate) fn lookup_typed_hook<'a>(catalog: &'a TypedHookCatalog, hook_id: &str) -> Option<&'a TypedHookCatalogEntry> {
    catalog.hooks.get(hook_id)
}

/// Evaluate one catalog entry by id.  The catalog has already validated the
/// descriptor and provenance shape; a missing id remains an explicit error so
/// an orphaned IR reference cannot become a false negative trigger.
pub(crate) fn evaluate_typed_hook_by_id(
    catalog: &TypedHookCatalog,
    hook_id: &str,
    search_term: &str,
    previous_search_term: &str,
) -> TypedHookResult<bool> {
    let entry = lookup_typed_hook(catalog, hook_id)
        .ok_or_else(|| TypedHookError::new(format!("typed hook id {hook_id:?} is missing")))?;
    evaluate_typed_trigger(&entry.descriptor, search_term, previous_search_term)
}

fn ensure_descriptor_size(value: &JsonValue) -> TypedHookResult<()> {
    let bytes =
        serde_json::to_vec(value).map_err(|error| TypedHookError::new(format!("typed hook schema: {error}")))?;
    if bytes.len() > MAX_SERIALIZED_DESCRIPTOR_BYTES {
        return Err(TypedHookError::new(format!(
            "typed hook descriptor exceeds {MAX_SERIALIZED_DESCRIPTOR_BYTES} bytes"
        )));
    }
    Ok(())
}

fn validate_typed_hook_ir(descriptor: &TypedHookIr) -> TypedHookResult<()> {
    if descriptor.version != IR_VERSION {
        return Err(TypedHookError::new(format!(
            "typed hook version {} is unsupported",
            descriptor.version
        )));
    }
    if descriptor.kind != IR_KIND {
        return Err(TypedHookError::new(format!(
            "typed hook kind {:?} is unsupported",
            descriptor.kind
        )));
    }
    let (label, expected_result, expected_params) = typed_hook_contract(&descriptor.source_field).ok_or_else(|| {
        TypedHookError::new(format!(
            "typed hook source field {:?} is unsupported",
            descriptor.source_field
        ))
    })?;
    if descriptor.result_type != expected_result {
        return Err(TypedHookError::new(format!(
            "{label} typed hook resultType does not match its contract"
        )));
    }
    if descriptor.params.len() != expected_params.len() {
        return Err(TypedHookError::new(format!(
            "{label} typed hook requires exactly {} params",
            expected_params.len()
        )));
    }
    for (expected_index, param) in descriptor.params.iter().enumerate() {
        if param.index != expected_index as u64 || param.value_type != expected_params[expected_index] {
            return Err(TypedHookError::new(format!(
                "{label} parameter {expected_index} must be {:?} with matching index",
                expected_params[expected_index]
            )));
        }
    }

    let mut state = ValidationState::default();
    let result_type = validate_expr(&descriptor.expr, &descriptor.params, &mut state, 0, "expr")?;
    if result_type != descriptor.result_type {
        return Err(TypedHookError::new(format!(
            "expression result type {result_type:?} does not match descriptor resultType {:?}",
            descriptor.result_type
        )));
    }
    Ok(())
}

fn typed_hook_contract(source_field: &str) -> Option<(&'static str, TypedValueType, &'static [TypedValueType])> {
    match source_field {
        SOURCE_FIELD => Some((
            "trigger",
            TypedValueType::Bool,
            &[TypedValueType::String, TypedValueType::String],
        )),
        GET_QUERY_TERM_SOURCE_FIELD => Some(("getQueryTerm", TypedValueType::String, &[TypedValueType::String])),
        "postProcess" => Some((
            "postProcess",
            TypedValueType::SuggestionArray,
            &[TypedValueType::String, TypedValueType::StringArray],
        )),
        "script" => Some(("script", TypedValueType::StringArray, &[TypedValueType::StringArray])),
        "filterTemplateSuggestions" => Some((
            "filterTemplateSuggestions",
            TypedValueType::SuggestionArray,
            &[TypedValueType::SuggestionArray],
        )),
        _ => None,
    }
}

fn is_string_literal(expression: &TypedExpr, expected: &str) -> bool {
    matches!(expression, TypedExpr::String { value } if value == expected)
}

fn validate_expr(
    expression: &TypedExpr,
    params: &[TypedHookParam],
    state: &mut ValidationState,
    depth: usize,
    path: &str,
) -> TypedHookResult<TypedValueType> {
    state.nodes += 1;
    if state.nodes > MAX_NODES || depth > MAX_DEPTH {
        return Err(TypedHookError::new(format!(
            "{path} exceeds typed hook complexity limits"
        )));
    }
    let child = |expression: &TypedExpr,
                 expected: Option<TypedValueType>,
                 name: &str,
                 state: &mut ValidationState|
     -> TypedHookResult<TypedValueType> {
        let result = validate_expr(expression, params, state, depth + 1, &format!("{path}.{name}"))?;
        if expected.is_some_and(|expected| expected != result) {
            return Err(TypedHookError::new(format!(
                "{path}.{name} has type {result:?}, expected {expected:?}"
            )));
        }
        Ok(result)
    };
    match expression {
        TypedExpr::Arg { index } => {
            if *index > usize::MAX as u64 {
                return Err(TypedHookError::new(format!(
                    "{path}.index {index} cannot be represented"
                )));
            }
            let Some(param) = params.get(*index as usize) else {
                return Err(TypedHookError::new(format!(
                    "{path}.index {index} is outside the parameter contract"
                )));
            };
            Ok(param.value_type)
        },
        TypedExpr::String { value } => {
            if value.encode_utf16().count() > MAX_STRING_LITERAL_UNITS {
                return Err(TypedHookError::new(format!(
                    "{path} exceeds {MAX_STRING_LITERAL_UNITS} UTF-16 code units"
                )));
            }
            Ok(TypedValueType::String)
        },
        TypedExpr::Bool { .. } => Ok(TypedValueType::Bool),
        TypedExpr::Null => Ok(TypedValueType::Null),
        TypedExpr::Integer { value } => {
            ensure_safe_integer(*value, path)?;
            Ok(TypedValueType::Integer)
        },
        TypedExpr::Array { items } => {
            for (index, item) in items.iter().enumerate() {
                child(item, Some(TypedValueType::String), &format!("items[{index}]"), state)?;
            }
            Ok(TypedValueType::StringArray)
        },
        TypedExpr::Length { value } => {
            let value_type = child(value, None, "value", state)?;
            if !matches!(value_type, TypedValueType::String | TypedValueType::StringArray) {
                return Err(TypedHookError::new(format!(
                    "{path}.value must be a string or string-array"
                )));
            }
            Ok(TypedValueType::Integer)
        },
        TypedExpr::StringIncludes { value, needle } => {
            child(value, Some(TypedValueType::String), "value", state)?;
            child(needle, Some(TypedValueType::String), "needle", state)?;
            Ok(TypedValueType::Bool)
        },
        TypedExpr::StringIndexOf { value, needle } | TypedExpr::StringLastIndexOf { value, needle } => {
            child(value, Some(TypedValueType::String), "value", state)?;
            child(needle, Some(TypedValueType::String), "needle", state)?;
            Ok(TypedValueType::Integer)
        },
        TypedExpr::StringSlice { value, start } => {
            child(value, Some(TypedValueType::String), "value", state)?;
            child(start, Some(TypedValueType::Integer), "start", state)?;
            Ok(TypedValueType::String)
        },
        TypedExpr::StringSliceAfterFirst { value, needle } => {
            child(value, Some(TypedValueType::String), "value", state)?;
            child(needle, Some(TypedValueType::String), "needle", state)?;
            if !is_string_literal(needle, ":") {
                return Err(TypedHookError::new(format!(
                    "{path}.needle must be the closed getQueryTerm colon literal"
                )));
            }
            Ok(TypedValueType::String)
        },
        TypedExpr::StringSubstring { value, start, end } => {
            child(value, Some(TypedValueType::String), "value", state)?;
            child(start, Some(TypedValueType::Integer), "start", state)?;
            child(end, Some(TypedValueType::Integer), "end", state)?;
            Ok(TypedValueType::String)
        },
        TypedExpr::StringSplit { value, separator } => {
            child(value, Some(TypedValueType::String), "value", state)?;
            child(separator, Some(TypedValueType::String), "separator", state)?;
            Ok(TypedValueType::StringArray)
        },
        TypedExpr::StringTrim { value }
        | TypedExpr::StringTrimStart { value }
        | TypedExpr::StringTrimEnd { value }
        | TypedExpr::StringToLower { value }
        | TypedExpr::StringToUpper { value } => {
            child(value, Some(TypedValueType::String), "value", state)?;
            Ok(TypedValueType::String)
        },
        TypedExpr::StringReplace {
            value,
            needle,
            replacement,
        }
        | TypedExpr::StringReplaceAll {
            value,
            needle,
            replacement,
        } => {
            child(value, Some(TypedValueType::String), "value", state)?;
            child(needle, Some(TypedValueType::String), "needle", state)?;
            if !matches!(needle.as_ref(), TypedExpr::String { .. }) {
                return Err(TypedHookError::new(format!("{path}.needle must be a string literal")));
            }
            child(replacement, Some(TypedValueType::String), "replacement", state)?;
            Ok(TypedValueType::String)
        },
        TypedExpr::StringStartsWith { value, needle } | TypedExpr::StringEndsWith { value, needle } => {
            child(value, Some(TypedValueType::String), "value", state)?;
            child(needle, Some(TypedValueType::String), "needle", state)?;
            Ok(TypedValueType::Bool)
        },
        TypedExpr::StringPadStart { value, target, pad } | TypedExpr::StringPadEnd { value, target, pad } => {
            child(value, Some(TypedValueType::String), "value", state)?;
            child(target, Some(TypedValueType::Integer), "target", state)?;
            child(pad, Some(TypedValueType::String), "pad", state)?;
            Ok(TypedValueType::String)
        },
        TypedExpr::StringRepeat { value, count } => {
            child(value, Some(TypedValueType::String), "value", state)?;
            child(count, Some(TypedValueType::Integer), "count", state)?;
            Ok(TypedValueType::String)
        },
        TypedExpr::StringConcat { parts } => {
            if parts.is_empty() {
                return Err(TypedHookError::new(format!("{path}.parts must be a non-empty array")));
            }
            for (index, part) in parts.iter().enumerate() {
                child(part, Some(TypedValueType::String), &format!("parts[{index}]"), state)?;
            }
            Ok(TypedValueType::String)
        },
        TypedExpr::StringCharAt { value, index } | TypedExpr::StringAt { value, index } => {
            child(value, Some(TypedValueType::String), "value", state)?;
            child(index, Some(TypedValueType::Integer), "index", state)?;
            Ok(TypedValueType::String)
        },
        TypedExpr::ArrayIncludes { value, needle } => {
            child(value, Some(TypedValueType::StringArray), "value", state)?;
            child(needle, Some(TypedValueType::String), "needle", state)?;
            Ok(TypedValueType::Bool)
        },
        TypedExpr::StrictEq { left, right } | TypedExpr::StrictNe { left, right } => {
            let left_type = child(left, None, "left", state)?;
            child(right, Some(left_type), "right", state)?;
            if left_type == TypedValueType::StringArray {
                return Err(TypedHookError::new(format!(
                    "{path} cannot compare string arrays by identity"
                )));
            }
            Ok(TypedValueType::Bool)
        },
        TypedExpr::Add { left, right } | TypedExpr::Sub { left, right } | TypedExpr::Mul { left, right } => {
            child(left, Some(TypedValueType::Integer), "left", state)?;
            child(right, Some(TypedValueType::Integer), "right", state)?;
            Ok(TypedValueType::Integer)
        },
        TypedExpr::LessThan { left, right }
        | TypedExpr::LessThanOrEqual { left, right }
        | TypedExpr::GreaterThan { left, right }
        | TypedExpr::GreaterThanOrEqual { left, right } => {
            child(left, Some(TypedValueType::Integer), "left", state)?;
            child(right, Some(TypedValueType::Integer), "right", state)?;
            Ok(TypedValueType::Bool)
        },
        TypedExpr::Not { value } => {
            child(value, Some(TypedValueType::Bool), "value", state)?;
            Ok(TypedValueType::Bool)
        },
        TypedExpr::Nullish { left, right } => {
            let left_type = child(left, None, "left", state)?;
            let right_type = child(right, None, "right", state)?;
            let result_type = if left_type == TypedValueType::Null {
                right_type
            } else if right_type == TypedValueType::Null || left_type == right_type {
                left_type
            } else {
                return Err(TypedHookError::new(format!(
                    "{path} ?? operands must share a type or be null"
                )));
            };
            Ok(result_type)
        },
        TypedExpr::And { left, right } | TypedExpr::Or { left, right } => {
            child(left, Some(TypedValueType::Bool), "left", state)?;
            child(right, Some(TypedValueType::Bool), "right", state)?;
            Ok(TypedValueType::Bool)
        },
        TypedExpr::If {
            condition,
            then_branch,
            else_branch,
        } => {
            child(condition, Some(TypedValueType::Bool), "condition", state)?;
            let then_type = child(then_branch, None, "then", state)?;
            child(else_branch, Some(then_type), "else", state)?;
            Ok(then_type)
        },
    }
}

fn ensure_safe_integer(value: i64, path: &str) -> TypedHookResult<()> {
    if !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&value) {
        return Err(TypedHookError::new(format!("{path} must be a JavaScript safe integer")));
    }
    Ok(())
}

/// Evaluate a validated trigger descriptor against its two string arguments.
/// The return type is deliberately boolean because the trigger contract is
/// fixed to `{ string, string } -> bool` in this first native slice.
pub(crate) fn evaluate_typed_trigger(
    descriptor: &TypedHookIr,
    search_term: &str,
    previous_search_term: &str,
) -> TypedHookResult<bool> {
    let arguments = [
        TypedValue::String(Utf16String::from_str(search_term)),
        TypedValue::String(Utf16String::from_str(previous_search_term)),
    ];
    let value = evaluate_expr(&descriptor.expr, &arguments)?;
    match value {
        TypedValue::Bool(value) => Ok(value),
        _ => Err(TypedHookError::new(
            "validated trigger expression did not evaluate to bool",
        )),
    }
}

/// Evaluate a getQueryTerm descriptor.  This helper is intentionally not
/// called by the completion runtime; the production path continues to use
/// its existing QuickJS adapter until the path switch is allowed.
pub(crate) fn evaluate_typed_get_query_term(descriptor: &TypedHookIr, search_term: &str) -> TypedHookResult<String> {
    validate_typed_hook_ir(descriptor)?;
    if descriptor.source_field != GET_QUERY_TERM_SOURCE_FIELD {
        return Err(TypedHookError::new("descriptor is not a getQueryTerm research hook"));
    }
    let arguments = [TypedValue::String(Utf16String::from_str(search_term))];
    let value = evaluate_expr(&descriptor.expr, &arguments)?;
    let TypedValue::String(value) = value else {
        return Err(TypedHookError::new(
            "validated getQueryTerm expression did not evaluate to string",
        ));
    };
    String::from_utf16(&value.0)
        .map_err(|error| TypedHookError::new(format!("getQueryTerm returned invalid UTF-16: {error}")))
}

fn evaluate_expr(expression: &TypedExpr, arguments: &[TypedValue]) -> TypedHookResult<TypedValue> {
    match expression {
        TypedExpr::Arg { index } => {
            let index = usize::try_from(*index)
                .map_err(|error| TypedHookError::new(format!("argument index {index} is unavailable: {error}")))?;
            arguments
                .get(index)
                .cloned()
                .ok_or_else(|| TypedHookError::new(format!("argument index {index} is unavailable")))
        },
        TypedExpr::String { value } => Ok(TypedValue::String(Utf16String::from_str(value))),
        TypedExpr::Bool { value } => Ok(TypedValue::Bool(*value)),
        TypedExpr::Null => Ok(TypedValue::Null),
        TypedExpr::Integer { value } => {
            ensure_safe_integer(*value, "integer")?;
            Ok(TypedValue::Integer(*value))
        },
        TypedExpr::Array { items } => items
            .iter()
            .map(|item| match evaluate_expr(item, arguments)? {
                TypedValue::String(value) => Ok(value),
                _ => Err(TypedHookError::new("array item did not evaluate to string")),
            })
            .collect::<TypedHookResult<Vec<_>>>()
            .map(TypedValue::StringArray),
        TypedExpr::Length { value } => match evaluate_expr(value, arguments)? {
            TypedValue::String(value) => safe_length(value.len()),
            TypedValue::StringArray(value) => safe_length(value.len()),
            _ => Err(TypedHookError::new("length receiver was not string-like")),
        },
        TypedExpr::StringIncludes { value, needle } => {
            let value = evaluate_string(value, arguments, "string-includes.value")?;
            let needle = evaluate_string(needle, arguments, "string-includes.needle")?;
            Ok(TypedValue::Bool(utf16_index_of(&value, &needle).is_some()))
        },
        TypedExpr::StringIndexOf { value, needle } => {
            let value = evaluate_string(value, arguments, "string-index-of.value")?;
            let needle = evaluate_string(needle, arguments, "string-index-of.needle")?;
            Ok(TypedValue::Integer(utf16_index_of_i64(&value, &needle)?))
        },
        TypedExpr::StringLastIndexOf { value, needle } => {
            let value = evaluate_string(value, arguments, "string-last-index-of.value")?;
            let needle = evaluate_string(needle, arguments, "string-last-index-of.needle")?;
            Ok(TypedValue::Integer(utf16_last_index_of_i64(&value, &needle)?))
        },
        TypedExpr::StringSlice { value, start } => {
            let value = evaluate_string(value, arguments, "string-slice.value")?;
            let start = evaluate_integer(start, arguments, "string-slice.start")?;
            Ok(TypedValue::String(js_slice(&value, start)?))
        },
        TypedExpr::StringSliceAfterFirst { value, needle } => {
            let value = evaluate_string(value, arguments, "string-slice-after-first.value")?;
            let needle = evaluate_string(needle, arguments, "string-slice-after-first.needle")?;
            let Some(index) = utf16_index_of(&value, &needle) else {
                return Ok(TypedValue::String(value));
            };
            let start = index
                .checked_add(1)
                .ok_or_else(|| TypedHookError::new("string slice start overflowed"))?;
            let start = i64::try_from(start)
                .map_err(|error| TypedHookError::new(format!("string slice start is too large: {error}")))?;
            Ok(TypedValue::String(js_slice(&value, start)?))
        },
        TypedExpr::StringSubstring { value, start, end } => {
            let value = evaluate_string(value, arguments, "string-substring.value")?;
            let start = evaluate_integer(start, arguments, "string-substring.start")?;
            let end = evaluate_integer(end, arguments, "string-substring.end")?;
            Ok(TypedValue::String(js_substring(&value, start, end)?))
        },
        TypedExpr::StringSplit { value, separator } => {
            let value = evaluate_string(value, arguments, "string-split.value")?;
            let separator = evaluate_string(separator, arguments, "string-split.separator")?;
            Ok(TypedValue::StringArray(js_split(&value, &separator)))
        },
        TypedExpr::StringTrim { value } => {
            let value = evaluate_string(value, arguments, "string-trim.value")?;
            Ok(TypedValue::String(js_trim(&value, true, true)))
        },
        TypedExpr::StringTrimStart { value } => {
            let value = evaluate_string(value, arguments, "string-trim-start.value")?;
            Ok(TypedValue::String(js_trim(&value, true, false)))
        },
        TypedExpr::StringTrimEnd { value } => {
            let value = evaluate_string(value, arguments, "string-trim-end.value")?;
            Ok(TypedValue::String(js_trim(&value, false, true)))
        },
        TypedExpr::StringReplace {
            value,
            needle,
            replacement,
        } => {
            let value = evaluate_string(value, arguments, "string-replace.value")?;
            let needle = evaluate_string(needle, arguments, "string-replace.needle")?;
            let replacement = evaluate_string(replacement, arguments, "string-replace.replacement")?;
            Ok(TypedValue::String(js_replace(&value, &needle, &replacement, false)?))
        },
        TypedExpr::StringReplaceAll {
            value,
            needle,
            replacement,
        } => {
            let value = evaluate_string(value, arguments, "string-replace-all.value")?;
            let needle = evaluate_string(needle, arguments, "string-replace-all.needle")?;
            let replacement = evaluate_string(replacement, arguments, "string-replace-all.replacement")?;
            Ok(TypedValue::String(js_replace(&value, &needle, &replacement, true)?))
        },
        TypedExpr::StringStartsWith { value, needle } => {
            let value = evaluate_string(value, arguments, "string-starts-with.value")?;
            let needle = evaluate_string(needle, arguments, "string-starts-with.needle")?;
            Ok(TypedValue::Bool(utf16_starts_with(&value, &needle)))
        },
        TypedExpr::StringEndsWith { value, needle } => {
            let value = evaluate_string(value, arguments, "string-ends-with.value")?;
            let needle = evaluate_string(needle, arguments, "string-ends-with.needle")?;
            Ok(TypedValue::Bool(utf16_ends_with(&value, &needle)))
        },
        TypedExpr::StringToLower { value } => {
            let value = evaluate_string(value, arguments, "string-to-lower.value")?;
            Ok(TypedValue::String(js_map_case(&value, false)))
        },
        TypedExpr::StringToUpper { value } => {
            let value = evaluate_string(value, arguments, "string-to-upper.value")?;
            Ok(TypedValue::String(js_map_case(&value, true)))
        },
        TypedExpr::StringPadStart { value, target, pad } => {
            let value = evaluate_string(value, arguments, "string-pad-start.value")?;
            let target = evaluate_integer(target, arguments, "string-pad-start.target")?;
            let pad = evaluate_string(pad, arguments, "string-pad-start.pad")?;
            Ok(TypedValue::String(js_pad(&value, target, &pad, false)?))
        },
        TypedExpr::StringPadEnd { value, target, pad } => {
            let value = evaluate_string(value, arguments, "string-pad-end.value")?;
            let target = evaluate_integer(target, arguments, "string-pad-end.target")?;
            let pad = evaluate_string(pad, arguments, "string-pad-end.pad")?;
            Ok(TypedValue::String(js_pad(&value, target, &pad, true)?))
        },
        TypedExpr::StringRepeat { value, count } => {
            let value = evaluate_string(value, arguments, "string-repeat.value")?;
            let count = evaluate_integer(count, arguments, "string-repeat.count")?;
            Ok(TypedValue::String(js_repeat(&value, count)?))
        },
        TypedExpr::StringConcat { parts } => {
            let mut units = Vec::new();
            for part in parts {
                let part = evaluate_string(part, arguments, "string-concat.parts")?;
                units.extend_from_slice(&part.0);
            }
            Ok(TypedValue::String(ensure_string_limit(units)?))
        },
        TypedExpr::StringCharAt { value, index } => {
            let value = evaluate_string(value, arguments, "string-char-at.value")?;
            let index = evaluate_integer(index, arguments, "string-char-at.index")?;
            Ok(TypedValue::String(js_char_at(&value, index)))
        },
        TypedExpr::StringAt { value, index } => {
            let value = evaluate_string(value, arguments, "string-at.value")?;
            let index = evaluate_integer(index, arguments, "string-at.index")?;
            Ok(TypedValue::String(js_at(&value, index)?))
        },
        TypedExpr::ArrayIncludes { value, needle } => {
            let value = evaluate_array(value, arguments, "array-includes.value")?;
            let needle = evaluate_string(needle, arguments, "array-includes.needle")?;
            Ok(TypedValue::Bool(value.iter().any(|item| item == &needle)))
        },
        TypedExpr::StrictEq { left, right } => {
            let left = evaluate_expr(left, arguments)?;
            let right = evaluate_expr(right, arguments)?;
            Ok(TypedValue::Bool(strict_equal(&left, &right)?))
        },
        TypedExpr::StrictNe { left, right } => {
            let left = evaluate_expr(left, arguments)?;
            let right = evaluate_expr(right, arguments)?;
            Ok(TypedValue::Bool(!strict_equal(&left, &right)?))
        },
        TypedExpr::Add { left, right } => {
            let left = evaluate_integer(left, arguments, "add.left")?;
            let right = evaluate_integer(right, arguments, "add.right")?;
            Ok(TypedValue::Integer(safe_arithmetic(left.checked_add(right), "add")?))
        },
        TypedExpr::Sub { left, right } => {
            let left = evaluate_integer(left, arguments, "sub.left")?;
            let right = evaluate_integer(right, arguments, "sub.right")?;
            Ok(TypedValue::Integer(safe_arithmetic(left.checked_sub(right), "sub")?))
        },
        TypedExpr::Mul { left, right } => {
            let left = evaluate_integer(left, arguments, "mul.left")?;
            let right = evaluate_integer(right, arguments, "mul.right")?;
            Ok(TypedValue::Integer(safe_arithmetic(left.checked_mul(right), "mul")?))
        },
        TypedExpr::LessThan { left, right } => {
            let left = evaluate_integer(left, arguments, "lt.left")?;
            let right = evaluate_integer(right, arguments, "lt.right")?;
            Ok(TypedValue::Bool(left < right))
        },
        TypedExpr::LessThanOrEqual { left, right } => {
            let left = evaluate_integer(left, arguments, "le.left")?;
            let right = evaluate_integer(right, arguments, "le.right")?;
            Ok(TypedValue::Bool(left <= right))
        },
        TypedExpr::GreaterThan { left, right } => {
            let left = evaluate_integer(left, arguments, "gt.left")?;
            let right = evaluate_integer(right, arguments, "gt.right")?;
            Ok(TypedValue::Bool(left > right))
        },
        TypedExpr::GreaterThanOrEqual { left, right } => {
            let left = evaluate_integer(left, arguments, "ge.left")?;
            let right = evaluate_integer(right, arguments, "ge.right")?;
            Ok(TypedValue::Bool(left >= right))
        },
        TypedExpr::Not { value } => {
            let value = evaluate_bool(value, arguments, "not.value")?;
            Ok(TypedValue::Bool(!value))
        },
        TypedExpr::Nullish { left, right } => {
            let left = evaluate_expr(left, arguments)?;
            if matches!(left, TypedValue::Null) {
                evaluate_expr(right, arguments)
            } else {
                Ok(left)
            }
        },
        TypedExpr::And { left, right } => {
            let left = evaluate_bool(left, arguments, "and.left")?;
            if !left {
                return Ok(TypedValue::Bool(false));
            }
            Ok(TypedValue::Bool(evaluate_bool(right, arguments, "and.right")?))
        },
        TypedExpr::Or { left, right } => {
            let left = evaluate_bool(left, arguments, "or.left")?;
            if left {
                return Ok(TypedValue::Bool(true));
            }
            Ok(TypedValue::Bool(evaluate_bool(right, arguments, "or.right")?))
        },
        TypedExpr::If {
            condition,
            then_branch,
            else_branch,
        } => {
            let condition = evaluate_bool(condition, arguments, "if.condition")?;
            if condition {
                evaluate_expr(then_branch, arguments)
            } else {
                evaluate_expr(else_branch, arguments)
            }
        },
    }
}

fn safe_length(length: usize) -> TypedHookResult<TypedValue> {
    let value = i64::try_from(length).map_err(|error| TypedHookError::new(format!("length is too large: {error}")))?;
    ensure_safe_integer(value, "length")?;
    Ok(TypedValue::Integer(value))
}

fn evaluate_string(expression: &TypedExpr, arguments: &[TypedValue], label: &str) -> TypedHookResult<Utf16String> {
    match evaluate_expr(expression, arguments)? {
        TypedValue::String(value) => Ok(value),
        _ => Err(TypedHookError::new(format!("{label} is not a string"))),
    }
}

fn evaluate_array(expression: &TypedExpr, arguments: &[TypedValue], label: &str) -> TypedHookResult<Vec<Utf16String>> {
    match evaluate_expr(expression, arguments)? {
        TypedValue::StringArray(value) => Ok(value),
        _ => Err(TypedHookError::new(format!("{label} is not a string array"))),
    }
}

fn evaluate_integer(expression: &TypedExpr, arguments: &[TypedValue], label: &str) -> TypedHookResult<i64> {
    match evaluate_expr(expression, arguments)? {
        TypedValue::Integer(value) => Ok(value),
        _ => Err(TypedHookError::new(format!("{label} is not an integer"))),
    }
}

fn evaluate_bool(expression: &TypedExpr, arguments: &[TypedValue], label: &str) -> TypedHookResult<bool> {
    match evaluate_expr(expression, arguments)? {
        TypedValue::Bool(value) => Ok(value),
        _ => Err(TypedHookError::new(format!("{label} is not a bool"))),
    }
}

fn strict_equal(left: &TypedValue, right: &TypedValue) -> TypedHookResult<bool> {
    match (left, right) {
        (TypedValue::String(left), TypedValue::String(right)) => Ok(left == right),
        (TypedValue::Bool(left), TypedValue::Bool(right)) => Ok(left == right),
        (TypedValue::Integer(left), TypedValue::Integer(right)) => Ok(left == right),
        (TypedValue::Null, TypedValue::Null) => Ok(true),
        (TypedValue::StringArray(_), TypedValue::StringArray(_)) => {
            Err(TypedHookError::new("string-array strict equality is not representable"))
        },
        _ => Err(TypedHookError::new("strict equality operands have different types")),
    }
}

fn safe_arithmetic(value: Option<i64>, path: &str) -> TypedHookResult<i64> {
    let value =
        value.ok_or_else(|| TypedHookError::new(format!("{path} overflowed the JavaScript safe integer range")))?;
    ensure_safe_integer(value, path)?;
    Ok(value)
}

fn utf16_index_of_i64(value: &Utf16String, needle: &Utf16String) -> TypedHookResult<i64> {
    utf16_index_of(value, needle)
        .map(|index| {
            i64::try_from(index).map_err(|error| TypedHookError::new(format!("string index is too large: {error}")))
        })
        .transpose()
        .map(|index| index.unwrap_or(-1))
}

fn utf16_last_index_of(value: &Utf16String, needle: &Utf16String) -> Option<usize> {
    if needle.0.is_empty() {
        return Some(value.len());
    }
    if needle.0.len() > value.0.len() {
        return None;
    }
    value
        .0
        .windows(needle.0.len())
        .rposition(|window| window == needle.0.as_slice())
}

fn utf16_last_index_of_i64(value: &Utf16String, needle: &Utf16String) -> TypedHookResult<i64> {
    utf16_last_index_of(value, needle)
        .map(|index| {
            i64::try_from(index).map_err(|error| TypedHookError::new(format!("string index is too large: {error}")))
        })
        .transpose()
        .map(|index| index.unwrap_or(-1))
}

fn utf16_starts_with(value: &Utf16String, needle: &Utf16String) -> bool {
    value.0.starts_with(&needle.0)
}

fn utf16_ends_with(value: &Utf16String, needle: &Utf16String) -> bool {
    value.0.ends_with(&needle.0)
}

fn is_js_trim_unit(unit: u16) -> bool {
    matches!(
        unit,
        0x0009
            | 0x000A
            | 0x000B
            | 0x000C
            | 0x000D
            | 0x0020
            | 0x00A0
            | 0x1680
            | 0x2028
            | 0x2029
            | 0x202F
            | 0x205F
            | 0x3000
            | 0xFEFF
    ) || (0x2000..=0x200A).contains(&unit)
}

fn js_trim(value: &Utf16String, start: bool, end: bool) -> Utf16String {
    let mut units = value.0.as_slice();
    if start {
        while units.first().is_some_and(|unit| is_js_trim_unit(*unit)) {
            units = &units[1..];
        }
    }
    if end {
        while units.last().is_some_and(|unit| is_js_trim_unit(*unit)) {
            units = &units[..units.len() - 1];
        }
    }
    Utf16String::from_units(units.to_vec())
}

fn map_scalar_case(scalar: char, upper: bool) -> String {
    if upper {
        scalar.to_uppercase().collect()
    } else {
        scalar.to_lowercase().collect()
    }
}

fn js_map_case(value: &Utf16String, upper: bool) -> Utf16String {
    let mut out = Vec::with_capacity(value.0.len());
    let mut index = 0;
    while index < value.0.len() {
        let unit = value.0[index];
        if (0xD800..=0xDBFF).contains(&unit)
            && let Some(next) = value.0.get(index + 1)
            && (0xDC00..=0xDFFF).contains(next)
        {
            let scalar = char::decode_utf16([unit, *next])
                .next()
                .and_then(Result::ok)
                .expect("paired surrogates decode");
            out.extend(map_scalar_case(scalar, upper).encode_utf16());
            index += 2;
            continue;
        }
        if (0xD800..=0xDFFF).contains(&unit) {
            out.push(unit);
            index += 1;
            continue;
        }
        let scalar = char::from_u32(u32::from(unit)).expect("BMP code unit is a scalar");
        out.extend(map_scalar_case(scalar, upper).encode_utf16());
        index += 1;
    }
    Utf16String::from_units(out)
}

fn js_substring(value: &Utf16String, start: i64, end: i64) -> TypedHookResult<Utf16String> {
    let length =
        i64::try_from(value.len()).map_err(|error| TypedHookError::new(format!("string is too long: {error}")))?;
    let clamp = |index: i64| index.clamp(0, length);
    let mut from = clamp(start);
    let mut to = clamp(end);
    if from > to {
        std::mem::swap(&mut from, &mut to);
    }
    Ok(Utf16String::from_units(value.0[from as usize..to as usize].to_vec()))
}

fn js_replace(
    value: &Utf16String,
    needle: &Utf16String,
    replacement: &Utf16String,
    all: bool,
) -> TypedHookResult<Utf16String> {
    if needle.0.is_empty() {
        if !all {
            let mut out = replacement.0.clone();
            out.extend_from_slice(&value.0);
            return ensure_string_limit(out);
        }
        let mut out = replacement.0.clone();
        for unit in &value.0 {
            out.push(*unit);
            out.extend_from_slice(&replacement.0);
        }
        return ensure_string_limit(out);
    }
    if !all {
        let Some(index) = utf16_index_of(value, needle) else {
            return Ok(value.clone());
        };
        let mut out = value.0[..index].to_vec();
        out.extend_from_slice(&replacement.0);
        out.extend_from_slice(&value.0[index + needle.0.len()..]);
        return ensure_string_limit(out);
    }
    let mut out = Vec::new();
    let mut start = 0;
    while let Some(relative) = value.0[start..]
        .windows(needle.0.len())
        .position(|window| window == needle.0.as_slice())
    {
        let end = start + relative;
        out.extend_from_slice(&value.0[start..end]);
        out.extend_from_slice(&replacement.0);
        start = end + needle.0.len();
    }
    out.extend_from_slice(&value.0[start..]);
    ensure_string_limit(out)
}

fn js_pad(value: &Utf16String, target: i64, pad: &Utf16String, end: bool) -> TypedHookResult<Utf16String> {
    let target = if target < 0 { 0 } else { target };
    let length =
        i64::try_from(value.len()).map_err(|error| TypedHookError::new(format!("string is too long: {error}")))?;
    if target <= length || pad.0.is_empty() {
        return Ok(value.clone());
    }
    let needed = usize::try_from(target - length)
        .map_err(|error| TypedHookError::new(format!("pad target is too large: {error}")))?;
    let mut fill = Vec::with_capacity(needed);
    while fill.len() < needed {
        fill.extend_from_slice(&pad.0);
    }
    fill.truncate(needed);
    let out = if end {
        let mut out = value.0.clone();
        out.extend_from_slice(&fill);
        out
    } else {
        fill.extend_from_slice(&value.0);
        fill
    };
    ensure_string_limit(out)
}

fn js_repeat(value: &Utf16String, count: i64) -> TypedHookResult<Utf16String> {
    if count < 0 {
        return Err(TypedHookError::new("string-repeat count must be non-negative"));
    }
    let count = usize::try_from(count).map_err(|error| TypedHookError::new(format!("string-repeat count: {error}")))?;
    let units = value
        .len()
        .checked_mul(count)
        .ok_or_else(|| TypedHookError::new("string-repeat overflowed the JavaScript safe integer range"))?;
    if units > MAX_STRING_LITERAL_UNITS {
        return Err(TypedHookError::new("string-repeat exceeds the UTF-16 code-unit limit"));
    }
    let mut out = Vec::with_capacity(units);
    for _ in 0..count {
        out.extend_from_slice(&value.0);
    }
    Ok(Utf16String::from_units(out))
}

fn js_char_at(value: &Utf16String, index: i64) -> Utf16String {
    if index < 0 {
        return Utf16String::from_units(Vec::<u16>::new());
    }
    let Ok(index) = usize::try_from(index) else {
        return Utf16String::from_units(Vec::<u16>::new());
    };
    match value.0.get(index) {
        Some(unit) => Utf16String::from_units(vec![*unit]),
        None => Utf16String::from_units(Vec::<u16>::new()),
    }
}

fn js_at(value: &Utf16String, index: i64) -> TypedHookResult<Utf16String> {
    let length =
        i64::try_from(value.len()).map_err(|error| TypedHookError::new(format!("string is too long: {error}")))?;
    let actual = if index < 0 { length + index } else { index };
    Ok(js_char_at(value, actual))
}

fn ensure_string_limit(units: Vec<u16>) -> TypedHookResult<Utf16String> {
    if units.len() > MAX_STRING_LITERAL_UNITS {
        return Err(TypedHookError::new(format!(
            "string exceeds {MAX_STRING_LITERAL_UNITS} UTF-16 code units"
        )));
    }
    Ok(Utf16String::from_units(units))
}

fn json_to_typed_value(value: &JsonValue, expected: TypedValueType) -> TypedHookResult<TypedValue> {
    match expected {
        TypedValueType::String => {
            let Some(text) = value.as_str() else {
                return Err(TypedHookError::new("argument is not a string"));
            };
            Ok(TypedValue::String(Utf16String::from_str(text)))
        },
        TypedValueType::Bool => {
            let Some(flag) = value.as_bool() else {
                return Err(TypedHookError::new("argument is not a bool"));
            };
            Ok(TypedValue::Bool(flag))
        },
        TypedValueType::Integer => {
            let Some(number) = value.as_i64() else {
                return Err(TypedHookError::new("argument is not an integer"));
            };
            ensure_safe_integer(number, "argument")?;
            Ok(TypedValue::Integer(number))
        },
        TypedValueType::StringArray => {
            let Some(items) = value.as_array() else {
                return Err(TypedHookError::new("argument is not a string array"));
            };
            let values = items
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(Utf16String::from_str)
                        .ok_or_else(|| TypedHookError::new("string-array item is not a string"))
                })
                .collect::<TypedHookResult<Vec<_>>>()?;
            Ok(TypedValue::StringArray(values))
        },
        TypedValueType::Null => {
            if value.is_null() {
                Ok(TypedValue::Null)
            } else {
                Err(TypedHookError::new("argument is not null"))
            }
        },
        TypedValueType::Json
        | TypedValueType::Suggestion
        | TypedValueType::SuggestionArray
        | TypedValueType::StringRecord => Err(TypedHookError::new(
            "json/suggestion value types are compile-only until object ops land",
        )),
    }
}

fn typed_value_to_json(value: &TypedValue) -> TypedHookResult<JsonValue> {
    match value {
        TypedValue::String(value) => {
            let text = String::from_utf16(&value.0)
                .map_err(|error| TypedHookError::new(format!("evaluated string is not valid UTF-16: {error}")))?;
            Ok(JsonValue::String(text))
        },
        TypedValue::Bool(value) => Ok(JsonValue::Bool(*value)),
        TypedValue::Integer(value) => Ok(JsonValue::from(*value)),
        TypedValue::StringArray(items) => {
            let values = items
                .iter()
                .map(|item| {
                    String::from_utf16(&item.0)
                        .map(JsonValue::String)
                        .map_err(|error| TypedHookError::new(format!("evaluated string is not valid UTF-16: {error}")))
                })
                .collect::<TypedHookResult<Vec<_>>>()?;
            Ok(JsonValue::Array(values))
        },
        TypedValue::Null => Ok(JsonValue::Null),
    }
}

/// Evaluate a validated descriptor against JSON arguments that match its
/// field contract.  Used by the T2.1 cross-language golden.
pub(crate) fn evaluate_typed_hook_json(descriptor: &TypedHookIr, args: &[JsonValue]) -> TypedHookResult<JsonValue> {
    if args.len() != descriptor.params.len() {
        return Err(TypedHookError::new("evaluate args must match the field contract"));
    }
    let arguments = descriptor
        .params
        .iter()
        .zip(args)
        .map(|(param, value)| json_to_typed_value(value, param.value_type))
        .collect::<TypedHookResult<Vec<_>>>()?;
    typed_value_to_json(&evaluate_expr(&descriptor.expr, &arguments)?)
}

fn utf16_index_of(value: &Utf16String, needle: &Utf16String) -> Option<usize> {
    if needle.0.is_empty() {
        return Some(0);
    }
    if needle.0.len() > value.0.len() {
        return None;
    }
    value
        .0
        .windows(needle.0.len())
        .position(|window| window == needle.0.as_slice())
}

fn js_slice(value: &Utf16String, start: i64) -> TypedHookResult<Utf16String> {
    let length =
        i64::try_from(value.len()).map_err(|error| TypedHookError::new(format!("string is too long: {error}")))?;
    let index = if start < 0 {
        (length + start).max(0)
    } else {
        start.min(length)
    } as usize;
    Ok(Utf16String::from_units(value.0[index..].to_vec()))
}

fn js_split(value: &Utf16String, separator: &Utf16String) -> Vec<Utf16String> {
    if separator.0.is_empty() {
        return value
            .0
            .iter()
            .map(|unit| Utf16String::from_units(vec![*unit]))
            .collect();
    }

    let mut result = Vec::new();
    let mut start = 0;
    while let Some(relative) = value.0[start..]
        .windows(separator.0.len())
        .position(|window| window == separator.0.as_slice())
    {
        let end = start + relative;
        result.push(Utf16String::from_units(value.0[start..end].to_vec()));
        start = end + separator.0.len();
    }
    result.push(Utf16String::from_units(value.0[start..].to_vec()));
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs::{self, Metadata, OpenOptions};
    use std::io::Read;
    use std::path::{Component, Path, PathBuf};

    #[cfg(unix)]
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    const TYPED_TRIGGER_REFERENCE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/typed-hooks/reference.json"
    ));
    const TYPED_GET_QUERY_TERM_REFERENCE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/typed-hooks/asdf-get-query-term-reference.json"
    ));

    const ASDF_REFERENCE_HARNESS_FILES: &[&str] = &[
        "scripts/audit-spec-hooks.mjs",
        "scripts/capture-hook-reference.mjs",
        "scripts/capture-typed-trigger-reference.mjs",
        "scripts/filepaths-helper.mjs",
        "scripts/reference-audit-worker.mjs",
        "scripts/reference-hook-worker.mjs",
        "scripts/reference-safe-io.mjs",
        "scripts/spec-hook-contract.mjs",
        "scripts/spec-pair.mjs",
        "scripts/typed-hook-ir.mjs",
    ];

    fn arg(index: u64) -> TypedExpr {
        TypedExpr::Arg { index }
    }

    fn string(value: &str) -> TypedExpr {
        TypedExpr::String {
            value: value.to_owned(),
        }
    }

    fn integer(value: i64) -> TypedExpr {
        TypedExpr::Integer { value }
    }

    fn bool_value(value: bool) -> TypedExpr {
        TypedExpr::Bool { value }
    }

    fn length(value: TypedExpr) -> TypedExpr {
        TypedExpr::Length { value: Box::new(value) }
    }

    fn strict_eq(left: TypedExpr, right: TypedExpr) -> TypedExpr {
        TypedExpr::StrictEq {
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    fn strict_ne(left: TypedExpr, right: TypedExpr) -> TypedExpr {
        TypedExpr::StrictNe {
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    fn gt(left: TypedExpr, right: TypedExpr) -> TypedExpr {
        TypedExpr::GreaterThan {
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    fn and(left: TypedExpr, right: TypedExpr) -> TypedExpr {
        TypedExpr::And {
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    fn or(left: TypedExpr, right: TypedExpr) -> TypedExpr {
        TypedExpr::Or {
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    fn trigger_descriptor(expression: TypedExpr) -> JsonValue {
        json!({
            "version": IR_VERSION,
            "kind": IR_KIND,
            "sourceField": SOURCE_FIELD,
            "resultType": "bool",
            "params": [
                {"index": 0, "type": "string"},
                {"index": 1, "type": "string"},
            ],
            "expr": serde_json::to_value(expression).expect("expression JSON"),
        })
    }

    fn parse_expression(expression: TypedExpr) -> TypedHookIr {
        parse_typed_hook_ir(&trigger_descriptor(expression)).expect("valid trigger descriptor")
    }

    fn get_query_term_descriptor() -> JsonValue {
        json!({
            "version": IR_VERSION,
            "kind": IR_KIND,
            "sourceField": GET_QUERY_TERM_SOURCE_FIELD,
            "resultType": "string",
            "params": [{"index": 0, "type": "string"}],
            "expr": {
                "op": "if",
                "condition": {
                    "op": "string-includes",
                    "value": {"op": "arg", "index": 0},
                    "needle": {"op": "string", "value": "latest"},
                },
                "then": {
                    "op": "string-slice-after-first",
                    "value": {"op": "arg", "index": 0},
                    "needle": {"op": "string", "value": ":"},
                },
                "else": {"op": "arg", "index": 0},
            },
        })
    }

    fn parse_get_query_term_descriptor() -> TypedHookIr {
        parse_typed_hook_ir(&get_query_term_descriptor()).expect("valid getQueryTerm descriptor")
    }

    fn evaluate(expression: TypedExpr, search: &str, previous: &str) -> bool {
        let descriptor = parse_expression(expression);
        evaluate_typed_trigger(&descriptor, search, previous).expect("evaluate trigger")
    }

    #[test]
    fn closed_get_query_term_preserves_js_utf16_and_first_index_of() {
        let descriptor = parse_get_query_term_descriptor();
        assert_eq!(
            evaluate_typed_get_query_term(&descriptor, "prefix:latest:tail").expect("query term"),
            "latest:tail"
        );
        assert_eq!(
            evaluate_typed_get_query_term(&descriptor, "latest").expect("missing colon"),
            "latest"
        );
        assert_eq!(
            evaluate_typed_get_query_term(&descriptor, "a:latest:b:latest").expect("first colon"),
            "latest:b:latest"
        );
        assert_eq!(
            evaluate_typed_get_query_term(&descriptor, "😀:latest").expect("UTF-16 query term"),
            "latest"
        );
        assert_eq!(
            evaluate_typed_get_query_term(&descriptor, "😀").expect("false branch"),
            "😀"
        );
    }

    #[test]
    fn parses_and_evaluates_all_nine_trigger_shapes() {
        let cases = vec![
            (
                or(
                    strict_eq(length(arg(0)), integer(0)),
                    and(strict_eq(length(arg(1)), integer(0)), gt(length(arg(0)), integer(0))),
                ),
                vec![("", "x", true), ("x", "", true), ("x", "y", false)],
            ),
            (
                strict_ne(
                    length(TypedExpr::StringSplit {
                        value: Box::new(arg(0)),
                        separator: Box::new(string(",")),
                    }),
                    length(TypedExpr::StringSplit {
                        value: Box::new(arg(1)),
                        separator: Box::new(string(",")),
                    }),
                ),
                vec![("a,b", "a", true), ("a,b", "a,c", false)],
            ),
            (bool_value(true), vec![("", "", true), ("x", "y", true)]),
            (
                strict_ne(
                    TypedExpr::StringIndexOf {
                        value: Box::new(arg(0)),
                        needle: Box::new(string("/")),
                    },
                    TypedExpr::StringIndexOf {
                        value: Box::new(arg(1)),
                        needle: Box::new(string("/")),
                    },
                ),
                vec![("a/b", "ab", true), ("a/b", "c/d", false), ("é/", "x/", false)],
            ),
            (
                or(strict_eq(arg(0), string("-g")), strict_eq(arg(0), string("--global"))),
                vec![("-g", "", true), ("--global", "", true), ("-x", "", false)],
            ),
            (
                strict_ne(
                    length(TypedExpr::StringSplit {
                        value: Box::new(arg(0)),
                        separator: Box::new(string(":")),
                    }),
                    length(TypedExpr::StringSplit {
                        value: Box::new(arg(1)),
                        separator: Box::new(string(":")),
                    }),
                ),
                vec![("a:b", "a", true), ("a:b", "a:c", false)],
            ),
            (
                or(strict_eq(arg(0), string("-g")), strict_eq(arg(0), string("--global"))),
                vec![("--global", "", true), ("--local", "", false)],
            ),
            (bool_value(true), vec![("", "", true)]),
            (
                and(strict_eq(length(arg(0)), integer(0)), gt(length(arg(1)), integer(0))),
                vec![("", "x", true), ("x", "x", false), ("", "", false)],
            ),
        ];

        for (expression, inputs) in cases {
            for (search, previous, expected) in inputs {
                assert_eq!(evaluate(expression.clone(), search, previous), expected);
            }
        }
    }

    #[test]
    fn every_operation_and_type_has_runtime_semantics() {
        let includes = TypedExpr::StringIncludes {
            value: Box::new(arg(0)),
            needle: Box::new(string("bc")),
        };
        assert!(evaluate(includes, "abcd", ""));

        let index_of = strict_eq(
            TypedExpr::StringIndexOf {
                value: Box::new(arg(0)),
                needle: Box::new(string("bc")),
            },
            integer(1),
        );
        assert!(evaluate(index_of, "abcd", ""));

        let slice = strict_eq(
            TypedExpr::StringSlice {
                value: Box::new(arg(0)),
                start: Box::new(integer(2)),
            },
            string("cd"),
        );
        assert!(evaluate(slice, "abcd", ""));

        let split = strict_eq(
            length(TypedExpr::StringSplit {
                value: Box::new(arg(0)),
                separator: Box::new(string(",")),
            }),
            integer(3),
        );
        assert!(evaluate(split, "a,b,", ""));

        let array_includes = TypedExpr::ArrayIncludes {
            value: Box::new(TypedExpr::Array {
                items: vec![string("bash"), string("-c")],
            }),
            needle: Box::new(string("-c")),
        };
        assert!(evaluate(array_includes, "", ""));

        assert!(evaluate(strict_eq(string("x"), string("x")), "", ""));
        assert!(evaluate(strict_ne(string("x"), string("y")), "", ""));

        let lazy = TypedExpr::If {
            condition: Box::new(bool_value(false)),
            then_branch: Box::new(TypedExpr::StringSlice {
                value: Box::new(string("unreachable")),
                start: Box::new(integer(MAX_SAFE_INTEGER)),
            }),
            else_branch: Box::new(string("lazy")),
        };
        let value = evaluate_expr(
            &parse_expression(strict_eq(bool_value(true), bool_value(true))).expr,
            &[
                TypedValue::String(Utf16String::from_str("")),
                TypedValue::String(Utf16String::from_str("")),
            ],
        )
        .expect("baseline expression");
        assert_eq!(value, TypedValue::Bool(true));
        assert_eq!(
            evaluate_expr(
                &lazy,
                &[
                    TypedValue::String(Utf16String::from_str("")),
                    TypedValue::String(Utf16String::from_str("")),
                ],
            )
            .expect("lazy branch"),
            TypedValue::String(Utf16String::from_str("lazy"))
        );

        let last_index = strict_eq(
            TypedExpr::StringLastIndexOf {
                value: Box::new(arg(0)),
                needle: Box::new(string(":")),
            },
            integer(3),
        );
        assert!(evaluate(last_index, "a:b:c", ""));

        let starts = TypedExpr::StringStartsWith {
            value: Box::new(arg(0)),
            needle: Box::new(string("ab")),
        };
        assert!(evaluate(starts, "abcd", ""));

        let ends = TypedExpr::StringEndsWith {
            value: Box::new(arg(0)),
            needle: Box::new(string("cd")),
        };
        assert!(evaluate(ends, "abcd", ""));

        let trimmed = strict_eq(
            TypedExpr::StringTrim {
                value: Box::new(arg(0)),
            },
            string("hi"),
        );
        assert!(evaluate(trimmed, "  hi  ", ""));

        let replaced = strict_eq(
            TypedExpr::StringReplaceAll {
                value: Box::new(arg(0)),
                needle: Box::new(string("-")),
                replacement: Box::new(string("_")),
            },
            string("a_b_c"),
        );
        assert!(evaluate(replaced, "a-b-c", ""));

        let lower = strict_eq(
            TypedExpr::StringToLower {
                value: Box::new(arg(0)),
            },
            string("abc"),
        );
        assert!(evaluate(lower, "AbC", ""));

        let padded = strict_eq(
            TypedExpr::StringPadStart {
                value: Box::new(arg(0)),
                target: Box::new(integer(4)),
                pad: Box::new(string("0")),
            },
            string("0012"),
        );
        assert!(evaluate(padded, "12", ""));

        let repeated = strict_eq(
            TypedExpr::StringRepeat {
                value: Box::new(arg(0)),
                count: Box::new(integer(3)),
            },
            string("ababab"),
        );
        assert!(evaluate(repeated, "ab", ""));

        let concat = strict_eq(
            TypedExpr::StringConcat {
                parts: vec![arg(0), string("!")],
            },
            string("hi!"),
        );
        assert!(evaluate(concat, "hi", ""));

        let char_at = strict_eq(
            TypedExpr::StringCharAt {
                value: Box::new(arg(0)),
                index: Box::new(integer(1)),
            },
            string("b"),
        );
        assert!(evaluate(char_at, "abc", ""));

        let at = strict_eq(
            TypedExpr::StringAt {
                value: Box::new(arg(0)),
                index: Box::new(integer(-1)),
            },
            string("c"),
        );
        assert!(evaluate(at, "abc", ""));

        let substring = strict_eq(
            TypedExpr::StringSubstring {
                value: Box::new(arg(0)),
                start: Box::new(integer(1)),
                end: Box::new(integer(3)),
            },
            string("bc"),
        );
        assert!(evaluate(substring, "abcd", ""));

        assert!(evaluate(
            strict_eq(
                TypedExpr::Sub {
                    left: Box::new(length(arg(0))),
                    right: Box::new(integer(1)),
                },
                integer(0),
            ),
            "x",
            "",
        ));
        assert!(evaluate(
            TypedExpr::LessThan {
                left: Box::new(length(arg(0))),
                right: Box::new(length(arg(1))),
            },
            "a",
            "bb",
        ));
        assert!(evaluate(
            TypedExpr::Not {
                value: Box::new(TypedExpr::StringStartsWith {
                    value: Box::new(arg(0)),
                    needle: Box::new(string("x")),
                }),
            },
            "hello",
            "",
        ));

        let nullish = evaluate_expr(
            &TypedExpr::Nullish {
                left: Box::new(TypedExpr::Null),
                right: Box::new(string("fallback")),
            },
            &[
                TypedValue::String(Utf16String::from_str("")),
                TypedValue::String(Utf16String::from_str("")),
            ],
        )
        .expect("nullish");
        assert_eq!(nullish, TypedValue::String(Utf16String::from_str("fallback")));

        let overflow = evaluate_expr(
            &TypedExpr::Add {
                left: Box::new(integer(MAX_SAFE_INTEGER)),
                right: Box::new(integer(1)),
            },
            &[],
        );
        assert!(overflow.is_err());
    }

    #[test]
    fn malformed_schema_and_types_fail_closed() {
        let valid = trigger_descriptor(bool_value(true));
        let mut unknown_descriptor = valid.clone();
        unknown_descriptor["unexpected"] = json!(true);
        assert!(parse_typed_hook_ir(&unknown_descriptor).is_err());

        let mut unknown_operation = valid.clone();
        unknown_operation["expr"] = json!({"op": "future", "value": true});
        assert!(parse_typed_hook_ir(&unknown_operation).is_err());

        let mut generic_add = valid.clone();
        generic_add["expr"] = json!({
            "op": "add",
            "left": {"op": "integer", "value": 1},
            "right": {"op": "integer", "value": 1}
        });
        assert!(parse_typed_hook_ir(&generic_add).is_err());

        let mut extra_operation_field = valid.clone();
        extra_operation_field["expr"] = json!({"op": "bool", "value": true, "extra": 1});
        assert!(parse_typed_hook_ir(&extra_operation_field).is_err());

        let mut wrong_field = valid.clone();
        wrong_field["sourceField"] = json!("postProcess");
        assert!(parse_typed_hook_ir(&wrong_field).is_err());

        let mut wrong_params = valid.clone();
        wrong_params["params"] = json!([{"index": 0, "type": "bool"}, {"index": 1, "type": "string"}]);
        assert!(parse_typed_hook_ir(&wrong_params).is_err());

        let mut wrong_count = valid.clone();
        wrong_count["params"] = json!([{"index": 0, "type": "string"}]);
        assert!(parse_typed_hook_ir(&wrong_count).is_err());

        let mut out_of_range_arg = valid.clone();
        out_of_range_arg["expr"] = json!({"op": "arg", "index": 2});
        assert!(parse_typed_hook_ir(&out_of_range_arg).is_err());

        let mut wrong_type = valid.clone();
        wrong_type["expr"] = json!({
            "op": "string-includes",
            "value": {"op": "bool", "value": true},
            "needle": {"op": "string", "value": "x"}
        });
        assert!(parse_typed_hook_ir(&wrong_type).is_err());

        let mut unsafe_integer = valid.clone();
        unsafe_integer["expr"] = json!({"op": "integer", "value": 9_007_199_254_740_992_i64});
        assert!(parse_typed_hook_ir(&unsafe_integer).is_err());

        let mut array_identity = valid.clone();
        array_identity["expr"] = json!({
            "op": "strict-eq",
            "left": {"op": "array", "items": []},
            "right": {"op": "array", "items": []}
        });
        assert!(parse_typed_hook_ir(&array_identity).is_err());

        let mut query = get_query_term_descriptor();
        query["expr"]["then"]["needle"] = json!("not-an-expression");
        assert!(parse_typed_hook_ir(&query).is_err());
        let mut query_with_generic_add = get_query_term_descriptor();
        query_with_generic_add["expr"]["then"] = json!({
            "op": "string-slice",
            "value": {"op": "arg", "index": 0},
            "start": {
                "op": "add",
                "left": {"op": "string-index-of", "value": {"op": "arg", "index": 0}, "needle": {"op": "string", "value": ":"}},
                "right": {"op": "integer", "value": 1}
            }
        });
        let parsed =
            parse_typed_hook_ir(&query_with_generic_add).expect("add + string-slice is the v2 getQueryTerm shape");
        assert_eq!(
            evaluate_typed_get_query_term(&parsed, "nodejs:latest").expect("query term"),
            "latest"
        );
    }

    #[test]
    fn node_and_depth_limits_match_the_compiler() {
        let mut expression = bool_value(true);
        for _ in 0..32 {
            expression = or(bool_value(false), expression);
        }
        let descriptor = trigger_descriptor(expression);
        assert!(parse_typed_hook_ir(&descriptor).is_err());

        let mut expression = bool_value(true);
        for _ in 0..25 {
            expression = TypedExpr::If {
                condition: Box::new(bool_value(true)),
                then_branch: Box::new(expression),
                else_branch: Box::new(bool_value(false)),
            };
        }
        let descriptor = trigger_descriptor(expression);
        assert!(parse_typed_hook_ir(&descriptor).is_err());
    }

    #[test]
    fn utf16_length_index_slice_and_empty_split_match_javascript() {
        let length = strict_eq(length(arg(0)), integer(2));
        assert!(evaluate(length, "😀", ""));

        let index = strict_eq(
            TypedExpr::StringIndexOf {
                value: Box::new(arg(0)),
                needle: Box::new(string("😀")),
            },
            integer(1),
        );
        assert!(evaluate(index, "a😀", ""));

        let source = Utf16String::from_str("a😀b");
        let low_surrogate = TypedExpr::StringSlice {
            value: Box::new(string("a😀b")),
            start: Box::new(integer(2)),
        };
        let result = evaluate_expr(
            &low_surrogate,
            &[
                TypedValue::String(source.clone()),
                TypedValue::String(Utf16String::from_str("")),
            ],
        )
        .expect("slice");
        assert_eq!(
            result,
            TypedValue::String(Utf16String::from_units(vec![0xde00, b'b' as u16]))
        );

        let split = TypedExpr::StringSplit {
            value: Box::new(string("😀")),
            separator: Box::new(string("")),
        };
        let result = evaluate_expr(
            &split,
            &[
                TypedValue::String(source),
                TypedValue::String(Utf16String::from_str("")),
            ],
        )
        .expect("empty split");
        assert_eq!(
            result,
            TypedValue::StringArray(vec![
                Utf16String::from_units(vec![0xd83d]),
                Utf16String::from_units(vec![0xde00]),
            ])
        );

        let empty_nonempty_split = TypedExpr::StringSplit {
            value: Box::new(string("")),
            separator: Box::new(string(",")),
        };
        let result = evaluate_expr(
            &empty_nonempty_split,
            &[
                TypedValue::String(Utf16String::from_str("")),
                TypedValue::String(Utf16String::from_str("")),
            ],
        )
        .expect("empty source split");
        assert_eq!(
            result,
            TypedValue::StringArray(vec![Utf16String::from_units(Vec::<u16>::new())])
        );
    }

    #[test]
    fn logical_operators_are_lazy() {
        let unreachable = gt(integer(1), integer(0));
        let and_expression = and(bool_value(false), unreachable.clone());
        let or_expression = or(bool_value(true), unreachable);
        assert_eq!(
            evaluate_expr(
                &and_expression,
                &[
                    TypedValue::String(Utf16String::from_str("")),
                    TypedValue::String(Utf16String::from_str("")),
                ],
            )
            .expect("lazy and"),
            TypedValue::Bool(false)
        );
        assert_eq!(
            evaluate_expr(
                &or_expression,
                &[
                    TypedValue::String(Utf16String::from_str("")),
                    TypedValue::String(Utf16String::from_str("")),
                ],
            )
            .expect("lazy or"),
            TypedValue::Bool(true)
        );
    }

    fn catalog_entry(expression: TypedExpr) -> JsonValue {
        json!({
            "module": "typed-hooks.js",
            "moduleSha256": "a".repeat(64),
            "path": "root.args[0].generators[0].trigger",
            "sourceField": SOURCE_FIELD,
            "functionBodySha256": "b".repeat(64),
            "descriptor": trigger_descriptor(expression),
        })
    }

    fn catalog_value(hooks: serde_json::Map<String, JsonValue>) -> JsonValue {
        json!({
            "version": IR_VERSION,
            "kind": CATALOG_KIND,
            "contracts": {
                "trigger": {
                    "irVersion": IR_VERSION,
                    "params": ["string", "string"],
                    "resultType": "bool",
                }
            },
            "hooks": hooks,
        })
    }

    #[test]
    fn parses_deterministic_catalog_and_evaluates_by_id() {
        let mut hooks = serde_json::Map::new();
        hooks.insert(
            "é/hooks#one".to_owned(),
            catalog_entry(strict_eq(arg(0), string("ready"))),
        );
        hooks.insert("z/hooks#two".to_owned(), catalog_entry(bool_value(true)));
        let value = catalog_value(hooks);
        let catalog = parse_typed_hook_catalog(&value).expect("valid typed hook catalog");
        let ids: Vec<_> = catalog.hooks.keys().map(String::as_str).collect();
        assert_eq!(ids, ["z/hooks#two", "é/hooks#one"]);

        let entry = lookup_typed_hook(&catalog, "é/hooks#one").expect("catalog entry");
        assert_eq!(entry.module, "typed-hooks.js");
        assert_eq!(entry.path, "root.args[0].generators[0].trigger");
        assert!(evaluate_typed_hook_by_id(&catalog, "é/hooks#one", "ready", "").expect("evaluate"));
        assert!(!evaluate_typed_hook_by_id(&catalog, "é/hooks#one", "waiting", "").expect("evaluate"));
        assert!(evaluate_typed_hook_by_id(&catalog, "z/hooks#two", "", "").expect("evaluate"));
        assert!(lookup_typed_hook(&catalog, "missing").is_none());
        assert!(evaluate_typed_hook_by_id(&catalog, "missing", "", "").is_err());

        let bytes = serde_json::to_vec(&value).expect("catalog JSON");
        let from_bytes = parse_typed_hook_catalog_bytes(&bytes).expect("catalog bytes");
        assert_eq!(from_bytes, catalog);

        let descriptor = parse_expression(bool_value(true));
        let descriptor_bytes = serde_json::to_vec(&descriptor).expect("descriptor JSON");
        assert_eq!(
            parse_typed_hook_ir_bytes(&descriptor_bytes).expect("descriptor bytes"),
            descriptor
        );
    }

    #[test]
    fn catalog_missing_tampered_unknown_and_orphan_shapes_fail_closed() {
        let mut valid = catalog_value(serde_json::Map::from_iter([(
            "hook#one".to_owned(),
            catalog_entry(bool_value(true)),
        )]));

        let mut missing = valid.clone();
        missing["hooks"]["hook#one"]
            .as_object_mut()
            .expect("entry object")
            .remove("descriptor");
        assert!(parse_typed_hook_catalog(&missing).is_err());

        let mut unknown_root = valid.clone();
        unknown_root["extra"] = json!(true);
        assert!(parse_typed_hook_catalog(&unknown_root).is_err());

        let mut unknown_entry = valid.clone();
        unknown_entry["hooks"]["hook#one"]["extra"] = json!(true);
        assert!(parse_typed_hook_catalog(&unknown_entry).is_err());

        let mut unknown_contract = valid.clone();
        unknown_contract["contracts"]["trigger"]["extra"] = json!(true);
        assert!(parse_typed_hook_catalog(&unknown_contract).is_err());

        let mut unknown_descriptor = valid.clone();
        unknown_descriptor["hooks"]["hook#one"]["descriptor"]["expr"]["extra"] = json!(true);
        assert!(parse_typed_hook_catalog(&unknown_descriptor).is_err());

        let mut tampered_module = valid.clone();
        tampered_module["hooks"]["hook#one"]["module"] = json!("source-modules/typed-hooks.js");
        assert!(parse_typed_hook_catalog(&tampered_module).is_err());

        let mut tampered_sha = valid.clone();
        tampered_sha["hooks"]["hook#one"]["moduleSha256"] = json!("A".repeat(64));
        assert!(parse_typed_hook_catalog(&tampered_sha).is_err());

        let mut tampered_path = valid.clone();
        tampered_path["hooks"]["hook#one"]["path"] = json!("root.args[0].generators[custom]");
        assert!(parse_typed_hook_catalog(&tampered_path).is_err());

        let mut tampered_field = valid.clone();
        tampered_field["hooks"]["hook#one"]["sourceField"] = json!("custom");
        assert!(parse_typed_hook_catalog(&tampered_field).is_err());

        let mut tampered_contract = valid.clone();
        tampered_contract["contracts"]["trigger"]["params"] = json!(["string"]);
        assert!(parse_typed_hook_catalog(&tampered_contract).is_err());

        let mut tampered_descriptor = valid.clone();
        tampered_descriptor["hooks"]["hook#one"]["descriptor"]["resultType"] = json!("string");
        assert!(parse_typed_hook_catalog(&tampered_descriptor).is_err());

        valid["hooks"]["hook#one"]["functionBodySha256"] = json!("0".repeat(63));
        assert!(parse_typed_hook_catalog(&valid).is_err());

        let unpaired_surrogate = br#"{"version":1,"kind":"typed-hook-expression","sourceField":"trigger","resultType":"bool","params":[{"index":0,"type":"string"},{"index":1,"type":"string"}],"expr":{"op":"strict-eq","left":{"op":"string","value":"\ud800"},"right":{"op":"string","value":"x"}}}"#;
        assert!(parse_typed_hook_ir_bytes(unpaired_surrogate).is_err());
    }

    #[test]
    fn catalog_path_id_module_and_count_limits_are_strict() {
        let mut hooks = serde_json::Map::new();
        hooks.insert("a/path#unicode-✓".to_owned(), catalog_entry(bool_value(true)));
        let valid = catalog_value(hooks);
        assert!(parse_typed_hook_catalog(&valid).is_ok());

        for invalid_id in ["", "bad\\id", "bad\0id", &"x".repeat(MAX_HOOK_ID_BYTES + 1)] {
            let mut value = catalog_value(serde_json::Map::from_iter([(
                invalid_id.to_owned(),
                catalog_entry(bool_value(true)),
            )]));
            assert!(parse_typed_hook_catalog(&value).is_err(), "id {invalid_id:?}");
            value["hooks"] = json!({});
        }

        for invalid_module in [
            "",
            ".js",
            "..js",
            "source-modules/x.js",
            "x\\y.js",
            &format!("{}.js", "x".repeat(MAX_MODULE_BASENAME_BYTES)),
        ] {
            let mut entry = catalog_entry(bool_value(true));
            entry["module"] = json!(invalid_module);
            let value = catalog_value(serde_json::Map::from_iter([("hook".to_owned(), entry)]));
            assert!(parse_typed_hook_catalog(&value).is_err(), "module {invalid_module:?}");
        }

        for invalid_path in [
            "root..args",
            "root.args[]",
            "root.args[0].bad-name",
            "root.args[0].generators[custom]",
        ] {
            let mut entry = catalog_entry(bool_value(true));
            entry["path"] = json!(invalid_path);
            let value = catalog_value(serde_json::Map::from_iter([("hook".to_owned(), entry)]));
            assert!(parse_typed_hook_catalog(&value).is_err(), "path {invalid_path:?}");
        }

        let mut oversized_path_entry = catalog_entry(bool_value(true));
        oversized_path_entry["path"] = json!(format!("root.{}", "a".repeat(MAX_HOOK_PATH_BYTES)));
        let oversized_path = catalog_value(serde_json::Map::from_iter([("hook".to_owned(), oversized_path_entry)]));
        assert!(parse_typed_hook_catalog(&oversized_path).is_err());

        let mut too_many = BTreeMap::new();
        for index in 0..=MAX_CATALOG_HOOKS {
            too_many.insert(
                format!("hook-{index}"),
                TypedHookCatalogEntry {
                    module: "typed-hooks.js".to_owned(),
                    module_sha256: "a".repeat(64),
                    path: "root.args[0]".to_owned(),
                    source_field: SOURCE_FIELD.to_owned(),
                    function_body_sha256: "b".repeat(64),
                    descriptor: parse_expression(bool_value(true)),
                },
            );
        }
        let catalog = TypedHookCatalog {
            version: IR_VERSION,
            kind: CATALOG_KIND.to_owned(),
            contracts: TypedHookContracts {
                trigger: TypedHookContract {
                    ir_version: IR_VERSION,
                    params: vec![TypedValueType::String, TypedValueType::String],
                    result_type: TypedValueType::Bool,
                },
                get_query_term: None,
            },
            hooks: too_many,
        };
        assert!(validate_typed_hook_catalog(&catalog).is_err());
    }

    fn typed_trigger_reference_value() -> JsonValue {
        serde_json::from_slice(TYPED_TRIGGER_REFERENCE).expect("reference JSON")
    }

    #[test]
    fn checked_in_typed_trigger_reference_matches_native_evaluator() {
        let baseline =
            parse_typed_trigger_reference_bytes(TYPED_TRIGGER_REFERENCE).expect("checked-in typed trigger reference");
        assert_eq!(baseline.catalog.hooks.len(), baseline.expected.len());
        assert!(!baseline.cases.is_empty());

        for (hook_id, expected_values) in &baseline.expected {
            let entry = baseline.catalog.hooks.get(hook_id).expect("catalog hook");
            assert_eq!(expected_values.len(), baseline.cases.len());
            for (index, case) in baseline.cases.iter().enumerate() {
                let actual = evaluate_typed_trigger(&entry.descriptor, &case.args[0], &case.args[1])
                    .expect("native typed trigger evaluation");
                assert_eq!(actual, expected_values[index], "hook {hook_id}, case {}", case.id);
            }
        }
    }

    #[test]
    fn typed_trigger_reference_schema_and_provenance_fail_closed() {
        let mut unknown = typed_trigger_reference_value();
        unknown["unexpected"] = json!(true);
        assert!(parse_typed_trigger_reference(&unknown).is_err());

        for (field, invalid) in [("version", json!(2)), ("kind", json!("future-reference"))] {
            let mut value = typed_trigger_reference_value();
            value[field] = invalid;
            assert!(parse_typed_trigger_reference(&value).is_err(), "field {field}");
        }

        for field in [
            "generatorSha256",
            "harnessSha256",
            "pairSha256",
            "hookManifestSha256",
            "sidecarSha256",
        ] {
            let mut value = typed_trigger_reference_value();
            value[field] = json!("A".repeat(64));
            assert!(parse_typed_trigger_reference(&value).is_err(), "field {field}");
        }

        let mut wrong_sidecar_digest = typed_trigger_reference_value();
        wrong_sidecar_digest["sidecarSha256"] = json!("0".repeat(64));
        assert!(parse_typed_trigger_reference(&wrong_sidecar_digest).is_err());

        let mut wrong_case_field = typed_trigger_reference_value();
        wrong_case_field["cases"][0]["extra"] = json!(true);
        assert!(parse_typed_trigger_reference(&wrong_case_field).is_err());

        let mut wrong_args_count = typed_trigger_reference_value();
        wrong_args_count["cases"][0]["args"] = json!([""]);
        assert!(parse_typed_trigger_reference(&wrong_args_count).is_err());

        let mut wrong_args_type = typed_trigger_reference_value();
        wrong_args_type["cases"][0]["args"] = json!(["", false]);
        assert!(parse_typed_trigger_reference(&wrong_args_type).is_err());
    }

    #[test]
    fn typed_trigger_reference_case_ids_and_limits_are_strict() {
        let mut empty_id = typed_trigger_reference_value();
        empty_id["cases"][0]["id"] = json!("");
        assert!(parse_typed_trigger_reference(&empty_id).is_err());

        let mut duplicate_id = typed_trigger_reference_value();
        let first_id = duplicate_id["cases"][0]["id"].clone();
        duplicate_id["cases"][1]["id"] = first_id;
        assert!(parse_typed_trigger_reference(&duplicate_id).is_err());

        for dangerous_id in ["case/name", "case\\name", "case\0name"] {
            let mut value = typed_trigger_reference_value();
            value["cases"][0]["id"] = json!(dangerous_id);
            assert!(parse_typed_trigger_reference(&value).is_err(), "id {dangerous_id:?}");
        }

        let mut too_many_cases = typed_trigger_reference_value();
        let case = too_many_cases["cases"][0].clone();
        too_many_cases["cases"] = JsonValue::Array(vec![case; MAX_REFERENCE_CASES + 1]);
        assert!(parse_typed_trigger_reference(&too_many_cases).is_err());

        let mut oversized_corpus = typed_trigger_reference_value();
        oversized_corpus["cases"][0]["args"][0] = json!("x".repeat(MAX_REFERENCE_CORPUS_BYTES));
        assert!(parse_typed_trigger_reference(&oversized_corpus).is_err());

        let mut marker = typed_trigger_reference_value();
        marker["cases"][0]["args"][0] = json!(REFERENCE_EXEC_MARKER);
        assert!(parse_typed_trigger_reference(&marker).is_err());
    }

    #[test]
    fn typed_trigger_reference_expected_ids_lengths_and_types_are_strict() {
        let mut missing_expected = typed_trigger_reference_value();
        let first_id = missing_expected["expected"]
            .as_object_mut()
            .expect("expected map")
            .keys()
            .next()
            .cloned()
            .expect("expected hook");
        missing_expected["expected"]
            .as_object_mut()
            .expect("expected map")
            .remove(&first_id);
        assert!(parse_typed_trigger_reference(&missing_expected).is_err());

        let mut extra_expected = typed_trigger_reference_value();
        extra_expected["expected"]["not-a-catalog-hook"] = json!([]);
        assert!(parse_typed_trigger_reference(&extra_expected).is_err());

        let first_id = typed_trigger_reference_value()["expected"]
            .as_object()
            .expect("expected map")
            .keys()
            .next()
            .cloned()
            .expect("expected hook");

        let mut wrong_length = typed_trigger_reference_value();
        wrong_length["expected"][&first_id] = json!([]);
        assert!(parse_typed_trigger_reference(&wrong_length).is_err());

        let mut wrong_type = typed_trigger_reference_value();
        wrong_type["expected"][&first_id] = json!([true, "not-a-bool"]);
        assert!(parse_typed_trigger_reference(&wrong_type).is_err());
    }

    fn typed_get_query_term_reference_value() -> JsonValue {
        serde_json::from_slice(TYPED_GET_QUERY_TERM_REFERENCE).expect("getQueryTerm reference JSON")
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct ReferenceFileIdentity {
        length: u64,
        #[cfg(unix)]
        device: u64,
        #[cfg(unix)]
        inode: u64,
        #[cfg(unix)]
        mode: u32,
    }

    fn reference_file_identity(metadata: &Metadata) -> ReferenceFileIdentity {
        ReferenceFileIdentity {
            length: metadata.len(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(unix)]
            mode: metadata.mode(),
        }
    }

    fn safe_repo_relative_path(value: &str) -> bool {
        !value.is_empty()
            && !value.contains(['\\', '\0'])
            && Path::new(value)
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
            && value
                .split('/')
                .all(|component| !component.is_empty() && component != "." && component != "..")
    }

    fn reject_symlink_ancestors(path: &Path) -> Result<(), String> {
        let mut current = PathBuf::new();
        for component in path.components() {
            match component {
                Component::Prefix(prefix) => current.push(prefix.as_os_str()),
                Component::RootDir => current.push(Path::new("/")),
                Component::CurDir => {},
                Component::ParentDir => current.push(".."),
                Component::Normal(name) => current.push(name),
            }
            let metadata =
                fs::symlink_metadata(&current).map_err(|error| format!("inspect {}: {error}", current.display()))?;
            if metadata.file_type().is_symlink() {
                return Err(format!("{} contains a symbolic-link ancestor", path.display()));
            }
        }
        Ok(())
    }

    /// Read one checked-in provenance file without following a symlink or
    /// accepting a path outside the repository.  The descriptor only carries
    /// relative names; this helper is deliberately test-only and never
    /// executes or parses JavaScript.
    fn read_reference_file(repo_root: &Path, base: &str, relative: &str) -> Result<Vec<u8>, String> {
        if !safe_repo_relative_path(base) || !safe_repo_relative_path(relative) {
            return Err(format!(
                "unsafe repository-relative provenance path {base:?}/{relative:?}"
            ));
        }
        reject_symlink_ancestors(repo_root)?;
        let root_metadata = fs::symlink_metadata(repo_root)
            .map_err(|error| format!("inspect repository root {}: {error}", repo_root.display()))?;
        if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
            return Err(format!(
                "repository root is not a real directory: {}",
                repo_root.display()
            ));
        }
        let canonical_root = fs::canonicalize(repo_root)
            .map_err(|error| format!("canonicalize repository root {}: {error}", repo_root.display()))?;
        let mut path = repo_root.to_path_buf();
        for component in base.split('/').chain(relative.split('/')) {
            path.push(component);
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| format!("inspect provenance file {}: {error}", path.display()))?;
            if metadata.file_type().is_symlink() {
                return Err(format!("provenance file {} is a symbolic link", path.display()));
            }
        }
        let before = fs::symlink_metadata(&path)
            .map_err(|error| format!("inspect provenance file {}: {error}", path.display()))?;
        if before.file_type().is_symlink() || !before.is_file() {
            return Err(format!("provenance path is not a regular file: {}", path.display()));
        }
        let canonical_file = fs::canonicalize(&path)
            .map_err(|error| format!("canonicalize provenance file {}: {error}", path.display()))?;
        if canonical_file.strip_prefix(&canonical_root).is_err() {
            return Err(format!("provenance file escapes repository root: {}", path.display()));
        }
        let before_identity = reference_file_identity(&before);

        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NOFOLLOW);
        let mut file = options
            .open(&path)
            .map_err(|error| format!("open provenance file {}: {error}", path.display()))?;
        let opened = file
            .metadata()
            .map_err(|error| format!("stat provenance file {}: {error}", path.display()))?;
        if opened.file_type().is_symlink() || !opened.is_file() || reference_file_identity(&opened) != before_identity {
            return Err(format!("provenance file changed while opening: {}", path.display()));
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|error| format!("read provenance file {}: {error}", path.display()))?;
        let after_open = file
            .metadata()
            .map_err(|error| format!("restat provenance file {}: {error}", path.display()))?;
        if reference_file_identity(&after_open) != before_identity {
            return Err(format!("provenance file drifted while reading: {}", path.display()));
        }
        let after = fs::symlink_metadata(&path)
            .map_err(|error| format!("restat provenance path {}: {error}", path.display()))?;
        if after.file_type().is_symlink() || reference_file_identity(&after) != before_identity {
            return Err(format!("provenance path drifted while reading: {}", path.display()));
        }
        Ok(bytes)
    }

    fn expected_reference_hook_file(id: &str) -> String {
        let mut name = String::with_capacity(id.len() + 4);
        for character in id.chars() {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                name.push(character);
            } else {
                name.push('_');
            }
        }
        name.push_str(".js");
        name
    }

    fn closure_function_body_sha256(bytes: &[u8]) -> Result<String, String> {
        let text = std::str::from_utf8(bytes).map_err(|error| format!("hook artifact is not UTF-8: {error}"))?;
        let text = text.trim();
        let body = text
            .strip_prefix("export default ")
            .ok_or_else(|| "hook artifact has no standalone export default body".to_owned())?;
        let body = body.strip_suffix(';').unwrap_or(body).trim();
        Ok(sha256_hex(body.as_bytes()))
    }

    fn json_string<'a>(value: &'a JsonValue, field: &str) -> Result<&'a str, String> {
        value
            .get(field)
            .and_then(JsonValue::as_str)
            .ok_or_else(|| format!("manifest field {field:?} is not a string"))
    }

    fn assert_current_asdf_get_query_term_provenance(
        baseline: &TypedGetQueryTermReferenceBaseline,
    ) -> Result<(), String> {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let source_root = repo_root.join("bundle/specs");
        let ir_root = repo_root.join("bundle/specs-ir");
        crate::spec_pair::verify_pair(Some(&source_root), &ir_root)
            .map_err(|error| format!("source/IR pair verification failed: {error:#}"))?;

        let marker = read_reference_file(&repo_root, "bundle/specs-ir", ".spec-pair.json")?;
        let marker: JsonValue =
            serde_json::from_slice(&marker).map_err(|error| format!("pair marker JSON is invalid: {error}"))?;
        let marker_pair_sha256 = json_string(&marker, "pairSha256")?;
        if marker_pair_sha256 != baseline.pair_sha256 {
            return Err("pairSha256 differs from the checked-in pair marker".to_owned());
        }

        let generator = read_reference_file(&repo_root, "scripts", "compile-spec-ir.mjs")?;
        if sha256_hex(&generator) != baseline.generator_sha256 {
            return Err("generatorSha256 differs from scripts/compile-spec-ir.mjs".to_owned());
        }

        let mut harness_records = Vec::with_capacity(ASDF_REFERENCE_HARNESS_FILES.len());
        for file in ASDF_REFERENCE_HARNESS_FILES {
            let (base, relative) = file
                .rsplit_once('/')
                .ok_or_else(|| format!("harness path has no repository directory: {file}"))?;
            let bytes = read_reference_file(&repo_root, base, relative)?;
            harness_records.push(((*file).to_owned(), sha256_hex(&bytes)));
        }
        harness_records.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
        let mut harness_canonical = String::new();
        for (file, digest) in harness_records {
            harness_canonical.push_str(&file);
            harness_canonical.push('\0');
            harness_canonical.push_str(&digest);
            harness_canonical.push('\n');
        }
        if sha256_hex(harness_canonical.as_bytes()) != baseline.harness_sha256 {
            return Err("harnessSha256 differs from the current reference harness".to_owned());
        }

        let hook_manifest = read_reference_file(&repo_root, "bundle/specs-ir", "hook-modules.json")?;
        if sha256_hex(&hook_manifest) != baseline.hook_manifest_sha256 {
            return Err("hookManifestSha256 differs from hook-modules.json".to_owned());
        }
        let hook_manifest: JsonValue = serde_json::from_slice(&hook_manifest)
            .map_err(|error| format!("hook-modules.json is invalid JSON: {error}"))?;
        let manifest_hooks = hook_manifest
            .get("hooks")
            .and_then(JsonValue::as_object)
            .ok_or_else(|| "hook-modules.json has no hooks object".to_owned())?;
        let manifest_modules = hook_manifest
            .get("modules")
            .and_then(JsonValue::as_object)
            .ok_or_else(|| "hook-modules.json has no modules object".to_owned())?;

        for id in ASDF_GET_QUERY_TERM_CANDIDATE_IDS {
            let candidate = baseline
                .candidates
                .get(id)
                .ok_or_else(|| format!("baseline candidate {id:?} is missing"))?;
            if candidate.hook_file != expected_reference_hook_file(id) {
                return Err(format!("candidate {id:?} has an unexpected hook artifact name"));
            }
            let source = read_reference_file(&repo_root, "bundle/specs", &candidate.source)?;
            if sha256_hex(&source) != candidate.source_sha256 {
                return Err(format!("candidate {id:?} source bytes drifted"));
            }
            let ir = read_reference_file(&repo_root, "bundle/specs-ir", &candidate.ir)?;
            if sha256_hex(&ir) != candidate.ir_sha256 {
                return Err(format!("candidate {id:?} IR bytes drifted"));
            }
            let module = read_reference_file(&repo_root, "bundle/specs-ir/source-modules", &candidate.module)?;
            if sha256_hex(&module) != candidate.module_sha256 {
                return Err(format!("candidate {id:?} closure module bytes drifted"));
            }
            let hook = read_reference_file(&repo_root, "bundle/specs-ir/hooks", &candidate.hook_file)?;
            if sha256_hex(&hook) != candidate.hook_file_sha256 {
                return Err(format!("candidate {id:?} hook artifact bytes drifted"));
            }
            if closure_function_body_sha256(&hook)? != candidate.function_body_sha256 {
                return Err(format!("candidate {id:?} function body bytes drifted"));
            }

            let descriptor = manifest_hooks
                .get(id)
                .ok_or_else(|| format!("hook manifest entry {id:?} is missing"))?;
            for (field, expected) in [
                ("module", candidate.module.as_str()),
                ("moduleSha256", candidate.module_sha256.as_str()),
                ("path", candidate.path.as_str()),
                ("sourceField", candidate.source_field.as_str()),
                ("functionBodySha256", candidate.function_body_sha256.as_str()),
            ] {
                if json_string(descriptor, field)? != expected {
                    return Err(format!("candidate {id:?} differs from hook manifest {field}"));
                }
            }
            let module_metadata = manifest_modules
                .get(candidate.module.as_str())
                .ok_or_else(|| format!("module manifest entry {:?} is missing", candidate.module))?;
            for (field, expected) in [
                ("source", candidate.source.as_str()),
                ("sourceSha256", candidate.source_sha256.as_str()),
                ("moduleSha256", candidate.module_sha256.as_str()),
            ] {
                if json_string(module_metadata, field)? != expected {
                    return Err(format!("candidate {id:?} differs from module manifest {field}"));
                }
            }
        }
        Ok(())
    }

    #[test]
    fn checked_in_asdf_get_query_term_reference_matches_native_evaluator() {
        let baseline = parse_typed_get_query_term_reference_bytes(TYPED_GET_QUERY_TERM_REFERENCE)
            .expect("checked-in getQueryTerm reference");
        assert_current_asdf_get_query_term_provenance(&baseline).expect("current getQueryTerm provenance");
        assert_eq!(baseline.candidates.len(), 2);
        assert_eq!(baseline.candidates.len(), baseline.expected.len());
        for (hook_id, expected_values) in &baseline.expected {
            let candidate = baseline.candidates.get(hook_id).expect("candidate");
            assert_eq!(expected_values.len(), baseline.cases.len());
            for (index, case) in baseline.cases.iter().enumerate() {
                let actual = evaluate_typed_get_query_term(&candidate.descriptor, &case.args[0])
                    .expect("native getQueryTerm evaluation");
                assert_eq!(actual, expected_values[index], "hook {hook_id}, case {}", case.id);
            }
        }
    }

    #[test]
    fn asdf_reference_file_reader_rejects_escape_missing_and_symlink_paths() {
        let root = tempfile::tempdir().expect("reference root");
        let root_path = fs::canonicalize(root.path()).expect("canonical reference root");
        let base = root_path.join("base");
        fs::create_dir(&base).expect("reference base");
        fs::write(base.join("ok.txt"), b"stable").expect("reference file");
        assert_eq!(
            read_reference_file(&root_path, "base", "ok.txt").expect("safe reference file"),
            b"stable"
        );
        assert!(read_reference_file(&root_path, "base", "../ok.txt").is_err());
        assert!(read_reference_file(&root_path, "base", "missing.txt").is_err());

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(base.join("ok.txt"), base.join("link.txt")).expect("file symlink");
            assert!(read_reference_file(&root_path, "base", "link.txt").is_err());
            std::os::unix::fs::symlink(&base, root_path.join("linked-base")).expect("directory symlink");
            assert!(read_reference_file(&root_path, "linked-base", "ok.txt").is_err());
        }
    }

    #[test]
    fn asdf_reference_provenance_rejects_baseline_hash_drift() {
        let mut baseline = parse_typed_get_query_term_reference_bytes(TYPED_GET_QUERY_TERM_REFERENCE)
            .expect("checked-in getQueryTerm reference");
        baseline
            .candidates
            .get_mut("asdf#getQueryTerm#7")
            .expect("asdf candidate")
            .module_sha256 = "0".repeat(64);
        let error =
            assert_current_asdf_get_query_term_provenance(&baseline).expect_err("module hash drift must fail closed");
        assert!(error.contains("closure module bytes drifted"), "{error}");
    }

    #[test]
    fn asdf_get_query_term_reference_is_closed_and_fail_closed() {
        let mut unknown = typed_get_query_term_reference_value();
        unknown["unexpected"] = json!(true);
        assert!(parse_typed_get_query_term_reference(&unknown).is_err());

        for (field, invalid) in [("version", json!(2)), ("kind", json!("future-reference"))] {
            let mut value = typed_get_query_term_reference_value();
            value[field] = invalid;
            assert!(parse_typed_get_query_term_reference(&value).is_err(), "field {field}");
        }

        let mut extra_candidate = typed_get_query_term_reference_value();
        extra_candidate["candidates"]["not-asdf"] = extra_candidate["candidates"]["asdf#getQueryTerm#7"].clone();
        assert!(parse_typed_get_query_term_reference(&extra_candidate).is_err());

        let mut wrong_source = typed_get_query_term_reference_value();
        wrong_source["candidates"]["asdf#getQueryTerm#7"]["sourceField"] = json!("trigger");
        assert!(parse_typed_get_query_term_reference(&wrong_source).is_err());

        let mut unknown_then = typed_get_query_term_reference_value();
        unknown_then["candidates"]["asdf#getQueryTerm#7"]["descriptor"]["expr"]["then"] = json!({
            "op": "future-slice",
            "value": {"op": "arg", "index": 0}
        });
        assert!(parse_typed_get_query_term_reference(&unknown_then).is_err());

        let mut wrong_expected = typed_get_query_term_reference_value();
        wrong_expected["expected"]["asdf#getQueryTerm#7"] = json!(["only-one"]);
        assert!(parse_typed_get_query_term_reference(&wrong_expected).is_err());
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct TypedIrV2OpsGolden {
        version: u64,
        kind: String,
        cases: Vec<TypedIrV2OpsCase>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct TypedIrV2OpsCase {
        id: String,
        #[serde(rename = "sourceField")]
        source_field: String,
        #[allow(dead_code)]
        body: Option<String>,
        args: Vec<JsonValue>,
        expected: JsonValue,
        descriptor: JsonValue,
    }

    #[test]
    fn typed_ir_v2_ops_golden_matches_javascript_evaluator() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/native-hooks/typed-ir-v2-ops.json");
        let bytes = fs::read(&path).expect("typed-ir-v2-ops golden");
        let golden: TypedIrV2OpsGolden = serde_json::from_slice(&bytes).expect("typed-ir-v2-ops schema");
        assert_eq!(golden.version, 1);
        assert_eq!(golden.kind, "typed-ir-v2-ops");
        assert!(
            golden.cases.len() >= 30,
            "v2 op golden must cover the new string/numeric/nullish ops, got {}",
            golden.cases.len()
        );
        for case in &golden.cases {
            let descriptor = parse_typed_hook_ir(&case.descriptor)
                .unwrap_or_else(|error| panic!("case {} failed to parse: {error}", case.id));
            assert_eq!(descriptor.source_field, case.source_field, "{}", case.id);
            let actual = evaluate_typed_hook_json(&descriptor, &case.args)
                .unwrap_or_else(|error| panic!("case {} failed to evaluate: {error}", case.id));
            assert_eq!(actual, case.expected, "{}", case.id);
        }
    }

    #[test]
    fn script_and_filter_contracts_parse_without_entering_production_sidecars() {
        let script = json!({
            "version": IR_VERSION,
            "kind": IR_KIND,
            "sourceField": "script",
            "resultType": "string-array",
            "params": [{"index": 0, "type": "string-array"}],
            "expr": {
                "op": "array",
                "items": [
                    {"op": "string", "value": "echo"},
                    {"op": "string", "value": "-n"}
                ]
            }
        });
        let descriptor = parse_typed_hook_ir(&script).expect("script descriptor");
        assert_eq!(
            evaluate_typed_hook_json(&descriptor, &[json!([])]).expect("script eval"),
            json!(["echo", "-n"])
        );

        let filter = json!({
            "version": IR_VERSION,
            "kind": IR_KIND,
            "sourceField": "filterTemplateSuggestions",
            "resultType": "suggestion-array",
            "params": [{"index": 0, "type": "suggestion-array"}],
            "expr": {"op": "arg", "index": 0}
        });
        assert!(parse_typed_hook_ir(&filter).is_ok());
        assert!(
            evaluate_typed_hook_json(
                &parse_typed_hook_ir(&filter).expect("filter"),
                &[json!([{"name": "a"}])]
            )
            .is_err()
        );
    }
}
