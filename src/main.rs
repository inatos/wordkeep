//! wordkeep - an AI-agnostic MCP server that hands coding agents
//! token-budgeted, high-signal context instead of full-file dumps.
//!
//! Transport: MCP stdio (newline-delimited JSON-RPC 2.0). No async runtime,
//! no network, single static binary - fast cold start when a client spawns it.
//!
//! Tools:
//!   * repo_map         - structural map of the source tree (types, namespaces, signatures)
//!   * outline          - single-file table of contents with line numbers
//!   * symbol_refs      - find where a symbol is defined, called, and referenced
//!   * call_graph       - one hop of callers/callees for a function, to scope a refactor
//!   * call_path        - the shortest call chain between two functions (depth-bounded BFS)
//!   * doc_comment      - a symbol's leading doc comment + signature, no body
//!   * symbol_context   - a symbol's body + one call-graph hop + its type_layout, composed
//!   * usage_examples   - a symbol's top call sites with surrounding context lines
//!   * undocumented     - exported symbols lacking a doc comment, ranked by fan-in
//!   * dead_code        - defined symbols with zero callers and zero references
//!   * big_functions    - largest function bodies by line span, ranked
//!   * symbol_diff      - how one symbol's definition changed vs a git ref
//!   * module_map       - symbols grouped by module + cross-module call edges
//!   * diff_map         - functions a git diff changed and their immediate callers
//!   * include_graph    - one hop of #include includers/includees for a header
//!   * type_layout      - fields of a struct/class, flagging non-POD members
//!   * test_map         - which tests reference a symbol, to pick the narrowest run
//!   * knowledge_search - BM25 retrieval over docs/, designs, and .cursor rules
//!   * knowledge_upsert - write/update a markdown section for knowledge_search
//!   * trace_summary    - condense a Tracy CSV export into the hottest zones (or diff two)
//!   * trace_profile    - hitch workflow: max-sorted zones + diff_map + index_stale
//!   * integration_hooks - curated cross-subsystem call sites + optional call_path
//!   * index_stale      - detect when on-disk indexes lag git changes
//!   * stats            - how many tokens wordkeep has saved vs reading raw material
//!   * mas_post         - append compact state to a recursive-MAS session blackboard
//!   * mas_read         - read token-budgeted blackboard entries for subagent handoff
//!   * mas_status       - recursion round bookkeeping and convergence hint
//!   * mas_finalize     - finalize a session and optionally promote to knowledge notes
//!   * session_handoff  - paste-ready next-session prime from MAS/defects/runs
//!   * defect_upsert    - structured defect registry write
//!   * defect_list      - list unresolved defects (eyeball_fail first)
//!   * run_record       - metadata-only gate/run history write
//!   * run_history      - filter recent runs and missing artifacts
//!   * artifact_index   - metadata index over configured artifact roots
//!   * session_pressure - heuristic context-pressure signal
//!   * commit_scope     - read-only git dirty-path grouping
//!
//! Languages: C/C++, GLSL, Rust, Python, C#, and TypeScript via tree-sitter (GLSL
//! through tree-sitter-glsl, a tree-sitter-c fork sharing the C node kinds;
//! TSX and Svelte ride the TypeScript grammar - Svelte by parsing only its
//! `<script>` blocks); Daslang (.das) via a lightweight line scanner, or the
//! vendored grammar when built with `--features daslang`. symbol_refs/call_graph
//! cover the tree-sitter languages; Daslang is mapped by repo_map. doc_comment,
//! symbol_context, and diff_map compose those extractors and parse through the
//! `incremental` tree cache so re-touching a file in a session stays warm.
//! Cross-cutting helpers: `lang` (extension → grammar dispatch), `cache` (on-disk
//! mtime caches so a cold spawn reuses prior work), `incremental` (warm
//! tree-sitter trees within a session), `symbol_def` (shared definition locator),
//! `walk` (skips build/vendor/target and .gitignore'd dirs), `stats` (savings).

mod artifacts;
mod big_functions;
mod cache;
mod call_graph;
mod call_path;
mod commit_scope;
mod config;
#[cfg(feature = "dashboard")]
mod dashboard;
mod dead_code;
mod defects;
mod diff_map;
mod doc_comment;
mod include_graph;
mod incremental;
mod index_stale;
mod integration_hooks;
mod knowledge;
mod lang;
mod mas;
mod mcp;
mod module_map;
mod outline;
mod repo_map;
mod runs;
mod session_pressure;
mod stats;
mod symbol_context;
mod symbol_def;
mod symbol_diff;
mod symbol_refs;
mod test_map;
mod trace;
mod trace_profile;
mod type_layout;
mod undocumented;
mod usage_examples;
mod walk;
mod workspace;

use serde_json::json;
use std::path::PathBuf;
use std::time::Instant;

/// Wrap a tool handler to record wall-clock latency and outcome via [`stats::finish`].
fn instrument(name: &'static str, inner: mcp::Handler) -> mcp::Handler {
    Box::new(move |args| {
        let t0 = Instant::now();
        let r = inner(args);
        stats::finish(name, t0.elapsed().as_micros() as u64, &r);
        r
    })
}

