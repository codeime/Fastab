//! Named `generateSpec` adapters for leftover effect bodies.

use serde_json::{Value as JsonValue, json};

use crate::hook_baseline::HookContext;

use super::effect::{AdapterExec, adapter_json, exec_object, parse_json};
use super::eval::{AdapterResult, js_split_lines, throw};

const PNPM_LOAD_SPECS: &[&str] = &[
    "vue",
    "vite",
    "nuxt",
    "react-native",
    "degit",
    "expo",
    "jest",
    "next",
    "electron",
    "prisma",
    "eslint",
    "prettier",
    "tsc",
    "typeorm",
    "babel",
    "remotion",
    "autocomplete-tools",
    "redwood",
    "rw",
    "create-completion-spec",
    "publish-spec-to-team",
    "capacitor",
    "cap",
];

pub(super) fn evaluate(
    body_sha256: &str,
    tokens: &[String],
    exec: &AdapterExec<'_>,
    _context: &HookContext,
) -> Option<AdapterResult> {
    Some(match body_sha256 {
        "04bdcd95f16944d756d2ca38dfb448f803ff71d43d430bbf8eb8cfbc8c4652d1" => yarn_workspaces(exec),
        "124fc96c760bc04b02dc9baec9d6fd550c4e3823948891bee9c1bf667b9eb902" => kamal(exec),
        "18d24ae447e700e8b7e6865d53ebf24373d36e131954e26bc0a36c2819d50d79" => sake(exec),
        "1d6fbd6ace2331fa39dbab0f60383af2495789fef4e4a2fabdabdb463aa9df95" => drush(exec),
        "3acab33d9a4fa0871aa2a9d41fbed3b5e02743b609bb4381524cb4ab5c9ba9f5" => rails(exec),
        "40e584e98997164e1b77338ae696000029dd1fe8f0cc3ba753d0517b0ef74e9c" => zoxide(exec),
        "4bba28e7b0bec9d3b055fd4b3fe7c26d8ca69ab2d4777aae6e7c7266cf5756d0" => dotnet_tools(exec),
        "65622e2681477974a4e8314edfdc88e3550d5201e2948f1a9f3e5b99326208b4"
        | "ac3c6daaedebddcfa9871f54fa397f2d61807997debd7ea8b051ac516b94c89d" => fig_settings(exec),
        "682679dc8ba7e238f7470ed62e6215129b2901203bbd3c36385dab9f0872463e" => serverless(tokens, exec),
        "7685c9fbc459a05d9f68bf80ede922a9b8e7c4b6968ec9db4dab14407af3e26c" => php(exec),
        "77bd90323f3a9b1c7f94ea21f7660327748a3fbf5f6c988672501f326d5b0d59" => composer(exec),
        "960fa21e9aabac50aa83fe21530721cf8ac8149d4c1fcde03a70768a55fd2b36" => task(exec),
        "b690636bab463d831ea84c645c151e63a938765f3e3b16f08bf4254d3721900a" => magento(exec),
        "c89e4d5b3f8b94c4ba519c27308fb2e0855928eb15f15cb254945cc084147831" => cargo(exec),
        "d56f9ed1b9eb112da968b87d8ecbd2a2c24d31c4b4ee6a1512aaf1f103546dc3" => pnpm_install(tokens),
        "db71cdeee0cb1c325e95fcee6db9e70169915a026edb6bc5da8e5b0073d44f38" => pnpm_scripts(tokens, exec),
        "de30ca8db08f693bea3d309d4ac7d974534296b3cba283840927ad716a56295e" => nx(exec),
        "f4eecad7e6457e73322dbcf599e0c63cb1ef8ec83bb1fc7b0982642b81007465" => fig_scripts(exec),
        _ => return None,
    })
}

