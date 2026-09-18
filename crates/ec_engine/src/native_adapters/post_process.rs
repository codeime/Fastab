//! Named postProcess adapters for leftover side-effect-free bodies.
//!
//! Each function is bound to one `bodySha256`. Where a factory body closed
//! over per-site literals, the adapter takes a [`HookSite`] and reads the
//! site from the tokens or the generator's script; a token list that names
//! no known site (the T1.2 harness) falls back to the representative hook
//! the baseline was captured against.

use std::collections::HashSet;

use serde_json::{Map, Value as JsonValue, json};

use super::bunx_names::BUNX_EXCLUDE_NAMES;
use super::effect::HookSite;
use super::eval::{
    AdapterError, AdapterResult, js_index_of, js_slice, js_split_lines, js_to_string, json_parse, object_assign,
    suggestion_object, throw,
};
use super::git_config_keys::GIT_CONFIG_KNOWN_NAMES;
use super::turbo_icon::TURBO_ICON;

fn lines_to_named(stdout: &str, extras: &[(&str, JsonValue)], transform: impl Fn(&str) -> String) -> AdapterResult {
    let mut out = Vec::new();
    for line in js_split_lines(stdout) {
        let mut object = Map::new();
        object.insert("name".into(), JsonValue::String(transform(&line)));
        for (key, value) in extras {
            object.insert((*key).into(), value.clone());
        }
        out.push(JsonValue::Object(object));
    }
    Ok(JsonValue::Array(out))
}

/// `06cf60a4…` bunx/npx — exclude the closed-over command list.
pub(super) fn bunx_npx(stdout: &str) -> AdapterResult {
    let excluded: HashSet<&str> = BUNX_EXCLUDE_NAMES.iter().copied().collect();
    let mut out = Vec::new();
    for line in js_split_lines(stdout) {
        if excluded.contains(line.as_str()) {
            continue;
        }
        out.push(suggestion_object(line, &[("icon", json!("fig://icon?type=command"))]));
    }
    Ok(JsonValue::Array(out))
}

/// `08c4a4a0…` just assignments. Helper `c` is JSON.parse → null on failure.
pub(super) fn just_assignments(stdout: &str) -> AdapterResult {
    let parsed = match json_parse(stdout) {
        Ok(value) => value,
        Err(_) => return Ok(json!([])),
    };
    let Some(assignments) = parsed.get("assignments").and_then(JsonValue::as_object) else {
        return Ok(json!([]));
    };
    Ok(JsonValue::Array(
        assignments
            .keys()
            .map(|name| suggestion_object(name, &[("icon", json!("fig://icon?type=string"))]))
            .collect(),
    ))
}

/// `1db94727…` rush projects. Helper `P` is JSONC; parse failures return [].
pub(super) fn rush_projects(stdout: &str) -> AdapterResult {
    if stdout.is_empty() {
        return Ok(json!([]));
    }
    let parsed = match json_parse(stdout.trim()) {
        Ok(value) => value,
        Err(_) => return Ok(json!([])),
    };
    let mut out = Vec::new();
    if let Some(projects) = parsed.get("projects").and_then(JsonValue::as_array) {
        for project in projects {
            out.push(suggestion_object(
                js_to_string(project.get("packageName")),
                &[("description", json!("Projects"))],
            ));
        }
    }
    Ok(JsonValue::Array(out))
}

fn dcli_format_date(unix: i64) -> String {
    // `new Date(unix * 1e3).toLocaleString()` — unused by current baselines.
    format!("{unix}")
}

/// `20afa252…` dcli devices.
pub(super) fn dcli_devices(stdout: &str) -> AdapterResult {
    let mut parsed = match json_parse(stdout) {
        Ok(JsonValue::Array(items)) => items,
        Ok(_) => return Ok(json!([])),
        Err(_) => return Ok(json!([])),
    };
    parsed.sort_by(|left, right| {
        let left = left
            .get("lastActivityDateUnix")
            .and_then(JsonValue::as_i64)
            .unwrap_or(0);
        let right = right
            .get("lastActivityDateUnix")
            .and_then(JsonValue::as_i64)
            .unwrap_or(0);
        right.cmp(&left)
    });
    Ok(JsonValue::Array(
        parsed
            .into_iter()
            .map(|item| {
                suggestion_object(
                    js_to_string(item.get("deviceName")),
                    &[
                        (
                            "description",
                            json!(format!(
                                "Last activity: {}",
                                dcli_format_date(
                                    item.get("lastActivityDateUnix")
                                        .and_then(JsonValue::as_i64)
                                        .unwrap_or(0)
                                )
                            )),
                        ),
                        ("insertValue", json!(js_to_string(item.get("deviceId")))),
                    ],
                )
            })
            .collect(),
    ))
}

/// `40446df6…` limactl instances, `A(e)` with `ne=76`. `e` is
/// `{isDangerous: true}` under `copy`, `delete`, `shell` and `stop`;
/// `show-ssh` and `start` pass nothing. `copy` declares a plain `A()` on
/// its TARGET, but SOURCE is variadic and the walk never leaves a
/// variadic positional (`positional_arg` keeps serving it), so the site
/// that runs at every `copy` position is SOURCE's.
pub(super) fn limactl_instances(stdout: &str, site: HookSite<'_>) -> AdapterResult {
    let is_dangerous = !matches!(site.words().first(), Some(&"show-ssh" | &"start"));
    let mut extras = vec![("description", json!("Instance name")), ("priority", json!(76))];
    if is_dangerous {
        extras.push(("isDangerous", json!(true)));
    }
    lines_to_named(stdout, &extras, |line| line.to_string())
}

fn remote_icon(url: &str) -> &'static str {
    if url.contains("github.com") {
        "github"
    } else if url.contains("gitlab.com") {
        "gitlab"
    } else if url.contains("heroku.com") {
        "heroku"
    } else {
        "box"
    }
}

/// `40f323a2…` / `d060a61e…` git/pre-commit remotes. Trailing empty lines TypeError.
pub(super) fn git_remotes(stdout: &str) -> AdapterResult {
    let mut remotes = Map::new();
    for line in js_split_lines(stdout) {
        let cells: Vec<&str> = line.split('\t').collect();
        let name = cells.first().copied().unwrap_or("");
        let Some(url_field) = cells.get(1) else {
            return Err(throw("TypeError"));
        };
        let url = url_field.split(' ').next().unwrap_or("");
        remotes.insert(name.to_string(), JsonValue::String(url.to_string()));
    }
    Ok(JsonValue::Array(
        remotes
            .iter()
            .map(|(name, url)| {
                let icon = remote_icon(url.as_str().unwrap_or(""));
                suggestion_object(
                    name,
                    &[
                        ("icon", json!(format!("fig://icon?type={icon}"))),
                        ("description", json!("Remote")),
                    ],
                )
            })
            .collect(),
    ))
}

/// `46cfb8ef…` react-native devices.
pub(super) fn react_native_devices(stdout: &str) -> AdapterResult {
    let parsed = json_parse(stdout)?;
    let devices = parsed
        .get("devices")
        .and_then(JsonValue::as_object)
        .ok_or_else(|| throw("TypeError"))?;
    let mut out = Vec::new();
    for devices in devices.values() {
        let Some(items) = devices.as_array() else {
            continue;
        };
        for item in items {
            out.push(suggestion_object(
                js_to_string(item.get("name")),
                &[(
                    "icon",
                    json!("https://developer.apple.com/library/archive/Resources/1282/Images/apple2.png"),
                )],
            ));
        }
    }
    Ok(JsonValue::Array(out))
}

