//! Headless completion engine.
//!
//! Bundled Fig specs are compiled to static JSON IR at build time. Lookup and
//! generators run in Rust.

mod cobra;
#[cfg(all(test, feature = "js-compat"))]
mod dual_path;
#[cfg(all(test, feature = "js-compat"))]
mod engine_golden;
mod filegen;
mod generate;
mod history;
mod hook_backend;
#[cfg(test)]
mod hook_baseline;
mod hook_types;
mod ir;
#[cfg(feature = "js-compat")]
mod js_host;
mod lookup;
mod native_adapters;
mod process;
mod query;
mod rank;
mod runtime;
mod snapshot;
mod spec_pair;
mod typed_hook;
mod versioned;
mod worker;

pub use ir::{ArgSpec, Builtin, OptionSpec, Registry, Spec, Template};
pub use lookup::{completion_buffer, current_command_slice, tokenize};
pub use native_adapters::{dump_native_adapter_catalog, native_adapter_catalog_path};
pub use rank::{ACCEPTANCE_STATE_KEY, AcceptanceIndex};
pub use runtime::{CompleteRequest, CompleteResult, CurrentArg, Engine, Suggestion, ranking_root_command};
pub use worker::{EngineClient, default_specs_dir, engine_attempt_timeout, ui_completion_deadline};
