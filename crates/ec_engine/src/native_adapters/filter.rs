//! `direnv#filterTemplateSuggestions#0` and its two siblings share this body.
//! Factory extras `e` disagree across call sites (`isDangerous: true` here);
//! `ne` is the `.envrc` priority bump (76).

use serde_json::{Value as JsonValue, json};

use super::eval::{AdapterResult, suggestion_object};

/// bodySha256 `16eb363f9957097622cbe2ed626cfc4859c24b797e5e2acff58cf40f4f9fbbad`
///
/// ```javascript
/// t=>{let n=i=>i.includes(".envrc");return t.filter(i=>n(i.name)||i.name.endsWith("/")).map(i=>({...i,priority:n(i.name)&&ne,...e}))}
/// ```
pub(super) fn direnv_envrc(suggestions: &[JsonValue]) -> AdapterResult {
    const PRIORITY: i64 = 76;
    let mut out = Vec::new();
    for item in suggestions {
        let name = item.get("name").and_then(JsonValue::as_str).unwrap_or("");
        let is_envrc = name.contains(".envrc");
        if !is_envrc && !name.ends_with('/') {
            continue;
        }
        let mut merged = match item {
            JsonValue::Object(object) => object.clone(),
            _ => match suggestion_object(name, &[]) {
                JsonValue::Object(object) => object,
                other => {
                    out.push(other);
                    continue;
                },
            },
        };
        merged.insert("priority".into(), if is_envrc { json!(PRIORITY) } else { json!(false) });
        merged.insert("isDangerous".into(), json!(true));
        out.push(JsonValue::Object(merged));
    }
    Ok(JsonValue::Array(out))
}