/// `47dd9ebf…` asdf plugins, `a(n)`. `n` is `{isDangerous: true}` under
/// `plugin remove`, `plugin-remove` and `uninstall`; the other ten sites
/// pass nothing.
pub(super) fn asdf_plugins(stdout: &str, site: HookSite<'_>) -> AdapterResult {
    let words = site.words();
    let is_dangerous = match words.first() {
        Some(&"plugin") => matches!(words.get(1), Some(&"remove")),
        Some(&"plugin-remove" | &"uninstall") => true,
        Some(_) => false,
        // The representative `asdf#postProcess#1` is `plugin remove`.
        None => true,
    };
    let mut extras = vec![
        ("description", json!("Plugin name")),
        ("priority", json!(76)),
        ("icon", json!("fig://icon?type=package")),
    ];
    if is_dangerous {
        extras.push(("isDangerous", json!(true)));
    }
    lines_to_named(stdout, &extras, |line| line.to_string())
}

fn cargo_workspace_packages(metadata: &JsonValue) -> Result<Vec<&JsonValue>, AdapterError> {
    let workspace_root = js_to_string(metadata.get("workspace_root"));
    let wanted = format!("{workspace_root}/Cargo.toml");
    let packages = metadata
        .get("packages")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| throw("TypeError"))?;
    if let Some(found) = packages
        .iter()
        .find(|package| package.get("source").and_then(JsonValue::as_str) == Some(wanted.as_str()))
    {
        return Ok(vec![found]);
    }
    Ok(packages
        .iter()
        .filter(|package| package.get("source").is_none_or(JsonValue::is_null))
        .collect())
}

/// `566065d4…` cargo metadata dependencies.
pub(super) fn cargo_deps(stdout: &str) -> AdapterResult {
    let parsed = json_parse(stdout)?;
    let packages = cargo_workspace_packages(&parsed)?;
    let mut rows = Vec::new();
    for package in packages {
        let Some(deps) = package.get("dependencies").and_then(JsonValue::as_array) else {
            continue;
        };
        for dep in deps {
            rows.push(suggestion_object(
                js_to_string(dep.get("name")),
                &[("description", json!(js_to_string(dep.get("req"))))],
            ));
        }
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for row in rows {
        let name = js_to_string(row.get("name"));
        if seen.insert(name) {
            out.push(row);
        }
    }
    Ok(JsonValue::Array(out))
}

fn fnm_parse_version(line: &str) -> Result<JsonValue, AdapterError> {
    let regex = fancy_regex::Regex::new(r"(?iu)v(?<major>\d+)\.(?<minor>\d+)\.(?<patch>\d+)(?: \((?<ltsName>\w+)\))?")
        .expect("fnm version regex");
    let caps = regex.captures(line).ok().flatten();
    let major = caps
        .as_ref()
        .and_then(|caps| caps.name("major"))
        .and_then(|part| part.as_str().parse::<i64>().ok());
    let minor = caps
        .as_ref()
        .and_then(|caps| caps.name("minor"))
        .and_then(|part| part.as_str().parse::<i64>().ok());
    let patch = caps
        .as_ref()
        .and_then(|caps| caps.name("patch"))
        .and_then(|part| part.as_str().parse::<i64>().ok());
    // JS `Number(undefined)` is NaN; later `% 2` is NaN, `find` can still miss
    // and then `.major` throws. Missing groups become null here; the caller
    // throws TypeError when it reads `.major` on the find result.
    Ok(json!({
        "major": major,
        "minor": minor,
        "patch": patch,
        "original": line,
        "ltsName": caps.as_ref().and_then(|caps| caps.name("ltsName")).map(|part| part.as_str()),
    }))
}

/// `57813e77…` fnm list. Unparseable lines still produce objects; missing even
/// majors throw when reading `.major` on `undefined`.
pub(super) fn fnm_list(stdout: &str) -> AdapterResult {
    let mut versions = Vec::new();
    for line in js_split_lines(stdout).into_iter().rev() {
        if line.is_empty() {
            continue;
        }
        versions.push(fnm_parse_version(&line)?);
    }
    let even = versions.iter().find(|entry| {
        entry
            .get("major")
            .and_then(JsonValue::as_i64)
            .is_some_and(|major| major % 2 == 0)
    });
    let Some(even_major) = even.and_then(|entry| entry.get("major")).and_then(JsonValue::as_i64) else {
        return Err(throw("TypeError"));
    };
    let mut latest = Map::new();
    for entry in &versions {
        let Some(major) = entry.get("major").and_then(JsonValue::as_i64) else {
            continue;
        };
        let minor = entry.get("minor").and_then(JsonValue::as_i64).unwrap_or(0);
        let key = major.to_string();
        let keep = match latest.get(&key) {
            Some(current) => current.get("minor").and_then(JsonValue::as_i64).unwrap_or(0) < minor,
            None => true,
        };
        if keep {
            latest.insert(key, entry.clone());
        }
    }
    let filtered: Vec<JsonValue> = versions
        .iter()
        .filter(|entry| {
            let Some(major) = entry.get("major").and_then(JsonValue::as_i64) else {
                return false;
            };
            if major == even_major {
                return true;
            }
            let original = js_to_string(entry.get("original"));
            let latest_original = latest
                .get(&major.to_string())
                .map(|item| js_to_string(item.get("original")))
                .unwrap_or_default();
            (major % 2 == 0 || major == even_major - 1 || major == even_major + 1) && original == latest_original
        })
        .cloned()
        .collect();
    let mut unique = Vec::new();
    for entry in filtered.iter().chain(versions.iter()) {
        let original = js_to_string(entry.get("original"));
        if unique
            .iter()
            .any(|seen: &JsonValue| js_to_string(seen.get("original")) == original)
        {
            continue;
        }
        unique.push(entry.clone());
    }
    Ok(JsonValue::Array(
        unique
            .into_iter()
            .map(|entry| {
                let original = js_to_string(entry.get("original"));
                let major = entry.get("major").and_then(JsonValue::as_i64).unwrap_or(0);
                let latest_original = latest
                    .get(&major.to_string())
                    .map(|item| js_to_string(item.get("original")))
                    .unwrap_or_default();
                if let Some(lts) = entry.get("ltsName").and_then(JsonValue::as_str) {
                    if !lts.is_empty() && latest_original == original {
                        return suggestion_object(
                            format!("lts/{lts}"),
                            &[
                                ("displayName", json!(original.clone())),
                                ("description", json!(format!("Node.js {original}"))),
                            ],
                        );
                    }
                }
                suggestion_object(
                    original.split(' ').next().unwrap_or(&original),
                    &[("description", json!(format!("Node.js {original}")))],
                )
            })
            .collect(),
    ))
}

/// `581af18c…` dcli access keys.
pub(super) fn dcli_access_keys(stdout: &str) -> AdapterResult {
    let mut parsed = match json_parse(stdout) {
        Ok(JsonValue::Array(items)) => items,
        Ok(_) | Err(_) => return Ok(json!([])),
    };
    parsed.sort_by(|left, right| {
        let left = left.get("creationDateUnix").and_then(JsonValue::as_i64).unwrap_or(0);
        let right = right.get("creationDateUnix").and_then(JsonValue::as_i64).unwrap_or(0);
        right.cmp(&left)
    });
    Ok(JsonValue::Array(
        parsed
            .into_iter()
            .map(|item| {
                let device = js_to_string(item.get("deviceName"));
                let key = js_to_string(item.get("accessKey"));
                suggestion_object(
                    format!("{device} ({key})"),
                    &[
                        (
                            "description",
                            json!(format!(
                                "Created: {}",
                                dcli_format_date(item.get("creationDateUnix").and_then(JsonValue::as_i64).unwrap_or(0))
                            )),
                        ),
                        ("insertValue", json!(key)),
                    ],
                )
            })
            .collect(),
    ))
}

/// `599dd22a…` rustup channels. Empty stdout is `[]`; `Date` is unused on fixtures.
pub(super) fn rustup_channels(stdout: &str) -> AdapterResult {
    if stdout.is_empty() {
        return Ok(json!([]));
    }
    let parsed = json_parse(stdout)?;
    let releases = parsed.as_array().ok_or_else(|| throw("TypeError"))?;
    let mut out = vec![
        suggestion_object("stable", &[]),
        suggestion_object("beta", &[]),
        suggestion_object("nightly", &[]),
    ];
    for release in releases {
        let tag = js_to_string(release.get("tag_name"));
        let name = js_to_string(release.get("name"));
        let published = js_to_string(release.get("published_at"));
        out.push(suggestion_object(
            tag,
            &[("description", json!(format!("{name} - {published}")))],
        ));
    }
    Ok(JsonValue::Array(out))
}

/// `5b988a42…` tldr pages matching `/*.md$/`.
pub(super) fn tldr_pages(stdout: &str) -> AdapterResult {
    Ok(JsonValue::Array(
        js_split_lines(stdout)
            .into_iter()
            .filter(|line| line.ends_with(".md"))
            .map(|line| {
                let name = line
                    .split(' ')
                    .next_back()
                    .map(|part| js_slice(part, 0, Some(-3)))
                    .unwrap_or_default();
                suggestion_object(
                    name,
                    &[
                        ("description", json!("Tldr page")),
                        ("icon", json!("fig://icon?type=string")),
                    ],
                )
            })
            .collect(),
    ))
}

/// `5cde47b7…` rustup toolchains, `i({excludeShort})`. Nine sites list the
/// channel prefixes (`stable`, `nightly`, …) ahead of the full names;
/// `toolchain uninstall` passes `excludeShort: true` and lists only the
/// full names — a bare channel there would remove whichever toolchain
/// it currently resolves to.
pub(super) fn rustup_toolchains(stdout: &str, site: HookSite<'_>) -> AdapterResult {
    let words = site.words();
    let exclude_short = words.first() == Some(&"toolchain") && words.get(1) == Some(&"uninstall");
    let names: Vec<String> = js_split_lines(stdout)
        .into_iter()
        .map(|line| line.split(' ').next().unwrap_or("").to_string())
        .collect();
    let mut prefixes = Vec::new();
    if !exclude_short {
        let mut seen = HashSet::new();
        for name in &names {
            let prefix = name.split('-').next().unwrap_or("").to_string();
            if seen.insert(prefix.clone()) {
                prefixes.push(prefix);
            }
        }
    }
    Ok(JsonValue::Array(
        prefixes
            .into_iter()
            .chain(names)
            .map(|name| suggestion_object(name, &[]))
            .collect(),
    ))
}

/// `61d0b086…` rustup targets, `a({installed})`. `target remove` passes
/// `installed: true` and keeps only lines carrying an `(installed)` word;
/// `target add`, `toolchain install --target` and `set default-host`
/// list every target.
pub(super) fn rustup_targets(stdout: &str, site: HookSite<'_>) -> AdapterResult {
    let words = site.words();
    let installed_only = words.first() == Some(&"target") && words.get(1) == Some(&"remove");
    Ok(JsonValue::Array(
        js_split_lines(stdout)
            .into_iter()
            .map(|line| line.split(' ').map(ToOwned::to_owned).collect::<Vec<String>>())
            .filter(|parts| !installed_only || parts.iter().any(|part| part == "(installed)"))
            .map(|parts| suggestion_object(parts.first().map_or("", String::as_str), &[]))
            .collect(),
    ))
}

/// `66349787…` pre-commit hook ids. Empty parse throws → `undefined`.
pub(super) fn precommit_hooks(stdout: &str) -> AdapterResult {
    let parsed = if stdout.trim().is_empty() {
        return Ok(JsonValue::Null);
    } else if let Ok(value) = serde_json::from_str::<JsonValue>(stdout) {
        value
    } else {
        JsonValue::String(stdout.to_string())
    };
    let mut out = Vec::new();
    if let Some(repos) = parsed.get("repos").and_then(JsonValue::as_array) {
        for repo in repos {
            let Some(hooks) = repo.get("hooks").and_then(JsonValue::as_array) else {
                continue;
            };
            for hook in hooks {
                out.push(suggestion_object(js_to_string(hook.get("id")), &[]));
            }
        }
    }
    Ok(JsonValue::Array(out))
}

/// `7684a7c6…` brew services, `o(i)`. `i` is the verb the description
/// leads with: `Cleanup` under `cleanup`, and `Run` / `Start` / `Stop` /
/// `Restart` under the matching `services` subcommand.
pub(super) fn brew_services(stdout: &str, site: HookSite<'_>) -> AdapterResult {
    let words = site.words();
    let verb = match (words.first(), words.get(1)) {
        (Some(&"services"), Some(&"run")) => "Run",
        (Some(&"services"), Some(&"start")) => "Start",
        (Some(&"services"), Some(&"stop")) => "Stop",
        (Some(&"services"), Some(&"restart")) => "Restart",
        // `cleanup`, also the representative `brew#postProcess#15`.
        _ => "Cleanup",
    };
    Ok(JsonValue::Array(
        js_split_lines(stdout)
            .into_iter()
            .filter(|line| !line.contains("unbound"))
            .map(|line| {
                suggestion_object(
                    &line,
                    &[
                        ("icon", json!("fig://icon?type=package")),
                        ("description", json!(format!("{verb} {line}"))),
                    ],
                )
            })
            .collect(),
    ))
}

/// `775c0c96…` deno specifier matchers. No match → `undefined`.
pub(super) fn deno_url(stdout: &str) -> AdapterResult {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(json!([]));
    }
    let url = fancy_regex::Regex::new(r"^(https?://.*\.(?:m?[jt]sx?))(?:\?.*)?(?:#.*)?$").expect("deno url regex");
    if let Some(caps) = url.captures(trimmed).ok().flatten() {
        if let Some(name) = caps.get(1) {
            return Ok(json!([{
                "name": name.as_str(),
                "icon": "fig://template?badge=📋&color=000000",
                "priority": 100
            }]));
        }
    }
    if trimmed.starts_with("npm:") {
        return Ok(json!([{
            "name": trimmed,
            "icon": "fig://template?badge=📋&color=000000",
            "priority": 100
        }]));
    }
    Ok(JsonValue::Null)
}