fn main() {
    // Root resolution order: --root flag > WORDKEEP_ROOT env > cwd.
    let mut root = std::env::var("WORDKEEP_ROOT")
        .map(PathBuf::from)
        .ok()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    let mut want_dashboard = false;
    let mut run_record_args: Option<Vec<String>> = None;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--root" => {
                if let Some(p) = it.next() {
                    root = PathBuf::from(p);
                }
            }
            "dashboard" => want_dashboard = true,
            "run-record" => {
                run_record_args = Some(it.collect());
                break;
            }
            "--help" | "-h" => {
                eprintln!(
                    "wordkeep [--root <workspace>]   speak MCP over stdio (default)\n\
                     wordkeep run-record [flags]     record gate/run metadata (no command exec)\n\
                     wordkeep dashboard              live savings dashboard (build --features dashboard)"
                );
                return;
            }
            _ => {}
        }
    }

    if let Some(argv) = run_record_args {
        match runs::cli_record(&root, &argv) {
            Ok(()) => return,
            Err(code) => std::process::exit(code),
        }
    }

    if want_dashboard {
        #[cfg(feature = "dashboard")]
        {
            if let Err(e) = dashboard::run() {
                eprintln!("[wordkeep] dashboard error: {e}");
                std::process::exit(1);
            }
            return;
        }
        #[cfg(not(feature = "dashboard"))]
        {
            eprintln!(
                "[wordkeep] the `dashboard` subcommand requires building with `--features dashboard`."
            );
            std::process::exit(2);
        }
    }

    eprintln!("[wordkeep] root = {}", root.display());
    stats::set_workspace_root(&root);

    let root_map = root.clone();
    let root_outline = root.clone();
    let root_refs = root.clone();
    let root_cg = root.clone();
    let root_callpath = root.clone();
    let root_doc = root.clone();
    let root_ctx = root.clone();
    let root_usage = root.clone();
    let root_undoc = root.clone();
    let root_dead = root.clone();
    let root_big = root.clone();
    let root_symdiff = root.clone();
    let root_modmap = root.clone();
    let root_diff = root.clone();
    let root_inc = root.clone();
    let root_layout = root.clone();
    let root_tests = root.clone();
    let root_kb = root.clone();
    let root_kb_upsert = root.clone();
    let root_trace = root.clone();
    let root_trace_profile = root.clone();
    let root_hooks = root.clone();
    let root_stale = root.clone();
    let root_mas_post = root.clone();
    let root_mas_read = root.clone();
    let root_mas_status = root.clone();
    let root_mas_finalize = root.clone();
    let root_session_handoff = root.clone();
    let root_defect_upsert = root.clone();
    let root_defect_list = root.clone();
    let root_run_record = root.clone();
    let root_run_history = root.clone();
    let root_artifact_index = root.clone();
    let root_session_pressure = root.clone();
    let root_commit_scope = root;

    let raw_tools = vec![
        mcp::Tool {
            name: "repo_map",
            description: "Token-budgeted STRUCTURAL map of the source tree (C/C++, GLSL, Rust, \
                          Python, Daslang, and C#): top-level namespaces, types, and function signatures \
                          per file. Call this INSTEAD of reading whole files when you need to \
                          locate code or understand layout cheaply.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "paths": { "type": "array", "items": { "type": "string" },
                               "description": "Subdirs (relative to root) to map. Default: profile / default_paths / src." },
                    "profile": { "type": "string",
                                 "description": "Named path_profiles entry from .wordkeep/config.json (ignored when paths is set)." },
                    "pattern": { "type": "string",
                                 "description": "Case-insensitive substring filter on file path." },
                    "token_budget": { "type": "integer",
                                      "description": "Approx max tokens to return. Default 8000." }
                }
            }),
            handler: Box::new(move |args| repo_map::build(&root_map, args)),
        },
        mcp::Tool {
            name: "outline",
            description: "Token-budgeted TABLE OF CONTENTS for a SINGLE file: its top-level \
                          namespaces, types, and function signatures, each with a line number, \
                          in source order. Use it to jump straight to a definition (read just \
                          that line range) instead of reading the whole file. Covers C/C++, GLSL, \
                          Rust, Python, TypeScript/TSX/Svelte, C#, and Daslang.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file": { "type": "string",
                              "description": "Path (relative to root) of the file to outline. Alias: path." },
                    "path": { "type": "string",
                              "description": "Alias for file; same meaning." },
                    "token_budget": { "type": "integer",
                                      "description": "Approx max tokens to return. Default 2400." }
                }
            }),
            handler: Box::new(move |args| outline::build(&root_outline, args)),
        },
        mcp::Tool {
            name: "symbol_refs",
            description: "Find where a symbol is DEFINED, CALLED, and otherwise referenced \
                          across the tree (C/C++, GLSL, Rust, Python, C#, TypeScript/TSX/Svelte). Uses \
                          tree-sitter to classify each exact-name occurrence, so it ignores comments, \
                          strings, and substring collisions. The first call warms a per-file memo; \
                          later calls for any symbol skip re-parsing. Use to trace a function/type \
                          before changing it.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "symbol": { "type": "string",
                                "description": "Exact symbol name to locate (function, type, method)." },
                    "paths": { "type": "array", "items": { "type": "string" },
                               "description": "Subdirs (relative to root) to search. Default: .wordkeep/config.json default_paths, else src only." },
                    "kind": { "type": "string", "enum": ["all", "def", "call", "ref"],
                              "description": "Restrict to definitions, calls, or plain refs. Default \"all\"." },
                    "max": { "type": "integer", "description": "Max occurrences to list. Default 60." },
                    "token_budget": { "type": "integer",
                                      "description": "Approx max tokens to return. Default 1200." }
                },
                "required": ["symbol"]
            }),
            handler: Box::new(move |args| symbol_refs::find(&root_refs, args)),
        },
        mcp::Tool {
            name: "call_graph",
            description: "One hop of the call graph for a function (C/C++, GLSL, Rust, Python, \
                          TypeScript/TSX/Svelte): the functions it CALLS (callees) and the functions \
                          that CALL it (callers), each with a file:line site. Built on the same \
                          tree-sitter classifier as symbol_refs, with results cached per file. Use \
                          to scope the blast radius before changing a signature. Names match on their \
                          trailing :: segment, so `Bar`, `Foo::Bar`, and `void Foo::Bar()` all line up.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "symbol": { "type": "string",
                                "description": "Function name to analyze (plain or qualified)." },
                    "paths": { "type": "array", "items": { "type": "string" },
                               "description": "Subdirs (relative to root) to search. Default: .wordkeep/config.json default_paths, else src only." },
                    "direction": { "type": "string", "enum": ["both", "callers", "callees"],
                                   "description": "Which edges to report. Default \"both\"." },
                    "max": { "type": "integer", "description": "Max entries per list. Default 60." },
                    "token_budget": { "type": "integer",
                                      "description": "Approx max tokens to return. Default 1200." }
                },
                "required": ["symbol"]
            }),
            handler: Box::new(move |args| call_graph::build(&root_cg, args)),
        },
        mcp::Tool {
            name: "call_path",
            description: "Given two functions, return the shortest CALL CHAIN from one to the \
                          other - a depth-bounded, cycle-safe BFS over the same one-hop edges \
                          call_graph distils - so you can trace how a leaf change propagates up \
                          to an entry point (or down to a primitive) without walking call_graph \
                          by hand. Names match on their trailing :: segment. Token-budgeted.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "from": { "type": "string",
                              "description": "Caller-side function to start from (plain or qualified)." },
                    "to": { "type": "string",
                            "description": "Callee-side function to reach (plain or qualified)." },
                    "paths": { "type": "array", "items": { "type": "string" },
                               "description": "Subdirs (relative to root) to search. Default: .wordkeep/config.json default_paths, else src only." },
                    "max_depth": { "type": "integer",
                                   "description": "Max hops to search. Default 8." },
                    "token_budget": { "type": "integer",
                                      "description": "Approx max tokens to return. Default 1200." }
                },
                "required": ["from", "to"]
            }),
            handler: Box::new(move |args| call_path::build(&root_callpath, args)),
        },
        mcp::Tool {
            name: "doc_comment",
            description: "Given a symbol, return JUST its leading documentation (///, /** */, \
                          \"\"\"…\"\"\", JSDoc) plus its signature - its intent and contract without \
                          the body. tree-sitter reads the comment nodes adjacent to the \
                          declaration; the definition is found by the shared symbol locator. \
                          Covers C/C++, GLSL, Rust, Python, TypeScript/TSX/Svelte.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "symbol": { "type": "string",
                                "description": "Symbol whose doc comment to return (function, type, method)." },
                    "paths": { "type": "array", "items": { "type": "string" },
                               "description": "Subdirs (relative to root) to search. Default: .wordkeep/config.json default_paths, else src only." }
                },
                "required": ["symbol"]
            }),
            handler: Box::new(move |args| doc_comment::build(&root_doc, args)),
        },
        mcp::Tool {
            name: "symbol_context",
            description: "Given a symbol, return its definition body PLUS one hop of call_graph \
                          (callees + callers) and, when it is a record, its type_layout - composing \
                          the existing extractors into a single 'everything I need to edit this \
                          safely' payload, still token-budgeted. Covers C/C++, GLSL, Rust, Python, \
                          TypeScript/TSX/Svelte.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "symbol": { "type": "string",
                                "description": "Symbol to assemble context for (function, type, method)." },
                    "paths": { "type": "array", "items": { "type": "string" },
                               "description": "Subdirs (relative to root) to search. Default: .wordkeep/config.json default_paths, else src only." },
                    "max": { "type": "integer", "description": "Max callers/callees to list. Default 40." },
                    "token_budget": { "type": "integer",
                                      "description": "Approx max tokens to return. Default 1600." }
                },
                "required": ["symbol"]
            }),
            handler: Box::new(move |args| symbol_context::build(&root_ctx, args)),
        },
        mcp::Tool {
            name: "usage_examples",
            description: "Given a symbol, return its few highest-signal CALL SITES with \
                          surrounding context lines (not just locations), so you see how an API \
                          is actually used in practice. Reuses the symbol_refs call classifier to \
                          find the sites, then reads a few lines around each. Token-budgeted.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "symbol": { "type": "string",
                                "description": "Symbol whose call sites to show." },
                    "paths": { "type": "array", "items": { "type": "string" },
                               "description": "Subdirs (relative to root) to search. Default: .wordkeep/config.json default_paths, else src only." },
                    "max": { "type": "integer", "description": "Max call sites to show. Default 5." },
                    "context": { "type": "integer",
                                 "description": "Context lines above/below each site. Default 2." },
                    "token_budget": { "type": "integer",
                                      "description": "Approx max tokens to return. Default 1200." }
                },
                "required": ["symbol"]
            }),
            handler: Box::new(move |args| usage_examples::build(&root_usage, args)),
        },
        mcp::Tool {
            name: "undocumented",
            description: "List exported symbols (functions and records) that LACK a leading doc \
                          comment, ranked by fan-in (how many distinct functions call them), so \
                          you know which public APIs most need documentation. Reuses the \
                          doc_comment locator for doc-presence and call_graph for caller counts. \
                          Skips leading-underscore names unless \"include_private\". Token-budgeted.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "paths": { "type": "array", "items": { "type": "string" },
                               "description": "Subdirs (relative to root) to scan. Default: .wordkeep/config.json default_paths, else src only." },
                    "include_private": { "type": "boolean",
                                         "description": "Include leading-underscore names. Default false." },
                    "max": { "type": "integer", "description": "Max symbols to list. Default 40." },
                    "token_budget": { "type": "integer",
                                      "description": "Approx max tokens to return. Default 1200." }
                }
            }),
            handler: Box::new(move |args| undocumented::build(&root_undoc, args)),
        },
        mcp::Tool {
            name: "dead_code",
            description: "List defined symbols (functions and records) with ZERO callers and \
                          ZERO non-def references across the tree - likely removable exports. \
                          Reuses call_graph fan-in and symbol_refs occurrence tallies. Tags \
                          public/exported/header symbols [public], probable entry points \
                          (main, test files) [entry?], symbols used only under generated_paths \
                          [gen-only], and stale symbols [stale Nd] via git last-touch. Ranks \
                          clean stale dead code first. Token-budgeted.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "paths": { "type": "array", "items": { "type": "string" },
                               "description": "Subdirs (relative to root) to scan. Default: .wordkeep/config.json default_paths, else src only." },
                    "generated_paths": { "type": "array", "items": { "type": "string" },
                                         "description": "Optional extra dirs (e.g. build, vendor) to scan for \
                                                         references only; symbols used there but not in paths \
                                                         are tagged [gen-only]. Default []." },
                    "stale_days": { "type": "integer",
                                    "description": "Tag and rank symbols in files last touched this many days ago or more. Default 90." },
                    "include_private": { "type": "boolean",
                                         "description": "Include leading-underscore names. Default false." },
                    "max": { "type": "integer", "description": "Max symbols to list. Default 40." },
                    "token_budget": { "type": "integer",
                                      "description": "Approx max tokens to return. Default 1200." }
                }
            }),
            handler: Box::new(move |args| dead_code::build(&root_dead, args)),
        },
        mcp::Tool {
            name: "big_functions",
            description: "List the largest function bodies by line span, ranked, as refactor \
                          candidates - find complex code without reading every file. Reuses \
                          symbol_def function spans. Token-budgeted.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "paths": { "type": "array", "items": { "type": "string" },
                               "description": "Subdirs (relative to root) to scan. Default: .wordkeep/config.json default_paths, else src only." },
                    "max": { "type": "integer", "description": "Max functions to list. Default 30." },
                    "min_lines": { "type": "integer",
                                   "description": "Minimum line span to include. Default 1." },
                    "token_budget": { "type": "integer",
                                      "description": "Approx max tokens to return. Default 1200." }
                }
            }),
            handler: Box::new(move |args| big_functions::build(&root_big, args)),
        },
        mcp::Tool {
            name: "symbol_diff",
            description: "Given a symbol and a git ref, show how THAT ONE symbol's definition \
                          changed - stacked old/new body excerpts when the signature is unchanged, \
                          a labeled old/new signature section plus span hunks when both changed \
                          (optional stacked old/new body when body is true), otherwise +/- hunks \
                          within its tree-sitter span. A focused complement to diff_map's tree-wide \
                          list. Token-budgeted.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "symbol": { "type": "string",
                                "description": "Symbol whose definition diff to show." },
                    "ref": { "type": "string",
                             "description": "Git revision to diff against. Default \"HEAD\"." },
                    "paths": { "type": "array", "items": { "type": "string" },
                               "description": "Subdirs to search for the definition. Default: .wordkeep/config.json default_paths, else src only." },
                    "context": { "type": "integer",
                                 "description": "Unified diff context lines. Default 3." },
                    "body": { "type": "boolean",
                              "description": "When signature changed, also append stacked old/new body excerpts. Default false." },
                    "token_budget": { "type": "integer",
                                      "description": "Approx max tokens to return. Default 1200." }
                },
                "required": ["symbol"]
            }),
            handler: Box::new(move |args| symbol_diff::build(&root_symdiff, args)),
        },
        mcp::Tool {
            name: "module_map",
            description: "Group symbols by directory/module and report CROSS-MODULE call edges \
                          (coupling) - outgoing (→) and incoming (←) - so you see architectural \
                          dependency shape at a glance. Optional per-edge caller→callee samples, \
                          min_edge count filter, and focus module prefix. Reuses call_graph adjacency. \
                          Token-budgeted.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "paths": { "type": "array", "items": { "type": "string" },
                               "description": "Subdirs (relative to root) to scan. Default: .wordkeep/config.json default_paths, else src only." },
                    "depth": { "type": "integer",
                               "description": "Truncate module path to this many components (e.g. 2 → src/audio)." },
                    "max": { "type": "integer", "description": "Max modules to list. Default 40." },
                    "samples": { "type": "integer",
                                 "description": "Max caller→callee pairs to show per outgoing edge. Default 3; 0 disables." },
                    "min_edge": { "type": "integer",
                                  "description": "Drop cross-module edges with fewer call pairs than this. Default 1." },
                    "focus": { "type": "string",
                               "description": "Show only modules matching this prefix or coupled to one that does. Default \"\" (all)." },
                    "token_budget": { "type": "integer",
                                      "description": "Approx max tokens to return. Default 1200." }
                }
            }),
            handler: Box::new(move |args| module_map::build(&root_modmap, args)),
        },
        mcp::Tool {
            name: "include_graph",
            description: "One hop of the C/C++ `#include` graph for a header: what it INCLUDES \
                          (includees) and which files INCLUDE it (includers). A fast line scanner \
                          (no preprocessor) - use it to scope the blast radius of an ABI/header \
                          change before editing a .h. Match is by basename, refined by path suffix \
                          when the query contains a '/'.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "header": { "type": "string",
                                "description": "Header to analyze: a basename (\"scripting.h\") or path suffix (\"scripting/scripting.h\")." },
                    "paths": { "type": "array", "items": { "type": "string" },
                               "description": "Subdirs (relative to root) to search. Default: .wordkeep/config.json default_paths, else src only." },
                    "token_budget": { "type": "integer",
                                      "description": "Approx max tokens to return. Default 1200." }
                },
                "required": ["header"]
            }),
            handler: Box::new(move |args| include_graph::build(&root_inc, args)),
        },
        mcp::Tool {
            name: "type_layout",
            description: "Dump the FIELDS of a struct/class/union (C/C++ or Rust) in declaration \
                          order - each field's type and name - and flag members that look non-POD \
                          (std::string/vector/map, smart pointers, std::function, virtuals, Rust \
                          String/Vec/Box/Rc). Use it to check struct/class fields stay POD-friendly or \
                          to understand a type's memory shape before changing it.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "type": { "type": "string",
                              "description": "Type name to lay out (struct/class/union)." },
                    "paths": { "type": "array", "items": { "type": "string" },
                               "description": "Subdirs (relative to root) to search. Default: .wordkeep/config.json default_paths, else src only." },
                    "token_budget": { "type": "integer",
                                      "description": "Approx max tokens to return. Default 1200." }
                },
                "required": ["type"]
            }),
            handler: Box::new(move |args| type_layout::build(&root_layout, args)),
        },
        mcp::Tool {
            name: "diff_map",
            description: "Given a git ref (default HEAD) or the working tree, list the functions \
                          whose definitions the diff CHANGED and, via one hop of call_graph, their \
                          immediate CALLERS - the blast radius of a change without reading every \
                          hunk. Parses the unified diff for changed lines, then maps them back to \
                          enclosing function spans. Returns a graceful note when git is unavailable \
                          or the path is not a repo.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "ref": { "type": "string",
                             "description": "Git revision to diff against (e.g. HEAD, main, a SHA). Default \"HEAD\"." },
                    "paths": { "type": "array", "items": { "type": "string" },
                               "description": "Subdirs (relative to root) to scope the diff and caller search. Default: .wordkeep/config.json default_paths, else src only." },
                    "max": { "type": "integer",
                             "description": "Max distinct changed symbols to resolve callers for. Default 40." },
                    "token_budget": { "type": "integer",
                                      "description": "Approx max tokens to return. Default 1200." }
                }
            }),
            handler: Box::new(move |args| diff_map::build(&root_diff, args)),
        },
        mcp::Tool {
            name: "test_map",
            description: "Given a symbol, list the TEST files that reference it (definitions, \
                          calls, refs), ranked by hit count, so you can run the narrowest relevant \
                          suite instead of everything. Reuses the symbol_refs tree-sitter classifier, \
                          scoped to the test tree (default [\"tests\"]). Optional \"tags\" filters to \
                          Catch2 tag(s) (e.g. [\"unit\"]) and prints a filter command.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "symbol": { "type": "string",
                                "description": "Symbol the tests should reference." },
                    "paths": { "type": "array", "items": { "type": "string" },
                               "description": "Test roots (relative to root). Default [\"tests\"]." },
                    "tags": { "type": "array", "items": { "type": "string" },
                              "description": "Catch2 tag filter - file must contain all listed tags (e.g. [\"unit\"])." },
                    "max": { "type": "integer", "description": "Max test files to list. Default 40." },
                    "token_budget": { "type": "integer",
                                      "description": "Approx max tokens to return. Default 1200." }
                },
                "required": ["symbol"]
            }),
            handler: Box::new(move |args| test_map::build(&root_tests, args)),
        },
        mcp::Tool {
            name: "knowledge_search",
            description: "BM25 retrieval over project markdown (docs/, designs, .cursor rules, README). \
                          Returns the top-k most relevant chunks for a query instead of always-loading \
                          every rule. Use to recall conventions, pitfalls, and design decisions.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string",
                               "description": "Natural-language or keyword query." },
                    "k": { "type": "integer", "description": "Chunks to return. Default 5." },
                    "roots": { "type": "array", "items": { "type": "string" },
                               "description": "Doc roots to search. Default default_doc_roots or .cursor/rules, docs, .github, README.md." },
                    "include_defects": { "type": "boolean",
                                         "description": "Boost unresolved .wordkeep/defects.json into results. Default true." },
                    "token_budget": { "type": "integer",
                                      "description": "Approx max tokens to return. Default 1200." },
                    "semantic": { "type": "boolean",
                                  "description": "Rerank BM25 hits with embeddings (only when built with --features embeddings). Default true." }
                },
                "required": ["query"]
            }),
            handler: Box::new(move |args| knowledge::search(&root_kb, args)),
        },
        mcp::Tool {
            name: "knowledge_upsert",
            description: "Write or update a markdown section under .cursor/rules/, docs/, \
                          .wordkeep/notes/, root README.md, or configured knowledge_write_roots \
                          so the next knowledge_search can retrieve it. Default mode upsert_section \
                          replaces an existing # heading body or appends a new section. mode pitfall \
                          formats symptom/root_cause/fix (+ optional test_tag). Reports all missing \
                          mode-specific fields together.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string",
                              "description": "Relative path (.md/.mdc). README.md or under allowed write roots." },
                    "heading": { "type": "string",
                                 "description": "Section heading (written as # heading). Required except mode pitfall." },
                    "body": { "type": "string",
                              "description": "Markdown body for the section (no leading #). Required except mode pitfall." },
                    "description": { "type": "string",
                                       "description": "Frontmatter description when creating a new .mdc file." },
                    "always_apply": { "type": "boolean",
                                      "description": "Frontmatter alwaysApply when creating a new .mdc file. Default false." },
                    "mode": { "type": "string",
                              "enum": ["upsert_section", "append_section", "replace_file", "pitfall"],
                              "description": "upsert_section (default): replace heading body or append; append_section: always append; replace_file: overwrite whole file; pitfall: structured symptom/root_cause/fix entry." },
                    "symptom": { "type": "string", "description": "Required for mode pitfall - what the user sees." },
                    "root_cause": { "type": "string", "description": "Required for mode pitfall - why it happens." },
                    "fix": { "type": "string", "description": "Required for mode pitfall - verified fix." },
                    "test_tag": { "type": "string", "description": "Optional Catch2 tag for mode pitfall (e.g. unit)." }
                },
                "required": ["path"]
            }),
            handler: Box::new(move |args| knowledge::upsert(&root_kb_upsert, args)),
        },
        mcp::Tool {
            name: "trace_summary",
            description: "Condense a Tracy capture into the hottest zones: \
                          total ms, share, call count, mean us, and source site. Use sort_by: \"max\" \
                          when the user reports hitches - mean can hide single-frame spikes. Accepts a \
                          pre-exported .csv (debug/trace_*.csv) or a native .tracy capture (exported \
                          on the fly via tracy-csvexport). Set \"baseline\" to diff two captures and \
                          surface per-zone regressions instead. For the full hitch workflow use trace_profile.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file": { "type": "string",
                              "description": "Trace to summarize: a .csv export or native .tracy capture (name or path). Default: newest .csv in dir." },
                    "baseline": { "type": "string",
                                  "description": "Second capture (.csv or .tracy) to diff against. Enables regression-diff mode." },
                    "dir": { "type": "string",
                             "description": "Directory of trace CSVs, relative to root. Default \"debug\"." },
                    "top": { "type": "integer", "description": "Rows to show. Default 15." },
                    "sort_by": { "type": "string", "enum": ["total", "mean", "max", "count"],
                                 "description": "Sort key (single-file mode). Default \"total\". Use \"max\" for hitch reports." },
                    "token_budget": { "type": "integer",
                                      "description": "Approx max tokens to return. Default 1200." }
                }
            }),
            handler: Box::new(move |args| trace::summary(&root_trace, args)),
        },
        mcp::Tool {
            name: "trace_profile",
            description: "Tracy hitch workflow in one call: trace_summary with sort_by=max (default), \
                          diff_map blast radius vs git ref, and index_stale freshness check. Use when \
                          the user attaches a .tracy capture and reports stutter/hitches.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file": { "type": "string",
                              "description": "Trace file (.csv or .tracy). Default: newest in dir." },
                    "baseline": { "type": "string",
                                  "description": "Optional second capture for regression diff inside trace_summary." },
                    "dir": { "type": "string", "description": "Trace directory relative to root. Default \"debug\"." },
                    "git_ref": { "type": "string",
                                 "description": "Git ref for diff_map and index_stale. Default \"HEAD\"." },
                    "paths": { "type": "array", "items": { "type": "string" },
                               "description": "Subdirs for diff_map. Default: .wordkeep/config.json default_paths, else src only." },
                    "top": { "type": "integer", "description": "Zone rows in trace section. Default 18." },
                    "diff_max": { "type": "integer", "description": "Max changed symbols in diff_map section. Default 25." },
                    "token_budget": { "type": "integer",
                                      "description": "Approx max tokens for the combined report. Default 2400." }
                }
            }),
            handler: Box::new(move |args| trace_profile::build(&root_trace_profile, args)),
        },
        mcp::Tool {
            name: "integration_hooks",
            description: "Search curated cross-subsystem call sites from .wordkeep/integration-hooks.md \
                          (cross-subsystem wiring). Faster than discovering cross-file chains \
                          by hand. Optional from/to runs call_path to verify or explore ad-hoc chains.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string",
                               "description": "Substring filter on hook text (tags, symbol names, subsystems)." },
                    "from": { "type": "string", "description": "Filter hooks mentioning this symbol; with \"to\", also run call_path." },
                    "to": { "type": "string", "description": "Filter hooks mentioning this symbol; with \"from\", also run call_path." },
                    "paths": { "type": "array", "items": { "type": "string" },
                               "description": "Subdirs for call_path verification. Default: .wordkeep/config.json default_paths, else src only." },
                    "max_depth": { "type": "integer", "description": "call_path max hops when from+to set. Default 8." },
                    "token_budget": { "type": "integer",
                                      "description": "Approx max tokens to return. Default 1200." }
                }
            }),
            handler: Box::new(move |args| integration_hooks::find(&root_hooks, args)),
        },
        mcp::Tool {
            name: "index_stale",
            description: "Detect when wordkeep on-disk indexes (call_graph, knowledge chunks) may lag \
                          git changes. Recommends rebuilding wordkeep and reloading the MCP client \
                          when stale. Call after large diffs if symbol_refs shows 0 callers.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "ref": { "type": "string",
                             "description": "Git revision to compare against. Default \"HEAD\"." }
                }
            }),
            handler: Box::new(move |args| index_stale::check(&root_stale, args)),
        },
        mcp::Tool {
            name: "stats",
            description: "Report estimated context avoided versus reading the raw material \
                          (source files, docs, trace CSVs) directly. Shows per-tool call counts, \
                          avg latency, truncation/error counts, distilled vs returned tokens, \
                          reduction % (one decimal near 100%), and improvement signals (slow tools, \
                          high truncation, never-called). Pass \"format\": \"json\" for machine-readable \
                          output with elapsed_us percentiles. Optional \"workspace\" filters event \
                          percentiles. Pass \"reset\": true to zero counters. Aggregates persist in \
                          savings.json; per-call events append to workspace events.jsonl.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "reset": { "type": "boolean",
                               "description": "Zero all counters and restart tracking. Default false." },
                    "insights": { "type": "boolean",
                                  "description": "Append improvement-signals section (text format). Default true." },
                    "format": { "type": "string",
                                "description": "\"text\" (default) or \"json\"." },
                    "workspace": { "type": "string",
                                   "description": "Workspace id filter for JSON percentiles; defaults to current workspace." }
                }
            }),
            handler: Box::new(stats::report),
        },
        mcp::Tool {
            name: "mas_post",
            description: "Append a compact entry to a Recursive-MAS session blackboard for \
                          subagent handoff (Planner/Critic/Solver). Pass summary plus optional \
                          claims, decisions, open_questions, anchors (file:line), handoff_to, \
                          tags, and approved. Server stamps id/round/ts and enforces a per-entry \
                          token cap (default ~400, env WORDKEEP_MAS_ENTRY_TOKENS).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "session": { "type": "string",
                                 "description": "Session slug [A-Za-z0-9._-]. Required." },
                    "role": { "type": "string",
                              "description": "Agent role (planner, critic, solver, …). Required." },
                    "summary": { "type": "string",
                                 "description": "Compact state summary for the next agent. Required." },
                    "claims": { "type": "array", "items": { "type": "string" },
                                "description": "Optional factual claims." },
                    "decisions": { "type": "array", "items": { "type": "string" },
                                   "description": "Optional decisions made." },
                    "open_questions": { "type": "array", "items": { "type": "string" },
                                        "description": "Optional unresolved questions." },
                    "anchors": { "type": "array", "items": { "type": "string" },
                                 "description": "Optional file:line refs." },
                    "commands": { "type": "array", "items": { "type": "string" },
                                  "description": "Optional shell/commands to preserve across sessions." },
                    "constraints": { "type": "array", "items": { "type": "string" },
                                     "description": "Optional hard constraints for the next agent." },
                    "kind": { "type": "string",
                              "description": "note (default ~400 tok) or handoff (~1600 tok; overflow spills to notes)." },
                    "note_ref": { "type": "string",
                                  "description": "Optional link to a spilled/full note path." },
                    "handoff_to": { "type": "string",
                                    "description": "Role that should read this entry next." },
                    "tags": { "type": "array", "items": { "type": "string" },
                              "description": "Optional filter tags." },
                    "approved": { "type": "boolean",
                                  "description": "Convergence signal (e.g. critic approval)." },
                    "max_rounds": { "type": "integer",
                                    "description": "Set recursion budget when creating session. Default 3." }
                },
                "required": ["session", "role", "summary"]
            }),
            handler: Box::new(move |args| mas::post(&root_mas_post, args)),
        },
        mcp::Tool {
            name: "mas_read",
            description: "Read a Recursive-MAS session blackboard, token-budgeted, newest-first. \
                          Filter by recipient (matches handoff_to), role, round, tag, or since_id. \
                          Use instead of re-pasting prior agent output between subagent spawns.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "session": { "type": "string", "description": "Session slug. Required." },
                    "recipient": { "type": "string",
                                   "description": "Filter entries whose handoff_to matches." },
                    "role": { "type": "string", "description": "Filter by posting role." },
                    "round": { "type": "integer", "description": "Filter by recursion round." },
                    "tag": { "type": "string", "description": "Filter by tag." },
                    "since_id": { "type": "integer",
                                  "description": "Only entries with id greater than this." },
                    "token_budget": { "type": "integer",
                                      "description": "Approx max tokens to return. Default 1200." }
                },
                "required": ["session"]
            }),
            handler: Box::new(move |args| mas::read(&root_mas_read, args)),
        },
        mcp::Tool {
            name: "mas_status",
            description: "Recursion bookkeeping for a Recursive-MAS session: round, per-role \
                          entry counts, rounds remaining, entry token cap, and a convergence hint. \
                          Pass advance_round: true to increment the round (closes the loop). \
                          Creates the session if missing.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "session": { "type": "string", "description": "Session slug. Required." },
                    "max_rounds": { "type": "integer",
                                    "description": "Recursion budget when creating session. Default 3." },
                    "advance_round": { "type": "boolean",
                                       "description": "Increment round after the current loop. Default false." }
                },
                "required": ["session"]
            }),
            handler: Box::new(move |args| mas::status(&root_mas_status, args)),
        },
        mcp::Tool {
            name: "mas_finalize",
            description: "Mark a Recursive-MAS session final, store the consolidated result, and \
                          optionally promote it to .wordkeep/notes/ via knowledge_upsert so \
                          knowledge_search can recall the decision later. Also emits a paste-ready \
                          next-session handoff prompt by default.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "session": { "type": "string", "description": "Session slug. Required." },
                    "result": { "type": "string",
                                "description": "Final consolidated answer. Required." },
                    "promote": { "type": "boolean",
                                 "description": "Write result to .wordkeep/notes/. Default from config mas.auto_promote, else false." },
                    "include_handoff_prompt": { "type": "boolean",
                                 "description": "Append paste-ready next-session prompt. Default true." },
                    "note_path": { "type": "string",
                                   "description": "Relative note path when promote is true. Default .wordkeep/notes/mas-<session>.md." },
                    "note_heading": { "type": "string",
                                      "description": "Section heading when promote is true." }
                },
                "required": ["session", "result"]
            }),
            handler: Box::new(move |args| mas::finalize(&root_mas_finalize, args)),
        },
        mcp::Tool {
            name: "session_handoff",
            description: "Format a deterministic, paste-ready next-session prime prompt from MAS \
                          state, unresolved defects, recent gate runs, anchors, commands, and \
                          constraints. Works for already-finalized sessions; may finalize+promote \
                          in one call when finalize:true.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "session": { "type": "string", "description": "MAS session slug. Required." },
                    "result": { "type": "string", "description": "Optional goal/result override." },
                    "finalize": { "type": "boolean", "description": "Also finalize the session. Default false." },
                    "promote": { "type": "boolean", "description": "When finalize, promote to notes." },
                    "note_path": { "type": "string" },
                    "note_heading": { "type": "string" }
                },
                "required": ["session"]
            }),
            handler: Box::new(move |args| mas::session_handoff(&root_session_handoff, args)),
        },
        mcp::Tool {
            name: "defect_upsert",
            description: "Create or update a structured defect in .wordkeep/defects.json \
                          (open|gated|eyeball_fail|done). Open eyeball defects are boosted in \
                          knowledge_search so they outrank stale gate-passed prose.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Stable id; auto-generated when omitted." },
                    "summary": { "type": "string", "description": "Short defect summary. Required." },
                    "status": { "type": "string", "description": "open|gated|eyeball_fail|done. Default open." },
                    "subsystem": { "type": "string" },
                    "acceptance": { "type": "array", "items": { "type": "string" } },
                    "evidence": { "type": "array", "items": { "type": "string" } },
                    "anchors": { "type": "array", "items": { "type": "string" } },
                    "tags": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["summary"]
            }),
            handler: Box::new(move |args| defects::upsert(&root_defect_upsert, args)),
        },
        mcp::Tool {
            name: "defect_list",
            description: "List defects (default: unresolved). Ordered eyeball_fail → open → gated → done.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "include_done": { "type": "boolean", "description": "Include done defects. Default false." },
                    "status": { "type": "string" },
                    "subsystem": { "type": "string" },
                    "tag": { "type": "string" },
                    "query": { "type": "string" }
                }
            }),
            handler: Box::new(move |args| defects::list(&root_defect_list, args)),
        },
        mcp::Tool {
            name: "run_record",
            description: "Record gate/run metadata only (never executes commands). Prefer the \
                          `wordkeep run-record` CLI from shell gates when MCP is unavailable.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "command": { "type": "string", "description": "Command string that was run. Required." },
                    "status": { "type": "string", "description": "running|passed|failed|cancelled." },
                    "exit_code": { "type": "integer" },
                    "duration_ms": { "type": "integer" },
                    "summary": { "type": "string" },
                    "log_path": { "type": "string" },
                    "key_failures": { "type": "array", "items": { "type": "string" } },
                    "artifacts": { "type": "array", "items": { "type": "string" } },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "defect_ids": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["command"]
            }),
            handler: Box::new(move |args| runs::record(&root_run_record, args)),
        },
        mcp::Tool {
            name: "run_history",
            description: "List recent run records; flags missing logs/artifacts.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "status": { "type": "string" },
                    "tag": { "type": "string" },
                    "limit": { "type": "integer", "description": "Max runs to show. Default 20." }
                }
            }),
            handler: Box::new(move |args| runs::history(&root_run_history, args)),
        },
        mcp::Tool {
            name: "artifact_index",
            description: "Index metadata under configured artifact_roots (path/mtime/size/type, \
                          PNG dimensions, shallow JSON keys). Indexes evidence; does NOT grade images. \
                          Never follows symlinks outside the workspace.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "roots": { "type": "array", "items": { "type": "string" },
                               "description": "Override artifact_roots from config." },
                    "query": { "type": "string" },
                    "kind": { "type": "string" },
                    "newest": { "type": "boolean" },
                    "refresh": { "type": "boolean", "description": "Rescan roots. Default false (use cache)." },
                    "limit": { "type": "integer" },
                    "token_budget": { "type": "integer" }
                }
            }),
            handler: Box::new(move |args| artifacts::index(&root_artifact_index, args)),
        },
        mcp::Tool {
            name: "session_pressure",
            description: "Heuristic context-pressure proxy (low|medium|high|critical) from tool \
                          events since last durable handoff, error/truncation rate, MAS open \
                          questions/round budget, unresolved defects, and failed runs. Host turn \
                          count / true context-window usage is unavailable.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "session": { "type": "string", "description": "Optional MAS session to include in scoring." }
                }
            }),
            handler: Box::new(move |args| session_pressure::report(&root_session_pressure, args)),
        },
        mcp::Tool {
            name: "commit_scope",
            description: "Read-only: group git status --porcelain paths by configured commit_scopes \
                          and warn about secrets, generated dirs, binaries, and large files. \
                          Never stages or commits.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "large_file_bytes": { "type": "integer",
                                          "description": "Override large-file warning threshold." }
                }
            }),
            handler: Box::new(move |args| commit_scope::propose(&root_commit_scope, args)),
        },
    ];

    stats::init_registry(raw_tools.iter().map(|t| t.name).collect());

    let tools: Vec<mcp::Tool> = raw_tools
        .into_iter()
        .map(|t| {
            let name = t.name;
            mcp::Tool {
                name: t.name,
                description: t.description,
                input_schema: t.input_schema,
                handler: instrument(name, t.handler),
            }
        })
        .collect();

    const README_RESOURCE: &str = include_str!("../README.md");
    let resources = vec![mcp::Resource {
        uri: "wordkeep://readme",
        name: "Wordkeep README",
        description: "Canonical Wordkeep README (also accepted as wordkeep://README).",
        mime_type: "text/markdown",
        text: README_RESOURCE,
    }];

    let server = mcp::Server {
        tools,
        resources,
        server_name: "wordkeep".to_string(),
        server_version: env!("CARGO_PKG_VERSION").to_string(),
    };

    if let Err(e) = server.run() {
        eprintln!("[wordkeep] fatal: {e}");
        std::process::exit(1);
    }
}
