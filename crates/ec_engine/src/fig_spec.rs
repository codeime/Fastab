//! Fig JSON → static IR conversion used by generateSpec / loadSpec.
//!
//! These parsers used to live next to the QuickJS host. Native typed / adapter
//! hooks still return Fig-shaped JSON, so the conversion stays in-process.

use serde_json::Value as JsonValue;

use crate::ir::{
    ArgSpec, Builtin, FilterStrategy, GeneratorSpec, GeneratorTrigger, LoadSpec, OptionSpec, ParserDirectives, Spec,
    SuggestionMeta, SuggestionSeed, Template,
};

fn json_name(value: &JsonValue) -> Option<String> {
    match value {
        JsonValue::String(name) if !name.is_empty() => Some(name.clone()),
        JsonValue::Array(names) => names.iter().find_map(|item| item.as_str().map(ToOwned::to_owned)),
        _ => None,
    }
}

pub fn spec_from_fig_json(value: &JsonValue) -> Option<Spec> {
    let object = value.as_object()?;
    let names = json_names(object.get("name")).or_else(|| json_names(object.get("names")))?;
    if names.is_empty() {
        return None;
    }
    Some(Spec {
        names,
        description: object
            .get("description")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string(),
        subcommands: json_array(object.get("subcommands"))
            .iter()
            .filter_map(spec_from_fig_json)
            .collect(),
        options: json_array(object.get("options"))
            .iter()
            .filter_map(option_from_fig_json)
            .collect(),
        persistent_options: json_array(object.get("persistentOptions"))
            .iter()
            .filter_map(option_from_fig_json)
            .collect(),
        args: args_from_fig(object.get("args")),
        additional_suggestions: json_array(object.get("additionalSuggestions"))
            .iter()
            .filter_map(seed_from_fig_json)
            .collect(),
        meta: meta_from_fig(object),
        load_spec: object
            .get("loadSpec")
            .and_then(JsonValue::as_str)
            .map(|path| LoadSpec::Path(path.to_string())),
        requires_subcommand: object.get("requiresSubcommand").and_then(JsonValue::as_bool),
        filter_strategy: filter_strategy_from_fig(object.get("filterStrategy")),
        parser_directives: parser_directives_from_fig(object.get("parserDirectives")),
        js_generate_spec: None,
        generate_spec_cache_key: object
            .get("generateSpecCacheKey")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned),
        js_load_spec: object
            .get("jsLoadSpec")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned),
    })
}

pub fn merge_generated_spec(wrapper: &Spec, generated: Spec) -> Spec {
    let mut merged = generated;
    merged.names = if wrapper.names.is_empty() {
        merged.names
    } else {
        wrapper.names.clone()
    };
    if !wrapper.subcommands.is_empty() {
        merged.subcommands = merge_specs(wrapper.subcommands.clone(), merged.subcommands);
    }
    if !wrapper.options.is_empty() {
        merged.options = merge_options(wrapper.options.clone(), merged.options);
    }
    if !wrapper.persistent_options.is_empty() {
        merged.persistent_options = merge_options(wrapper.persistent_options.clone(), merged.persistent_options);
    }
    if !wrapper.args.is_empty() {
        merged.args = wrapper.args.clone();
    }
    if merged.description.is_empty() {
        merged.description = wrapper.description.clone();
    }
    merged
}

fn merge_specs(mut dest: Vec<Spec>, incoming: Vec<Spec>) -> Vec<Spec> {
    for spec in incoming {
        if let Some(existing) = dest
            .iter_mut()
            .find(|existing| spec.names.iter().any(|name| existing.has_name(name)))
        {
            *existing = spec;
        } else {
            dest.push(spec);
        }
    }
    dest
}

fn merge_options(mut dest: Vec<OptionSpec>, incoming: Vec<OptionSpec>) -> Vec<OptionSpec> {
    for option in incoming {
        if let Some(existing) = dest.iter_mut().find(|existing| {
            option
                .names
                .iter()
                .any(|name| existing.names.iter().any(|candidate| candidate == name))
        }) {
            *existing = option;
        } else {
            dest.push(option);
        }
    }
    dest
}

fn option_from_fig_json(value: &JsonValue) -> Option<OptionSpec> {
    let object = value.as_object()?;
    let names = json_names(object.get("name")).or_else(|| json_names(object.get("names")))?;
    if names.is_empty() {
        return None;
    }
    Some(OptionSpec {
        names,
        description: object
            .get("description")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string(),
        args: args_from_fig(object.get("args")),
        meta: meta_from_fig(object),
        load_spec: object
            .get("loadSpec")
            .and_then(JsonValue::as_str)
            .map(|path| LoadSpec::Path(path.to_string())),
        ..OptionSpec::default()
    })
}