fn package_json_deps(stdout: &str, tokens: &[String]) -> AdapterResult {
    if stdout.trim().is_empty() {
        return Ok(json!([]));
    }
    let parsed = match json_parse(stdout) {
        Ok(value) => value,
        Err(_) => return Ok(json!([])),
    };
    let mut deps = match parsed.get("dependencies") {
        Some(JsonValue::Object(object)) => object.clone(),
        _ => Map::new(),
    };
    object_assign(&mut deps, parsed.get("devDependencies"));
    let mut optional = match parsed.get("optionalDependencies") {
        Some(JsonValue::Object(object)) => object.clone(),
        _ => Map::new(),
    };
    object_assign(&mut deps, Some(&JsonValue::Object(optional.clone())));
    if let Some(JsonValue::Object(object)) = parsed.get("optionalDependencies") {
        optional = object.clone();
    }
    Ok(JsonValue::Array(
        deps.iter()
            .filter(|(name, _)| !tokens.iter().any(|token| token == *name))
            .map(|(name, value)| {
                // JS: `s[c] ? "dependency" : r[c] ? "optionalDependency" : "devDependency"`
                let js_truthy = match value {
                    JsonValue::Null => false,
                    JsonValue::Bool(false) => false,
                    JsonValue::String(text) if text.is_empty() => false,
                    JsonValue::Number(number) if number.as_f64() == Some(0.0) => false,
                    _ => true,
                };
                let description = if js_truthy {
                    "dependency"
                } else if optional.contains_key(name) {
                    "optionalDependency"
                } else {
                    "devDependency"
                };
                suggestion_object(name, &[("icon", json!("📦")), ("description", json!(description))])
            })
            .collect(),
    ))
}

