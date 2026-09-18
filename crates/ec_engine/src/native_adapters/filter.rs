//! `direnv#filterTemplateSuggestions#0` and its two siblings share this body.
//! Factory extras `e` differ by site: `allow` and `deny` pass
//! `{isDangerous: true}`, `edit` passes nothing. `ne` is the `.envrc`
//! priority bump (76).

use serde_json::{Value as JsonValue, json};

use super::effect::HookSite;
use super::eval::{AdapterResult, suggestion_object};

/// bodySha256 `16eb363f9957097622cbe2ed626cfc4859c24b797e5e2acff58cf40f4f9fbbad`
///
/// ```javascript
/// t=>{let n=i=>i.includes(".envrc");return t.filter(i=>n(i.name)||i.name.endsWith("/")).map(i=>({...i,priority:n(i.name)&&ne,...e}))}
/// ```
pub(super) fn direnv_envrc(suggestions: &[JsonValue], site: HookSite<'_>) -> AdapterResult {
    const PRIORITY: i64 = 76;
    // `N({isDangerous: true})` under allow / deny, `N()` under edit. A token
    // list with no subcommand (the T1.2 harness) reads as the representative
    // `allow` site.
    let is_dangerous = !matches!(site.words().first(), Some(&"edit"));
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
        if is_dangerous {
            merged.insert("isDangerous".into(), json!(true));
        }
        out.push(JsonValue::Object(merged));
    }
    Ok(JsonValue::Array(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(list: &[&str]) -> Vec<String> {
        list.iter().map(|token| (*token).to_owned()).collect()
    }

    fn run(site: &[&str]) -> Vec<JsonValue> {
        let list = tokens(site);
        direnv_envrc(
            &[
                json!({"name": ".envrc"}),
                json!({"name": "src/"}),
                json!({"name": "notes.md"}),
            ],
            HookSite::tokens(&list),
        )
        .expect("filter")
        .as_array()
        .expect("array")
        .clone()
    }

    #[test]
    fn allow_and_deny_flag_the_rows_dangerous_and_edit_does_not() {
        for site in [&["direnv", "allow", ""][..], &["direnv", "deny", ""], &["direnv", ""]] {
            let rows = run(site);
            assert_eq!(rows.len(), 2, "{site:?}: notes.md is dropped");
            assert!(rows.iter().all(|row| row["isDangerous"] == json!(true)), "{site:?}");
        }
        let rows = run(&["direnv", "edit", ""]);
        assert!(rows.iter().all(|row| row.get("isDangerous").is_none()));
        assert_eq!(rows[0]["priority"], json!(76));
        assert_eq!(rows[1]["priority"], json!(false));
    }
}
