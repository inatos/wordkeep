//! Library test surface for Wordkeep's MCP modules.
//!
//! The production entry point remains the lean `wordkeep` stdio binary. Keeping
//! the modules available as a library target lets `cargo test --lib` exercise
//! their unit tests without changing the binary's runtime structure.

#![allow(dead_code)]

mod artifacts;
mod big_functions;
mod cache;
pub mod call_graph;
mod call_path;
mod coeffects;
mod commit_scope;
mod config;
mod continuation;
#[cfg(feature = "dashboard")]
mod dashboard;
mod dead_code;
mod defects;
pub mod diff_map;
mod doc_comment;
mod effect_journal;
mod include_graph;
mod incremental;
mod index_stale;
mod integration_hooks;
mod izakaya;
pub mod knowledge;
mod lang;
mod mas;
mod mcp;
mod module_map;
mod outline;
mod profile_upsert;
mod progress;
pub mod repo_map;
mod runs;
mod runtime;
mod session_pressure;
mod stats;
mod symbol_context;
mod symbol_def;
mod symbol_diff;
mod symbol_refs;
mod symbol_resolve;
mod test_map;
mod trace;
mod trace_profile;
mod triage;
mod type_layout;
mod undocumented;
mod usage_examples;
mod walk;
mod workspace;