/// `83c37762…` yarn package.json deps.
pub(super) fn yarn_deps(stdout: &str, tokens: &[String]) -> AdapterResult {
    package_json_deps(stdout, tokens)
}

fn title_case(value: &str) -> String {
    value
        .trim()
        .split_inclusive(|ch: char| ch.is_whitespace())
        .map(|word| {
            let trimmed = word.trim_end();
            if trimmed.is_empty() {
                return word.to_string();
            }
            let mut chars = trimmed.chars();
            let Some(first) = chars.next() else {
                return word.to_string();
            };
            let mut out = first.to_uppercase().collect::<String>();
            out.extend(chars.flat_map(char::to_lowercase));
            let trailing = &word[trimmed.len()..];
            out.push_str(trailing);
            out
        })
        .collect()
}

/// `86847ea0…` yo generators.
pub(super) fn yo_generators(stdout: &str) -> AdapterResult {
    Ok(JsonValue::Array(
        js_split_lines(stdout)
            .into_iter()
            .filter(|line| {
                let trimmed = line.trim();
                !trimmed.is_empty() && trimmed != "Available Generators:"
            })
            .map(|line| {
                let name = line.trim();
                let display = title_case(name);
                suggestion_object(
                    name,
                    &[
                        ("icon", json!("https://avatars.githubusercontent.com/u/1714870?v=4")),
                        ("displayName", json!(display.clone())),
                        ("description", json!(format!("{display} Generator"))),
                        ("priority", json!(100)),
                    ],
                )
            })
            .collect(),
    ))
}

const TASK_STATUSES: &[&str] = &[
    "ACTIVE",
    "COMPLETED",
    "LATEST",
    "PENDING",
    "SCHEDULED",
    "UDA",
    "YEAR",
    "ANNOTATED",
    "DELETED",
    "MONTH",
    "PRIORITY",
    "TAGGED",
    "UNBLOCKED",
    "YESTERDAY",
    "BLOCKED",
    "DUE",
    "ORPHAN",
    "PROJECT",
    "TEMPLATE",
    "UNTIL",
    "BLOCKING",
    "DUETODAY",
    "OVERDUE",
    "QUARTER",
    "TODAY",
    "WAITING",
    "CHILD",
    "INSTANCE",
    "PARENT",
    "READY",
    "TOMORROW",
    "WEEK",
];
const TASK_DATES: &[&str] = &[
    "now",
    "today",
    "sod",
    "eod",
    "yesterday",
    "tomorrow",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
    "soy",
    "eoy",
    "eoq",
    "som",
    "socm",
    "eom",
    "ecom",
    "sow",
    "socw",
    "eow",
    "eocw",
    "soww",
    "eoww",
];
const TASK_PRIORITIES: &[&str] = &["H", "M", "L"];
const TASK_RECUR: &[&str] = &[
    "daily",
    "day",
    "1da",
    "2da",
    "weekdays",
    "weekly",
    "1wk",
    "2wk",
    "biweekly",
    "fortnight",
    "monthly",
    "month",
    "1mo",
    "2mo",
    "quarterly",
    "1qtr",
    "3qtr",
    "semiannual",
    "annual",
    "yearly",
    "1yr",
    "2yrs",
];

fn task_projects(tasks: &[JsonValue]) -> Vec<JsonValue> {
    let mut counts = Map::new();
    for task in tasks {
        if task.get("status").and_then(JsonValue::as_str) == Some("completed") {
            continue;
        }
        let project = js_to_string(task.get("project"));
        let next = counts.get(&project).and_then(JsonValue::as_i64).unwrap_or(0) + 1;
        counts.insert(project, json!(next));
    }
    counts
        .iter()
        .map(|(project, count)| {
            suggestion_object(
                format!("project:{project}"),
                &[
                    ("displayName", json!(format!("Project: {project}"))),
                    ("description", json!(format!("{count} tasks"))),
                    ("icon", json!("🗂")),
                ],
            )
        })
        .collect()
}

fn task_virtual_tags() -> Vec<JsonValue> {
    TASK_STATUSES
        .iter()
        .map(|tag| {
            suggestion_object(
                format!("+{tag}"),
                &[("displayName", json!(format!("Tag: {tag}"))), ("icon", json!("🏷"))],
            )
        })
        .collect()
}

fn task_tags(tasks: &[JsonValue], prefix: &str, label: &str, icon: &str) -> Vec<JsonValue> {
    let mut seen = Vec::new();
    let mut set = HashSet::new();
    for task in tasks {
        let Some(tags) = task.get("tags").and_then(JsonValue::as_array) else {
            continue;
        };
        for tag in tags {
            let name = js_to_string(Some(tag));
            if set.insert(name.clone()) {
                seen.push(name);
            }
        }
    }
    seen.into_iter()
        .map(|tag| {
            suggestion_object(
                format!("{prefix}{tag}"),
                &[("displayName", json!(format!("{label}: {tag}"))), ("icon", json!(icon))],
            )
        })
        .collect()
}

fn task_dates() -> Vec<JsonValue> {
    let prefixes = [
        "due",
        "due.by",
        "due.before",
        "due.after",
        "scheduled",
        "scheduled.by",
        "scheduled.before",
        "scheduled.after",
        "until",
        "until.by",
        "until.before",
        "until.after",
        "wait",
        "wait.by",
        "wait.before",
        "wait.after",
        "entry",
        "entry.by",
        "entry.before",
        "entry.after",
    ];
    prefixes
        .into_iter()
        .flat_map(|prefix| {
            TASK_DATES
                .iter()
                .map(move |date| suggestion_object(format!("{prefix}:{date}"), &[]))
        })
        .collect()
}

fn task_priority_terms() -> Vec<JsonValue> {
    let mut out: Vec<JsonValue> = TASK_PRIORITIES
        .iter()
        .flat_map(|priority| {
            [
                suggestion_object(format!("priority:{priority}"), &[]),
                suggestion_object(format!("priority.is:{priority}"), &[]),
                suggestion_object(format!("priority.not:{priority}"), &[]),
            ]
        })
        .collect();
    out.push(suggestion_object("priority.none:", &[]));
    out
}

fn task_ids(tasks: &[JsonValue]) -> Vec<JsonValue> {
    tasks
        .iter()
        .filter(|task| task.get("status").and_then(JsonValue::as_str) != Some("completed"))
        .map(|task| {
            let id = js_to_string(task.get("id"));
            let description = js_to_string(task.get("description"));
            suggestion_object(
                &id,
                &[
                    ("displayName", json!(format!("{id} - {description}"))),
                    ("description", json!(description)),
                    ("icon", json!("☑️")),
                ],
            )
        })
        .collect()
}

fn task_recur() -> Vec<JsonValue> {
    TASK_RECUR
        .iter()
        .map(|item| suggestion_object(format!("recur:{item}"), &[]))
        .collect()
}

fn task_parse(stdout: &str) -> Result<Vec<JsonValue>, AdapterError> {
    match json_parse(stdout)? {
        JsonValue::Array(items) => Ok(items),
        _ => Err(throw("TypeError")),
    }
}

/// `86e2f494…` taskwarrior projects+tags+dates+priority.
pub(super) fn taskwarrior_a(stdout: &str) -> AdapterResult {
    let tasks = task_parse(stdout)?;
    let mut out = task_projects(&tasks);
    out.extend(task_tags(&tasks, "+", "Tag", "🏷"));
    out.extend(task_dates());
    out.extend(task_virtual_tags());
    out.extend(task_priority_terms());
    Ok(JsonValue::Array(out))
}

