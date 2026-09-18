//! Named `custom` adapters for leftover effect bodies.

use std::collections::{HashMap, HashSet};

use fancy_regex::Regex;
use serde_json::{Value as JsonValue, json};

use crate::hook_types::HookContext;

use super::effect::{
    AdapterExec, AdapterExecRequest, HookSite, JsMap, adapter_list, env_var, exec_object, key_value, key_value_list,
    key_value_list_chooses_keys, last_token, named_strings, owning_option, parse_json, site_option, value_list,
};
use super::eval::{AdapterError, AdapterResult, js_split_lines, suggestion_object, throw};

const KNOWN_HOST: &str = r"(?:[a-zA-Z0-9-]+\.)+[a-zA-Z0-9]+";

pub(super) fn evaluate(
    body_sha256: &str,
    tokens: &[String],
    exec: &AdapterExec<'_>,
    context: &HookContext,
) -> Option<AdapterResult> {
    Some(match body_sha256 {
        "03b126c52218b618b258b4113e23cf47ef5a0197646c1f48f7983f6bf146ead8" => make_targets(exec),
        "082bc08b4bdda4806561b75e8cc0731eebc9d4400f2113e5c7be189485a43558" => scc_key_value_list(tokens, exec),
        "111e4cf667dc7a906760475f6dd4eca45bb903941a433cc4775e2f8c2cdb7d40" => value_list(
            &last_token(tokens),
            ":",
            &adapter_list("man-sections.json"),
            true,
            false,
        ),
        "1479ee375b43e8542737587098c3443c5b480cddafb76ed3a5e1d458c119e608" => pkgutil_files(tokens, exec),
        "15969249b10aedd33ec7f60746954920fc08b8e25e0671ca53aaf51ac946ce37" => nx_project_names(exec),
        "1a93472e0d5a250a7a3ba467851694ca669a32bb9d509dfabcd5528b74702941" => git_or_remote_branches(tokens, exec),
        "1b1c58278249c1860643584345a4e2999f6f809c8700b284e7a26ce6cbd6fa20" => spring_versions(exec, "bootVersion"),
        "1ddb3946a1205a82600a80ab80897691a797873651f7afb8ff97a0159c139fe7" => value_list(
            &last_token(tokens),
            ",",
            &adapter_list("chezmoi-includes.json"),
            true,
            false,
        ),
        "20e727a6b059f8d895ae13dcba72e4308f8935db13897c58057e7805f0182e1e" => deno_tasks(tokens, exec),
        "2176333a01195561f613dca132852a71d43c790bdd316c99b3533f02ba9fff46" => value_list(
            &last_token(tokens),
            ",",
            &adapter_list("esbuild_custom_4.json"),
            false,
            false,
        ),
        "25d872a1238176acad55fdc6ab4925964829c5b33510679a754b1de0a5af8008"
        | "61c3b8a8846c19ae82d4c833b36fec1e52cebac1e8ebbd976587556ea427847d"
        | "a0947a29c5844aa06f36b4666c32b578a97663219aa1b47687c2916c0102d57b" => ssh_config_hosts(exec, context),
        "2a1148465e4a6f039eaf6a53448cf72d9ddcbe1de292c76c631364edda8ef870" => ykman_modes(tokens),
        "2d5b190a7bae97d96654250225aaefb33dde63770d47876954c52e32ddbc255e" => firefox_port(tokens),
        "2ed14222e43605a4615b0d098db0dca8fb7016d90702476658c98a8d3397be57"
        | "353cc07bf9fb8f63179771f62005442bc7b3c5f0e22102ff5f567156226a0366"
        | "5395d7fb1d0b608ce456527e703936e65ead88c78e1d64d17e5223015e947473"
        | "cd92b7bcc8f44295973c4b7bfc5a4851b633bff0e57f4ee98cfe91680c2b82ac"
        | "e299db05c81077e0ef9b1344daef841fa83f1849ad704e508af664ed388ce510"
        | "f6c6c160699c68fe7cf0500219e46b8c6c7b327c7db559d3632e1de31cf40fc4" => npms_search(tokens, exec, &[]),
        "ad600744147080b3bbb0ee8363c354bcc1ec7198ac5a9b16ccbf6285f70b82eb" => {
            npms_search(tokens, exec, &["magnolia-light-module"])
        },
        "2fadd1a502cd5f4ba4a55f8bdc1ee236ea4d572c77e2188e7cdcb3c77856036d" => key_value(
            &last_token(tokens),
            "=",
            &adapter_list("file_custom_0.json"),
            &named_strings(&["1048576", "256", "2048", "65536", "50", "60", "8192"]),
            true,
        ),
        "4df08d834f2264ea2b300b6b8e847fe753da5c9a5ecfaebf87596a84d584625d" => spring_versions(exec, "javaVersion"),
        "4e14f9a18a339f1a0ff2799de460a98abbbd4f754e729069e600065d6b23bf9f" => dscacheutil_query(tokens, exec),
        "5104550a1de4f3c8210aefeca860ac9ba05a66d588d3940c98b15fb473090866" => tldr_pages(exec, context),
        "5693d96360a2e105298439a7a22abbde058eaa41e74ef120f1b1d722e2eedbf6"
        | "faf81d363c15195974dc41b68bfaeb799bc2a67b0d6143555695df0e4bce9827" => fig_plugins_for_site(tokens, exec),
        "5aa8d63e46c44f4cf86dbb62c9035c2a33506a4aa04da4b928adf5ef102b2a9a" => {
            value_list(&last_token(tokens), ",", &[], false, false)
        },
        "5c415cacc2e9cfe61ff86d5ec428d6b0089216ab3e13fd1acb53ed513b9e2606" => chezmoi_attrs(tokens),
        "63f4199d5d1d44c71d013364bf8dbb477411f11d776cde35821ca712569034c0" => scc_value_list(tokens, exec),
        "63f7ebc4331aeef1adf022c2217ba0c013735b99a32015c475374c61c076ff95" => oxlint_rules(tokens, exec),
        "661801a6a065f6a47a99557d48b30b8ddaf73a2b08a407d4db2b1434196c8fc9" => gh_port_key_value(tokens),
        "7118b51f5ab4abf816ec0fd32b660d4495361defafb617d48a91bab93e74d914" => {
            key_value(&last_token(tokens), "=", &[], &[], true)
        },
        "7238b45ae90afed81b58fe2198530b0074949e85863636d179d805d7fe23d1c0" => key_value_list(
            &last_token(tokens),
            ":",
            ",",
            &adapter_list("airflow_custom_0.json"),
            &named_strings(&["password"]),
            true,
            false,
            false,
            true,
        ),
        "7cdfa0e8c803cb9e4c6176f51aa7730cdd864347528547cf8291722c94fd8784" => nx_comma_targets(tokens, exec),
        "7d44d284dc19be5e46e12cea613a04edbc98e806a318d94d52a38b0c296c9927" => {
            Ok(JsonValue::Array(adapter_list("oxlint-categories.json")))
        },
        "83fee23ebcadbf1d63827dd335c7cf0b8a40bbc3d7266022e9ad49e4e231e1ae" => esbuild_key_value_list(tokens, exec),
        "84025fe23bbe188d73039e3d402687d1f0b39418ad70f09a1e4e6ed495038bb3" => value_list(
            &last_token(tokens),
            ",",
            &adapter_list("deno-lint-rules.json"),
            true,
            false,
        ),
        "9523c6ca2b912383473caefc1b9f7eea0a712e3e648b161621a7b945277df483" => goto_aliases(exec, context),
        "9c22acfa8a0ec3748d93b0fa87f7aca3df2f20e71e41da53f60dacae3209e570" => git_flow_branches(tokens, exec),
        "a22d06a56581e86c16f9dc836857cc1c418d4c19cf441b9207b8b333aac56608" => spring_dependencies(tokens, exec),
        "a66254e7246fa779b200beb5f1907554b3cdafd35415f8bb20cfe87ae545a01c" => fig_ai(tokens, exec, context),
        "add93fb6b6f290a3c95e19f260fff11c67a5039441066c0c3e97c39663b7d9fb" => twilio_tree(tokens),
        "aeef663f3f913be3aae37442b98f532b8495d5cfbe89c42e731c48d60e2fa291" => nx_colon_targets(tokens, exec),
        "b5a3021322137127b1384a405317a1e561d6f8da31e33d391262f8bb12350849" => rich_styles(tokens),
        "b78df2cf1a8bb3a6e158fed0c9f86cc6ab1a59af3b1c78902a272db13ffe65e1" => man_apropos(tokens, exec),
        "c7306386562b66c67f69ba7a123f8a4116b4e523cbe4160c6e20193fda412fab"
        | "d928c7d8792f6d2a77531253fa3a5cce3b7cf3b0f255bddb090cbd33dd38cb12"
        | "ece47a53affb3d4f9b613f64e9e2e6b2dfa848b291ac79d4618a66c06f4769d6" => known_hosts(tokens, exec, context),
        "d972eb5f899bdf11a9a76db260c3984cd04d2ab1d4d0a2d98661c1f1a56530b3" => value_list(
            &last_token(tokens),
            ",",
            &adapter_list("osqueryi-tables.json"),
            false,
            false,
        ),
        "dc40f21f14541d8f6743f532137900894daa56f7d8fede9edadae126dfc8074b" => key_value_list(
            &last_token(tokens),
            "=",
            ",",
            &adapter_list("bun_custom_15.json"),
            &named_strings(&["js", "jsx", "ts", "tsx", "json", "toml", "text", "file", "wasm", "napi"]),
            true,
            false,
            false,
            true,
        ),
        "ddf08d87219a1404bfeec7328e0a92e5d615202ab064e51081c6a2181619777e" => cargo_crates(tokens, exec),
        "e0f755fcbcdd4aa9db41d69f013e92b961ad64f347b05a750718d30d8a851634" => cargo_targets(tokens, exec, context),
        "f4cb5caf468f2f8af4b0b33e6bb43d9ccf8af8bc4d230519b947b17cd7043111" => rich_box(tokens),
        "fb26b35043ba67c50e86361dc595f1ca096a2c9b46796349d8eafbee0939e4c7" => dscl_list(tokens, exec),
        "fde524eda218ac28c5bce41a27df6ac6bdc9254da9e25f8dcb5880c8b7026c18" => nx_run_configurations(tokens, exec),
        "fde95bc4219fd9ff0a036e870a458c96dd54ba80ebe06fb725b1224fd0b51ebf" => cargo_config(tokens),
        _ => return None,
    })
}

fn suggestion_from_name(name: impl Into<String>) -> JsonValue {
    suggestion_object(name, &[])
}

