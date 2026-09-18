//! Shared hook types used by the native backend (typed IR + named adapters).

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// The subset of `Fig.ShellContext` that generators actually read.
#[derive(Debug, Clone, Default)]
pub struct ShellContext {
    pub current_process: String,
    pub environment_variables: Arc<Vec<(String, String)>>,
}

#[derive(Debug, Clone)]
pub struct ScriptCommand {
    pub command: String,
    pub args: Vec<String>,
    pub timeout_ms: Option<i64>,
}

/// Development/test diagnostic side channel. The normal hook APIs keep their
/// historical `Option` returns so a missing source, a JavaScript failure, and
/// a valid empty result do not look identical while investigating the native
/// migration. It intentionally carries no hook input, cwd, shell, environment,
/// command, or error text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum HookDiagnostic {
    Success,
    EmptyResult,
    SourceMissing,
    RuntimeUnavailable,
    EvalError,
    InvokeError,
    PromiseRejected,
    PromiseError,
    PromisePending,
    Timeout,
    JsonConversionError,
    ResultConversionError,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookDiagnosticRecord {
    pub hook_id: String,
    pub outcome: HookDiagnostic,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HookContext {
    #[serde(rename = "currentWorkingDirectory")]
    pub current_working_directory: String,
    #[serde(rename = "currentProcess")]
    pub current_process: String,
    #[serde(rename = "sshPrefix")]
    pub ssh_prefix: String,
    #[serde(rename = "environmentVariables")]
    pub environment_variables: BTreeMap<String, String>,
    #[serde(rename = "searchTerm")]
    pub search_term: String,
    #[serde(rename = "isDangerous")]
    pub is_dangerous: bool,
}

impl HookContext {
    pub fn from_shell(cwd: &str, shell: &ShellContext, search_term: &str, is_dangerous: bool) -> Self {
        Self {
            current_working_directory: cwd.to_string(),
            current_process: shell.current_process.clone(),
            ssh_prefix: String::new(),
            environment_variables: shell.environment_variables.iter().cloned().collect(),
            search_term: search_term.to_string(),
            is_dangerous,
        }
    }
}

pub fn clean_output(output: &str) -> String {
    output
        .replace("\r\n", "\n")
        .replace("\x1b[?25h", "")
        .trim_start_matches('\n')
        .trim_end_matches('\n')
        .to_string()
}