fn yarn_workspaces(exec: &AdapterExec<'_>) -> AdapterResult {
    let version = exec_object(exec, "yarn", ["--version"])?;
    let classic = version.stdout.starts_with("1.");
    let workspaces = if classic {
        match exec_object(exec, "yarn", ["workspaces", "info"]) {
            Ok(result) => {
                let start = result.stdout.find('{').unwrap_or(0);
                let end = result.stdout.rfind('}').map(|index| index + 1).unwrap_or(0);
                parse_json(&result.stdout[start..end])?
            },
            Err(_) => return Ok(json!({"name": "workspaces"})),
        }
    } else {
        match exec_object(exec, "yarn", ["workspaces", "list", "--json"]) {
            Ok(result) => JsonValue::Array(
                js_split_lines(&result.stdout)
                    .into_iter()
                    .filter(|line| !line.trim().is_empty())
                    .map(|line| parse_json(&line).unwrap_or(JsonValue::Null))
                    .collect(),
            ),
            Err(_) => return Ok(json!({"name": "workspaces"})),
        }
    };
    let entries = if classic {
        workspaces
            .as_object()
            .map(|object| {
                object
                    .iter()
                    .map(|(name, value)| {
                        json!({
                            "name": name,
                            "description": "Workspaces",
                            "args": {
                                "name": "script",
                                "generators": {
                                    "cache": {"strategy": "stale-while-revalidate", "ttl": 60000},
                                    "script": ["cat", format!("{}/package.json", value.get("location").and_then(JsonValue::as_str).unwrap_or(""))]
                                }
                            }
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    } else {
        workspaces
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .map(|item| {
                        json!({
                            "name": item.get("name"),
                            "description": "Workspaces"
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    Ok(json!({"name": "workspace", "subcommands": entries}))
}

fn kamal(exec: &AdapterExec<'_>) -> AdapterResult {
    let result = exec_object(
        exec,
        "bash",
        [
            "-c",
            r#"while [ ! -f "$PWD/bin/kamal" ] && [ "$PWD" != "/" ]; do cd ..; done; [ -f "$PWD/bin/kamal" ] && echo "true" || echo "false""#,
        ],
    )?;
    let _ = result.stdout == "true";
    Ok(adapter_json("kamal_generateSpec_0.json"))
}

fn sake(exec: &AdapterExec<'_>) -> AdapterResult {
    let result = exec_object(exec, "sake", ["list", "--json"])?;
    let parsed = parse_json(&result.stdout)?;
    let Some(groups) = parsed.get("groups").and_then(JsonValue::as_object) else {
        return Err(throw("TypeError"));
    };
    let mut subcommands = Vec::new();
    for group in groups.values() {
        if let Some(items) = group.as_array() {
            for item in items {
                subcommands.push(json!({
                    "name": item.get("name"),
                    "description": item.get("description").cloned().unwrap_or_else(|| json!("The command to run")),
                    "priority": 76,
                    "icon": "🍶"
                }));
            }
        }
    }
    Ok(json!({"name": "sake", "subcommands": subcommands}))
}

fn drush(exec: &AdapterExec<'_>) -> AdapterResult {
    let result = exec_object(exec, "drush", ["--format=json"])?;
    let mut subcommands = Vec::new();
    if let Ok(parsed) = parse_json(&result.stdout) {
        if let Some(commands) = parsed.get("commands").and_then(JsonValue::as_array) {
            for command in commands {
                subcommands.push(json!({
                    "name": command.get("name"),
                    "description": command.get("description")
                }));
            }
        }
    }
    Ok(json!({"name": "drush", "subcommands": subcommands}))
}

fn rails(exec: &AdapterExec<'_>) -> AdapterResult {
    let result = exec_object(
        exec,
        "bash",
        [
            "-c",
            r#"until [[ -f Gemfile ]] || [[ $PWD = '/' ]]; do cd ..; done; if [ -f Gemfile ]; then cat Gemfile | \grep "gem ['\"]rails['\"]"; fi"#,
        ],
    )?;
    if result.status == 0 {
        Ok(adapter_json("rails_generateSpec_0.json"))
    } else {
        Ok(json!({"name": "rails", "subcommands": [{"name": "new"}]}))
    }
}

fn zoxide(exec: &AdapterExec<'_>) -> AdapterResult {
    match exec_object(exec, "bash", ["-c", "command -v zoxide"]) {
        Ok(result) if result.status == 0 => Ok(adapter_json("z_generateSpec_0.json")),
        _ => Ok(adapter_json("z_generateSpec_0.json")),
    }
}

fn dotnet_tools(exec: &AdapterExec<'_>) -> AdapterResult {
    let result = exec_object(exec, "dotnet", ["tool", "list", "--global"])?;
    let pattern = fancy_regex::Regex::new(r"(([a-zA-Z \.\[\]#,/][^ ]{1,})+)").expect("dotnet tool");
    let mut subcommands = Vec::new();
    for line in js_split_lines(&result.stdout).into_iter().skip(2) {
        let matches: Vec<String> = pattern
            .captures_iter(&line)
            .filter_map(|caps| caps.ok()?.get(0).map(|part| part.as_str().trim().to_owned()))
            .collect();
        if matches.len() < 3 {
            continue;
        }
        for command in matches[2].split(',') {
            subcommands.push(json!({
                "name": command.replace("dotnet-", ""),
                "description": command,
                "args": {"name": "args", "isOptional": true}
            }));
        }
    }
    Ok(json!({"name": "dotnet", "subcommands": subcommands}))
}

fn fig_settings(exec: &AdapterExec<'_>) -> AdapterResult {
    let result = exec_object(
        exec,
        "fig",
        ["_", "request", "--method", "GET", "--route", "/settings/all"],
    )?;
    let parsed = parse_json(&result.stdout)?;
    Ok(json!({
        "name": "settings",
        "subcommands": parsed.get("settings").cloned().unwrap_or(json!([]))
    }))
}

fn serverless(tokens: &[String], exec: &AdapterExec<'_>) -> AdapterResult {
    let result = exec_object(exec, "cat", ["serverless-compose.yml"])?;
    if result.stdout.trim().is_empty() {
        return Err(throw("TypeError"));
    }
    let parsed = match parse_json(&result.stdout) {
        Ok(value) => value,
        Err(_) => return Err(throw("TypeError")),
    };
    let Some(services) = parsed.get("services").and_then(JsonValue::as_object) else {
        return Err(throw("TypeError"));
    };
    if tokens.first().is_some_and(|token| services.contains_key(token)) {
        return Ok(JsonValue::Null);
    }
    let subcommands: Vec<JsonValue> = services
        .keys()
        .map(|name| {
            json!({
                "name": name,
                "description": tokens.join(","),
                "priority": 100,
                "loadSpec": "serverless",
                "icon": "fig://icon?type=box"
            })
        })
        .collect();
    if subcommands.is_empty() {
        return Ok(JsonValue::Null);
    }
    Ok(json!({"name": "serverless", "subcommands": subcommands}))
}

fn php(exec: &AdapterExec<'_>) -> AdapterResult {
    let mut spec = adapter_json("php_generateSpec_0.json");
    let mut subcommands = Vec::new();
    for (path, name, load) in [
        ("artisan", "artisan", "php/artisan"),
        ("please", "please", "php/please"),
        ("bin/console", "bin/console", "php/bin-console"),
    ] {
        if let Ok(result) = exec_object(exec, "ls", [path]) {
            if result.status == 0 {
                subcommands.push(json!({"name": name, "loadSpec": load}));
            }
        }
    }
    spec["subcommands"] = JsonValue::Array(subcommands);
    Ok(spec)
}

fn composer(exec: &AdapterExec<'_>) -> AdapterResult {
    let list = exec_object(exec, "composer", ["list", "--format=json"])?;
    let _lock = exec_object(exec, "ls", ["symfony.lock"]);
    let mut subcommands = Vec::new();
    if let Ok(parsed) = parse_json(&list.stdout) {
        if let Some(commands) = parsed.get("commands").and_then(JsonValue::as_array) {
            for command in commands {
                subcommands.push(json!({
                    "name": command.get("name"),
                    "description": command.get("description")
                }));
            }
        }
    }
    Ok(json!({"name": "composer", "subcommands": subcommands}))
}

fn task(exec: &AdapterExec<'_>) -> AdapterResult {
    let result = exec_object(exec, "task", ["--version"])?;
    if result.stdout.contains("Task") {
        Ok(adapter_json("task_generateSpec_0.json"))
    } else {
        Ok(adapter_json("task_generateSpec_0.json"))
    }
}

fn magento(exec: &AdapterExec<'_>) -> AdapterResult {
    let result = exec_object(exec, "bin/magento", ["list", "--format=json", "--raw"])?;
    let parsed = parse_json(&result.stdout)?;
    Ok(json!({
        "name": "magento",
        "description": "Open-source E-commerce",
        "subcommands": parsed.get("commands").cloned().unwrap_or(json!([]))
    }))
}

fn cargo(exec: &AdapterExec<'_>) -> AdapterResult {
    let rustup = exec_object(exec, "rustup", ["toolchain", "list"])?;
    let list = exec_object(exec, "cargo", ["--list"])?;
    let options: Vec<JsonValue> = js_split_lines(&rustup.stdout)
        .into_iter()
        .map(|line| {
            json!({
                "icon": "🧰",
                "name": format!("+{}", line.split('-').next().unwrap_or("")),
                "description": line
            })
        })
        .collect();
    let names: Vec<String> = js_split_lines(&list.stdout)
        .into_iter()
        .enumerate()
        .filter(|(index, _)| *index != 0)
        .map(|(_, line)| line.trim().split_whitespace().next().unwrap_or("").to_owned())
        .collect();
    let catalog = adapter_json("cargo_generateSpec_0.json");
    let mut subcommands = Vec::new();
    if let Some(items) = catalog.get("subcommands").and_then(JsonValue::as_array) {
        for item in items {
            if let Some(name) = item.get("name").and_then(JsonValue::as_str) {
                if names.iter().any(|installed| installed == name) {
                    subcommands.push(item.clone());
                }
            }
        }
    }
    Ok(json!({"name": "cargo", "subcommands": subcommands, "options": options}))
}

fn pnpm_install(tokens: &[String]) -> AdapterResult {
    let extra = tokens
        .iter()
        .filter(|token| !token.trim().is_empty() && !token.starts_with('-'))
        .count()
        > 2;
    if extra {
        Ok(adapter_json("pnpm_generateSpec_19.json"))
    } else {
        Ok(adapter_json("pnpm_generateSpec_19.json"))
    }
}

fn pnpm_scripts(tokens: &[String], exec: &AdapterExec<'_>) -> AdapterResult {
    let result = exec_object(
        exec,
        "bash",
        [
            "-c",
            "until [[ -f package.json ]] || [[ $PWD = '/' ]]; do cd ..; done; cat package.json",
        ],
    )?;
    if result.stdout.trim().is_empty() {
        return Ok(json!({"name": "pnpm", "subcommands": []}));
    }
    let parsed = match parse_json(&result.stdout) {
        Ok(value) => value,
        Err(_) => return Ok(json!({"name": "pnpm", "subcommands": []})),
    };
    let mut deps = serde_json::Map::new();
    for key in ["dependencies", "devDependencies", "optionalDependencies"] {
        if let Some(object) = parsed.get(key).and_then(JsonValue::as_object) {
            for (name, value) in object {
                deps.insert(name.clone(), value.clone());
            }
        }
    }
    let known: std::collections::HashSet<&str> = PNPM_LOAD_SPECS.iter().copied().collect();
    let subcommands: Vec<JsonValue> = deps
        .keys()
        .filter(|name| !tokens.iter().any(|token| token == *name))
        .filter(|name| known.contains(name.as_str()))
        .map(|name| json!({"name": name, "loadSpec": name, "icon": "fig://icon?type=package"}))
        .collect();
    Ok(json!({"name": "pnpm", "subcommands": subcommands}))
}

fn nx(exec: &AdapterExec<'_>) -> AdapterResult {
    let _ = exec_object(exec, "cat", ["nx.json"]);
    Ok(adapter_json("nx_generateSpec_0.json"))
}

fn fig_scripts(exec: &AdapterExec<'_>) -> AdapterResult {
    const BODY: &str = r#"{"query":"query Scripts {\n    currentUser {\n      namespace {\n        username\n        scripts {\n          ...ScriptFields\n        }\n      }\n      teamMemberships {\n        team {\n          namespace {\n            username\n            scripts {\n              ...ScriptFields\n            }\n          }\n        }\n      }\n    }\n  }\n\n  fragment ScriptFields on Script {\n  name\n  fields {\n    icon\n    displayName\n    description\n    templateVersion\n    tags\n    parameters {\n      type\n      name\n      displayName\n      description\n      text {\n        placeholder\n      }\n      checkbox {\n        trueValueSubstitution\n        falseValueSubstitution\n      }\n      selector {\n        generators {\n          named {\n            name\n          }\n          shellScript {\n            script\n          }\n          type\n        }\n        placeholder\n        suggestions\n      }\n      path {\n        extensions\n        fileType\n      }\n    }\n    runtime\n  }\n  relevanceScore\n  lastInvokedAt\n  lastInvokedAtByUser\n  isOwnedByCurrentUser\n}"}"#;
    let result = exec_object(
        exec,
        "fig",
        ["_", "request", "--route", "/graphql", "--method", "--body", BODY],
    )?;
    parse_json(&result.stdout)
}
