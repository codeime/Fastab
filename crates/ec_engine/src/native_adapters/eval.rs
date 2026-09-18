//! Shared evaluation helpers for named native adapters.
//!
//! Suggestion normalisation copies `scripts/hook-baseline-lib.mjs`
//! `normalizeSuggestion` so baseline parity compares the same JSON the T1.2
//! collector wrote. JS throws become `{kind:"error", value: errorClass}`.

use serde_json::{Map, Value as JsonValue};

#[cfg(test)]
use crate::hook_baseline::{Expected, FigName, FigSuggestion, FigSuggestionObject};

#[derive(Debug)]
pub(crate) struct AdapterError {
    pub(super) js_class: Option<&'static str>,
}

pub(super) type AdapterResult = Result<JsonValue, AdapterError>;

pub(crate) fn throw(class: &'static str) -> AdapterError {
    AdapterError { js_class: Some(class) }
}

pub(super) fn json_parse(source: &str) -> Result<JsonValue, AdapterError> {
    serde_json::from_str(source).map_err(|_error| throw("SyntaxError"))
}

pub(super) fn js_split_lines(source: &str) -> Vec<String> {
    source.split('\n').map(ToOwned::to_owned).collect()
}

pub(super) fn js_index_of(value: &str, needle: &str) -> i64 {
    match value.find(needle) {
        Some(index) => value[..index].encode_utf16().count() as i64,
        None => -1,
    }
}

pub(super) fn js_slice(value: &str, start: i64, end: Option<i64>) -> String {
    let units: Vec<u16> = value.encode_utf16().collect();
    let len = units.len() as i64;
    let resolve = |index: i64| {
        if index < 0 {
            (len + index).max(0)
        } else {
            index.min(len)
        }
    };
    let from = resolve(start) as usize;
    let to = resolve(end.unwrap_or(len)) as usize;
    if from >= to {
        return String::new();
    }
    String::from_utf16_lossy(&units[from..to])
}

pub(super) fn js_to_string(value: Option<&JsonValue>) -> String {
    match value {
        None | Some(JsonValue::Null) => "undefined".into(),
        Some(JsonValue::String(text)) => text.clone(),
        Some(JsonValue::Bool(flag)) => flag.to_string(),
        Some(JsonValue::Number(number)) => number.to_string(),
        Some(other) => other.to_string(),
    }
}

pub(super) fn object_assign(target: &mut Map<String, JsonValue>, source: Option<&JsonValue>) {
    let Some(JsonValue::Object(fields)) = source else {
        return;
    };
    for (key, value) in fields {
        target.insert(key.clone(), value.clone());
    }
}

pub(super) fn suggestion_object(name: impl Into<String>, extra: &[(&str, JsonValue)]) -> JsonValue {
    let mut object = Map::new();
    object.insert("name".into(), JsonValue::String(name.into()));
    for (key, value) in extra {
        object.insert((*key).into(), value.clone());
    }
    JsonValue::Object(object)
}

#[cfg(test)]
pub(super) fn normalize_expected(value: JsonValue) -> Expected {
    match normalize_suggestions(value) {
        Ok(suggestions) => Expected::Suggestions { value: suggestions },
        Err(message) => Expected::Error { value: message },
    }
}

#[cfg(test)]
pub(super) fn expected_from_adapter(result: AdapterResult) -> Expected {
    match result {
        Ok(value) => normalize_expected(value),
        Err(error) => Expected::Error {
            value: error.js_class.unwrap_or("Error").to_string(),
        },
    }
}

#[cfg(test)]
fn normalize_suggestions(value: JsonValue) -> Result<Vec<FigSuggestion>, String> {
    let JsonValue::Array(items) = value else {
        return Err("suggestions result is not an array".into());
    };
    items.iter().map(normalize_suggestion).collect()
}

#[cfg(test)]
fn normalize_suggestion(item: &JsonValue) -> Result<FigSuggestion, String> {
    if let Some(name) = item.as_str() {
        if name.is_empty() {
            return Err("empty string suggestion".into());
        }
        return Ok(FigSuggestion::Name(name.to_owned()));
    }
    if !item.is_object() || item.is_null() {
        return Err("suggestion must be a string or object".into());
    }
    let object = item.as_object().expect("object");
    let name = match object.get("name") {
        Some(JsonValue::String(name)) if !name.is_empty() => FigName::One(name.clone()),
        Some(JsonValue::Array(names))
            if !names.is_empty()
                && names
                    .iter()
                    .all(|part| part.as_str().is_some_and(|name| !name.is_empty())) =>
        {
            FigName::Many(
                names
                    .iter()
                    .filter_map(JsonValue::as_str)
                    .map(ToOwned::to_owned)
                    .collect(),
            )
        },
        _ => return Err("suggestion is missing name".into()),
    };
    Ok(FigSuggestion::Object(FigSuggestionObject {
        name,
        display_name: nonempty_string(object.get("displayName")),
        insert_value: nonempty_string(object.get("insertValue")),
        description: nonempty_string(object.get("description")),
        icon: nonempty_string(object.get("icon")),
        priority: object.get("priority").and_then(JsonValue::as_i64),
        hidden: object.get("hidden").and_then(JsonValue::as_bool),
        is_dangerous: object.get("isDangerous").and_then(JsonValue::as_bool),
        suggestion_type: nonempty_string(object.get("type")),
        args: object.get("args").cloned().filter(|value| !value.is_null()),
        replace_value: nonempty_string(object.get("replaceValue")),
        deprecated: object.get("deprecated").and_then(JsonValue::as_bool),
        should_add_space: object.get("shouldAddSpace").and_then(JsonValue::as_bool),
    }))
}

#[cfg(test)]
fn nonempty_string(value: Option<&JsonValue>) -> Option<String> {
    value
        .and_then(JsonValue::as_str)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}