fn make_targets(exec: &AdapterExec<'_>) -> AdapterResult {
    let first = exec_object(
        exec,
        "bash",
        [
            "-c",
            "make -qp | awk -F':' '/^[a-zA-Z0-9][^$#\\/\\t=]*:([^=]|$)/ {split($1,A,/ /);for(i in A)print A[i]}' | sort -u",
        ],
    )?;
    let mut targets = JsMap::<JsonValue>::new();
    for line in js_split_lines(&first.stdout) {
        if line == "Makefile" {
            continue;
        }
        targets.insert(
            line.clone(),
            json!({
                "name": line.trim(),
                "description": "Make target",
                "icon": "🎯",
                "priority": 80
            }),
        );
    }
    let second = exec_object(exec, "cat", ["Makefile", "makefile"])?;
    let special = HashSet::from([
        ".PHONY",
        ".SUFFIXES",
        ".DEFAULT",
        ".PRECIOUS",
        ".INTERMEDIATE",
        ".SECONDARY",
        ".SECONDEXPANSION",
        ".DELETE_ON_ERROR",
        ".IGNORE",
        ".LOW_RESOLUTION_TIME",
        ".SILENT",
        ".EXPORT_ALL_VARIABLES",
        ".NOTPARALLEL",
        ".ONESHELL",
        ".POSIX",
    ]);
    // The spec's `/gm` regex: `^` and `$` are per line, or the second pass
    // would only ever see the first recipe of the Makefile.
    let pattern =
        Regex::new(r"(?m)((?:^#.*\n)*)(?:^\.[A-Z_]+:.*\n)*(^\S*?):.*?(?:\s#+[ \t]*(.+))?$").expect("make recipe regex");
    let variable = Regex::new(r"\$\(.+?\)").expect("make variable regex");
    let comment_marks = Regex::new(r"(?m)^#+\s*").expect("make comment regex");
    for caps in pattern.captures_iter(&second.stdout) {
        let caps = match caps {
            Ok(caps) => caps,
            Err(_) => continue,
        };
        let name = caps.get(2).map(|part| part.as_str()).unwrap_or("");
        if special.contains(name) || variable.is_match(name).unwrap_or(false) {
            continue;
        }
        // JS: `o ? o.trim() : r ? r.replace(/^#+\s*/gm, "").trim() : "Make target"`.
        // An unmatched optional group is `undefined` (falsy); a matched but
        // empty header is `""`, also falsy.
        let comment = caps.get(3).map(|part| part.as_str().trim().to_owned());
        let header = caps.get(1).map(|part| part.as_str().to_owned());
        let description = comment
            .or_else(|| {
                header
                    .filter(|text| !text.is_empty())
                    .map(|text| comment_marks.replace_all(&text, "").trim().to_owned())
            })
            .unwrap_or_else(|| "Make target".into());
        let trimmed = name.trim().to_owned();
        targets.insert(
            trimmed.clone(),
            json!({
                "name": trimmed,
                "description": description,
                "icon": "🎯",
                "priority": 80
            }),
        );
    }
    Ok(JsonValue::Array(targets.into_values()))
}

/// scc's `z`: `scc --language` prints one `Name (ext,ext,…)` per line. The
/// languages keep print order; the extension map is a plain object written
/// in print order, a later language taking over an extension in place.
struct SccLanguages {
    extensions: JsMap<String>,
    languages: Vec<String>,
}

fn scc_languages(exec: &AdapterExec<'_>) -> Result<SccLanguages, AdapterError> {
    let result = exec_object(exec, "scc", ["--language"])?;
    let line = Regex::new(r"(?m)^(.*) \((.*)\)$").expect("scc language regex");
    let mut extensions = JsMap::new();
    let mut languages = Vec::new();
    for caps in line.captures_iter(&result.stdout) {
        let Ok(caps) = caps else { continue };
        let language = caps.get(1).map_or("", |part| part.as_str());
        languages.push(language.to_owned());
        for extension in caps.get(2).map_or("", |part| part.as_str()).split(',') {
            extensions.insert(extension.to_owned(), language.to_owned());
        }
    }
    Ok(SccLanguages { extensions, languages })
}

/// scc's `N`: the language names as bare suggestions.
fn scc_language_values(exec: &AdapterExec<'_>) -> Result<Vec<JsonValue>, AdapterError> {
    Ok(scc_languages(exec)?
        .languages
        .into_iter()
        .map(suggestion_from_name)
        .collect())
}

/// `Object.entries(extensions)` shaped as suggestions; `--include-ext` adds
/// the string icon, `--count-as` does not.
fn scc_extension_rows(exec: &AdapterExec<'_>, icon: Option<&str>) -> Result<Vec<JsonValue>, AdapterError> {
    Ok(scc_languages(exec)?
        .extensions
        .entries()
        .map(|(extension, language)| {
            let mut row = json!({ "name": extension, "description": language });
            if let Some(icon) = icon {
                row["icon"] = json!(icon);
            }
            row
        })
        .collect())
}

/// The ten `--format` names, each with the string icon (`Y` in the spec).
fn scc_formats() -> Vec<JsonValue> {
    [
        "tabular",
        "wide",
        "json",
        "csv",
        "csv-stream",
        "cloc-yaml",
        "html",
        "html-table",
        "sql",
        "sql-insert",
    ]
    .into_iter()
    .map(|name| json!({ "name": name, "icon": "fig://icon?type=string" }))
    .collect()
}

/// `--format-multi`'s value side: every line of `ls -lAF1` named by what
/// follows its last `/`, then a `stdout` row at priority 75.
fn scc_format_outputs(exec: &AdapterExec<'_>) -> Result<Vec<JsonValue>, AdapterError> {
    let result = exec_object(exec, "ls", ["-lAF1"])?;
    let mut rows: Vec<JsonValue> = result
        .stdout
        .split('\n')
        .map(|line| {
            let name = &line[line.rfind('/').map_or(0, |index| index + 1)..];
            json!({ "name": name, "icon": format!("fig://path/{line}") })
        })
        .collect();
    rows.push(json!({ "name": "stdout", "priority": 75 }));
    Ok(rows)
}

/// The `keyValueList({separator: ":"})` body scc shares across four
/// options. Each site closed over different `keys` / `values`, so the
/// owning option picks them (see `owning_option`).
fn scc_key_value_list(tokens: &[String], exec: &AdapterExec<'_>) -> AdapterResult {
    let token = last_token(tokens);
    // Only the side being completed is evaluated, as the factory's `E` does;
    // `--format-multi` must not run `ls` while its keys are being chosen.
    let choosing_keys = key_value_list_chooses_keys(&token, ":", ",");
    let (keys, values) = match owning_option(tokens) {
        Some("--format-multi") => {
            if choosing_keys {
                (scc_formats(), Vec::new())
            } else {
                (Vec::new(), scc_format_outputs(exec)?)
            }
        },
        Some("--remap-all" | "--remap-unknown") => {
            if choosing_keys {
                (Vec::new(), Vec::new())
            } else {
                (Vec::new(), scc_language_values(exec)?)
            }
        },
        // `--count-as`, the representative site the T1.2 baseline was
        // captured against, doubles as the answer for a token list that
        // names no option.
        _ => {
            if choosing_keys {
                (scc_extension_rows(exec, None)?, Vec::new())
            } else {
                (Vec::new(), scc_language_values(exec)?)
            }
        },
    };
    key_value_list(&token, ":", ",", &keys, &values, true, false, false, true)
}

/// `-i` / `--include-ext`: `valueList` over the extension rows.
fn scc_value_list(tokens: &[String], exec: &AdapterExec<'_>) -> AdapterResult {
    let values = scc_extension_rows(exec, Some("fig://icon?type=string"))?;
    value_list(&last_token(tokens), ",", &values, false, false)
}

fn pkgutil_files(tokens: &[String], exec: &AdapterExec<'_>) -> AdapterResult {
    let Some(index) = tokens.iter().position(|token| token == "--edit-pkg") else {
        return Ok(json!([]));
    };
    if index + 1 >= tokens.len() {
        return Ok(json!([]));
    }
    let package = &tokens[index + 1];
    let prefix = last_token(tokens);
    let result = exec_object(exec, "pkgutil", ["--files", package])?;
    Ok(JsonValue::Array(
        js_split_lines(&result.stdout)
            .into_iter()
            .filter(|line| prefix.is_empty() || line.starts_with(&prefix))
            .map(suggestion_from_name)
            .collect(),
    ))
}

struct NxGraph {
    projects: JsMap<Vec<String>>,
    configurations: HashMap<String, Vec<String>>,
    targets: JsMap<Vec<String>>,
}

fn nx_graph(exec: &AdapterExec<'_>) -> NxGraph {
    let mut graph = NxGraph {
        projects: JsMap::new(),
        configurations: HashMap::new(),
        targets: JsMap::new(),
    };
    let Ok(nx_json) = exec_object(exec, "cat", ["nx.json"]) else {
        return graph;
    };
    let parsed = match parse_json(&nx_json.stdout) {
        Ok(value) => value,
        Err(_) => return graph,
    };
    let layout = parsed.get("workspaceLayout");
    let apps = layout
        .and_then(|value| value.get("appsDir"))
        .and_then(JsonValue::as_str)
        .unwrap_or("apps");
    let libs = layout
        .and_then(|value| value.get("libsDir"))
        .and_then(JsonValue::as_str)
        .unwrap_or("libs");
    let find_args = if apps == libs {
        vec![apps.to_owned(), "-name".into(), "project.json".into()]
    } else {
        vec![apps.to_owned(), libs.to_owned(), "-name".into(), "project.json".into()]
    };
    let Ok(found) = exec_object(exec, "find", find_args) else {
        return graph;
    };
    for path in js_split_lines(&found.stdout) {
        if path.is_empty() {
            continue;
        }
        let Ok(project_file) = exec_object(exec, "cat", [path]) else {
            continue;
        };
        let Ok(project) = parse_json(&project_file.stdout) else {
            continue;
        };
        let name = project.get("name").and_then(JsonValue::as_str).unwrap_or("").to_owned();
        if name.is_empty() {
            continue;
        }
        let mut target_names = Vec::new();
        if let Some(targets) = project.get("targets").and_then(JsonValue::as_object) {
            for (target, spec) in targets {
                target_names.push(target.clone());
                let mut configurations = Vec::new();
                if let Some(object) = spec.get("configurations").and_then(JsonValue::as_object) {
                    configurations.extend(object.keys().cloned());
                }
                graph.configurations.insert(format!("{name}:{target}"), configurations);
                graph.targets.entry_or_default(target.clone()).push(name.clone());
            }
        }
        graph.projects.insert(name, target_names);
    }
    graph
}