fn args_from_fig(value: Option<&JsonValue>) -> Vec<ArgSpec> {
    match value {
        Some(JsonValue::Array(items)) => items.iter().filter_map(arg_from_fig_json).collect(),
        Some(item) => arg_from_fig_json(item).into_iter().collect(),
        None => Vec::new(),
    }
}

fn arg_from_fig_json(value: &JsonValue) -> Option<ArgSpec> {
    let object = value.as_object()?;
    let templates = object
        .get("template")
        .or_else(|| object.get("templates"))
        .map(templates_from_fig)
        .unwrap_or_default();
    let cache = cache_fields_from_fig(object);
    Some(ArgSpec {
        name: object
            .get("name")
            .and_then(|name| json_name(name).or_else(|| name.as_str().map(ToOwned::to_owned)))
            .unwrap_or_default(),
        description: object
            .get("description")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string(),
        templates,
        script: script_from_fig(object.get("script")),
        split_on: object.get("splitOn").and_then(JsonValue::as_str).map(ToOwned::to_owned),
        js_post_process: object
            .get("jsPostProcess")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned),
        js_custom: object
            .get("jsCustom")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned),
        js_script: object
            .get("jsScript")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned),
        cache_key: cache.0,
        cache_by_directory: cache.1,
        cache_ttl_ms: cache.2,
        cache_strategy: cache.3,
        script_timeout_ms: object.get("scriptTimeout").and_then(JsonValue::as_i64),
        builtin: object.get("builtin").and_then(builtin_from_fig),
        builtins: json_array(object.get("builtins"))
            .iter()
            .filter_map(builtin_from_fig)
            .collect(),
        suggestions: json_array(object.get("suggestions"))
            .iter()
            .filter_map(seed_from_fig_json)
            .collect(),
        is_optional: object.get("isOptional").and_then(JsonValue::as_bool).unwrap_or(false),
        is_variadic: object.get("isVariadic").and_then(JsonValue::as_bool).unwrap_or(false),
        is_command: object.get("isCommand").and_then(JsonValue::as_bool).unwrap_or(false),
        is_script: object.get("isScript").and_then(JsonValue::as_bool).unwrap_or(false),
        is_module: object
            .get("isModule")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned),
        load_spec: object.get("loadSpec").and_then(load_spec_from_fig),
        js_load_spec: object
            .get("jsLoadSpec")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned),
        js_get_query_term: object
            .get("jsGetQueryTerm")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned),
        debounce_ms: debounce_ms_from_fig(object),
        parser_directives: parser_directives_from_fig(object.get("parserDirectives")),
        generators: generators_from_fig(object),
        meta: meta_from_fig(object),
        ..ArgSpec::default()
    })
}

fn script_from_fig(value: Option<&JsonValue>) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    if let Some(script) = value.as_str() {
        let script = script.trim();
        if script.is_empty() {
            return Vec::new();
        }
        return vec!["sh".into(), "-c".into(), script.to_string()];
    }
    if let Some(items) = value.as_array() {
        return items
            .iter()
            .filter_map(|item| item.as_str().map(ToOwned::to_owned))
            .collect();
    }
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    let Some(command) = object.get("command").and_then(JsonValue::as_str) else {
        return Vec::new();
    };
    if command.trim().is_empty() {
        return Vec::new();
    }
    let mut script = vec![command.to_string()];
    if let Some(args) = object.get("args").and_then(JsonValue::as_array) {
        script.extend(args.iter().filter_map(|item| item.as_str().map(ToOwned::to_owned)));
    }
    script
}

fn load_spec_from_fig(value: &JsonValue) -> Option<LoadSpec> {
    if let Some(path) = value.as_str() {
        return Some(LoadSpec::Path(path.to_string()));
    }
    spec_from_fig_json(value).map(|spec| LoadSpec::Inline(Box::new(spec)))
}

fn builtin_from_fig(value: &JsonValue) -> Option<Builtin> {
    match value.as_str()? {
        "git-refs" => Some(Builtin::GitRefs),
        "git-branches" => Some(Builtin::GitBranches),
        "git-tags" => Some(Builtin::GitTags),
        "git-commits" => Some(Builtin::GitCommits),
        "git-remotes" => Some(Builtin::GitRemotes),
        "git-changed-files" => Some(Builtin::GitChangedFiles),
        "git-stashes" => Some(Builtin::GitStashes),
        "git-aliases" => Some(Builtin::GitAliases),
        "npm-scripts" => Some(Builtin::NpmScripts),
        "npm-deps" => Some(Builtin::NpmDeps),
        "cobra" => Some(Builtin::Cobra),
        _ => None,
    }
}