/// `97aa92d3…` asdf versions (reversed), the body `s(n)` and `v(n)` share.
/// `n` is `{isDangerous: true}` under `uninstall` only; the other ten
/// sites (`install`, `local`, `global`, `list all`, …) pass nothing.
pub(super) fn asdf_versions(stdout: &str, site: HookSite<'_>) -> AdapterResult {
    let words = site.words();
    // The representative `asdf#postProcess#11` is `uninstall`.
    let is_dangerous = matches!(words.first(), Some(&"uninstall") | None);
    let mut extras = vec![
        ("description", json!("Plugin version")),
        ("priority", json!(76)),
        ("icon", json!("fig://icon?type=commit")),
    ];
    if is_dangerous {
        extras.push(("isDangerous", json!(true)));
    }
    Ok(JsonValue::Array(
        js_split_lines(stdout)
            .into_iter()
            .rev()
            .map(|line| suggestion_object(line.trim(), &extras))
            .collect(),
    ))
}

fn map_space_lines(stdout: &str, icon_prefix: &str) -> AdapterResult {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    for line in js_split_lines(stdout) {
        let index = js_index_of(&line, " ");
        let name = js_slice(&line, 0, Some(index));
        let description = js_slice(&line, index + 1, None);
        if seen.insert(name.clone()) {
            entries.push((name, description));
        } else if let Some(existing) = entries.iter_mut().find(|(key, _)| key == &name) {
            existing.1 = description;
        }
    }
    entries.retain(|(name, _)| name != "(null)");
    Ok(JsonValue::Array(
        entries
            .into_iter()
            .map(|(name, description)| {
                suggestion_object(
                    name,
                    &[
                        ("description", json!(description.clone())),
                        ("icon", json!(format!("{icon_prefix}{description}"))),
                    ],
                )
            })
            .collect(),
    ))
}

/// `9e08e5a9…` `open` app map.
pub(super) fn open_apps(stdout: &str) -> AdapterResult {
    map_space_lines(stdout, "fig://")
}

fn parse_snaplet_snapshots(stdout: &str) -> Vec<JsonValue> {
    let cleaned = {
        let collapsed = fancy_regex::Regex::new(r" +").expect("spaces");
        let ansi = fancy_regex::Regex::new("\u{001b}\\[[0-9;]*[A-Za-z]").expect("ansi");
        ansi.replace_all(&collapsed.replace_all(stdout, " "), "").into_owned()
    };
    let lines: Vec<&str> = cleaned.split('\n').collect();
    let mut out = Vec::new();
    if lines.len() > 4 && lines.get(1).is_some_and(|line| line.starts_with("NAME")) {
        for line in lines.iter().skip(2).take(lines.len().saturating_sub(4)) {
            let mut cells: Vec<&str> = line.split(' ').collect();
            if cells.is_empty() {
                continue;
            }
            let name = cells.remove(0);
            let status = if cells.first().is_some_and(|cell| cell.contains("SUCCESS")) {
                cells.remove(0);
                "SUCCESS"
            } else if !cells.is_empty() {
                cells.remove(0);
                "ERROR"
            } else {
                "ERROR"
            };
            let created = format!(
                "{} {} {}",
                cells.first().copied().unwrap_or(""),
                cells.get(1).copied().unwrap_or(""),
                cells.get(2).copied().unwrap_or("")
            );
            if cells.len() >= 3 {
                cells.drain(..3.min(cells.len()));
            }
            let size = format!(
                "{}{}",
                cells.first().copied().unwrap_or(""),
                cells.get(1).copied().unwrap_or("")
            );
            if cells.len() >= 2 {
                cells.drain(..2.min(cells.len()));
            }
            let src = if cells.first().is_some_and(|cell| cell.contains('☁')) {
                "CLOUD"
            } else {
                "LOCAL"
            };
            out.push(json!({
                "name": name,
                "status": status,
                "created": created,
                "size": size,
                "src": src
            }));
        }
    }
    out
}

/// `a148afe7…` snaplet cloud snapshots.
pub(super) fn snaplet_cloud(stdout: &str) -> AdapterResult {
    Ok(JsonValue::Array(
        parse_snaplet_snapshots(stdout)
            .into_iter()
            .filter(|item| {
                item.get("src").and_then(JsonValue::as_str) == Some("CLOUD")
                    && item.get("status").and_then(JsonValue::as_str) == Some("SUCCESS")
            })
            .map(|item| {
                suggestion_object(
                    js_to_string(item.get("name")),
                    &[(
                        "description",
                        json!(format!(
                            "✅ {} {} ☁️",
                            js_to_string(item.get("created")),
                            js_to_string(item.get("size"))
                        )),
                    )],
                )
            })
            .collect(),
    ))
}

/// `a2343033…` sake groups.
pub(super) fn sake_groups(stdout: &str) -> AdapterResult {
    let parsed = json_parse(stdout)?;
    let groups = parsed
        .get("groups")
        .and_then(JsonValue::as_object)
        .ok_or_else(|| throw("TypeError"))?;
    let mut out = Vec::new();
    for tasks in groups.values() {
        let Some(items) = tasks.as_array() else {
            continue;
        };
        for item in items {
            out.push(suggestion_object(
                js_to_string(item.get("name")),
                &[
                    ("description", json!(js_to_string(item.get("description")))),
                    ("priority", json!(76)),
                    ("icon", json!("🍶")),
                ],
            ));
        }
    }
    Ok(JsonValue::Array(out))
}

/// `a8bce6f4…` taskwarrior plus untag + recur.
pub(super) fn taskwarrior_b(stdout: &str) -> AdapterResult {
    let tasks = task_parse(stdout)?;
    let mut out = task_projects(&tasks);
    out.extend(task_tags(&tasks, "+", "Tag", "🏷"));
    out.extend(task_tags(&tasks, "-", "Untag", "❌"));
    out.extend(task_dates());
    out.extend(task_virtual_tags());
    out.extend(task_priority_terms());
    out.extend(task_recur());
    Ok(JsonValue::Array(out))
}

/// `b0575d96…` tailscale peers, `t({append})`. `file cp` passes
/// `append: ":"` so the inserted host reads `host:`, the form the command
/// requires; `ip` passes nothing.
pub(super) fn tailscale_peers(stdout: &str, site: HookSite<'_>) -> AdapterResult {
    let append = if site.words().first() == Some(&"file") { ":" } else { "" };
    let parsed = json_parse(stdout)?;
    let peers = parsed
        .get("Peer")
        .and_then(JsonValue::as_object)
        .ok_or_else(|| throw("TypeError"))?;
    Ok(JsonValue::Array(
        peers
            .values()
            .map(|peer| {
                let dns = js_to_string(peer.get("DNSName"));
                let host = format!("{}{append}", dns.split('.').next().unwrap_or(&dns));
                suggestion_object(
                    &host,
                    &[
                        ("displayName", json!(js_to_string(peer.get("HostName")))),
                        ("description", json!(js_to_string(peer.get("OS")))),
                    ],
                )
            })
            .collect(),
    ))
}

fn parse_snaplet_backups(stdout: &str) -> Vec<JsonValue> {
    let collapsed = fancy_regex::Regex::new(r" +").expect("spaces");
    let ansi = fancy_regex::Regex::new("\u{001b}\\[[0-9;]*[A-Za-z]").expect("ansi");
    let cleaned = ansi.replace_all(&collapsed.replace_all(stdout, " "), "").into_owned();
    let lines: Vec<&str> = cleaned.split('\n').collect();
    let mut out = Vec::new();
    if lines.len() > 2 && lines.get(1).is_some_and(|line| line.starts_with("NAME")) {
        for line in lines.iter().skip(2) {
            let mut cells: Vec<&str> = line.split(' ').collect();
            if cells.is_empty() {
                continue;
            }
            let name = cells.remove(0);
            let size = format!(
                "{}{}",
                cells.first().copied().unwrap_or(""),
                cells.get(1).copied().unwrap_or("")
            );
            if cells.len() >= 2 {
                cells.drain(..2.min(cells.len()));
            }
            let snapshot = cells.first().copied().unwrap_or("");
            out.push(json!({"name": name, "size": size, "snapshotName": snapshot}));
        }
    }
    out
}