fn nx_project_names(exec: &AdapterExec<'_>) -> AdapterResult {
    let graph = nx_graph(exec);
    Ok(JsonValue::Array(
        graph.projects.keys().map(|name| suggestion_from_name(name)).collect(),
    ))
}

fn nx_comma_targets(tokens: &[String], exec: &AdapterExec<'_>) -> AdapterResult {
    let graph = nx_graph(exec);
    let token = last_token(tokens);
    let used: HashSet<&str> = token.split(',').collect();
    // `y(g)` under `--exclude` / `--projects` walks project names; `y(h)`
    // under `--targets` walks target names. The representative
    // `nx#custom#11` is `format:write --exclude`.
    let names: Vec<&String> = if site_option(tokens) == Some("--targets") {
        graph.targets.keys().collect()
    } else {
        graph.projects.keys().collect()
    };
    Ok(JsonValue::Array(
        names
            .into_iter()
            .filter(|name| !used.contains(name.as_str()))
            .map(|name| json!({"name": name, "insertValue": format!("{name},")}))
            .collect(),
    ))
}

fn nx_colon_targets(tokens: &[String], exec: &AdapterExec<'_>) -> AdapterResult {
    let graph = nx_graph(exec);
    let token = last_token(tokens);
    let parts: Vec<&str> = token.split(':').collect();
    let project = if parts.len() > 1 { parts[0] } else { "" };
    let target = if parts.len() > 2 { parts[1] } else { "" };
    let mut out = Vec::new();
    if project.is_empty() {
        for name in graph.projects.keys() {
            out.push(json!({"name": name, "insertValue": format!("{name}:")}));
        }
    } else if target.is_empty() {
        if let Some(targets) = graph.projects.get(project) {
            for name in targets {
                if graph.configurations.contains_key(&format!("{project}:{name}")) {
                    out.push(json!({"name": name, "insertValue": format!("{name}:")}));
                } else {
                    out.push(suggestion_from_name(name));
                }
            }
        }
    } else if let Some(configurations) = graph.configurations.get(&format!("{project}:{target}")) {
        out.extend(configurations.iter().map(suggestion_from_name));
    }
    Ok(JsonValue::Array(out))
}

fn nx_run_configurations(tokens: &[String], exec: &AdapterExec<'_>) -> AdapterResult {
    let graph = nx_graph(exec);
    let mut parts = Vec::new();
    if tokens.join(" ").starts_with("nx run") {
        if let Some(token) = tokens.get(2) {
            parts.extend(token.split(':').map(ToOwned::to_owned));
        }
    } else {
        if let Some(token) = tokens.get(2) {
            parts.push(token.clone());
        } else {
            parts.push(String::new());
        }
        if let Some(token) = tokens.get(1) {
            parts.push(token.clone());
        } else {
            parts.push(String::new());
        }
    }
    let project = parts.first().cloned().unwrap_or_default();
    let target = parts.get(1).cloned().unwrap_or_default();
    match graph.configurations.get(&format!("{project}:{target}")) {
        Some(configurations) => Ok(JsonValue::Array(
            configurations.iter().map(suggestion_from_name).collect(),
        )),
        None => Err(throw("TypeError")),
    }
}

fn strip_git_warnings(stdout: &str) -> String {
    if stdout.starts_with("warning:") || stdout.starts_with("error:") {
        js_split_lines(stdout)
            .into_iter()
            .skip(1)
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        stdout.to_owned()
    }
}