fn generator_from_fig(value: &JsonValue) -> Option<GeneratorSpec> {
    let object = value.as_object()?;
    let cache = cache_fields_from_fig(object);
    Some(GeneratorSpec {
        templates: object
            .get("template")
            .or_else(|| object.get("templates"))
            .map(templates_from_fig)
            .unwrap_or_default(),
        script: script_from_fig(object.get("script")),
        split_on: object.get("splitOn").and_then(JsonValue::as_str).map(ToOwned::to_owned),
        js_post_process: object
            .get("jsPostProcess")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned),
        js_custom: object
            .get("jsCustom")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned),
        js_script: object
            .get("jsScript")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned),
        cache_key: cache.0,
        cache_by_directory: cache.1,
        cache_ttl_ms: cache.2,
        cache_strategy: cache.3,
        script_timeout_ms: object.get("scriptTimeout").and_then(JsonValue::as_i64),
        builtin: object.get("builtin").and_then(builtin_from_fig),
        get_query_term: object
            .get("getQueryTerm")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned),
        js_get_query_term: object
            .get("jsGetQueryTerm")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned),
        js_filter_template_suggestions: object
            .get("jsFilterTemplateSuggestions")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned),
        trigger: object.get("trigger").and_then(trigger_from_fig),
        extensions: json_string_list(object.get("extensions")),
        equals: json_string_list(object.get("equals")),
        show_folders: object
            .get("showFolders")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned),
        filter_folders: object.get("filterFolders").and_then(JsonValue::as_bool),
        file_priority: object.get("filePriority").and_then(JsonValue::as_i64),
        folder_priority: object.get("folderPriority").and_then(JsonValue::as_i64),
        root_directory: object
            .get("rootDirectory")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned),
        matches: object.get("matches").and_then(JsonValue::as_str).map(ToOwned::to_owned),
        matches_flags: object
            .get("matchesFlags")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned),
    })
}

fn generators_from_fig(object: &serde_json::Map<String, JsonValue>) -> Vec<GeneratorSpec> {
    let mut out = Vec::new();
    if let Some(one) = object.get("generator")
        && let Some(generator) = generator_from_fig(one)
    {
        out.push(generator);
    }
    // Fig accepts either `generators: { … }` or `generators: [{ … }]`.
    match object.get("generators") {
        Some(JsonValue::Array(items)) => {
            for item in items {
                if let Some(generator) = generator_from_fig(item) {
                    out.push(generator);
                }
            }
        },
        Some(item) => {
            if let Some(generator) = generator_from_fig(item) {
                out.push(generator);
            }
        },
        None => {},
    }
    out
}

fn debounce_ms_from_fig(object: &serde_json::Map<String, JsonValue>) -> Option<i64> {
    if let Some(ms) = object.get("debounceMs").and_then(JsonValue::as_i64) {
        return (ms > 0).then_some(ms);
    }
    match object.get("debounce") {
        Some(JsonValue::Bool(true)) => Some(200),
        Some(JsonValue::Number(number)) => number.as_i64().filter(|ms| *ms > 0),
        _ => None,
    }
}

fn cache_fields_from_fig(
    object: &serde_json::Map<String, JsonValue>,
) -> (Option<String>, Option<bool>, Option<i64>, Option<String>) {
    let nested = object.get("cache").and_then(JsonValue::as_object);
    let cache_key = object
        .get("cacheKey")
        .and_then(JsonValue::as_str)
        .or_else(|| nested.and_then(|cache| cache.get("cacheKey").or_else(|| cache.get("key"))?.as_str()))
        .map(ToOwned::to_owned);
    let cache_by_directory = object
        .get("cacheByDirectory")
        .and_then(JsonValue::as_bool)
        .or_else(|| nested.and_then(|cache| cache.get("cacheByDirectory")?.as_bool()));
    let cache_ttl_ms = object
        .get("cacheTtl")
        .and_then(JsonValue::as_i64)
        .or_else(|| nested.and_then(|cache| cache.get("ttl")?.as_i64()));
    let cache_strategy = object
        .get("cacheStrategy")
        .and_then(JsonValue::as_str)
        .or_else(|| nested.and_then(|cache| cache.get("strategy")?.as_str()))
        .map(ToOwned::to_owned);
    (cache_key, cache_by_directory, cache_ttl_ms, cache_strategy)
}