/// `b3739a28…` snaplet backups.
pub(super) fn snaplet_backups(stdout: &str) -> AdapterResult {
    Ok(JsonValue::Array(
        parse_snaplet_backups(stdout)
            .into_iter()
            .map(|item| {
                suggestion_object(
                    js_to_string(item.get("name")),
                    &[(
                        "description",
                        json!(format!(
                            "{} ({})",
                            js_to_string(item.get("size")),
                            js_to_string(item.get("snapshotName"))
                        )),
                    )],
                )
            })
            .collect(),
    ))
}

/// `b3b72135…` pnpm package.json deps.
pub(super) fn pnpm_deps(stdout: &str, tokens: &[String]) -> AdapterResult {
    package_json_deps(stdout, tokens)
}

/// `baa2ab8a…` tccutil services.
pub(super) fn tccutil_services(stdout: &str) -> AdapterResult {
    map_space_lines(stdout, "fig://")
}

/// `bcb6a45f…` turbo pipeline (legacy `pipeline` key, not `tasks`).
pub(super) fn turbo_pipeline(stdout: &str) -> AdapterResult {
    let parsed = match json_parse(stdout) {
        Ok(value) => value,
        Err(_) => return Ok(json!([])),
    };
    let pipeline = parsed
        .get("pipeline")
        .and_then(JsonValue::as_object)
        .ok_or_else(|| throw("TypeError"))?;
    Ok(JsonValue::Array(
        pipeline
            .iter()
            .map(|(name, spec)| {
                let mut bits = Vec::new();
                if spec.get("dependsOn").is_some() {
                    let depends = spec
                        .get("dependsOn")
                        .and_then(JsonValue::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .map(|item| format!("'{}'", js_to_string(Some(item))))
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .unwrap_or_default();
                    bits.push(format!("depends on {depends}"));
                }
                if spec.get("outputs").is_some() {
                    let outputs = spec
                        .get("outputs")
                        .and_then(JsonValue::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .map(|item| format!("'{}'", js_to_string(Some(item))))
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .unwrap_or_default();
                    bits.push(format!("outputs {outputs}"));
                }
                let description = if bits.is_empty() {
                    "Task".to_string()
                } else {
                    format!("Task: {}", bits.join(", "))
                };
                suggestion_object(
                    name,
                    &[("description", json!(description)), ("icon", json!(TURBO_ICON))],
                )
            })
            .collect(),
    ))
}

fn just_recipes(parsed: &JsonValue, _show_parameters: bool) -> AdapterResult {
    let mut out = Vec::new();
    if let Some(recipes) = parsed.get("recipes").and_then(JsonValue::as_object) {
        for (name, recipe) in recipes {
            if recipe.get("private").and_then(JsonValue::as_bool) == Some(true) {
                continue;
            }
            let params = recipe
                .get("parameters")
                .and_then(JsonValue::as_array)
                .map_or(0, Vec::len);
            let insert = if params == 0 { name.clone() } else { format!("{name} ") };
            let display = name.clone();
            let description = recipe
                .get("doc")
                .and_then(JsonValue::as_str)
                .filter(|text| !text.is_empty())
                .unwrap_or("Recipe");
            out.push(suggestion_object(
                name,
                &[
                    ("insertValue", json!(insert)),
                    ("displayName", json!(display)),
                    ("description", json!(description)),
                    ("icon", json!("fig://icon?type=command")),
                ],
            ));
        }
    }
    if let Some(aliases) = parsed.get("aliases").and_then(JsonValue::as_object) {
        for (name, alias) in aliases {
            out.push(suggestion_object(
                name,
                &[
                    (
                        "description",
                        json!(format!("Alias for '{}'", js_to_string(alias.get("target")))),
                    ),
                    ("icon", json!("fig://icon?type=commandkey")),
                ],
            ));
        }
    }
    Ok(JsonValue::Array(out))
}

/// `c3fac3c2…` just recipes.
pub(super) fn just_recipe_list(stdout: &str) -> AdapterResult {
    let parsed = match json_parse(stdout) {
        Ok(value) => value,
        Err(_) => return Ok(json!([])),
    };
    just_recipes(&parsed, false)
}

/// `ca1383d4…` taskwarrior plus task ids.
pub(super) fn taskwarrior_c(stdout: &str) -> AdapterResult {
    let tasks = task_parse(stdout)?;
    let mut out = task_ids(&tasks);
    out.extend(task_projects(&tasks));
    out.extend(task_tags(&tasks, "+", "Tag", "🏷"));
    out.extend(task_dates());
    out.extend(task_virtual_tags());
    out.extend(task_priority_terms());
    Ok(JsonValue::Array(out))
}

/// `d3e93ba8…` just recipes with arity walk.
pub(super) fn just_recipes_arity(stdout: &str, tokens: &[String]) -> AdapterResult {
    let parsed = match json_parse(stdout) {
        Ok(value) => value,
        Err(_) => return Ok(json!([])),
    };
    let mut arity = std::collections::HashMap::new();
    let mut max_arity: f64 = 0.0;
    if let Some(recipes) = parsed.get("recipes").and_then(JsonValue::as_object) {
        for (name, recipe) in recipes {
            let params = recipe
                .get("parameters")
                .and_then(JsonValue::as_array)
                .cloned()
                .unwrap_or_default();
            let mut count = params.len() as f64;
            if let Some(last) = params.last() {
                if last.get("kind").and_then(JsonValue::as_str) != Some("singular") && !params.is_empty() {
                    count = f64::INFINITY;
                }
            }
            if count > max_arity {
                max_arity = count;
            }
            arity.insert(name.clone(), count);
        }
    }
    if let Some(aliases) = parsed.get("aliases").and_then(JsonValue::as_object) {
        for (name, alias) in aliases {
            if let Some(target) = alias.get("target").and_then(JsonValue::as_str) {
                if let Some(count) = arity.get(target).copied() {
                    arity.insert(name.clone(), count);
                }
            }
        }
    }
    let walk = max_arity.min((tokens.len() as f64 - 2.0).max(0.0)) as usize;
    for offset in 0..walk {
        let index = tokens.len().saturating_sub(2 + offset);
        let Some(token) = tokens.get(index) else {
            break;
        };
        if let Some(needed) = arity.get(token) {
            if *needed > offset as f64 {
                return Ok(json!([]));
            }
            break;
        }
    }
    just_recipes(&parsed, true)
}

/// `de5329d8…` git/hub config keys.
pub(super) fn git_config(stdout: &str) -> AdapterResult {
    let known: HashSet<&str> = GIT_CONFIG_KNOWN_NAMES.iter().copied().collect();
    Ok(JsonValue::Array(
        js_split_lines(stdout.trim())
            .into_iter()
            .map(|line| {
                let index = js_index_of(&line, " ");
                js_slice(&line, 0, Some(index))
            })
            .filter(|name| {
                name.starts_with("alias.")
                    || name.starts_with("branch.")
                    || name.starts_with("remote.")
                    || !known.contains(name.as_str())
            })
            .map(|name| suggestion_object(name, &[("icon", json!("⚙️"))]))
            .collect(),
    ))
}

/// `deba5217…` vr scripts. Prefix is `"    • "`.
pub(super) fn vr_scripts(stdout: &str) -> AdapterResult {
    const PREFIX: &str = "    • ";
    let lines: Vec<String> = js_split_lines(stdout)
        .into_iter()
        .filter(|line| !line.is_empty())
        .collect();
    let mut out = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if !line.starts_with(PREFIX) {
            continue;
        }
        let name = line.replacen(PREFIX, "", 1);
        let mut object = Map::new();
        object.insert("name".into(), JsonValue::String(name));
        if let Some(next) = lines.get(index + 1) {
            object.insert("description".into(), json!(next.trim()));
        }
        out.push(JsonValue::Object(object));
    }
    Ok(JsonValue::Array(out))
}

/// `e0ff02cf…` kubectl/kubecolor cronjobs.
pub(super) fn kubectl_cronjob(stdout: &str) -> AdapterResult {
    if stdout.contains("The connection to the server") || stdout.contains("error:") {
        return Ok(json!([]));
    }
    Ok(JsonValue::Array(
        js_split_lines(stdout)
            .into_iter()
            .map(|line| {
                suggestion_object(
                    format!("cronjob/{line}"),
                    &[("icon", json!("fig://icon?type=kubernetes"))],
                )
            })
            .collect(),
    ))
}

/// `e66f8b07…` snaplet snapshot status.
pub(super) fn snaplet_status(stdout: &str) -> AdapterResult {
    Ok(JsonValue::Array(
        parse_snaplet_snapshots(stdout)
            .into_iter()
            .map(|item| {
                let ok = if item.get("status").and_then(JsonValue::as_str) == Some("SUCCESS") {
                    "✅"
                } else {
                    "❌"
                };
                let src = if item.get("src").and_then(JsonValue::as_str) == Some("CLOUD") {
                    "☁️"
                } else {
                    "💻"
                };
                suggestion_object(
                    js_to_string(item.get("name")),
                    &[(
                        "description",
                        json!(format!(
                            "{ok} {} {} {src}",
                            js_to_string(item.get("created")),
                            js_to_string(item.get("size"))
                        )),
                    )],
                )
            })
            .collect(),
    ))
}

fn deno_children(node: &JsonValue) -> Vec<JsonValue> {
    match node.get("kind").and_then(JsonValue::as_str) {
        Some("namespace") => node
            .pointer("/namespaceDef/elements")
            .and_then(JsonValue::as_array)
            .cloned()
            .unwrap_or_default(),
        Some("interface") => {
            let mut items = node
                .pointer("/interfaceDef/methods")
                .and_then(JsonValue::as_array)
                .cloned()
                .unwrap_or_default();
            items.extend(
                node.pointer("/interfaceDef/properties")
                    .and_then(JsonValue::as_array)
                    .cloned()
                    .unwrap_or_default(),
            );
            items
        },
        Some("class") => {
            let mut items = node
                .pointer("/classDef/methods")
                .and_then(JsonValue::as_array)
                .cloned()
                .unwrap_or_default();
            items.extend(
                node.pointer("/classDef/properties")
                    .and_then(JsonValue::as_array)
                    .cloned()
                    .unwrap_or_default(),
            );
            items
        },
        _ => Vec::new(),
    }
}

fn deno_walk<'a>(nodes: &'a [JsonValue], path: &[&str]) -> Vec<JsonValue> {
    let Some((first, rest)) = path.split_first() else {
        return nodes.to_vec();
    };
    if first.is_empty() {
        return nodes.to_vec();
    }
    nodes
        .iter()
        .filter(|node| node.get("name").and_then(JsonValue::as_str) == Some(*first))
        .flat_map(|node| deno_walk(&deno_children(node), rest))
        .collect()
}

