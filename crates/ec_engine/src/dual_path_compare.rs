//! Shared Native vs Js `CompleteResult` compare (T3.3).
//!
//! Used by `ec engine complete --compare` and the session tests. This does not
//! change the product default; callers opt in.

use serde_json::{Map, Value};

use crate::runtime::CompleteResult;

pub fn complete_result_diffs(native: &CompleteResult, js: &CompleteResult) -> Vec<Value> {
    json_diffs(
        &serde_json::to_value(normalise_complete_result(native)).unwrap_or(Value::Null),
        &serde_json::to_value(normalise_complete_result(js)).unwrap_or(Value::Null),
        "$",
    )
}

/// T3.3 compare-only. Product results stay lossless.
/// rustup `toolchain list` trailing newline → Native option `"+"`; JS Spec parse
/// drops it. JS icons often include U+FE0F where adapters store the bare emoji.
pub fn normalise_complete_result(result: &CompleteResult) -> CompleteResult {
    let mut out = result.clone();
    out.suggestions.retain(|row| !is_rustup_trailing_toolchain(&row.name));
    for row in &mut out.suggestions {
        row.name = strip_variation_selectors(&row.name);
        row.description = strip_variation_selectors(&row.description);
        if let Some(icon) = &mut row.icon {
            *icon = strip_variation_selectors(icon);
        }
        if let Some(display) = &mut row.display_name {
            *display = strip_variation_selectors(display);
        }
    }
    out
}

fn strip_variation_selectors(text: &str) -> String {
    text.replace('\u{fe0f}', "")
}

fn is_rustup_trailing_toolchain(name: &str) -> bool {
    let name = strip_variation_selectors(name);
    name.is_empty() || name == "+"
}

fn json_diffs(native: &Value, js: &Value, path: &str) -> Vec<Value> {
    if native == js {
        return Vec::new();
    }
    match (native, js) {
        (Value::Object(left), Value::Object(right)) => object_diffs(left, right, path),
        (Value::Array(left), Value::Array(right)) => array_diffs(left, right, path),
        _ => vec![serde_json::json!({ "path": path, "native": native, "js": js })],
    }
}

fn object_diffs(left: &Map<String, Value>, right: &Map<String, Value>, path: &str) -> Vec<Value> {
    let mut diffs = Vec::new();
    let mut keys: Vec<&String> = left.keys().chain(right.keys()).collect();
    keys.sort();
    keys.dedup();
    for key in keys {
        let child = format!("{path}.{key}");
        match (left.get(key), right.get(key)) {
            (Some(native), Some(js)) => diffs.extend(json_diffs(native, js, &child)),
            (native, js) => diffs.push(serde_json::json!({
                "path": child,
                "native": native,
                "js": js
            })),
        }
    }
    diffs
}

fn array_diffs(left: &[Value], right: &[Value], path: &str) -> Vec<Value> {
    if left.len() != right.len() {
        return vec![serde_json::json!({ "path": path, "native": left, "js": right })];
    }
    let mut diffs = Vec::new();
    for (index, (native, js)) in left.iter().zip(right.iter()).enumerate() {
        diffs.extend(json_diffs(native, js, &format!("{path}[{index}]")));
    }
    diffs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Suggestion;

    #[test]
    fn rustup_trailing_plus_is_dropped_for_compare() {
        let mut result = CompleteResult::default();
        result.suggestions.push(Suggestion::new("+", "", "option"));
        result.suggestions.push(Suggestion::new("build", "", "subcommand"));
        let out = normalise_complete_result(&result);
        assert_eq!(out.suggestions.len(), 1);
        assert_eq!(out.suggestions[0].name, "build");
    }

    #[test]
    fn json_diffs_report_nested_paths() {
        let mut native = CompleteResult::default();
        native.suggestions.push(Suggestion::new("checkout", "", "subcommand"));
        let mut js = CompleteResult::default();
        js.suggestions.push(Suggestion::new("cherry-pick", "", "subcommand"));
        let diffs = complete_result_diffs(&native, &js);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0]["path"], "$.suggestions[0].name");
    }
}