fn git_branch_rows(stdout: &str, insert_without_remotes: bool) -> Vec<JsonValue> {
    let cleaned = strip_git_warnings(stdout);
    if cleaned.starts_with("fatal:") {
        return Vec::new();
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for line in js_split_lines(&cleaned) {
        if line.trim().starts_with("HEAD") {
            continue;
        }
        let mut name = line.trim().to_owned();
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.len() > 1 {
            if words[0] == "*" {
                if line.contains("HEAD detached") {
                    continue;
                }
                out.push(json!({
                    "name": line.replace('*', "").trim(),
                    "description": "Current branch",
                    "priority": 100,
                    "icon": "⭐"
                }));
                if let Some(JsonValue::Object(object)) = out.last() {
                    if let Some(JsonValue::String(seen_name)) = object.get("name") {
                        seen.insert(seen_name.clone());
                    }
                }
                continue;
            }
            if words[0] == "+" {
                name = line.replace('+', "").trim().to_owned();
            }
        }
        let mut description = "Branch";
        if insert_without_remotes && name.starts_with("remotes/") {
            if let Some(index) = name[8..].find('/') {
                name = name[8 + index + 1..].to_owned();
            }
            description = "Remote branch";
        }
        if let Some(index) = name.find(' ') {
            name = name[..index].to_owned();
        }
        if !seen.insert(name.clone()) {
            continue;
        }
        out.push(json!({
            "name": name,
            "description": description,
            "icon": "fig://icon?type=git",
            "priority": 75
        }));
    }
    out
}

fn git_or_remote_branches(tokens: &[String], exec: &AdapterExec<'_>) -> AdapterResult {
    let result = if tokens.iter().any(|token| token == "-r") {
        exec_object(
            exec,
            "git",
            [
                "--no-optional-locks",
                "branch",
                "-r",
                "--no-color",
                "--sort=-committerdate",
            ],
        )?
    } else {
        exec_object(
            exec,
            "git",
            ["--no-optional-locks", "branch", "--no-color", "--sort=-committerdate"],
        )?
    };
    Ok(JsonValue::Array(git_branch_rows(&result.stdout, true)))
}

fn spring_metadata(exec: &AdapterExec<'_>) -> Result<Option<JsonValue>, AdapterError> {
    match exec_object(exec, "curl", ["-s", "https://start.spring.io/metadata/client"]) {
        Ok(result) => parse_json(&result.stdout).map(Some),
        Err(_) => Ok(None),
    }
}

fn spring_versions(exec: &AdapterExec<'_>, field: &str) -> AdapterResult {
    let Some(metadata) = spring_metadata(exec)? else {
        return Ok(json!([]));
    };
    let Some(values) = metadata
        .pointer(&format!("/{field}/values"))
        .and_then(JsonValue::as_array)
    else {
        return Ok(json!([]));
    };
    Ok(JsonValue::Array(
        values
            .iter()
            .map(|item| {
                json!({
                    "name": item.get("id"),
                    "displayName": item.get("name")
                })
            })
            .collect(),
    ))
}

fn spring_dependencies(tokens: &[String], exec: &AdapterExec<'_>) -> AdapterResult {
    let Some(metadata) = spring_metadata(exec)? else {
        return Ok(json!([]));
    };
    let token = last_token(tokens);
    let used: HashSet<&str> = token.split(',').collect();
    let mut rows = Vec::new();
    if let Some(groups) = metadata.pointer("/dependencies/values").and_then(JsonValue::as_array) {
        for group in groups {
            if let Some(values) = group.get("values").and_then(JsonValue::as_array) {
                for item in values {
                    let id = item.get("id").and_then(JsonValue::as_str).unwrap_or("");
                    if used.contains(id) {
                        continue;
                    }
                    rows.push(json!({
                        "name": id,
                        "displayName": item.get("name"),
                        "description": item.get("description")
                    }));
                }
            }
        }
    }
    rows.sort_by(|left, right| {
        left.get("displayName")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .cmp(right.get("displayName").and_then(JsonValue::as_str).unwrap_or(""))
    });
    Ok(JsonValue::Array(rows))
}

fn deno_tasks(tokens: &[String], exec: &AdapterExec<'_>) -> AdapterResult {
    let _ = tokens;
    let result = match exec_object(exec, "cat", ["deno.json"]) {
        Ok(result) => result,
        Err(_) => match exec_object(exec, "cat", ["deno.jsonc"]) {
            Ok(result) => result,
            Err(_) => return Ok(json!([])),
        },
    };
    let parsed = match parse_json(&result.stdout) {
        Ok(value) => value,
        Err(_) => return Ok(json!([])),
    };
    let Some(tasks) = parsed.get("tasks").and_then(JsonValue::as_object) else {
        return Ok(json!([]));
    };
    let fig = parsed.get("fig").and_then(JsonValue::as_object);
    Ok(JsonValue::Array(
        tasks
            .iter()
            .map(|(name, description)| {
                let extra = fig.and_then(|object| object.get(name)).and_then(JsonValue::as_object);
                json!({
                    "name": name,
                    "displayName": extra.and_then(|object| object.get("displayName")),
                    "description": extra.and_then(|object| object.get("description")).cloned().unwrap_or_else(|| description.clone()),
                    "icon": extra.and_then(|object| object.get("icon")).cloned().unwrap_or_else(|| json!("⚙️")),
                    "priority": extra.and_then(|object| object.get("priority")),
                    "hidden": extra.and_then(|object| object.get("hidden"))
                })
            })
            .collect(),
    ))
}

fn join_ssh_path(name: &str, base: &str, home: &str) -> String {
    if name.starts_with('/') || name.starts_with("~/") || name == "~" {
        return name.replacen('~', home, 1);
    }
    if base.starts_with('/') || base.starts_with("~/") || base == "~" {
        let expanded = base.replacen('~', home, 1);
        if expanded.ends_with('/') {
            format!("{expanded}{name}")
        } else {
            format!("{expanded}/{name}")
        }
    } else if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

fn ssh_config_lines(exec: &AdapterExec<'_>, home: &str, name: &str, base: &str) -> Result<Vec<String>, AdapterError> {
    let path = join_ssh_path(name, base, home);
    let result = exec_object(exec, "cat", [path])?;
    let rows: Vec<String> = js_split_lines(&result.stdout)
        .into_iter()
        .map(|line| line.trim().to_owned())
        .collect();
    let mut out = rows.clone();
    for line in &rows {
        if line.to_lowercase().starts_with("include ") {
            let include = line.split_once(' ').map(|(_, rest)| rest).unwrap_or("");
            out.extend(ssh_config_lines(exec, home, include, base)?);
        }
    }
    Ok(out)
}

fn ssh_config_hosts(exec: &AdapterExec<'_>, context: &HookContext) -> AdapterResult {
    let home = env_var(context, "HOME");
    let lines = ssh_config_lines(exec, &home, "config", "~/.ssh")?;
    Ok(JsonValue::Array(
        lines
            .into_iter()
            .filter(|line| line.trim().to_lowercase().starts_with("host ") && !line.contains('*'))
            .map(|line| {
                json!({
                    "name": line.split_whitespace().nth(1).unwrap_or(""),
                    "description": "SSH host",
                    "priority": 90
                })
            })
            .collect(),
    ))
}

fn known_hosts(tokens: &[String], exec: &AdapterExec<'_>, context: &HookContext) -> AdapterResult {
    let home = env_var(context, "HOME");
    let result = exec_object(exec, "cat", [format!("{home}/.ssh/known_hosts")])?;
    let regex = Regex::new(KNOWN_HOST).expect("known host");
    let mut names = Vec::new();
    for line in js_split_lines(&result.stdout) {
        if let Ok(Some(caps)) = regex.captures(&line) {
            if let Some(matched) = caps.get(0) {
                let name = matched.as_str().to_owned();
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    let prefix = tokens
        .get(1)
        .filter(|token| token.ends_with('@'))
        .cloned()
        .unwrap_or_default();
    Ok(JsonValue::Array(
        names
            .into_iter()
            .map(|name| json!({"name": format!("{prefix}{name}"), "description": "SSH host"}))
            .collect(),
    ))
}

fn ykman_modes(tokens: &[String]) -> AdapterResult {
    let parts: Vec<String> = last_token(tokens).split('+').map(ToOwned::to_owned).collect();
    let aliases = [
        ("OTP", ["o"].as_slice()),
        ("o", ["OTP"].as_slice()),
        ("FIDO", ["f"].as_slice()),
        ("f", ["FIDO"].as_slice()),
        ("CCID", ["c"].as_slice()),
        ("c", ["CCID"].as_slice()),
    ];
    let mut set: HashSet<String> = ["OTP", "o", "FIDO", "f", "CCID", "c"]
        .into_iter()
        .map(ToOwned::to_owned)
        .collect();
    // Preserve JS Map value insertion order: o, OTP, f, FIDO, c, CCID
    let ordered = ["o", "OTP", "f", "FIDO", "c", "CCID"];
    for part in &parts {
        set.remove(part);
        if let Some((_, extras)) = aliases.iter().find(|(name, _)| *name == part) {
            for extra in *extras {
                set.remove(*extra);
            }
        }
    }
    Ok(JsonValue::Array(
        ordered
            .into_iter()
            .filter(|name| set.contains(*name))
            .map(suggestion_from_name)
            .collect(),
    ))
}

fn firefox_port(tokens: &[String]) -> AdapterResult {
    let token = last_token(tokens);
    let parsed: Result<f64, _> = token.parse();
    let Ok(number) = parsed else {
        return Ok(json!([]));
    };
    if number.fract() != 0.0 {
        return Ok(json!([]));
    }
    if !(0.0..=65535.0).contains(&number) {
        return Ok(json!([]));
    }
    Ok(json!([{"name": token, "description": "Port number"}]))
}

fn npms_search(tokens: &[String], exec: &AdapterExec<'_>, keywords: &[&str]) -> AdapterResult {
    let token = last_token(tokens);
    if token.is_empty() {
        return Ok(json!([]));
    }
    let keyword_query = if keywords.is_empty() {
        String::new()
    } else {
        format!("+keywords:{}", keywords.join(","))
    };
    let search = if keyword_query.is_empty() {
        format!("https://api.npms.io/v2/search/suggestions?q={token}&size=20")
    } else {
        format!("https://api.npms.io/v2/search?size=20&q={token}{keyword_query}")
    };
    let registry = format!("https://registry.npmjs.org/{}", token.trim_end_matches('@'));
    let args = if token.ends_with('@') {
        vec![
            "-s".into(),
            "-H".into(),
            "Accept: application/vnd.npm.install-v1+json".into(),
            registry,
        ]
    } else {
        vec!["-s".into(), "-H".into(), "Accept: application/json".into(), search]
    };
    let result = match exec_object(exec, "curl", args) {
        Ok(result) => result,
        Err(_) => return Ok(json!([])),
    };
    let parsed = match parse_json(&result.stdout) {
        Ok(value) => value,
        Err(_) => return Ok(json!([])),
    };
    let versioned = if token.starts_with('@') {
        token.matches('@').count() > 1
    } else {
        token.contains('@')
    };
    if versioned {
        let mut rows = Vec::new();
        if let Some(tags) = parsed.get("dist-tags").and_then(JsonValue::as_object) {
            for (name, description) in tags {
                rows.push(json!({"name": name, "description": description}));
            }
        }
        if let Some(versions) = parsed.get("versions").and_then(JsonValue::as_object) {
            let mut names: Vec<_> = versions.keys().cloned().collect();
            names.reverse();
            rows.extend(names.into_iter().map(suggestion_from_name));
        }
        return Ok(JsonValue::Array(rows));
    }
    let items = if keyword_query.is_empty() {
        parsed.as_array().cloned().unwrap_or_default()
    } else {
        parsed
            .get("results")
            .and_then(JsonValue::as_array)
            .cloned()
            .unwrap_or_default()
    };
    Ok(JsonValue::Array(
        items
            .iter()
            .map(|item| {
                json!({
                    "name": item.pointer("/package/name"),
                    "description": item.pointer("/package/description")
                })
            })
            .collect(),
    ))
}

fn dscacheutil_query(tokens: &[String], exec: &AdapterExec<'_>) -> AdapterResult {
    let category = tokens.len().checked_sub(4).and_then(|index| tokens.get(index)).cloned();
    let key = tokens.len().checked_sub(2).and_then(|index| tokens.get(index)).cloned();
    let _ = key;
    let args = vec![
        JsonValue::String("-q".into()),
        category.map(JsonValue::String).unwrap_or(JsonValue::Null),
    ];
    let result = exec(AdapterExecRequest {
        command: "dscacheutil".into(),
        args,
        cwd: None,
        env: None,
        timeout: None,
    })?;
    Ok(JsonValue::Array(
        js_split_lines(&result.stdout)
            .into_iter()
            .filter(|line| !line.is_empty())
            .map(suggestion_from_name)
            .collect(),
    ))
}

fn tldr_pages(exec: &AdapterExec<'_>, context: &HookContext) -> AdapterResult {
    let home = env_var(context, "HOME");
    let roots = [
        "~/.tldrc/tldr/pages/android/",
        "~/.tldrc/tldr/pages/common/",
        "~/.tldrc/tldr/pages/linux/",
        "~/.tldrc/tldr/pages/osx/",
        "~/.tldrc/tldr/pages/sunos/",
        "~/.tldrc/tldr/pages/windows/",
    ];
    let args: Vec<String> = std::iter::once("-Al".into())
        .chain(roots.iter().map(|path| path.replacen('~', &home, 1)))
        .collect();
    let result = match exec_object(exec, "ls", args) {
        Ok(result) => result,
        Err(_) => return Ok(json!([])),
    };
    let page = Regex::new(r"\.md$").expect("tldr page");
    Ok(JsonValue::Array(
        js_split_lines(&result.stdout)
            .into_iter()
            .filter(|line| page.is_match(line).unwrap_or(false))
            .map(|line| {
                let name = line.split_whitespace().last().unwrap_or(&line);
                json!({
                    "name": name.trim_end_matches(".md").to_owned(),
                    "description": "Tldr page",
                    "icon": "fig://icon?type=string"
                })
            })
            .collect(),
    ))
}

fn fig_plugins_for_site(tokens: &[String], exec: &AdapterExec<'_>) -> AdapterResult {
    let words = HookSite::tokens(tokens).words();
    // `plugins remove` passes `{installed: true}`; `plugins add` and a
    // token list that names no subcommand (the representative add site)
    // pass `{installed: false}`.
    fig_plugins(exec, matches!(words.last(), Some(&"remove")))
}

fn fig_plugins(exec: &AdapterExec<'_>, installed: bool) -> AdapterResult {
    let args = if installed {
        vec!["plugins", "list", "--format", "json", "--installed"]
    } else {
        vec!["plugins", "list", "--format", "json"]
    };
    let result = exec_object(exec, "fig", args)?;
    let parsed = parse_json(&result.stdout)?;
    let JsonValue::Array(items) = parsed else {
        return Err(throw("TypeError"));
    };
    Ok(JsonValue::Array(
        items
            .iter()
            .map(|item| {
                let icon = item.get("icon").and_then(JsonValue::as_str).unwrap_or("");
                json!({
                    "name": item.get("name"),
                    "icon": if icon.starts_with("https://") { "📦" } else { icon },
                    "description": item.get("description")
                })
            })
            .collect(),
    ))
}

fn gh_port_key_value(tokens: &[String]) -> AdapterResult {
    let words = HookSite::tokens(tokens).words();
    // `codespace ports visibility` closed over the three visibilities;
    // `ports forward` (the representative) has no values.
    let values = if words.contains(&"visibility") {
        named_strings(&["public", "private", "org"])
    } else {
        Vec::new()
    };
    key_value(&last_token(tokens), ":", &[], &values, true)
}

const ESBUILD_FIND: &str =
    r"find . -depth 3 -type f -name '*.*' -not -path '*/node_modules/*' | sed 's/.*\.//' | sort -u";

const ESBUILD_SKIP: &[&str] = &["", "sample", "env"];

const ESBUILD_LOADERS: &[&str] = &[
    "js", "jsx", "ts", "tsx", "css", "json", "text", "base64", "file", "dataurl", "binary", "copy",
];

const ESBUILD_LOG_KEYS: &[&str] = &[
    "assign-to-constant",
    "assign-to-import",
    "call-import-namespace",
    "commonjs-variable-in-esm",
    "delete-super-property",
    "direct-eval",
    "duplicate-case",
    "duplicate-object-key",
    "empty-import-meta",
    "equals-nan",
    "equals-negative-zero",
    "equals-new-object",
    "html-comment-in-js",
    "impossible-typeof",
    "indirect-require",
    "private-name-will-throw",
    "semicolon-after-return",
    "suspicious-boolean-not",
    "this-is-undefined-in-esm",
    "unsupported-dynamic-import",
    "unsupported-jsx-comment",
    "unsupported-regexp",
    "unsupported-require-call",
    "css-syntax-error",
    "invalid-@charset",
    "invalid-@import",
    "invalid-@nest",
    "invalid-@layer",
    "invalid-calc",
    "js-comment-in-css",
    "unsupported-@charset",
    "unsupported-@namespace",
    "unsupported-css-property",
    "ambiguous-reexport",
    "different-path-case",
    "ignored-bare-import",
    "ignored-dynamic-import",
    "import-is-undefined",
    "require-resolve-not-external",
    "invalid-source-mappings",
    "sections-in-source-map",
    "missing-source-map",
    "unsupported-source-map-comment",
];

const ESBUILD_SUPPORTED_KEYS: &[&str] = &[
    "arbitrary-module-namespace-names",
    "array-spread",
    "arrow",
    "async-await",
    "async-generator",
    "bigint",
    "class",
    "class-field",
    "class-private-accessor",
    "class-private-brand-check",
    "class-private-field",
    "class-private-method",
    "class-private-static-accessor",
    "class-private-static-field",
    "class-private-static-method",
    "class-static-blocks",
    "class-static-field",
    "const-and-let",
    "default-argument",
    "destructuring",
    "dynamic-import",
    "exponent-operator",
    "export-star-as",
    "for-await",
    "for-of",
    "generator",
    "hashbang",
    "import-assertions",
    "import-meta",
    "logical-assignment",
    "nested-rest-binding",
    "new-target",
    "node-colon-prefix-import",
    "node-colon-prefix-require",
    "nullish-coalescing",
    "object-accessors",
    "object-extensions",
    "object-rest-spread",
    "optional-catch-binding",
    "optional-chain",
    "regexp-dot-all-flag",
    "regexp-lookbehind-assertions",
    "regexp-match-indices",
    "regexp-named-capture-groups",
    "regexp-sticky-and-unicode-flags",
    "regexp-unicode-property-escapes",
    "rest-argument",
    "template-literal",
    "top-level-await",
    "typeof-exotic-object-is-object",
    "unicode-escapes",
    "hex-rgba",
    "rebecca-purple",
    "modern-rgb-hsl",
    "inset-property",
    "nesting",
];

/// The `keyValueList` body esbuild shares across seven options. Each site
/// closed over different `keys` / `values`; the option (including the
/// `requiresSeparator` form `--loader:.js=`) picks them.
fn esbuild_key_value_list(tokens: &[String], exec: &AdapterExec<'_>) -> AdapterResult {
    let token = last_token(tokens);
    let choosing_keys = key_value_list_chooses_keys(&token, "=", ",");
    let (keys, values) = match site_option(tokens) {
        Some("--footer") => (named_strings(&["css", "js"]), Vec::new()),
        Some("--log-override") => {
            if choosing_keys {
                (named_strings(ESBUILD_LOG_KEYS), Vec::new())
            } else {
                (
                    Vec::new(),
                    named_strings(&["verbose", "debug", "info", "warning", "error", "silent"]),
                )
            }
        },
        Some("--supported") => {
            if choosing_keys {
                (named_strings(ESBUILD_SUPPORTED_KEYS), Vec::new())
            } else {
                (Vec::new(), named_strings(&["true", "false"]))
            }
        },
        Some("--banner" | "--out-extension" | "--resolve-extensions") => {
            if choosing_keys {
                (esbuild_extensions(exec)?, Vec::new())
            } else {
                (Vec::new(), Vec::new())
            }
        },
        // `--loader`, also the representative `esbuild#custom#0`.
        _ => {
            if choosing_keys {
                (esbuild_extensions(exec)?, Vec::new())
            } else {
                (Vec::new(), named_strings(ESBUILD_LOADERS))
            }
        },
    };
    key_value_list(&token, "=", ",", &keys, &values, true, false, false, true)
}

fn esbuild_extensions(exec: &AdapterExec<'_>) -> Result<Vec<JsonValue>, AdapterError> {
    let result = exec_object(exec, "bash", ["-c", ESBUILD_FIND])?;
    Ok(js_split_lines(result.stdout.trim())
        .into_iter()
        .filter(|ext| !ESBUILD_SKIP.contains(&ext.as_str()))
        .map(|ext| suggestion_object(format!(".{ext}"), &[]))
        .collect())
}

fn cargo_config(tokens: &[String]) -> AdapterResult {
    let token = last_token(tokens);
    let keys = adapter_list("cargo-config-keys.json");
    if !token.contains('=') {
        return key_value(&token, "=", &keys, &[], true);
    }
    key_value(&token, "=", &keys, &cargo_config_values(cargo_config_key(&token)), true)
}

fn cargo_config_key(token: &str) -> &str {
    let rest = token.strip_prefix("--config=").unwrap_or(token);
    rest.split('=').next().unwrap_or(rest)
}

fn cargo_config_values(key: &str) -> Vec<JsonValue> {
    match key {
        "build.incremental"
        | "http.debug"
        | "http.check-revoke"
        | "http.multiplexing"
        | "net.git-fetch-with-cli"
        | "net.offline" => named_strings(&["true", "false"]),
        "cargo-new.vcs" => vec![
            json!({"name": "git", "icon": "fig://icon?type=git", "description": "Initialize with Git"}),
            json!({"name": "hg", "icon": "⚗️", "description": "Initialize with Mercurial"}),
            json!({"name": "pijul", "icon": "🦚", "description": "Initialize with Pijul"}),
            json!({"name": "fossil", "icon": "🦴", "description": "Initialize with Fossil"}),
            json!({"name": "none", "icon": "🚫", "description": "Initialize with no VCS"}),
        ],
        _ => Vec::new(),
    }
}

fn chezmoi_attrs(tokens: &[String]) -> AdapterResult {
    let token = last_token(tokens);
    let current = token.rsplit_once(',').map(|(_, rest)| rest).unwrap_or(&token);
    if current.starts_with('+') || current.starts_with('-') {
        return Ok(JsonValue::Array(adapter_list("chezmoi-attrs-plus.json")));
    }
    if current.starts_with("no") {
        return Ok(JsonValue::Array(adapter_list("chezmoi_custom_24.json")));
    }
    Ok(JsonValue::Array(adapter_list("chezmoi_custom_24.json")))
}

fn oxlint_rules(tokens: &[String], exec: &AdapterExec<'_>) -> AdapterResult {
    let stdout = if tokens.first().is_some_and(|token| token.starts_with("npx")) {
        let result = exec_object(exec, "npx", ["oxlint", "--rules"])?;
        if result.status != 0 {
            return Ok(json!([]));
        }
        result.stdout
    } else {
        let which = exec_object(exec, "which", ["oxlint"])?;
        if which.status != 0 {
            return Ok(json!([]));
        }
        exec_object(exec, "oxlint", ["--rules"])?.stdout
    };
    Ok(JsonValue::Array(
        js_split_lines(&stdout)
            .into_iter()
            .map(|line| line.trim().to_owned())
            .filter(|line| line.starts_with("| "))
            .map(|line| {
                line.split('|')
                    .map(str::trim)
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .filter(|parts| parts.len() >= 3)
            .filter(|parts| {
                parts
                    .get(1)
                    .and_then(|name| name.chars().next())
                    .is_some_and(|ch| ch.is_ascii_lowercase())
            })
            .map(|parts| {
                let name = parts.get(1).cloned().unwrap_or_default();
                let plugin = parts.get(2).cloned().unwrap_or_default();
                let enabled = parts.get(3).is_some_and(|part| !part.is_empty());
                json!({
                    "name": name,
                    "description": format!(
                        "{plugin} plugin{}",
                        if enabled { " (enabled by default)" } else { "" }
                    ),
                    "icon": "fig://icon?type=command"
                })
            })
            .collect(),
    ))
}

fn goto_aliases(exec: &AdapterExec<'_>, context: &HookContext) -> AdapterResult {
    let home = env_var(context, "HOME");
    let result = exec_object(exec, "cat", [format!("{home}/.config/goto")])?;
    let mut seen = JsMap::new();
    for line in js_split_lines(&result.stdout) {
        let mut parts = line.split(' ');
        let name = parts.next().unwrap_or("").to_owned();
        let dest = parts.next().unwrap_or("");
        seen.insert(
            line.clone(),
            json!({
                "name": name,
                "description": format!("Goto {dest}"),
                "icon": "🔖",
                "priority": 80
            }),
        );
    }
    Ok(JsonValue::Array(seen.into_values()))
}

fn git_flow_branches(tokens: &[String], exec: &AdapterExec<'_>) -> AdapterResult {
    let kind = tokens.get(1).cloned().unwrap_or_else(|| "undefined".into());
    let prefix = exec_object(
        exec,
        "git",
        vec!["config".into(), "--get".into(), format!("gitflow.prefix.{kind}")],
    )?
    .stdout;
    let branches = exec_object(
        exec,
        "git",
        [
            "--no-optional-locks",
            "branch",
            "-a",
            "--no-color",
            "--sort=-committerdate",
        ],
    )?;
    let mut out = Vec::new();
    for line in js_split_lines(&branches.stdout) {
        let mut name = line.trim().to_owned();
        if name.starts_with('*') || name.starts_with('+') {
            name = name.chars().skip(2).collect();
        }
        if !name.starts_with(&prefix) {
            continue;
        }
        let stripped = name.replacen(&prefix, "", 1);
        if stripped.is_empty() {
            continue;
        }
        out.push(json!({
            "name": stripped,
            "description": format!("{} branch", prefix.replace('/', "")),
            "icon": "fig://icon?type=git"
        }));
    }
    Ok(JsonValue::Array(out))
}

/// Fig's shared `ai({ name, prompt, message, … })` factory. Disabled matches
/// the JS early return. Enabled still returns `[]`: the body then called
/// site-specific `prompt`/`message` closures and `fig _ request --route
/// /ai/chat`, which this fork does not ship. Reconstructing those closures
/// without the route would still produce no rows.
fn fig_ai(tokens: &[String], exec: &AdapterExec<'_>, context: &HookContext) -> AdapterResult {
    let _ = (tokens, context);
    let enabled = exec_object(exec, "fig", ["settings", "--format", "json", "autocomplete.ai.enabled"])?;
    let parsed = parse_json(&enabled.stdout)?;
    if !parsed.as_bool().unwrap_or(false) {
        return Ok(json!([]));
    }
    Ok(json!([]))
}

fn twilio_tree(tokens: &[String]) -> AdapterResult {
    let token = last_token(tokens);
    let walked: Vec<String> = token.split(':').map(ToOwned::to_owned).collect();
    let parents = if walked.is_empty() {
        Vec::new()
    } else {
        walked[..walked.len().saturating_sub(1)].to_vec()
    };
    let mut current = adapter_list("twilio-resources.json");
    for part in parents {
        let Some(next) = current
            .iter()
            .find(|item| item.get("name").and_then(JsonValue::as_str) == Some(part.as_str()))
        else {
            return Ok(json!([]));
        };
        match next.get("subcommands").and_then(JsonValue::as_array) {
            Some(children) => current = children.clone(),
            None => return Ok(json!([])),
        }
    }
    Ok(JsonValue::Array(
        current
            .into_iter()
            .map(|mut item| {
                if let JsonValue::Object(object) = &mut item {
                    object.entry("type").or_insert_with(|| json!("subcommand"));
                }
                item
            })
            .collect(),
    ))
}

fn rich_styles(tokens: &[String]) -> AdapterResult {
    let token = last_token(tokens);
    let parts: Vec<&str> = token.split_whitespace().collect();
    let used: HashSet<&str> = parts.iter().copied().collect();
    let styles = adapter_list("rich-styles.json");
    Ok(JsonValue::Array(
        styles
            .into_iter()
            .filter(|item| {
                item.get("name")
                    .and_then(JsonValue::as_str)
                    .is_none_or(|name| !used.contains(name))
            })
            .collect(),
    ))
}

fn man_apropos(tokens: &[String], exec: &AdapterExec<'_>) -> AdapterResult {
    let token = last_token(tokens);
    let result = match exec_object(exec, "man", ["-k", "."]) {
        Ok(result) => result,
        Err(_) => return Ok(json!([])),
    };
    let mut by_letter: HashMap<char, Vec<JsonValue>> = HashMap::new();
    let mut seen = HashSet::new();
    for line in js_split_lines(&result.stdout) {
        let Some(index) = line.find(" - ") else {
            continue;
        };
        let names = &line[..index];
        let mut description = line[index + 3..].to_owned();
        if description.is_empty() {
            description = "Manual page".into();
        }
        let mut chars = description.chars();
        if let Some(first) = chars.next() {
            description = format!("{}{}", first.to_uppercase(), chars.as_str());
        }
        for part in names.split(", ") {
            let open = part.rfind('(').unwrap_or(part.len());
            let name = part[..open].to_owned();
            let section = part[open..].to_owned();
            if !seen.insert(name.clone()) {
                continue;
            }
            let letter = name.chars().next().unwrap_or('a');
            by_letter.entry(letter).or_default().push(json!({
                "name": name,
                "description": format!("{section} {description}"),
                "icon": "fig://icon?type=string"
            }));
        }
    }
    let key = token.chars().next().unwrap_or('a');
    Ok(JsonValue::Array(by_letter.remove(&key).unwrap_or_default()))
}

/// cargo's `le`: the package whose `source` is the workspace `Cargo.toml`,
/// else every package without a `source` (the path members). `--no-deps`
/// metadata only lists members, so in practice this is all of them, but a
/// registry package would be dropped here exactly as the spec drops it.
fn cargo_current_packages(metadata: &JsonValue) -> Vec<&JsonValue> {
    let packages = metadata
        .get("packages")
        .and_then(JsonValue::as_array)
        .map_or(&[][..], Vec::as_slice);
    let manifest = format!(
        "{}/Cargo.toml",
        metadata
            .get("workspace_root")
            .and_then(JsonValue::as_str)
            .unwrap_or("undefined")
    );
    if let Some(found) = packages
        .iter()
        .find(|package| package.get("source").and_then(JsonValue::as_str) == Some(manifest.as_str()))
    {
        return vec![found];
    }
    packages
        .iter()
        .filter(|package| !package.get("source").is_some_and(json_truthy))
        .collect()
}

/// JS truthiness of a JSON value (`!i.source`).
fn json_truthy(value: &JsonValue) -> bool {
    match value {
        JsonValue::Null => false,
        JsonValue::Bool(flag) => *flag,
        JsonValue::Number(number) => number.as_f64().is_some_and(|number| number != 0.0),
        JsonValue::String(text) => !text.is_empty(),
        JsonValue::Array(_) | JsonValue::Object(_) => true,
    }
}

/// cargo's `u({kind})`. Every one of its call sites passes a kind, and the
/// option owning the argument names it one-to-one: `--bin` → `bin`,
/// `--example` → `example`, `--test` → `test`, `--bench` → `bench`.
fn cargo_targets(tokens: &[String], exec: &AdapterExec<'_>, context: &HookContext) -> AdapterResult {
    let result = exec_object(exec, "cargo", ["metadata", "--format-version", "1", "--no-deps"])?;
    let parsed = parse_json(&result.stdout)?;
    let kind = match owning_option(tokens) {
        Some("--bin") => "bin",
        Some("--example") => "example",
        Some("--test") => "test",
        Some("--bench") => "bench",
        // No spec site reaches this body from any other option, so this
        // only happens for synthetic token lists such as the T1.2 baseline's
        // bare `["cargo"]`. Those were captured against the representative
        // hook, `cargo#custom#1` = `bench --bin`, so behave as that site
        // rather than return the unfiltered list no call site ever showed.
        _ => "bin",
    };
    let cwd = &context.current_working_directory;
    let mut rows = Vec::new();
    for package in cargo_current_packages(&parsed) {
        let Some(targets) = package.get("targets").and_then(JsonValue::as_array) else {
            continue;
        };
        for target in targets {
            let matches_kind = target
                .get("kind")
                .and_then(JsonValue::as_array)
                .is_some_and(|kinds| kinds.iter().any(|entry| entry == kind));
            if !matches_kind {
                continue;
            }
            let path = target
                .get("src_path")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .replacen(cwd, "", 1);
            rows.push(json!({
                "icon": "🎯",
                "name": target.get("name"),
                "description": path
            }));
        }
    }
    Ok(JsonValue::Array(rows))
}

fn cargo_crates(tokens: &[String], exec: &AdapterExec<'_>) -> AdapterResult {
    let token = last_token(tokens);
    if token.contains('@') && !token.starts_with('@') {
        let name = token.split('@').next().unwrap_or(&token);
        let encoded = urlencoding_lite(name);
        let result = exec_object(
            exec,
            "curl",
            vec![
                "-sfL".into(),
                format!("https://crates.io/api/v1/crates/{encoded}/versions"),
            ],
        )?;
        let parsed = parse_json(&result.stdout)?;
        let Some(versions) = parsed.get("versions").and_then(JsonValue::as_array) else {
            return Ok(json!([]));
        };
        return Ok(JsonValue::Array(
            versions
                .iter()
                .map(|version| {
                    json!({
                        "name": format!("{}@{}", name, version.get("num").and_then(JsonValue::as_str).unwrap_or("")),
                        "insertValue": version.get("num"),
                        "description": version.get("num"),
                        "hidden": version.get("yanked")
                    })
                })
                .collect(),
        ));
    }
    if token.is_empty() {
        return Ok(json!([]));
    }
    let encoded = urlencoding_lite(&token);
    let search = exec_object(
        exec,
        "curl",
        vec![
            "-sfL".into(),
            format!("https://crates.io/api/v1/crates?q={encoded}&per_page=60"),
        ],
    )?;
    let parsed = parse_json(&search.stdout)?;
    Ok(JsonValue::Array(
        parsed
            .get("crates")
            .and_then(JsonValue::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|crate_row| {
                json!({
                    "icon": "📦",
                    "displayName": format!(
                        "{}@{}",
                        crate_row.get("name").and_then(JsonValue::as_str).unwrap_or(""),
                        crate_row.get("newest_version").and_then(JsonValue::as_str).unwrap_or("")
                    ),
                    "name": crate_row.get("name"),
                    "description": crate_row.get("description")
                })
            })
            .collect(),
    ))
}

fn urlencoding_lite(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn rich_box(tokens: &[String]) -> AdapterResult {
    let token = last_token(tokens);
    if token.is_empty() || token == "-d" {
        return Ok(json!([]));
    }
    let parts: Vec<&str> = token.split(',').collect();
    let description = match parts.len() {
        1 => rich_sides(parts[0], parts[0], parts[0], parts[0]),
        2 => rich_sides(parts[0], parts[1], parts[0], parts[1]),
        3 | 4 => rich_sides(parts[0], parts[1], parts[2], parts.get(3).copied().unwrap_or("")),
        _ => String::new(),
    };
    Ok(json!([{"name": token, "description": description}]))
}

fn rich_sides(top: &str, right: &str, bottom: &str, left: &str) -> String {
    format!(
        "Top: {}(!), right: {}(!), bottom: {}(!), left: {}(!)",
        top, right, bottom, left
    )
}

fn dscl_list(tokens: &[String], exec: &AdapterExec<'_>) -> AdapterResult {
    let token = last_token(tokens);
    if token.is_empty() {
        return Ok(json!([]));
    }
    let start = tokens.iter().position(|item| item == "dscl").unwrap_or(0);
    let reserved = HashSet::from(["-read", "-list", "-readall", "-search", "-create", "-delete"]);
    let mut source = None;
    for index in start + 1..tokens.len() {
        let current = &tokens[index];
        if current.starts_with('-') {
            continue;
        }
        let previous = tokens.get(index - 1).map(String::as_str).unwrap_or("");
        if !reserved.contains(previous) {
            source = Some(current.clone());
            break;
        }
    }
    let slash = token.rfind('/').map(|index| index as i64).unwrap_or(-1);
    let path = if slash < 0 {
        String::new()
    } else {
        token[..slash as usize].to_owned()
    };
    let args = vec![
        source.map(JsonValue::String).unwrap_or(JsonValue::Null),
        JsonValue::String("-list".into()),
        JsonValue::String(path),
    ];
    let result = exec(AdapterExecRequest {
        command: "dscl".into(),
        args,
        cwd: None,
        env: None,
        timeout: None,
    })?;
    Ok(JsonValue::Array(
        result
            .stdout
            .trim()
            .split('\n')
            .map(|line| {
                let hidden = line[line.rfind('/').map(|index| index + 1).unwrap_or(0)..].starts_with('_');
                json!({
                    "name": line,
                    "icon": "📁",
                    "priority": if hidden { 49 } else { 50 }
                })
            })
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    use super::*;
    use crate::hook_baseline::ExecRule;
    use crate::native_adapters::effect::mock_exec_from_rules;

    const CARGO_TARGETS_SHA: &str = "e0f755fcbcdd4aa9db41d69f013e92b961ad64f347b05a750718d30d8a851634";
    const SCC_KEY_VALUE_LIST_SHA: &str = "082bc08b4bdda4806561b75e8cc0731eebc9d4400f2113e5c7be189485a43558";
    const SCC_VALUE_LIST_SHA: &str = "63f4199d5d1d44c71d013364bf8dbb477411f11d776cde35821ca712569034c0";
    const ESBUILD_KEY_VALUE_LIST_SHA: &str = "83fee23ebcadbf1d63827dd335c7cf0b8a40bbc3d7266022e9ad49e4e231e1ae";
    const NX_COMMA_SHA: &str = "7cdfa0e8c803cb9e4c6176f51aa7730cdd864347528547cf8291722c94fd8784";
    const FIG_PLUGINS_V1_SHA: &str = "5693d96360a2e105298439a7a22abbde058eaa41e74ef120f1b1d722e2eedbf6";
    const FIG_PLUGINS_V2_SHA: &str = "faf81d363c15195974dc41b68bfaeb799bc2a67b0d6143555695df0e4bce9827";
    const GH_PORTS_SHA: &str = "661801a6a065f6a47a99557d48b30b8ddaf73a2b08a407d4db2b1434196c8fc9";

    fn rule(command: &str, args: &[&str], stdout: &str) -> ExecRule {
        ExecRule {
            command: Some(command.into()),
            args: Some(args.iter().map(|arg| (*arg).to_owned()).collect()),
            stdout: Some(stdout.into()),
            stderr: None,
            status: Some(0),
            delay_ms: None,
        }
    }

    fn tokens(list: &[&str]) -> Vec<String> {
        list.iter().map(|token| (*token).to_owned()).collect()
    }

    fn names(result: &AdapterResult) -> Vec<String> {
        result
            .as_ref()
            .expect("adapter result")
            .as_array()
            .expect("array")
            .iter()
            .map(|row| row["name"].as_str().unwrap_or("").to_owned())
            .collect()
    }

    fn context(cwd: &str) -> HookContext {
        HookContext {
            current_working_directory: cwd.into(),
            current_process: "zsh".into(),
            ssh_prefix: String::new(),
            environment_variables: Default::default(),
            search_term: String::new(),
            is_dangerous: false,
        }
    }

    const CARGO_METADATA: &str = r#"{
      "workspace_root": "/ws",
      "packages": [
        {"name": "app", "source": null, "targets": [
          {"name": "app", "kind": ["bin"], "src_path": "/ws/src/main.rs"},
          {"name": "app", "kind": ["lib"], "src_path": "/ws/src/lib.rs"},
          {"name": "demo", "kind": ["example"], "src_path": "/ws/examples/demo.rs"},
          {"name": "smoke", "kind": ["test"], "src_path": "/ws/tests/smoke.rs"},
          {"name": "speed", "kind": ["bench"], "src_path": "/ws/benches/speed.rs"}
        ]},
        {"name": "vendored", "source": "registry+https://github.com/rust-lang/crates.io-index", "targets": [
          {"name": "vendored", "kind": ["bin"], "src_path": "/reg/vendored/src/main.rs"}
        ]}
      ]
    }"#;

    #[test]
    fn cargo_targets_filter_by_the_kind_the_owning_option_closed_over() {
        let rules = [rule(
            "cargo",
            &["metadata", "--format-version", "1", "--no-deps"],
            CARGO_METADATA,
        )];
        let exec = mock_exec_from_rules(&rules);
        let ctx = context("/ws");
        for (option, expected) in [
            ("--bin", vec!["app"]),
            ("--example", vec!["demo"]),
            ("--test", vec!["smoke"]),
            ("--bench", vec!["speed"]),
        ] {
            let result = cargo_targets(&tokens(&["cargo", "run", option, ""]), &exec, &ctx);
            assert_eq!(names(&result), expected, "{option}");
        }
        // The description is the source path relative to the cwd, as
        // `src_path.replace(currentWorkingDirectory, "")` made it.
        let result = cargo_targets(&tokens(&["cargo", "run", "--bin", "a"]), &exec, &ctx).unwrap();
        assert_eq!(result[0]["description"], "/src/main.rs");
        assert_eq!(result[0]["icon"], "🎯");
    }

    #[test]
    fn cargo_targets_skip_registry_packages_like_the_spec_does() {
        // `le` keeps only packages without a `source`; the vendored crate's
        // `bin` target must not appear under `--bin`.
        let rules = [rule(
            "cargo",
            &["metadata", "--format-version", "1", "--no-deps"],
            CARGO_METADATA,
        )];
        let exec = mock_exec_from_rules(&rules);
        let result = cargo_targets(&tokens(&["cargo", "run", "--bin", ""]), &exec, &context("/ws"));
        assert_eq!(names(&result), ["app"]);
    }

    #[test]
    fn cargo_targets_without_an_owning_option_behave_as_the_representative_site() {
        let rules = [rule(
            "cargo",
            &["metadata", "--format-version", "1", "--no-deps"],
            CARGO_METADATA,
        )];
        let exec = mock_exec_from_rules(&rules);
        let result = cargo_targets(&tokens(&["cargo"]), &exec, &context("/ws"));
        assert_eq!(names(&result), ["app"], "cargo#custom#1 is `bench --bin`");
    }

    const SCC_LANGUAGES: &str = "C Header (h)\nC++ (cc,cpp,cxx,c++)\nRust (rs)\nZig (zig)\n";

    #[test]
    fn scc_languages_parse_the_language_listing() {
        let rules = [rule("scc", &["--language"], SCC_LANGUAGES)];
        let exec = mock_exec_from_rules(&rules);
        let parsed = scc_languages(&exec).unwrap();
        assert_eq!(parsed.languages, ["C Header", "C++", "Rust", "Zig"]);
        let extensions: Vec<(&str, &str)> = parsed
            .extensions
            .entries()
            .map(|(extension, language)| (extension.as_str(), language.as_str()))
            .collect();
        assert_eq!(
            extensions,
            [
                ("h", "C Header"),
                ("cc", "C++"),
                ("cpp", "C++"),
                ("cxx", "C++"),
                ("c++", "C++"),
                ("rs", "Rust"),
                ("zig", "Zig")
            ]
        );
    }

    #[test]
    fn scc_count_as_offers_extensions_then_languages() {
        let rules = [rule("scc", &["--language"], SCC_LANGUAGES)];
        let exec = mock_exec_from_rules(&rules);
        let keys = scc_key_value_list(&tokens(&["scc", "--count-as", ""]), &exec).unwrap();
        assert_eq!(keys[0]["name"], "h");
        assert_eq!(keys[0]["description"], "C Header");
        assert_eq!(keys[0]["insertValue"], "h:", "keys insert the `:` separator");
        assert!(keys[0].get("icon").is_none(), "`--count-as` keys carry no icon");

        let values = scc_key_value_list(&tokens(&["scc", "--count-as", "jst:"]), &exec).unwrap();
        assert_eq!(
            values
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["C Header", "C++", "Rust", "Zig"]
        );
    }

    #[test]
    fn scc_remap_options_have_no_keys_and_language_values() {
        let rules = [rule("scc", &["--language"], SCC_LANGUAGES)];
        let exec = mock_exec_from_rules(&rules);
        for option in ["--remap-all", "--remap-unknown"] {
            let keys = scc_key_value_list(&tokens(&["scc", option, ""]), &exec).unwrap();
            assert_eq!(keys, json!([]), "{option} keys");
            let values = scc_key_value_list(&tokens(&["scc", option, "-*- C++ -*-:"]), &exec).unwrap();
            assert_eq!(values[1]["name"], "C++", "{option} values");
        }
    }

    #[test]
    fn scc_format_multi_lists_formats_then_files_and_only_runs_ls_for_values() {
        // No `ls` rule: choosing a key must not need one.
        let rules = [rule("scc", &["--language"], SCC_LANGUAGES)];
        let exec = mock_exec_from_rules(&rules);
        let keys = scc_key_value_list(&tokens(&["scc", "--format-multi", ""]), &exec).unwrap();
        assert_eq!(keys[0]["name"], "tabular");
        assert_eq!(keys[0]["icon"], "fig://icon?type=string");
        assert_eq!(keys.as_array().unwrap().len(), 10);

        let rules = [rule(
            "ls",
            &["-lAF1"],
            "drwxr-xr-x  3 me  staff  96 Jan  1 00:00 src/\n-rw-r--r--  1 me  staff  12 Jan  1 00:00 a.rs",
        )];
        let exec = mock_exec_from_rules(&rules);
        let values = scc_key_value_list(&tokens(&["scc", "--format-multi", "csv:"]), &exec).unwrap();
        let rows = values.as_array().unwrap();
        assert_eq!(rows.len(), 3);
        // `-F` marks a directory with a trailing `/`, so its "name" is what
        // follows that slash: nothing. Faithful to the spec, odd as it is.
        assert_eq!(rows[0]["name"], "");
        assert_eq!(rows[1]["name"], "-rw-r--r--  1 me  staff  12 Jan  1 00:00 a.rs");
        assert_eq!(rows[2], json!({ "name": "stdout", "priority": 75 }));
    }

    #[test]
    fn scc_include_ext_offers_extensions_with_the_string_icon() {
        let rules = [rule("scc", &["--language"], SCC_LANGUAGES)];
        let exec = mock_exec_from_rules(&rules);
        let result = scc_value_list(&tokens(&["scc", "-i", "rs,"]), &exec).unwrap();
        let rows = result.as_array().unwrap();
        assert_eq!(rows[0]["name"], "h");
        assert_eq!(rows[0]["description"], "C Header");
        assert_eq!(rows[0]["icon"], "fig://icon?type=string");
        assert!(
            rows.iter().all(|row| row["name"] != "rs"),
            "an extension already in the list is not offered again"
        );
    }

    #[test]
    fn scc_without_the_tool_installed_yields_nothing_rather_than_an_error() {
        // The T1.2 baseline: exit 127 with empty stdout resolves (Fig's
        // executeCommand does not reject on a non-zero status), and `z("")`
        // finds no languages.
        let rules = [ExecRule {
            status: Some(127),
            stderr: Some("command not found".into()),
            ..rule("scc", &["--language"], "")
        }];
        let exec = mock_exec_from_rules(&rules);
        assert_eq!(
            scc_key_value_list(&tokens(&["scc", "--count-as", ""]), &exec).unwrap(),
            json!([])
        );
        assert_eq!(scc_value_list(&tokens(&["scc", "-i", ""]), &exec).unwrap(), json!([]));
    }

    /// Collects, for every `jsCustom` hook id in a compiled spec, the names of
    /// the option whose argument owns it.
    fn custom_hook_owners(ir: &JsonValue) -> BTreeMap<String, BTreeSet<String>> {
        fn hook_ids(node: &JsonValue, out: &mut BTreeSet<String>) {
            match node {
                JsonValue::Array(items) => items.iter().for_each(|item| hook_ids(item, out)),
                JsonValue::Object(map) => {
                    for (key, value) in map {
                        if key == "jsCustom" {
                            if let Some(id) = value.as_str() {
                                out.insert(id.to_owned());
                            }
                        } else {
                            hook_ids(value, out);
                        }
                    }
                },
                _ => {},
            }
        }
        fn walk(node: &JsonValue, owners: &mut BTreeMap<String, BTreeSet<String>>) {
            match node {
                JsonValue::Array(items) => items.iter().for_each(|item| walk(item, owners)),
                JsonValue::Object(map) => {
                    if let (Some(names), Some(args)) = (map.get("names").and_then(JsonValue::as_array), map.get("args"))
                    {
                        let mut ids = BTreeSet::new();
                        hook_ids(args, &mut ids);
                        for id in ids {
                            let entry = owners.entry(id).or_default();
                            entry.extend(names.iter().filter_map(JsonValue::as_str).map(ToOwned::to_owned));
                        }
                    }
                    map.values().for_each(|value| walk(value, owners));
                },
                _ => {},
            }
        }
        let mut owners = BTreeMap::new();
        walk(ir, &mut owners);
        owners
    }

    #[test]
    fn fig_plugins_remove_lists_only_installed() {
        let store = r#"[{"name":"store-plugin","icon":"x","description":"store"}]"#;
        let installed = r#"[{"name":"local-plugin","icon":"x","description":"local"}]"#;
        let rules = [
            rule("fig", &["plugins", "list", "--format", "json"], store),
            rule(
                "fig",
                &["plugins", "list", "--format", "json", "--installed"],
                installed,
            ),
        ];
        let exec = mock_exec_from_rules(&rules);
        assert_eq!(
            names(&fig_plugins_for_site(&tokens(&["fig", "plugins", "add", ""]), &exec)),
            ["store-plugin"]
        );
        assert_eq!(
            names(&fig_plugins_for_site(&tokens(&["fig", "plugins", "remove", ""]), &exec)),
            ["local-plugin"]
        );
        assert_eq!(
            names(&fig_plugins_for_site(&tokens(&["fig", ""]), &exec)),
            ["store-plugin"],
            "no subcommand is the representative add site"
        );
    }

    fn nx_graph_rules() -> [ExecRule; 4] {
        [
            rule("cat", &["nx.json"], "{}"),
            rule(
                "find",
                &["apps", "libs", "-name", "project.json"],
                "apps/web/project.json\nlibs/ui/project.json\n",
            ),
            rule(
                "cat",
                &["apps/web/project.json"],
                r#"{"name":"web","targets":{"build":{},"serve":{}}}"#,
            ),
            rule(
                "cat",
                &["libs/ui/project.json"],
                r#"{"name":"ui","targets":{"build":{},"lint":{}}}"#,
            ),
        ]
    }

    #[test]
    fn nx_comma_lists_projects_except_under_targets() {
        let rules = nx_graph_rules();
        let exec = mock_exec_from_rules(&rules);
        assert_eq!(
            names(&nx_comma_targets(
                &tokens(&["nx", "format:write", "--exclude", ""]),
                &exec
            )),
            ["web", "ui"]
        );
        assert_eq!(
            names(&nx_comma_targets(&tokens(&["nx", "run-many", "--projects", ""]), &exec)),
            ["web", "ui"]
        );
        assert_eq!(
            names(&nx_comma_targets(&tokens(&["nx", "graph", "--targets", ""]), &exec)),
            ["build", "serve", "lint"]
        );
        assert_eq!(
            names(&nx_comma_targets(&tokens(&["nx", ""]), &exec)),
            ["web", "ui"],
            "no option is the representative --exclude site"
        );
    }

    fn esbuild_find_rule() -> ExecRule {
        rule("bash", &["-c", ESBUILD_FIND], "js\ncss\nsample\n")
    }

    #[test]
    fn esbuild_key_value_list_picks_keys_and_values_from_the_option() {
        let rules = [esbuild_find_rule()];
        let exec = mock_exec_from_rules(&rules);
        assert_eq!(
            names(&esbuild_key_value_list(&tokens(&["esbuild", "--loader", ""]), &exec)),
            [".js", ".css"],
            "Oe drops sample"
        );
        let rows = esbuild_key_value_list(&tokens(&["esbuild", "--loader", ".js="]), &exec).unwrap();
        assert_eq!(rows[0]["name"], "js");
        assert_eq!(
            names(&esbuild_key_value_list(&tokens(&["esbuild", "--footer", ""]), &exec)),
            ["css", "js"]
        );
        assert_eq!(
            names(&esbuild_key_value_list(&tokens(&["esbuild", "--footer:"]), &exec)),
            ["css", "js"],
            "requiresSeparator keeps the option on the last token"
        );
        let rows =
            esbuild_key_value_list(&tokens(&["esbuild", "--log-override", "assign-to-constant="]), &exec).unwrap();
        assert_eq!(rows[0]["name"], "verbose");
        let rows = esbuild_key_value_list(&tokens(&["esbuild", "--supported", "arrow="]), &exec).unwrap();
        assert_eq!(names(&Ok(rows)), ["true", "false"]);
        assert_eq!(
            names(&esbuild_key_value_list(
                &tokens(&["esbuild", "--banner", ".css="]),
                &exec
            ))
            .len(),
            0,
            "banner has no values"
        );
    }

    #[test]
    fn gh_visibility_lists_the_three_port_visibilities() {
        assert!(
            names(&gh_port_key_value(&tokens(&[
                "gh",
                "codespace",
                "ports",
                "forward",
                ""
            ])))
            .is_empty()
        );
        assert_eq!(
            names(&gh_port_key_value(&tokens(&[
                "gh",
                "codespace",
                "ports",
                "visibility",
                "3000:"
            ]))),
            ["public", "private", "org"]
        );
    }

    #[test]
    fn cargo_config_values_follow_the_key_being_completed() {
        assert_eq!(
            names(&cargo_config(&tokens(&["cargo", "--config", "build.incremental="]))),
            ["true", "false"]
        );
        let rows = cargo_config(&tokens(&["cargo", "--config", "cargo-new.vcs="])).unwrap();
        assert_eq!(names(&Ok(rows.clone())), ["git", "hg", "pijul", "fossil", "none"]);
        assert_eq!(rows[0]["icon"], "fig://icon?type=git");
        assert!(names(&cargo_config(&tokens(&["cargo", "--config", "build.jobs="]))).is_empty());
        assert!(
            !names(&cargo_config(&tokens(&["cargo", "--config", ""]))).is_empty(),
            "keys still list when choosing keys"
        );
    }

    #[test]
    fn file_and_bun_and_airflow_keep_the_values_the_factory_closed_over() {
        let file = key_value(
            "bytes=",
            "=",
            &adapter_list("file_custom_0.json"),
            &named_strings(&["1048576", "256", "2048", "65536", "50", "60", "8192"]),
            true,
        )
        .unwrap();
        assert_eq!(file[0]["name"], "1048576");
        let bun = key_value_list(
            ".js=",
            "=",
            ",",
            &adapter_list("bun_custom_15.json"),
            &named_strings(&["js", "jsx", "ts", "tsx", "json", "toml", "text", "file", "wasm", "napi"]),
            true,
            false,
            false,
            true,
        )
        .unwrap();
        assert_eq!(bun[0]["name"], "js");
        let airflow = key_value_list(
            "user:",
            ":",
            ",",
            &adapter_list("airflow_custom_0.json"),
            &named_strings(&["password"]),
            true,
            false,
            false,
            true,
        )
        .unwrap();
        assert_eq!(airflow[0]["name"], "password");
    }

    #[test]
    fn magnolia_search_adds_the_light_module_keyword() {
        let rules = [rule(
            "curl",
            &[
                "-s",
                "-H",
                "Accept: application/json",
                "https://api.npms.io/v2/search?size=20&q=foo+keywords:magnolia-light-module",
            ],
            r#"{"results":[{"package":{"name":"@magnolia/cli","description":"cli"}}]}"#,
        )];
        let exec = mock_exec_from_rules(&rules);
        let rows = npms_search(&tokens(&["mgnl", "search", "foo"]), &exec, &["magnolia-light-module"]).unwrap();
        assert_eq!(rows[0]["name"], "@magnolia/cli");
    }

    #[test]
    fn shared_body_sites_are_owned_by_the_options_the_adapter_dispatches_on() {
        // These adapters pick a call site's closed-over literals from the
        // option that owns the argument. A spec update that binds the same
        // body under a new option would take the wrong branch silently, so
        // pin the set of owners against the compiled IR.
        let ir_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bundle/specs-ir");
        let sidecar: JsonValue =
            serde_json::from_str(&std::fs::read_to_string(ir_root.join("typed-hooks.json")).expect("typed-hooks.json"))
                .expect("typed-hooks.json parses");
        let adapters = sidecar["adapters"].as_object().expect("adapters map");
        let expectations: [(&str, &str, &[&str]); 8] = [
            (CARGO_TARGETS_SHA, "cargo", &["--bin", "--example", "--test", "--bench"]),
            (
                SCC_KEY_VALUE_LIST_SHA,
                "scc",
                &["--count-as", "--format-multi", "--remap-all", "--remap-unknown"],
            ),
            (SCC_VALUE_LIST_SHA, "scc", &["-i", "--include-ext"]),
            (
                ESBUILD_KEY_VALUE_LIST_SHA,
                "esbuild",
                &[
                    "--loader",
                    "--banner",
                    "--footer",
                    "--log-override",
                    "--out-extension",
                    "--resolve-extensions",
                    "--supported",
                ],
            ),
            (NX_COMMA_SHA, "nx", &["--exclude", "--projects", "--targets"]),
            (FIG_PLUGINS_V1_SHA, "fig/1.0.0", &["add", "remove"]),
            (FIG_PLUGINS_V2_SHA, "fig/2.0.0", &["add", "remove"]),
            (GH_PORTS_SHA, "gh", &["forward", "visibility"]),
        ];
        for (sha, spec, allowed) in expectations {
            let ir: JsonValue = serde_json::from_str(
                &std::fs::read_to_string(ir_root.join(format!("{spec}.json"))).expect("compiled spec"),
            )
            .expect("spec IR parses");
            let owners = custom_hook_owners(&ir);
            let bound: Vec<&String> = adapters
                .iter()
                .filter(|(_, binding)| binding["functionBodySha256"] == sha)
                .map(|(id, _)| id)
                .collect();
            assert!(!bound.is_empty(), "{spec}: nothing is bound to {sha}");
            let allowed: BTreeSet<&str> = allowed.iter().copied().collect();
            for id in bound {
                let names = owners
                    .get(id)
                    .unwrap_or_else(|| panic!("{id} is not under any option's args"));
                assert!(
                    names.iter().any(|name| allowed.contains(name.as_str())),
                    "{id} is owned by {names:?}, which the adapter does not dispatch on"
                );
            }
        }
    }
}