/// `e8289375…` deno doc tree.
pub(super) fn deno_docs(stdout: &str, tokens: &[String]) -> AdapterResult {
    let parsed = json_parse(stdout)?;
    let nodes = parsed.as_array().ok_or_else(|| throw("TypeError"))?;
    let last = tokens.last().cloned().unwrap_or_default();
    let path: Vec<&str> = last.split('.').collect();
    let path = if path.len() <= 1 {
        Vec::new()
    } else {
        path[..path.len() - 1].to_vec()
    };
    let walked = deno_walk(nodes, &path);
    let show_private = tokens.iter().any(|token| token == "--private");
    let filtered: Vec<JsonValue> = walked
        .into_iter()
        .filter(|node| {
            if let Some(kind) = node.get("kind").and_then(JsonValue::as_str) {
                if kind == "moduleDoc" || kind == "import" {
                    return false;
                }
            }
            if !show_private && node.get("declarationKind").and_then(JsonValue::as_str) == Some("private") {
                return false;
            }
            true
        })
        .collect();
    let mut seen = HashSet::new();
    let mut unique = Vec::new();
    for node in filtered {
        let name = js_to_string(node.get("name"));
        if seen.insert(name) {
            unique.push(node);
        }
    }
    Ok(JsonValue::Array(
        unique
            .into_iter()
            .map(|node| {
                let name = js_to_string(node.get("name"));
                let description = match node.get("kind").and_then(JsonValue::as_str) {
                    Some("typeAlias") => "Type".to_string(),
                    Some(kind) => {
                        let mut chars = kind.chars();
                        match chars.next() {
                            Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                            None => "Property".into(),
                        }
                    },
                    None => "Property".into(),
                };
                let priority = if name.starts_with(|ch: char| ch.is_ascii_uppercase()) {
                    60
                } else if name.starts_with('_') {
                    40
                } else {
                    50
                };
                suggestion_object(
                    name,
                    &[
                        ("description", json!(description)),
                        ("priority", json!(priority)),
                        ("icon", json!("fig://icon?type=asterisk")),
                    ],
                )
            })
            .collect(),
    ))
}

/// `e8a02695…` gource displays.
pub(super) fn gource_displays(stdout: &str) -> AdapterResult {
    if stdout.contains("command not found") || stdout.is_empty() {
        return Ok(json!([]));
    }
    let parsed = match json_parse(stdout) {
        Ok(value) => value,
        Err(_) => return Ok(json!([])),
    };
    let mut rows = Vec::new();
    let Some(displays) = parsed.get("SPDisplaysDataType").and_then(JsonValue::as_array) else {
        return Ok(json!([]));
    };
    for display in displays {
        let Some(drivers) = display.get("spdisplays_ndrvs").and_then(JsonValue::as_array) else {
            continue;
        };
        for driver in drivers {
            rows.push(json!({
                "description": js_to_string(driver.get("_name")),
                "icon": "🖥️"
            }));
        }
    }
    Ok(JsonValue::Array(
        rows.into_iter()
            .enumerate()
            .map(|(index, mut row)| {
                if let JsonValue::Object(object) = &mut row {
                    object.insert("name".into(), json!((index + 1).to_string()));
                }
                row
            })
            .collect(),
    ))
}

