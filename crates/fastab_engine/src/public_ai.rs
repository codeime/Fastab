//! A deliberately small, fail-closed projection of public static IR.
//!
//! This does not generate, sort, insert, or send anything. Provenance is granted
//! at the static collection site, after the actual lookup root and path have
//! been checked against a pinned public release. It cannot be reconstructed
//! from a final row's kind or display name.

use std::sync::Arc;

use crate::ir::{Registry, Spec, SuggestionMeta};
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

fn word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
}

fn plain_node(spec: &Spec) -> bool {
    !spec.meta.ai_resolved_reference
        && spec.load_spec.is_none()
        && spec.js_load_spec.is_none()
        && spec.js_generate_spec.is_none()
        && spec.args.is_empty()
        && spec.additional_suggestions.is_empty()
        && spec.parser_directives.as_ref().is_none_or(|directives| {
            directives.alias.is_none() && directives.js_alias.is_none()
        })
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
) -> Option<PublicAiContext> {
    if !request.include_public_ai
        || request.history_only
        || request.buffer.is_empty()
        || request.buffer.len() > MAX_INPUT_BYTES
        || request.cursor.is_some_and(|cursor| cursor as usize != request.buffer.len())
        || !request.buffer.bytes().all(|byte| word_byte(byte) || byte == b' ')
    {
        return None;
    }
    let words: Vec<&str> = request.buffer.split_ascii_whitespace().collect();
    if words.len() > MAX_PATH_TOKENS + 1
        || !words.iter().copied().eq(resolved_tokens.iter().map(String::as_str))
    {
        return None;
    }
    let command = *words.first()?;
    if !registry.is_public_ai_root(command, root) || !root.has_name(command) {
        return None;
    }
    let finished = if request.buffer.ends_with(' ') { words.len() } else { words.len().checked_sub(1)? };
    if finished == 0 || finished > MAX_PATH_TOKENS {
        return None;
    }
    let mut node = root.as_ref();
    if !plain_node(node) {
        return None;
    }
    for token in &words[1..finished] {
        node = node.find_subcommand(token)?;
        if !plain_node(node) {
            return None;
        }
    }
    // Check the node that the real parser actually selected, not just a
    // parallel name walk. A generated/replaced node cannot inherit trust.
    if node != current
        || current.subcommands.iter().any(|child| child.meta.js_get_query_term.is_some())
        || current.options.iter().chain(&current.persistent_options)
            .any(|option| option.meta.js_get_query_term.is_some())
    {
        return None;
    }
    Some(PublicAiContext {
        command_path: words[..finished].iter().map(|word| (*word).to_owned()).collect(),
        token_prefix: words.get(finished).copied().unwrap_or_default().to_owned(),
    })
}

/// Called only while constructing a row from a verified static collection.
/// The bounded metadata copies contain no insertion string, history, or icon.
pub(crate) fn mark_candidate(suggestion: &mut Suggestion, meta: &SuggestionMeta, remaining: &mut usize) {
    if *remaining == 0
        || meta.ai_resolved_reference
        || meta.js_get_query_term.is_some()
        || meta.get_query_term.is_some()
        || meta.suggestion_type.is_some()
        || suggestion.hidden
        || suggestion.is_dangerous
        || suggestion.name.is_empty()
        || suggestion.name.len() > MAX_NAME_BYTES
        || !suggestion.name.bytes().all(word_byte)
        || suggestion.insert_value.as_deref().is_some_and(|value| value != suggestion.name)
        || suggestion.separator_to_add.as_deref().is_some_and(|separator| !matches!(separator, "" | " " | "=" | ":"))
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
    *remaining -= 1;
}

pub(crate) fn clear(result: &mut CompleteResult) {
    result.public_ai_context = None;
    for suggestion in &mut result.suggestions {
        suggestion.public_ai_candidate = None;
    }
}

/// Run before local deduplication can hide an ambiguous origin, then again
/// after history/ranking. This never removes or reorders ordinary rows.
pub(crate) fn finalize(result: &mut CompleteResult) {
    let Some(context) = result.public_ai_context.as_ref() else {
        return;
    };
    if result.pending_generators {
        clear(result);
        return;
    }
    let mut accepted = 0;
    // At most MAX_CANDIDATES marked rows are inspected; do not allocate an
    // auxiliary set proportional to an unbounded generator/history result.
    for index in 0..result.suggestions.len() {
        let candidate = &result.suggestions[index];
        if candidate.public_ai_candidate.is_none() {
            continue;
        }
        let eligible = candidate.name.starts_with(&context.token_prefix)
            && !result.suggestions.iter().enumerate().any(|(other_index, other)| {
                other_index != index && other.name == candidate.name
            });
        if eligible {
            accepted += 1;
        } else {
            result.suggestions[index].public_ai_candidate = None;
        }
    }
    if accepted < 2 {
        clear(result);
    }
}
