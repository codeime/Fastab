//! A deliberately small, fail-closed projection of public static IR.
//!
//! This does not generate, sort, insert, or send anything. Provenance is granted
//! at the static collection site, after the actual lookup root and path have
//! been checked against a pinned public release. It cannot be reconstructed
//! from a final row's kind or display name.

use std::collections::HashMap;
use std::sync::Arc;

use crate::ir::{Registry, Spec, SuggestionMeta};
use crate::lookup::ActiveArgSource;
use crate::runtime::{CompleteRequest, CompleteResult, Suggestion};

pub(crate) const MAX_CANDIDATES: usize = 20;
const MAX_INPUT_BYTES: usize = 256;
const MAX_NAME_BYTES: usize = 128;
const MAX_DESCRIPTION_BYTES: usize = 256;
const MAX_PATH_TOKENS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicAiContext {
    pub command_path: Vec<String>,
    pub token_prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicAiCandidate {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct QueryState {
    pub subcommands_allowed: bool,
    pub options_allowed: bool,
    pub end_of_options: bool,
    pub active_arg_source: Option<ActiveArgSource>,
    pub active_arg_only_suggest_args: bool,
    /// Any parser transition through an argument loadSpec/isCommand-like
    /// value leaves the literal public spec path, even if the loaded spec
    /// happens to reuse the same names as a pinned node.
    pub crossed_loaded_spec: bool,
}

fn word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
}

fn static_path_node(spec: &Spec) -> bool {
    !spec.meta.ai_resolved_reference
        && !spec.meta.ai_generated
        && spec.load_spec.is_none()
        && spec.js_load_spec.is_none()
}

/// Only the whole, unquoted buffer at its end is eligible. Completed tokens
/// must be literal public command/subcommand names, never argument values or
/// consumed options. Alias definitions may exist, but any effective token
/// rewrite (including a wrapper) makes this path ineligible.
pub(crate) fn context(
    registry: &mut Registry,
    request: &CompleteRequest,
    root: &Arc<Spec>,
    current: &Spec,
    resolved_tokens: &[String],
    state: QueryState,
) -> Option<PublicAiContext> {
    if !request.include_public_ai
        || request.history_only
        || request.buffer.is_empty()
        || request.buffer.len() > MAX_INPUT_BYTES
        || request
            .cursor
            .is_some_and(|cursor| cursor as usize != request.buffer.len())
        || !request.buffer.bytes().all(|byte| word_byte(byte) || byte == b' ')
    {
        return None;
    }
    let words: Vec<&str> = request.buffer.split_ascii_whitespace().collect();
    if words.len() > MAX_PATH_TOKENS + 1 || !words.iter().copied().eq(resolved_tokens.iter().map(String::as_str)) {
        return None;
    }
    let command = *words.first()?;
    if !registry.is_public_ai_root(command, root) || !root.has_name(command) {
        return None;
    }
    let finished = if request.buffer.ends_with(' ') {
        words.len()
    } else {
        words.len().checked_sub(1)?
    };
    if finished == 0 || finished > MAX_PATH_TOKENS {
        return None;
    }
    let token_prefix = words.get(finished).copied().unwrap_or_default();
    if state.crossed_loaded_spec {
        return None;
    }
    let may_suggest_subcommands =
        state.subcommands_allowed && !state.end_of_options && !state.active_arg_only_suggest_args;
    let may_suggest_options = state.options_allowed && !state.end_of_options && !state.active_arg_only_suggest_args;
    if !may_suggest_subcommands && !may_suggest_options {
        return None;
    }
    match state.active_arg_source {
        Some(ActiveArgSource::OptionValue) => return None,
        Some(ActiveArgSource::OptionName) if !token_prefix.starts_with('-') || !may_suggest_options => {
            return None;
        },
        Some(ActiveArgSource::PositionalValue)
            if finished != 1 && !(token_prefix.starts_with('-') && may_suggest_options) =>
        {
            return None;
        },
        _ => {},
    }
    let mut node = root.as_ref();
    if !static_path_node(node) {
        return None;
    }
    for token in &words[1..finished] {
        node = node.find_subcommand(token)?;
        if !static_path_node(node) {
            return None;
        }
    }
    // `walk_spec` owns a cloned node and may have merged a generateSpec
    // result into it. Trust only if it still represents this literal static
    // path. Generated descendants are filtered at their collection sites.
    if !static_path_node(current) || node.names != current.names {
        return None;
    }
    Some(PublicAiContext {
        command_path: words[..finished].iter().map(|word| (*word).to_owned()).collect(),
        token_prefix: token_prefix.to_owned(),
    })
}

/// Called only while constructing a row from a verified static collection.
/// The bounded metadata copies contain no insertion string, history, or icon.
pub(crate) fn mark_candidate(suggestion: &mut Suggestion, meta: &SuggestionMeta) {
    if meta.ai_resolved_reference
        || meta.ai_generated
        || meta.js_get_query_term.is_some()
        || meta.get_query_term.is_some()
        || meta.suggestion_type.is_some()
        || suggestion.hidden
        || suggestion.is_dangerous
        || suggestion.name.is_empty()
        || suggestion.name.len() > MAX_NAME_BYTES
        || !suggestion.name.bytes().all(word_byte)
        || suggestion
            .insert_value
            .as_deref()
            .is_some_and(|value| value != suggestion.name)
        || suggestion
            .separator_to_add
            .as_deref()
            .is_some_and(|separator| !matches!(separator, "" | " " | "=" | ":"))
        || suggestion.description.chars().any(char::is_control)
    {
        return;
    }
    let mut end = suggestion.description.len().min(MAX_DESCRIPTION_BYTES);
    while !suggestion.description.is_char_boundary(end) {
        end -= 1;
    }
    suggestion.public_ai_candidate = Some(PublicAiCandidate {
        name: suggestion.name.clone(),
        description: suggestion.description[..end].to_owned(),
    });
}

pub(crate) fn clear(result: &mut CompleteResult) {
    result.public_ai_context = None;
    for suggestion in &mut result.suggestions {
        suggestion.public_ai_candidate = None;
    }
}

/// Check every marked name against all rows before local deduplication can
/// hide an ambiguous static/dynamic or history origin. Only marked names use
/// auxiliary storage; generated and history rows are scanned once.
pub(crate) fn validate_provenance(result: &mut CompleteResult) {
    let Some(context) = result.public_ai_context.as_ref() else {
        return;
    };
    if result.pending_generators {
        clear(result);
        return;
    }
    let mut counts: HashMap<String, u8> = result
        .suggestions
        .iter()
        .filter(|suggestion| suggestion.public_ai_candidate.is_some())
        .map(|suggestion| (suggestion.name.clone(), 0))
        .collect();
    for suggestion in &result.suggestions {
        if let Some(count) = counts.get_mut(&suggestion.name) {
            *count = count.saturating_add(1).min(2);
        }
    }
    let mut accepted = 0;
    for suggestion in &mut result.suggestions {
        if suggestion.public_ai_candidate.is_some() {
            if counts.get(&suggestion.name) == Some(&1) && safe_public_row(suggestion, context) {
                accepted += 1;
            } else {
                suggestion.public_ai_candidate = None;
            }
        }
    }
    if accepted < 2 {
        clear(result);
    }
}

/// Apply the request budget only after local history, matching, acceptance
/// recency and priority have determined the ordinary list's final order.
pub(crate) fn finalize_ranked_candidates(result: &mut CompleteResult) {
    let Some(context) = result.public_ai_context.as_ref() else {
        return;
    };
    if result.pending_generators {
        clear(result);
        return;
    }
    let mut accepted = 0;
    for suggestion in &mut result.suggestions {
        if suggestion.public_ai_candidate.is_some() {
            if accepted < MAX_CANDIDATES && safe_public_row(suggestion, context) {
                accepted += 1;
            } else {
                suggestion.public_ai_candidate = None;
            }
        }
    }
    if accepted < 2 {
        clear(result);
    }
}

fn safe_public_row(suggestion: &Suggestion, context: &PublicAiContext) -> bool {
    let Some(candidate) = suggestion.public_ai_candidate.as_ref() else {
        return false;
    };
    candidate.name == suggestion.name
        && suggestion.name.starts_with(&context.token_prefix)
        && !suggestion.hidden
        && !suggestion.is_dangerous
        && matches!(suggestion.kind.as_str(), "cmd" | "subcommand" | "option")
        && !matches!(suggestion.original_type.as_deref(), Some("auto-execute" | "special"))
        && suggestion.query_term.is_none()
        && !suggestion.name.is_empty()
        && suggestion.name.len() <= MAX_NAME_BYTES
        && suggestion.name.bytes().all(word_byte)
        && suggestion
            .insert_value
            .as_deref()
            .is_none_or(|value| value == suggestion.name)
        && suggestion
            .separator_to_add
            .as_deref()
            .is_none_or(|separator| matches!(separator, "" | " " | "="))
        && candidate.description.len() <= MAX_DESCRIPTION_BYTES
        && !candidate.description.chars().any(char::is_control)
}