/// `f04211ce…` cf table rows, `i(description, skip)`. Four generators share
/// the body and differ only by what they parse, so the site is the
/// generator's own script: `cf orgs` → `("Org", 3)`, `cf spaces` →
/// `("Space", 3)`, `cf services | cut …` → `("Service", 4)`, and
/// `cf apps | cut …` → `("App name", 4)`, also the representative.
pub(super) fn cf_lines(stdout: &str, site: HookSite<'_>) -> AdapterResult {
    let script = site.script_line();
    let (description, skip) = if script == "cf orgs" {
        ("Org", 3)
    } else if script == "cf spaces" {
        ("Space", 3)
    } else if script.contains("cf services") {
        ("Service", 4)
    } else {
        ("App name", 4)
    };
    let lines = js_split_lines(stdout.trim());
    Ok(JsonValue::Array(
        lines
            .get(skip..)
            .unwrap_or(&[])
            .iter()
            .map(|line| suggestion_object(line, &[("description", json!(description))]))
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(list: &[&str]) -> Vec<String> {
        list.iter().map(|token| (*token).to_owned()).collect()
    }

    fn rows(result: AdapterResult) -> Vec<JsonValue> {
        result.expect("adapter result").as_array().expect("array").clone()
    }

    fn names(rows: &[JsonValue]) -> Vec<&str> {
        rows.iter().map(|row| row["name"].as_str().unwrap_or("")).collect()
    }

    fn all_dangerous(rows: &[JsonValue]) -> bool {
        !rows.is_empty() && rows.iter().all(|row| row["isDangerous"] == json!(true))
    }

    fn none_dangerous(rows: &[JsonValue]) -> bool {
        !rows.is_empty() && rows.iter().all(|row| row.get("isDangerous").is_none())
    }

    #[test]
    fn limactl_marks_instances_dangerous_everywhere_but_start_and_show_ssh() {
        let stdout = "default\nubuntu";
        for site in [
            &["limactl", "copy", ""][..],
            &["limactl", "cp", "default:/tmp", ""],
            &["limactl", "delete", ""],
            &["limactl", "rm", ""],
            &["limactl", "shell", ""],
            &["limactl", "stop", ""],
            &["limactl", ""],
        ] {
            let list = tokens(site);
            let rows = rows(limactl_instances(stdout, HookSite::tokens(&list)));
            assert!(all_dangerous(&rows), "{site:?} → {rows:?}");
            assert_eq!(names(&rows), ["default", "ubuntu"]);
            assert_eq!(rows[0]["priority"], json!(76));
        }
        for site in [&["limactl", "start", ""][..], &["limactl", "show-ssh", ""]] {
            let list = tokens(site);
            let rows = rows(limactl_instances(stdout, HookSite::tokens(&list)));
            assert!(none_dangerous(&rows), "{site:?} → {rows:?}");
        }
    }

    #[test]
    fn asdf_plugins_are_dangerous_only_where_they_get_removed() {
        let stdout = "nodejs\npython";
        for site in [
            &["asdf", "plugin", "remove", ""][..],
            &["asdf", "plugin-remove", ""],
            &["asdf", "uninstall", ""],
            &["asdf", ""],
        ] {
            let list = tokens(site);
            assert!(
                all_dangerous(&rows(asdf_plugins(stdout, HookSite::tokens(&list)))),
                "{site:?}"
            );
        }
        for site in [
            &["asdf", "plugin", "update", ""][..],
            &["asdf", "install", ""],
            &["asdf", "list", ""],
            &["asdf", "local", ""],
            &["asdf", "where", ""],
        ] {
            let list = tokens(site);
            let rows = rows(asdf_plugins(stdout, HookSite::tokens(&list)));
            assert!(none_dangerous(&rows), "{site:?} → {rows:?}");
            assert_eq!(rows[0]["icon"], json!("fig://icon?type=package"));
        }
    }

    #[test]
    fn asdf_versions_are_dangerous_only_under_uninstall() {
        let stdout = "  18.0.0\n  20.1.0";
        let list = tokens(&["asdf", "uninstall", "nodejs", ""]);
        let uninstall = rows(asdf_versions(stdout, HookSite::tokens(&list)));
        assert!(all_dangerous(&uninstall));
        assert_eq!(names(&uninstall), ["20.1.0", "18.0.0"], "reversed and trimmed");
        for site in [
            &["asdf", "install", "nodejs", ""][..],
            &["asdf", "local", "nodejs", ""],
            &["asdf", "list", "all", "nodejs", ""],
            &["asdf", "list-all", "nodejs", ""],
        ] {
            let list = tokens(site);
            assert!(
                none_dangerous(&rows(asdf_versions(stdout, HookSite::tokens(&list)))),
                "{site:?}"
            );
        }
    }

    #[test]
    fn rustup_toolchain_uninstall_drops_the_channel_prefixes() {
        let stdout = "stable-aarch64-apple-darwin (default)\nnightly-aarch64-apple-darwin\nnightly-x86_64-apple-darwin";
        let list = tokens(&["rustup", "default", ""]);
        assert_eq!(
            names(&rows(rustup_toolchains(stdout, HookSite::tokens(&list)))),
            [
                "stable",
                "nightly",
                "stable-aarch64-apple-darwin",
                "nightly-aarch64-apple-darwin",
                "nightly-x86_64-apple-darwin"
            ]
        );
        let list = tokens(&["rustup", "toolchain", "uninstall", ""]);
        assert_eq!(
            names(&rows(rustup_toolchains(stdout, HookSite::tokens(&list)))),
            [
                "stable-aarch64-apple-darwin",
                "nightly-aarch64-apple-darwin",
                "nightly-x86_64-apple-darwin"
            ]
        );
        let list = tokens(&["rustup", "toolchain", "install", ""]);
        assert_eq!(rows(rustup_toolchains(stdout, HookSite::tokens(&list))).len(), 5);
    }

    #[test]
    fn rustup_target_remove_lists_only_installed_targets() {
        let stdout = "aarch64-apple-darwin (installed)\nwasm32-unknown-unknown\nx86_64-apple-darwin (installed)";
        let list = tokens(&["rustup", "target", "remove", ""]);
        assert_eq!(
            names(&rows(rustup_targets(stdout, HookSite::tokens(&list)))),
            ["aarch64-apple-darwin", "x86_64-apple-darwin"]
        );
        for site in [
            &["rustup", "target", "add", ""][..],
            &["rustup", "toolchain", "install", "--target", ""],
            &["rustup", "set", "default-host", ""],
        ] {
            let list = tokens(site);
            assert_eq!(
                rows(rustup_targets(stdout, HookSite::tokens(&list))).len(),
                3,
                "{site:?}"
            );
        }
    }

    #[test]
    fn brew_services_lead_the_description_with_the_subcommand_verb() {
        let stdout = "postgresql\nredis";
        let cases = [
            (&["brew", "services", "run", ""][..], "Run redis"),
            (&["brew", "services", "start", ""], "Start redis"),
            (&["brew", "services", "stop", ""], "Stop redis"),
            (&["brew", "services", "restart", ""], "Restart redis"),
            (&["brew", "cleanup", ""], "Cleanup redis"),
            (&["brew", ""], "Cleanup redis"),
        ];
        for (site, description) in cases {
            let list = tokens(site);
            let rows = rows(brew_services(stdout, HookSite::tokens(&list)));
            assert_eq!(rows[1]["description"], json!(description), "{site:?}");
        }
    }

    #[test]
    fn tailscale_file_cp_appends_the_host_colon() {
        let stdout = r#"{"Peer":{"a":{"DNSName":"laptop.tail.ts.net.","HostName":"laptop","OS":"macOS"}}}"#;
        let list = tokens(&["tailscale", "file", "cp", "notes.txt", ""]);
        assert_eq!(
            names(&rows(tailscale_peers(stdout, HookSite::tokens(&list)))),
            ["laptop:"]
        );
        let list = tokens(&["tailscale", "ip", ""]);
        assert_eq!(
            names(&rows(tailscale_peers(stdout, HookSite::tokens(&list)))),
            ["laptop"]
        );
    }

    #[test]
    fn cf_lines_pick_the_header_height_from_the_generator_script() {
        let stdout = "Getting…\n\nname\nalpha\nbeta";
        let list = tokens(&["cf", "target", "-o", ""]);
        let cases: [(&[&str], &str, &[&str]); 4] = [
            (&["cf", "orgs"], "Org", &["alpha", "beta"]),
            (&["cf", "spaces"], "Space", &["alpha", "beta"]),
            (&["bash", "-c", "cf services | cut -d \" \" -f1 "], "Service", &["beta"]),
            (&["bash", "-c", "cf apps | cut -d \" \" -f1"], "App name", &["beta"]),
        ];
        for (script, description, expected) in cases {
            let script = tokens(script);
            let rows = rows(cf_lines(
                stdout,
                HookSite {
                    tokens: &list,
                    script: &script,
                },
            ));
            assert_eq!(names(&rows), expected, "{script:?}");
            assert!(rows.iter().all(|row| row["description"] == json!(description)));
        }
        // Fewer lines than the header: `slice(n)` past the end is `[]`.
        let script = tokens(&["cf", "orgs"]);
        assert!(
            rows(cf_lines(
                "one\ntwo",
                HookSite {
                    tokens: &list,
                    script: &script,
                },
            ))
            .is_empty()
        );
    }
}