fn trigger_from_fig(value: &JsonValue) -> Option<GeneratorTrigger> {
    if let Some(string) = value.as_str() {
        return Some(GeneratorTrigger {
            on: "string".into(),
            string: Some(JsonValue::String(string.to_string())),
            length: None,
            js_trigger: None,
        });
    }
    let object = value.as_object()?;
    Some(GeneratorTrigger {
        on: object
            .get("on")
            .and_then(JsonValue::as_str)
            .unwrap_or("change")
            .to_string(),
        string: object.get("string").cloned(),
        length: object.get("length").and_then(JsonValue::as_i64),
        js_trigger: object
            .get("jsTrigger")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned),
    })
}

fn seed_from_fig_json(value: &JsonValue) -> Option<SuggestionSeed> {
    if let Some(name) = value.as_str() {
        return Some(SuggestionSeed {
            names: vec![name.to_string()],
            ..SuggestionSeed::default()
        });
    }
    let object = value.as_object()?;
    let names = json_names(object.get("name")).or_else(|| json_names(object.get("names")))?;
    Some(SuggestionSeed {
        names,
        description: object
            .get("description")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string(),
        meta: meta_from_fig(object),
        ..SuggestionSeed::default()
    })
}

fn meta_from_fig(object: &serde_json::Map<String, JsonValue>) -> SuggestionMeta {
    SuggestionMeta {
        suggestion_type: object.get("type").and_then(JsonValue::as_str).map(ToOwned::to_owned),
        original_type: object
            .get("originalType")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned),
        get_query_term: object
            .get("getQueryTerm")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned),
        js_get_query_term: object
            .get("jsGetQueryTerm")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned),
        insert_value: object
            .get("insertValue")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned),
        display_name: object
            .get("displayName")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned),
        separator_to_add: object
            .get("separatorToAdd")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned),
        should_add_space: object.get("shouldAddSpace").and_then(JsonValue::as_bool),
        hidden: object.get("hidden").and_then(JsonValue::as_bool).unwrap_or(false),
        priority: object.get("priority").and_then(JsonValue::as_i64),
        icon: object.get("icon").and_then(JsonValue::as_str).map(ToOwned::to_owned),
        is_dangerous: object.get("isDangerous").and_then(JsonValue::as_bool).unwrap_or(false),
    }
}

fn templates_from_fig(value: &JsonValue) -> Vec<Template> {
    let items: Vec<&str> = match value {
        JsonValue::String(item) => vec![item.as_str()],
        JsonValue::Array(items) => items.iter().filter_map(JsonValue::as_str).collect(),
        _ => return Vec::new(),
    };
    items
        .into_iter()
        .filter_map(|item| match item {
            "filepaths" => Some(Template::Filepaths),
            "folders" => Some(Template::Folders),
            "history" => Some(Template::History),
            "help" => Some(Template::Help),
            _ => None,
        })
        .collect()
}

fn filter_strategy_from_fig(value: Option<&JsonValue>) -> Option<FilterStrategy> {
    match value.and_then(JsonValue::as_str) {
        Some("prefix") => Some(FilterStrategy::Prefix),
        Some("fuzzy") => Some(FilterStrategy::Fuzzy),
        Some("default") => Some(FilterStrategy::Default),
        _ => None,
    }
}

fn parser_directives_from_fig(value: Option<&JsonValue>) -> Option<ParserDirectives> {
    let object = value.and_then(JsonValue::as_object)?;
    Some(ParserDirectives {
        options_must_precede_arguments: object.get("optionsMustPrecedeArguments").and_then(JsonValue::as_bool),
        flags_are_posix_noncompliant: object.get("flagsArePosixNoncompliant").and_then(JsonValue::as_bool),
        option_arg_separators: object
            .get("optionArgSeparators")
            .and_then(JsonValue::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(ToOwned::to_owned))
                    .collect()
            }),
        alias: object.get("alias").and_then(JsonValue::as_str).map(ToOwned::to_owned),
        js_alias: object.get("jsAlias").and_then(JsonValue::as_str).map(ToOwned::to_owned),
    })
}

fn json_names(value: Option<&JsonValue>) -> Option<Vec<String>> {
    let value = value?;
    if let Some(name) = value.as_str() {
        return Some(vec![name.to_string()]);
    }
    let items = value.as_array()?;
    let names: Vec<String> = items
        .iter()
        .filter_map(|item| item.as_str().map(ToOwned::to_owned))
        .collect();
    if names.is_empty() { None } else { Some(names) }
}

fn json_array(value: Option<&JsonValue>) -> &[JsonValue] {
    value.and_then(JsonValue::as_array).map_or(&[], Vec::as_slice)
}

fn json_string_list(value: Option<&JsonValue>) -> Vec<String> {
    match value {
        Some(JsonValue::String(item)) if !item.is_empty() => vec![item.clone()],
        Some(JsonValue::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str().map(ToOwned::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}
