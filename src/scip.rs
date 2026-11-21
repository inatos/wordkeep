//! Optional SCIP semantic precision overlay (`scip`, confidence 1.0).
//!
//! Wordkeep's default call graph runs on tree-sitter (`treesitter`, 0.5) and
//! heuristic tail-segment matching (`name-match`, 0.3). When a `.scip` index
//! produced by `rust-analyzer scip`, `scip-clang`, or a compatible indexer is
//! present in the workspace (or workspace cache), this module ingests its
//! precision call edges into the per-workspace SQLite [`Store`] with
//! `source = "scip"` and `confidence = 1.0`, cached by a SHA-256 fingerprint
//! over `compile_commands.json`, `Cargo.lock`, and the `.scip` artifact.
//!
//! When no `.scip` index is present (or an index is unreadable/corrupt), every
//! read path silently falls back to tree-sitter without failing any MCP tool.
//!
//! ## Supported `.scip` formats
//!
//! 1. **Official binary SCIP protobuf** (`scip.proto`): decoded with a lean
//!    zero-dependency wire reader covering `Index`, `Metadata`, `ToolInfo`,
//!    `Document`, `Occurrence` (`range`, `enclosing_range`, `symbol`,
//!    `symbol_roles`), and `SymbolInformation` (`symbol`, `kind`,
//!    `display_name`, `enclosing_symbol`).
//! 2. **Documented JSON SCIP fixture/stub** (auto-detected when the trimmed
//!    payload begins with `{`):
//!    ```json
//!    {
//!      "metadata": { "tool_info": { "name": "rust-analyzer", "version": "0.1" } },
//!      "documents": [
//!        {
//!          "relative_path": "src/engine.rs",
//!          "language": "rust",
//!          "symbols": [
//!            { "symbol": "rust-analyzer cargo app 0.1.0 engine/helper().", "kind": "function" }
//!          ],
//!          "occurrences": [
//!            { "range": [0, 3, 9], "enclosing_range": [0, 0, 2, 1], "symbol": "...", "symbol_roles": 1 },
//!            { "range": [5, 4, 10], "symbol": "...", "symbol_roles": 8 }
//!          ]
//!        }
//!      ],
//!      "edges": [
//!        { "file": "src/engine.rs", "src_name": "run", "dst_name": "helper", "line": 6, "kind": "call", "site_text": "helper();" }
//!      ]
//!    }
//!    ```
//!    Both `documents` occurrence-to-enclosing-definition extraction and
//!    optional explicit `edges` entries are supported.

#![allow(dead_code)]

use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::lang::Lang;
use crate::store::{
    self, EdgeInput, Store, CONFIDENCE_NAME_MATCH, CONFIDENCE_SCIP, CONFIDENCE_TREESITTER,
    EDGE_SOURCE_NAME_MATCH, EDGE_SOURCE_SCIP, EDGE_SOURCE_TREESITTER,
};
use crate::{cache, config, workspace};

// SQLite `meta` keys for SCIP cache/status tracking.
pub const META_SCIP_FINGERPRINT: &str = "scip_fingerprint";
pub const META_SCIP_MANIFEST_SHA: &str = "scip_manifest_sha";
pub const META_SCIP_INDEX_PATH: &str = "scip_index_path";
pub const META_SCIP_TOOL: &str = "scip_tool";
pub const META_SCIP_EDGE_COUNT: &str = "scip_edge_count";
pub const META_SCIP_DOC_COUNT: &str = "scip_doc_count";
pub const META_SCIP_INGESTED_AT: &str = "scip_ingested_at";
pub const META_SCIP_LAST_ERROR: &str = "scip_last_error";

// Official SCIP `SymbolRole` bitflags (`scip.proto`).
pub const ROLE_DEFINITION: i32 = 0x1;
pub const ROLE_IMPORT: i32 = 0x2;
pub const ROLE_WRITE_ACCESS: i32 = 0x4;
pub const ROLE_READ_ACCESS: i32 = 0x8;
pub const ROLE_GENERATED: i32 = 0x10;
pub const ROLE_TEST: i32 = 0x20;
pub const ROLE_FORWARD_DEFINITION: i32 = 0x40;

// Official SCIP `SymbolInformation.Kind` values for callables (`scip.proto`).
const KIND_CONSTRUCTOR: i32 = 9;
const KIND_FUNCTION: i32 = 17;
const KIND_MACRO: i32 = 25;
const KIND_METHOD: i32 = 26;
const KIND_ABSTRACT_METHOD: i32 = 66;
const KIND_METHOD_SPECIFICATION: i32 = 67;
const KIND_METHOD_ALIAS: i32 = 74;
const KIND_STATIC_METHOD: i32 = 80;

/// Max `.scip` size (bytes) hashed in full for fingerprint precision; larger
/// files use `(mtime_ns, size_bytes)` to stay instant on warm reads.
const FULL_HASH_MAX_BYTES: u64 = 32 * 1024 * 1024;

const COMPILE_DB_CANDIDATES: &[&str] = &[
    "compile_commands.json",
    "build/compile_commands.json",
    "build-vulkan/compile_commands.json",
    "build-debug/compile_commands.json",
    "build-release/compile_commands.json",
];

const CARGO_LOCK_CANDIDATES: &[&str] = &["Cargo.lock"];

/// External semantic indexers detected on `PATH`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DetectedTools {
    pub rust_analyzer: Option<PathBuf>,
    pub scip_clang: Option<PathBuf>,
    pub clangd: Option<PathBuf>,
}

/// SHA-256 fingerprint of detected build manifests (`compile_commands.json`, `Cargo.lock`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManifestFingerprint {
    pub compile_commands_path: Option<String>,
    pub compile_commands_sha: Option<String>,
    pub cargo_lock_path: Option<String>,
    pub cargo_lock_sha: Option<String>,
    pub combined_sha: String,
}

/// Outcome of syncing a `.scip` overlay into the workspace SQLite store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncOutcome {
    /// Fingerprint matched stored `meta.scip_fingerprint`; skipped re-parse.
    CacheHit {
        index_path: PathBuf,
        fingerprint: String,
        edge_count: usize,
    },
    /// Parsed `.scip` and replaced `"scip"` edges in SQLite.
    Ingested {
        index_path: PathBuf,
        fingerprint: String,
        edges: usize,
        documents: usize,
        tool: String,
    },
    /// No `.scip` file discovered — silently using tree-sitter.
    FallbackNoIndex,
    /// `.scip` discovery/parse/store failed — silently falling back to tree-sitter.
    FallbackError(String),
}

/// Full SCIP overlay status surfaced by `index_health`.
#[derive(Debug, Clone)]
pub struct ScipStatus {
    pub tools: DetectedTools,
    pub manifests: ManifestFingerprint,
    pub index_path: Option<PathBuf>,
    pub state: String,
    pub tool_name: Option<String>,
    pub fingerprint: Option<String>,
    pub edge_counts: BTreeMap<String, usize>,
    pub hint: Option<String>,
}

/// Decoded SCIP index (from either binary protobuf or JSON fixture).
#[derive(Debug, Clone, Default)]
pub struct ParsedIndex {
    pub tool_name: String,
    pub documents: Vec<ParsedDocument>,
    pub direct_edges: Vec<DirectEdge>,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedDocument {
    pub relative_path: String,
    pub language: String,
    pub occurrences: Vec<ParsedOccurrence>,
    pub symbols: Vec<ParsedSymbolInfo>,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedOccurrence {
    pub range: Vec<i32>,
    pub enclosing_range: Vec<i32>,
    pub symbol: String,
    pub symbol_roles: i32,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedSymbolInfo {
    pub symbol: String,
    pub kind: i32,
    pub display_name: String,
    pub enclosing_symbol: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectEdge {
    pub file: String,
    pub src_name: String,
    pub dst_name: String,
    pub line: usize,
    pub kind: String,
    pub site_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedEdge {
    pub rel_path: String,
    pub src_name: String,
    pub dst_name: String,
    pub line: usize,
    pub kind: String,
    pub site_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedSymName {
    qual_name: String,
    short_name: String,
    is_callable: bool,
}

/// Detect `rust-analyzer`, `scip-clang`, and `clangd` on the process `PATH`.
pub fn detect_tools() -> DetectedTools {
    detect_tools_in_path(std::env::var_os("PATH").as_deref())
}

/// Detect `rust-analyzer`, `scip-clang`, and `clangd` within a specific `PATH` value.
pub fn detect_tools_in_path(path_env: Option<&OsStr>) -> DetectedTools {
    DetectedTools {
        rust_analyzer: find_on_path("rust-analyzer", path_env),
        scip_clang: find_on_path("scip-clang", path_env),
        clangd: find_on_path("clangd", path_env),
    }
}

fn find_on_path(bin: &str, path_env: Option<&OsStr>) -> Option<PathBuf> {
    let paths = path_env?;
    for dir in std::env::split_paths(paths) {
        let cand = dir.join(bin);
        if is_executable_file(&cand) {
            return Some(cand);
        }
        #[cfg(windows)]
        {
            let exe = dir.join(format!("{bin}.exe"));
            if is_executable_file(&exe) {
                return Some(exe);
            }
        }
    }
    None
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Compute SHA-256 fingerprints of `compile_commands.json` and `Cargo.lock` if present.
pub fn manifest_fingerprint(root: &Path) -> ManifestFingerprint {
    let mut compile_commands_path = None;
    let mut compile_commands_sha = None;
    for rel in COMPILE_DB_CANDIDATES {
        let p = root.join(rel);
        if let Ok(bytes) = std::fs::read(&p) {
            compile_commands_path = Some((*rel).to_string());
            compile_commands_sha = Some(store::sha256_bytes(&bytes));
            break;
        }
    }

    let mut cargo_lock_path = None;
    let mut cargo_lock_sha = None;
    for rel in CARGO_LOCK_CANDIDATES {
        let p = root.join(rel);
        if let Ok(bytes) = std::fs::read(&p) {
            cargo_lock_path = Some((*rel).to_string());
            cargo_lock_sha = Some(store::sha256_bytes(&bytes));
            break;
        }
    }

    let cc_p = compile_commands_path.as_deref().unwrap_or("");
    let cc_s = compile_commands_sha.as_deref().unwrap_or("");
    let cl_p = cargo_lock_path.as_deref().unwrap_or("");
    let cl_s = cargo_lock_sha.as_deref().unwrap_or("");
    let combined_sha = store::sha256_hex(&format!("cc:{cc_p}:{cc_s}\ncl:{cl_p}:{cl_s}\n"));

    ManifestFingerprint {
        compile_commands_path,
        compile_commands_sha,
        cargo_lock_path,
        cargo_lock_sha,
        combined_sha,
    }
}

/// Discover a `.scip` index file in configured or standard workspace locations.
pub fn discover_scip_index(root: &Path) -> Option<PathBuf> {
    if let Ok(raw) = std::env::var("WORDKEEP_SCIP_INDEX") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            let p = PathBuf::from(trimmed);
            let full = if p.is_absolute() { p } else { root.join(p) };
            if full.is_file() {
                return Some(full);
            }
        }
    }

    if let Some(cfg) = config::load_config(root) {
        if let Some(rel) = cfg
            .get("scip")
            .and_then(|s| s.get("index_path"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let p = PathBuf::from(rel);
            let full = if p.is_absolute() { p } else { root.join(p) };
            if full.is_file() {
                return Some(full);
            }
        }
    }

    let candidates = [
        root.join("index.scip"),
        root.join(".wordkeep").join("index.scip"),
        root.join(".cache").join("wordkeep").join("index.scip"),
        workspace::workspace_file(root, "index.scip"),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

/// Compute the combined cache fingerprint for `scip_path` + workspace build manifests.
pub fn index_fingerprint(
    root: &Path,
    scip_path: &Path,
) -> Result<(String, ManifestFingerprint), String> {
    let manifests = manifest_fingerprint(root);
    let meta = std::fs::metadata(scip_path)
        .map_err(|e| format!("stat scip {}: {e}", scip_path.display()))?;
    let mtime_ns = cache::mtime_ns(&meta);
    let size = meta.len();
    let content_tag = if size <= FULL_HASH_MAX_BYTES {
        let bytes = std::fs::read(scip_path)
            .map_err(|e| format!("read scip {}: {e}", scip_path.display()))?;
        store::sha256_bytes(&bytes)
    } else {
        format!("stat:{mtime_ns}:{size}")
    };
    let rel_or_abs = scip_path
        .strip_prefix(root)
        .unwrap_or(scip_path)
        .to_string_lossy()
        .replace('\\', "/");
    let combined = store::sha256_hex(&format!(
        "manifest:{}\nindex:{rel_or_abs}\nsize:{size}\ncontent:{content_tag}\n",
        manifests.combined_sha
    ));
    Ok((combined, manifests))
}

/// Lightweight cache tag for `index_health`'s 30s in-memory memo so any change
/// to `index.scip`, `compile_commands.json`, or `Cargo.lock` busts the cache.
pub fn quick_cache_tag(root: &Path) -> String {
    let mut parts = Vec::new();
    if let Some(p) = discover_scip_index(root) {
        let sig = std::fs::metadata(&p)
            .map(|m| format!("{}:{}", cache::mtime_ns(&m), m.len()))
            .unwrap_or_else(|_| "0:0".into());
        parts.push(format!("scip={}@{sig}", p.display()));
    } else {
        parts.push("scip=none".into());
    }
    for rel in COMPILE_DB_CANDIDATES {
        let p = root.join(rel);
        if let Ok(m) = std::fs::metadata(&p) {
            parts.push(format!("cc={rel}@{}:{}", cache::mtime_ns(&m), m.len()));
            break;
        }
    }
    for rel in CARGO_LOCK_CANDIDATES {
        let p = root.join(rel);
        if let Ok(m) = std::fs::metadata(&p) {
            parts.push(format!("cl={rel}@{}:{}", cache::mtime_ns(&m), m.len()));
            break;
        }
    }
    parts.join("|")
}

/// Sync a discovered `.scip` index into the workspace SQLite store if present
/// and its fingerprint changed. Never fails — silently falls back to
/// tree-sitter when absent or unreadable.
pub fn sync_if_present(root: &Path) -> SyncOutcome {
    let Some(index_path) = discover_scip_index(root) else {
        return SyncOutcome::FallbackNoIndex;
    };
    let store = match Store::open(root) {
        Ok(s) => s,
        Err(e) => return SyncOutcome::FallbackError(e),
    };
    match ingest_if_changed(root, &store, &index_path) {
        Ok(outcome) => outcome,
        Err(e) => {
            let _ = store.set_meta(META_SCIP_LAST_ERROR, &e);
            SyncOutcome::FallbackError(e)
        }
    }
}

/// Ingest `scip_path` into `store` if its fingerprint (manifest SHAs + index
/// signature) differs from the cached `meta.scip_fingerprint`.
pub fn ingest_if_changed(
    root: &Path,
    store: &Store,
    scip_path: &Path,
) -> Result<SyncOutcome, String> {
    let (fingerprint, manifests) = index_fingerprint(root, scip_path)?;
    if let Ok(Some(cached_fp)) = store.get_meta(META_SCIP_FINGERPRINT) {
        if cached_fp == fingerprint {
            let edge_count = store
                .get_meta(META_SCIP_EDGE_COUNT)
                .ok()
                .flatten()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(0);
            return Ok(SyncOutcome::CacheHit {
                index_path: scip_path.to_path_buf(),
                fingerprint,
                edge_count,
            });
        }
    }

    let bytes = std::fs::read(scip_path)
        .map_err(|e| format!("read scip {}: {e}", scip_path.display()))?;
    let parsed = parse_scip_bytes(&bytes)?;
    let edges = extract_edges(root, &parsed);
    let doc_count = parsed.documents.len();
    let tool = if parsed.tool_name.is_empty() {
        "scip".to_string()
    } else {
        parsed.tool_name.clone()
    };

    let rel_index = scip_path
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| scip_path.display().to_string());

    let inserted = store.with_immediate_tx(|s| {
        s.clear_edges_by_source(EDGE_SOURCE_SCIP)?;
        let mut file_id_cache: HashMap<String, i64> = HashMap::new();
        let mut count = 0usize;

        for e in &edges {
            let file_id = if let Some(&id) = file_id_cache.get(&e.rel_path) {
                id
            } else if let Some(existing) = s.get_file(&e.rel_path)? {
                file_id_cache.insert(e.rel_path.clone(), existing.id);
                existing.id
            } else {
                let abs = root.join(&e.rel_path);
                let (mt, sz) = std::fs::metadata(&abs)
                    .map(|m| (cache::mtime_ns(&m), m.len()))
                    .unwrap_or((0, 0));
                let lang = Lang::from_path(&abs).map(|l| l.label()).unwrap_or("scip");
                let id = s.upsert_file(&e.rel_path, mt, sz, lang)?;
                file_id_cache.insert(e.rel_path.clone(), id);
                id
            };

            s.upsert_edge(&EdgeInput {
                file_id,
                src_symbol_id: None,
                src_name: &e.src_name,
                dst_name: &e.dst_name,
                kind: &e.kind,
                line: e.line,
                site_text: &e.site_text,
                source: EDGE_SOURCE_SCIP,
                confidence: CONFIDENCE_SCIP,
            })?;
            count += 1;
        }

        s.set_meta(META_SCIP_FINGERPRINT, &fingerprint)?;
        s.set_meta(META_SCIP_MANIFEST_SHA, &manifests.combined_sha)?;
        s.set_meta(META_SCIP_INDEX_PATH, &rel_index)?;
        s.set_meta(META_SCIP_TOOL, &tool)?;
        s.set_meta(META_SCIP_EDGE_COUNT, &count.to_string())?;
        s.set_meta(META_SCIP_DOC_COUNT, &doc_count.to_string())?;
        s.set_meta(META_SCIP_INGESTED_AT, &workspace::now_secs().to_string())?;
        s.set_meta(META_SCIP_LAST_ERROR, "")?;
        Ok(count)
    })?;

    Ok(SyncOutcome::Ingested {
        index_path: scip_path.to_path_buf(),
        fingerprint,
        edges: inserted,
        documents: doc_count,
        tool,
    })
}

/// Force-refresh the SCIP semantic overlay into the workspace SQLite store.
///
/// When `generate` / `generate_scip` is `true`, runs `scip-clang` (when
/// `compile_commands.json` exists) or `rust-analyzer scip` (when `Cargo.lock`
/// exists) to produce `<workspace_cache>/index.scip` before re-ingesting.
/// Honors cooperative MCP 2025-11 async task cancellation (`tasks/cancel`).
pub fn refresh(root: &Path, args: &Value) -> Result<String, String> {
    if crate::mcp::is_current_task_cancelled() {
        return Err("scip refresh cancelled".into());
    }
    let generate = args
        .get("generate_scip")
        .or_else(|| args.get("generate"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut generated_via: Option<String> = None;
    if generate {
        let tools = detect_tools();
        let manifests = manifest_fingerprint(root);
        let out_index = workspace::ensure_workspace_dir(root)?.join("index.scip");
        let mut cmd_opt: Option<(std::process::Command, String)> = None;
        if let (Some(bin), Some(cc)) = (&tools.scip_clang, &manifests.compile_commands_path) {
            let mut cmd = std::process::Command::new(bin);
            cmd.arg(format!("--compdb-path={cc}"))
                .arg(format!("--index-output-path={}", out_index.display()))
                .current_dir(root)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            cmd_opt = Some((cmd, "scip-clang".into()));
        } else if let (Some(bin), Some(_)) = (&tools.rust_analyzer, &manifests.cargo_lock_path) {
            let mut cmd = std::process::Command::new(bin);
            cmd.args(["scip", ".", "--output"])
                .arg(&out_index)
                .current_dir(root)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            cmd_opt = Some((cmd, "rust-analyzer".into()));
        }

        if let Some((mut cmd, label)) = cmd_opt {
            let mut child = cmd
                .spawn()
                .map_err(|e| format!("spawn {label} scip: {e}"))?;
            loop {
                if crate::mcp::is_current_task_cancelled() {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("scip refresh cancelled".into());
                }
                match child.try_wait().map_err(|e| format!("wait {label}: {e}"))? {
                    Some(st) if st.success() => {
                        generated_via = Some(label);
                        break;
                    }
                    Some(st) => {
                        return Err(format!(
                            "{label} scip exited with code {:?}",
                            st.code()
                        ));
                    }
                    None => std::thread::sleep(std::time::Duration::from_millis(25)),
                }
            }
        }
    }

    if crate::mcp::is_current_task_cancelled() {
        return Err("scip refresh cancelled".into());
    }

    if let Ok(store) = Store::open(root) {
        let _ = store.set_meta(META_SCIP_FINGERPRINT, "");
    }
    let st = status(root);
    let mut text = String::from("scip_refresh - SCIP semantic overlay refresh\n");
    if let Some(ref gen_tool) = generated_via {
        text.push_str(&format!("generated: {gen_tool}\n"));
    }
    text.push_str(&format_health_section_from(root, &st));

    crate::mcp::set_structured_content(serde_json::json!({
        "refreshed": true,
        "generated_via": generated_via,
        "state": st.state,
        "index_path": st.index_path.as_ref().map(|p| p.display().to_string()),
        "tool": st.tool_name,
        "fingerprint": st.fingerprint,
        "edges": st.edge_counts,
    }));
    Ok(text)
}

/// Inspect SCIP overlay environment, ingest/cache status, and edge provenance counts.
pub fn status(root: &Path) -> ScipStatus {
    let tools = detect_tools();
    let manifests = manifest_fingerprint(root);
    let outcome = sync_if_present(root);
    let store = Store::open(root).ok();
    let edge_counts = store
        .as_ref()
        .and_then(|s| s.count_edges_by_source().ok())
        .unwrap_or_default();
    let stored_tool = store
        .as_ref()
        .and_then(|s| s.get_meta(META_SCIP_TOOL).ok().flatten())
        .filter(|s| !s.is_empty());

    let (index_path, state, tool_name, fingerprint) = match outcome {
        SyncOutcome::CacheHit {
            index_path,
            fingerprint,
            ..
        } => (
            Some(index_path),
            "active (cached)".to_string(),
            stored_tool,
            Some(fingerprint),
        ),
        SyncOutcome::Ingested {
            index_path,
            fingerprint,
            tool,
            ..
        } => (
            Some(index_path),
            "active (ingested)".to_string(),
            Some(tool),
            Some(fingerprint),
        ),
        SyncOutcome::FallbackNoIndex => (
            None,
            "fallback (treesitter — no .scip index)".to_string(),
            None,
            None,
        ),
        SyncOutcome::FallbackError(err) => (
            discover_scip_index(root),
            format!("fallback (treesitter — {err})"),
            stored_tool,
            None,
        ),
    };

    let hint = if index_path.is_none() {
        build_generation_hint(&tools, &manifests)
    } else {
        None
    };

    ScipStatus {
        tools,
        manifests,
        index_path,
        state,
        tool_name,
        fingerprint,
        edge_counts,
        hint,
    }
}

fn build_generation_hint(
    tools: &DetectedTools,
    manifests: &ManifestFingerprint,
) -> Option<String> {
    if tools.scip_clang.is_some() {
        if let Some(cc) = manifests.compile_commands_path.as_deref() {
            return Some(format!(
                "scip-clang --compdb-path={cc} --index-output-path=index.scip"
            ));
        }
    }
    if tools.rust_analyzer.is_some() && manifests.cargo_lock_path.is_some() {
        return Some("rust-analyzer scip . --output index.scip".to_string());
    }
    if tools.clangd.is_some() && manifests.compile_commands_path.is_some() {
        return Some(
            "clangd detected with compile_commands.json; install scip-clang to emit index.scip"
                .to_string(),
        );
    }
    None
}

/// Format the `## SCIP / Semantic overlay` section for `index_health`.
pub fn format_health_section(root: &Path) -> String {
    let st = status(root);
    format_health_section_from(root, &st)
}

/// Format the `## SCIP / Semantic overlay` section from a precomputed [`ScipStatus`].
pub fn format_health_section_from(root: &Path, st: &ScipStatus) -> String {
    let mut out = String::new();

    let mark = |p: &Option<PathBuf>| if p.is_some() { "ok" } else { "missing" };
    out.push_str(&format!(
        "tools: rust-analyzer [{}], scip-clang [{}], clangd [{}]\n",
        mark(&st.tools.rust_analyzer),
        mark(&st.tools.scip_clang),
        mark(&st.tools.clangd),
    ));

    let cc = match (
        st.manifests.compile_commands_path.as_deref(),
        st.manifests.compile_commands_sha.as_deref(),
    ) {
        (Some(p), Some(sha)) => format!("{p} [sha={}]", short_sha(sha)),
        _ => "compile_commands.json [missing]".to_string(),
    };
    let cl = match (
        st.manifests.cargo_lock_path.as_deref(),
        st.manifests.cargo_lock_sha.as_deref(),
    ) {
        (Some(p), Some(sha)) => format!("{p} [sha={}]", short_sha(sha)),
        _ => "Cargo.lock [missing]".to_string(),
    };
    out.push_str(&format!("manifests: {cc}, {cl}\n"));

    if let Some(ref idx) = st.index_path {
        let rel = idx
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| idx.display().to_string());
        let mut details = Vec::new();
        if let Some(ref t) = st.tool_name {
            details.push(format!("tool={t}"));
        }
        if let Some(ref fp) = st.fingerprint {
            details.push(format!("fp={}", short_sha(fp)));
        }
        if details.is_empty() {
            out.push_str(&format!("overlay: {} ({rel})\n", st.state));
        } else {
            out.push_str(&format!(
                "overlay: {} ({rel}; {})\n",
                st.state,
                details.join(", ")
            ));
        }
    } else {
        out.push_str(&format!("overlay: {}\n", st.state));
    }

    let scip_n = st.edge_counts.get(EDGE_SOURCE_SCIP).copied().unwrap_or(0);
    let ts_n = st
        .edge_counts
        .get(EDGE_SOURCE_TREESITTER)
        .copied()
        .unwrap_or(0);
    let nm_n = st
        .edge_counts
        .get(EDGE_SOURCE_NAME_MATCH)
        .copied()
        .unwrap_or(0);
    out.push_str(&format!(
        "edges: {EDGE_SOURCE_SCIP}={scip_n} ({CONFIDENCE_SCIP:.1}), {EDGE_SOURCE_TREESITTER}={ts_n} ({CONFIDENCE_TREESITTER:.1}), {EDGE_SOURCE_NAME_MATCH}={nm_n} ({CONFIDENCE_NAME_MATCH:.1})\n"
    ));

    if let Some(ref hint) = st.hint {
        out.push_str(&format!("hint: {hint}\n"));
    }

    out
}

fn short_sha(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

// ---------------------------------------------------------------------------
// Parsing (.scip binary protobuf OR documented JSON fixture)
// ---------------------------------------------------------------------------

/// Parse `.scip` bytes: automatically dispatches JSON (`{...}`) vs binary SCIP protobuf.
pub fn parse_scip_bytes(bytes: &[u8]) -> Result<ParsedIndex, String> {
    if bytes.is_empty() {
        return Err("empty .scip file".into());
    }
    let first_non_ws = bytes.iter().copied().find(|b| !b.is_ascii_whitespace());
    if first_non_ws == Some(b'{') {
        parse_scip_json(bytes)
    } else {
        parse_scip_protobuf(bytes)
    }
}

fn parse_scip_json(bytes: &[u8]) -> Result<ParsedIndex, String> {
    let v: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("invalid JSON .scip: {e}"))?;
    let obj = v
        .as_object()
        .ok_or_else(|| "JSON .scip root must be an object".to_string())?;

    let tool_name = obj
        .get("metadata")
        .and_then(|m| m.get("tool_info"))
        .and_then(|t| t.get("name"))
        .and_then(Value::as_str)
        .or_else(|| obj.get("tool").and_then(Value::as_str))
        .unwrap_or("scip-json")
        .to_string();

    let mut documents = Vec::new();
    if let Some(docs) = obj.get("documents").and_then(Value::as_array) {
        for d in docs {
            let rel = d
                .get("relative_path")
                .or_else(|| d.get("path"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if rel.is_empty() {
                continue;
            }
            let language = d
                .get("language")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let mut occurrences = Vec::new();
            if let Some(occs) = d.get("occurrences").and_then(Value::as_array) {
                for o in occs {
                    let symbol = o
                        .get("symbol")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    if symbol.is_empty() {
                        continue;
                    }
                    let range = parse_i32_array(o.get("range"));
                    let enclosing_range = parse_i32_array(o.get("enclosing_range"));
                    let symbol_roles = o
                        .get("symbol_roles")
                        .and_then(Value::as_i64)
                        .unwrap_or(0) as i32;
                    occurrences.push(ParsedOccurrence {
                        range,
                        enclosing_range,
                        symbol,
                        symbol_roles,
                    });
                }
            }
            let mut symbols = Vec::new();
            if let Some(syms) = d.get("symbols").and_then(Value::as_array) {
                for s in syms {
                    let symbol = s
                        .get("symbol")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    if symbol.is_empty() {
                        continue;
                    }
                    let kind = parse_json_kind(s.get("kind"));
                    let display_name = s
                        .get("display_name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let enclosing_symbol = s
                        .get("enclosing_symbol")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    symbols.push(ParsedSymbolInfo {
                        symbol,
                        kind,
                        display_name,
                        enclosing_symbol,
                    });
                }
            }
            documents.push(ParsedDocument {
                relative_path: rel,
                language,
                occurrences,
                symbols,
            });
        }
    }

    let mut direct_edges = Vec::new();
    if let Some(edges) = obj.get("edges").and_then(Value::as_array) {
        for e in edges {
            let file = e
                .get("file")
                .or_else(|| e.get("rel_path"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let src_name = e
                .get("src_name")
                .or_else(|| e.get("caller"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let dst_name = e
                .get("dst_name")
                .or_else(|| e.get("callee"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if file.is_empty() || src_name.is_empty() || dst_name.is_empty() {
                continue;
            }
            let line = e.get("line").and_then(Value::as_u64).unwrap_or(1) as usize;
            let kind = e
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("call")
                .to_string();
            let site_text = e
                .get("site_text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            direct_edges.push(DirectEdge {
                file,
                src_name,
                dst_name,
                line,
                kind,
                site_text,
            });
        }
    }

    if documents.is_empty() && direct_edges.is_empty() {
        return Err("JSON .scip contains neither documents nor edges".into());
    }

    Ok(ParsedIndex {
        tool_name,
        documents,
        direct_edges,
    })
}

fn parse_i32_array(v: Option<&Value>) -> Vec<i32> {
    v.and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_i64().map(|n| n as i32))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_json_kind(v: Option<&Value>) -> i32 {
    match v {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0) as i32,
        Some(Value::String(s)) => match s.to_ascii_lowercase().as_str() {
            "function" | "fn" => KIND_FUNCTION,
            "method" => KIND_METHOD,
            "constructor" => KIND_CONSTRUCTOR,
            "macro" => KIND_MACRO,
            "static_method" | "staticmethod" => KIND_STATIC_METHOD,
            _ => 0,
        },
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Minimal pure-Rust protobuf wire decoder for official `scip.proto`
// ---------------------------------------------------------------------------

struct ProtoReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> ProtoReader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn eof(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn read_varint(&mut self) -> Result<u64, String> {
        let mut out: u64 = 0;
        let mut shift = 0u32;
        for _ in 0..10 {
            if self.pos >= self.buf.len() {
                return Err("protobuf truncated varint".into());
            }
            let b = self.buf[self.pos];
            self.pos += 1;
            out |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(out);
            }
            shift += 7;
        }
        Err("protobuf varint overflow".into())
    }

    fn read_tag(&mut self) -> Result<(u32, u8), String> {
        let key = self.read_varint()?;
        let field = (key >> 3) as u32;
        let wire = (key & 0x07) as u8;
        if field == 0 {
            return Err("protobuf invalid field number 0".into());
        }
        Ok((field, wire))
    }

    fn read_bytes(&mut self) -> Result<&'a [u8], String> {
        let len = self.read_varint()? as usize;
        if self.pos.saturating_add(len) > self.buf.len() {
            return Err("protobuf length-delimited field exceeds buffer".into());
        }
        let slice = &self.buf[self.pos..self.pos + len];
        self.pos += len;
        Ok(slice)
    }

    fn read_string(&mut self) -> Result<String, String> {
        let bytes = self.read_bytes()?;
        std::str::from_utf8(bytes)
            .map(|s| s.to_string())
            .map_err(|e| format!("protobuf invalid utf-8: {e}"))
    }

    fn read_packed_i32(&mut self, out: &mut Vec<i32>) -> Result<(), String> {
        let bytes = self.read_bytes()?;
        let mut sub = ProtoReader::new(bytes);
        while !sub.eof() {
            out.push(sub.read_varint()? as i32);
        }
        Ok(())
    }

    fn skip_field(&mut self, wire: u8) -> Result<(), String> {
        match wire {
            0 => {
                let _ = self.read_varint()?;
                Ok(())
            }
            1 => {
                if self.pos.saturating_add(8) > self.buf.len() {
                    return Err("protobuf truncated fixed64".into());
                }
                self.pos += 8;
                Ok(())
            }
            2 => {
                let _ = self.read_bytes()?;
                Ok(())
            }
            5 => {
                if self.pos.saturating_add(4) > self.buf.len() {
                    return Err("protobuf truncated fixed32".into());
                }
                self.pos += 4;
                Ok(())
            }
            other => Err(format!("protobuf unsupported wire type {other}")),
        }
    }
}

fn parse_scip_protobuf(bytes: &[u8]) -> Result<ParsedIndex, String> {
    let mut r = ProtoReader::new(bytes);
    let mut index = ParsedIndex::default();
    let mut saw_known_field = false;

    while !r.eof() {
        let (field, wire) = r.read_tag()?;
        match (field, wire) {
            (1, 2) => {
                saw_known_field = true;
                let meta_bytes = r.read_bytes()?;
                if let Some(tool) = parse_metadata_proto(meta_bytes)? {
                    index.tool_name = tool;
                }
            }
            (2, 2) => {
                saw_known_field = true;
                let doc_bytes = r.read_bytes()?;
                let doc = parse_document_proto(doc_bytes)?;
                if !doc.relative_path.is_empty() {
                    index.documents.push(doc);
                }
            }
            _ => r.skip_field(wire)?,
        }
    }

    if !saw_known_field {
        return Err("protobuf payload has no SCIP Index metadata or documents".into());
    }
    Ok(index)
}

fn parse_metadata_proto(bytes: &[u8]) -> Result<Option<String>, String> {
    let mut r = ProtoReader::new(bytes);
    let mut tool_name = None;
    while !r.eof() {
        let (field, wire) = r.read_tag()?;
        match (field, wire) {
            (2, 2) => {
                let ti = r.read_bytes()?;
                tool_name = parse_tool_info_proto(ti)?;
            }
            _ => r.skip_field(wire)?,
        }
    }
    Ok(tool_name)
}

fn parse_tool_info_proto(bytes: &[u8]) -> Result<Option<String>, String> {
    let mut r = ProtoReader::new(bytes);
    let mut name = None;
    while !r.eof() {
        let (field, wire) = r.read_tag()?;
        match (field, wire) {
            (1, 2) => name = Some(r.read_string()?),
            _ => r.skip_field(wire)?,
        }
    }
    Ok(name)
}

fn parse_document_proto(bytes: &[u8]) -> Result<ParsedDocument, String> {
    let mut r = ProtoReader::new(bytes);
    let mut doc = ParsedDocument::default();
    while !r.eof() {
        let (field, wire) = r.read_tag()?;
        match (field, wire) {
            (1, 2) => doc.relative_path = r.read_string()?,
            (2, 2) => {
                let occ_bytes = r.read_bytes()?;
                let occ = parse_occurrence_proto(occ_bytes)?;
                if !occ.symbol.is_empty() {
                    doc.occurrences.push(occ);
                }
            }
            (3, 2) => {
                let sym_bytes = r.read_bytes()?;
                let sym = parse_symbol_info_proto(sym_bytes)?;
                if !sym.symbol.is_empty() {
                    doc.symbols.push(sym);
                }
            }
            (4, 2) => doc.language = r.read_string()?,
            _ => r.skip_field(wire)?,
        }
    }
    Ok(doc)
}

fn parse_occurrence_proto(bytes: &[u8]) -> Result<ParsedOccurrence, String> {
    let mut r = ProtoReader::new(bytes);
    let mut occ = ParsedOccurrence::default();
    while !r.eof() {
        let (field, wire) = r.read_tag()?;
        match (field, wire) {
            (1, 2) => r.read_packed_i32(&mut occ.range)?,
            (1, 0) => occ.range.push(r.read_varint()? as i32),
            (2, 2) => occ.symbol = r.read_string()?,
            (3, 0) => occ.symbol_roles = r.read_varint()? as i32,
            (7, 2) => r.read_packed_i32(&mut occ.enclosing_range)?,
            (7, 0) => occ.enclosing_range.push(r.read_varint()? as i32),
            _ => r.skip_field(wire)?,
        }
    }
    Ok(occ)
}

fn parse_symbol_info_proto(bytes: &[u8]) -> Result<ParsedSymbolInfo, String> {
    let mut r = ProtoReader::new(bytes);
    let mut info = ParsedSymbolInfo::default();
    while !r.eof() {
        let (field, wire) = r.read_tag()?;
        match (field, wire) {
            (1, 2) => info.symbol = r.read_string()?,
            (5, 0) => info.kind = r.read_varint()? as i32,
            (6, 2) => info.display_name = r.read_string()?,
            (8, 2) => info.enclosing_symbol = r.read_string()?,
            _ => r.skip_field(wire)?,
        }
    }
    Ok(info)
}

// ---------------------------------------------------------------------------
// SCIP symbol parsing & call-edge extraction
// ---------------------------------------------------------------------------

fn is_callable_kind(kind: i32) -> bool {
    matches!(
        kind,
        KIND_CONSTRUCTOR
            | KIND_FUNCTION
            | KIND_MACRO
            | KIND_METHOD
            | KIND_ABSTRACT_METHOD
            | KIND_METHOD_SPECIFICATION
            | KIND_METHOD_ALIAS
            | KIND_STATIC_METHOD
    )
}

/// Parse a SCIP symbol string (e.g. `rust-analyzer cargo app 0.1.0 mod/Foo#bar().`)
/// or plain fixture symbol (`Foo::bar`) into qualified/short names + callable flag.
fn parse_scip_symbol(raw: &str) -> Option<ParsedSymName> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.starts_with("local ") {
        return None;
    }

    let parts: Vec<&str> = trimmed.splitn(5, ' ').collect();
    if parts.len() < 5 {
        // Minimal fixture symbol (e.g. "demo::helper" or "helper").
        let clean = trimmed.trim_end_matches("()").trim();
        if clean.is_empty() {
            return None;
        }
        let short = clean.rsplit("::").next().unwrap_or(clean).to_string();
        return Some(ParsedSymName {
            qual_name: clean.to_string(),
            short_name: short,
            is_callable: true,
        });
    }

    let mut desc = parts[4].trim();
    // Strip leading C++/scip-clang placeholder prefixes like `$ ` or `. `.
    while let Some(rest) = desc
        .strip_prefix("$ ")
        .or_else(|| desc.strip_prefix(". "))
    {
        desc = rest.trim_start();
    }
    if desc.is_empty() {
        return None;
    }

    let mut segments: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut is_callable = false;
    let chars: Vec<char> = desc.chars().collect();
    let mut i = 0usize;

    while i < chars.len() {
        let ch = chars[i];
        if ch == '`' {
            // Backtick-escaped identifier: `foo bar` (`` escapes a literal backtick).
            i += 1;
            while i < chars.len() {
                if chars[i] == '`' {
                    if i + 1 < chars.len() && chars[i + 1] == '`' {
                        cur.push('`');
                        i += 2;
                    } else {
                        i += 1;
                        break;
                    }
                } else {
                    cur.push(chars[i]);
                    i += 1;
                }
            }
            continue;
        }

        match ch {
            '/' | '#' | ':' => {
                if !cur.is_empty() {
                    segments.push(std::mem::take(&mut cur));
                }
                is_callable = false;
                i += 1;
            }
            '(' => {
                // Method/function descriptor: name(disambiguator).
                if !cur.is_empty() {
                    segments.push(std::mem::take(&mut cur));
                }
                // Advance past matching `)`.
                let mut depth = 1usize;
                i += 1;
                while i < chars.len() && depth > 0 {
                    if chars[i] == '(' {
                        depth += 1;
                    } else if chars[i] == ')' {
                        depth -= 1;
                    }
                    i += 1;
                }
                if i < chars.len() && chars[i] == '.' {
                    i += 1;
                }
                is_callable = true;
            }
            '!' => {
                if !cur.is_empty() {
                    segments.push(std::mem::take(&mut cur));
                }
                is_callable = true;
                i += 1;
            }
            '.' => {
                if !cur.is_empty() {
                    segments.push(std::mem::take(&mut cur));
                }
                is_callable = false;
                i += 1;
            }
            '[' => {
                // Type parameter `[T]` — skip.
                while i < chars.len() && chars[i] != ']' {
                    i += 1;
                }
                if i < chars.len() {
                    i += 1;
                }
            }
            _ => {
                cur.push(ch);
                i += 1;
            }
        }
    }
    if !cur.is_empty() {
        segments.push(cur);
    }

    // Drop placeholder `$` / `.` segments from C++ global namespaces.
    segments.retain(|s| !s.is_empty() && s != "$" && s != ".");
    if segments.is_empty() {
        return None;
    }

    let short_name = segments.last().cloned().unwrap_or_default();
    let qual_name = segments.join("::");
    Some(ParsedSymName {
        qual_name,
        short_name,
        is_callable,
    })
}

struct EnclosingDef {
    name: String,
    start_line: usize,
    end_line: usize,
    has_explicit_end: bool,
    is_test: bool,
}

/// Extract call edges `(src_name -> dst_name)` from a decoded SCIP index.
pub fn extract_edges(root: &Path, index: &ParsedIndex) -> Vec<ExtractedEdge> {
    let mut callable_by_sym: HashMap<&str, bool> = HashMap::new();
    for doc in &index.documents {
        for sym in &doc.symbols {
            if sym.kind != 0 {
                callable_by_sym.insert(sym.symbol.as_str(), is_callable_kind(sym.kind));
            }
        }
    }

    let mut out: Vec<ExtractedEdge> = Vec::new();

    for doc in &index.documents {
        let rel = normalize_doc_rel(&doc.relative_path);
        if rel.is_empty() {
            continue;
        }
        let file_lines: Vec<String> = std::fs::read_to_string(root.join(&rel))
            .map(|s| s.lines().map(str::to_string).collect())
            .unwrap_or_default();

        // 1. Collect callable definitions in this document.
        let mut defs: Vec<EnclosingDef> = Vec::new();
        for occ in &doc.occurrences {
            if (occ.symbol_roles & ROLE_DEFINITION) == 0
                || (occ.symbol_roles & ROLE_FORWARD_DEFINITION) != 0
            {
                continue;
            }
            let Some(parsed) = parse_scip_symbol(&occ.symbol) else {
                continue;
            };
            let callable = callable_by_sym
                .get(occ.symbol.as_str())
                .copied()
                .unwrap_or(parsed.is_callable);
            if !callable {
                continue;
            }
            let Some(def_line) = occ_start_line(&occ.range) else {
                continue;
            };
            let (start_line, end_line, has_explicit_end) =
                if let Some((es, ee)) = range_line_span(&occ.enclosing_range) {
                    (es, ee, true)
                } else if let Some((rs, re)) = range_line_span(&occ.range) {
                    if re > rs {
                        (rs, re, true)
                    } else {
                        (def_line, usize::MAX, false)
                    }
                } else {
                    (def_line, usize::MAX, false)
                };

            let is_test = (occ.symbol_roles & ROLE_TEST) != 0;
            let name = if is_test && !parsed.qual_name.contains("test") {
                format!("tests::{}", parsed.qual_name)
            } else {
                parsed.qual_name
            };
            defs.push(EnclosingDef {
                name,
                start_line,
                end_line,
                has_explicit_end,
                is_test,
            });
        }

        defs.sort_by_key(|d| (d.start_line, d.end_line));
        // Infer end_line for definitions lacking enclosing_range using the next def's start.
        for i in 0..defs.len() {
            if !defs[i].has_explicit_end {
                let next_start = defs
                    .get(i + 1)
                    .map(|n| n.start_line.saturating_sub(1))
                    .unwrap_or(usize::MAX);
                defs[i].end_line = next_start.max(defs[i].start_line);
            }
        }

        // 2. Attribute callable reference occurrences to their innermost enclosing def.
        for occ in &doc.occurrences {
            if (occ.symbol_roles & (ROLE_DEFINITION | ROLE_IMPORT | ROLE_FORWARD_DEFINITION)) != 0 {
                continue;
            }
            let Some(parsed) = parse_scip_symbol(&occ.symbol) else {
                continue;
            };
            let callable = callable_by_sym
                .get(occ.symbol.as_str())
                .copied()
                .unwrap_or(parsed.is_callable);
            if !callable {
                continue;
            }
            let Some(ref_line) = occ_start_line(&occ.range) else {
                continue;
            };

            let enclosing = defs
                .iter()
                .filter(|d| ref_line >= d.start_line && ref_line <= d.end_line)
                .min_by_key(|d| (d.end_line.saturating_sub(d.start_line), d.start_line));
            let Some(caller_def) = enclosing else {
                continue;
            };

            if caller_def.name == parsed.qual_name
                || crate::call_graph::seg(&caller_def.name) == parsed.short_name
            {
                continue;
            }

            let site_text = file_lines
                .get(ref_line.saturating_sub(1))
                .map(|l| collapse_line(l))
                .unwrap_or_default();

            let _ = caller_def.is_test;
            out.push(ExtractedEdge {
                rel_path: rel.clone(),
                src_name: caller_def.name.clone(),
                dst_name: parsed.qual_name,
                line: ref_line,
                kind: "call".to_string(),
                site_text,
            });
        }
    }

    for de in &index.direct_edges {
        let rel = normalize_doc_rel(&de.file);
        if rel.is_empty() {
            continue;
        }
        let site_text = if !de.site_text.is_empty() {
            de.site_text.clone()
        } else {
            std::fs::read_to_string(root.join(&rel))
                .ok()
                .and_then(|s| s.lines().nth(de.line.saturating_sub(1)).map(collapse_line))
                .unwrap_or_default()
        };
        out.push(ExtractedEdge {
            rel_path: rel,
            src_name: de.src_name.clone(),
            dst_name: de.dst_name.clone(),
            line: de.line.max(1),
            kind: if de.kind.is_empty() {
                "call".to_string()
            } else {
                de.kind.clone()
            },
            site_text,
        });
    }

    out
}

fn normalize_doc_rel(raw: &str) -> String {
    let s = raw.trim().replace('\\', "/");
    s.strip_prefix("./").unwrap_or(&s).to_string()
}

fn occ_start_line(range: &[i32]) -> Option<usize> {
    let first = *range.first()?;
    if first < 0 {
        return None;
    }
    Some(first as usize + 1)
}

fn range_line_span(range: &[i32]) -> Option<(usize, usize)> {
    if range.len() == 4 && range[0] >= 0 && range[2] >= range[0] {
        Some((range[0] as usize + 1, range[2] as usize + 1))
    } else if range.len() == 3 && range[0] >= 0 {
        let line = range[0] as usize + 1;
        Some((line, line))
    } else {
        None
    }
}

fn collapse_line(raw: &str) -> String {
    let one = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= 100 {
        one
    } else {
        let t: String = one.chars().take(99).collect();
        format!("{t}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(tag: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "wk_scip_{tag}_{}_{}",
            std::process::id(),
            n
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    struct ScipTestEnv {
        _lock: std::sync::MutexGuard<'static, ()>,
        xdg: PathBuf,
        prev_xdg: Option<std::ffi::OsString>,
    }

    impl ScipTestEnv {
        fn new(tag: &str) -> Self {
            let lock = crate::cache::test_env_lock();
            let xdg = temp_dir(&format!("xdg_{tag}"));
            let prev_xdg = std::env::var_os("XDG_CACHE_HOME");
            std::env::set_var("XDG_CACHE_HOME", &xdg);
            Self {
                _lock: lock,
                xdg,
                prev_xdg,
            }
        }
    }

    impl Drop for ScipTestEnv {
        fn drop(&mut self) {
            if let Some(ref prev) = self.prev_xdg {
                std::env::set_var("XDG_CACHE_HOME", prev);
            }
            let _ = std::fs::remove_dir_all(&self.xdg);
        }
    }

    // Minimal protobuf wire encoder helpers used solely to synthesize real
    // binary `.scip` fixtures conforming to `scip.proto`.
    fn pb_varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let mut b = (v & 0x7f) as u8;
            v >>= 7;
            if v != 0 {
                b |= 0x80;
            }
            out.push(b);
            if v == 0 {
                break;
            }
        }
    }

    fn pb_tag(field: u32, wire: u8, out: &mut Vec<u8>) {
        pb_varint(((field as u64) << 3) | (wire as u64), out);
    }

    fn pb_bytes(field: u32, data: &[u8], out: &mut Vec<u8>) {
        pb_tag(field, 2, out);
        pb_varint(data.len() as u64, out);
        out.extend_from_slice(data);
    }

    fn pb_string(field: u32, s: &str, out: &mut Vec<u8>) {
        pb_bytes(field, s.as_bytes(), out);
    }

    fn pb_packed_i32(field: u32, vals: &[i32], out: &mut Vec<u8>) {
        let mut inner = Vec::new();
        for &v in vals {
            pb_varint(v as u64, &mut inner);
        }
        pb_bytes(field, &inner, out);
    }

    fn pb_occurrence(
        range: &[i32],
        enclosing: &[i32],
        symbol: &str,
        roles: i32,
    ) -> Vec<u8> {
        let mut b = Vec::new();
        pb_packed_i32(1, range, &mut b);
        pb_string(2, symbol, &mut b);
        if roles != 0 {
            pb_tag(3, 0, &mut b);
            pb_varint(roles as u64, &mut b);
        }
        if !enclosing.is_empty() {
            pb_packed_i32(7, enclosing, &mut b);
        }
        b
    }

    fn synthesize_binary_scip(tool: &str, rel_path: &str) -> Vec<u8> {
        let sym_helper = "rust-analyzer cargo demo 0.1.0 engine/Widget#compute().";
        let sym_driver = "rust-analyzer cargo demo 0.1.0 engine/run_tick().";

        // Metadata { tool_info: ToolInfo { name: tool } }
        let mut ti = Vec::new();
        pb_string(1, tool, &mut ti);
        let mut meta = Vec::new();
        pb_bytes(2, &ti, &mut meta);

        // Document with 2 definitions + 1 call occurrence inside run_tick.
        let mut doc = Vec::new();
        pb_string(1, rel_path, &mut doc);
        pb_string(4, "rust", &mut doc);

        let occ_helper_def = pb_occurrence(&[0, 4, 11], &[0, 0, 2, 1], sym_helper, ROLE_DEFINITION);
        let occ_driver_def = pb_occurrence(&[3, 3, 11], &[3, 0, 6, 1], sym_driver, ROLE_DEFINITION);
        let occ_helper_call = pb_occurrence(&[4, 4, 11], &[], sym_helper, ROLE_READ_ACCESS);

        pb_bytes(2, &occ_helper_def, &mut doc);
        pb_bytes(2, &occ_driver_def, &mut doc);
        pb_bytes(2, &occ_helper_call, &mut doc);

        // Index { metadata, documents: [doc] }
        let mut index = Vec::new();
        pb_bytes(1, &meta, &mut index);
        pb_bytes(2, &doc, &mut index);
        index
    }

    #[test]
    fn parses_scip_symbol_descriptors_for_rust_and_cpp() {
        let r = parse_scip_symbol("rust-analyzer cargo betwixt 0.1.0 engine/Widget#area().")
            .unwrap();
        assert_eq!(r.qual_name, "engine::Widget::area");
        assert_eq!(r.short_name, "area");
        assert!(r.is_callable);

        let c = parse_scip_symbol("scip-clang cxx . . $ sim/Solver#step(d41d8cd9).").unwrap();
        assert_eq!(c.qual_name, "sim::Solver::step");
        assert_eq!(c.short_name, "step");
        assert!(c.is_callable);

        let field = parse_scip_symbol("rust-analyzer cargo betwixt 0.1.0 engine/Widget#id.")
            .unwrap();
        assert_eq!(field.qual_name, "engine::Widget::id");
        assert!(!field.is_callable);

        assert!(parse_scip_symbol("local 7").is_none());
    }

    #[test]
    fn binary_protobuf_scip_fixture_ingests_and_caches_by_manifest_sha() {
        let _env = ScipTestEnv::new("proto_cache");
        let root = temp_dir("proto_cache");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/engine.rs"),
            "fn compute() -> i32 {\n  42\n}\nfn run_tick() -> i32 {\n  compute()\n}\n",
        )
        .unwrap();
        std::fs::write(root.join("Cargo.lock"), "# lock v1\n").unwrap();
        std::fs::write(root.join("compile_commands.json"), "[]\n").unwrap();

        let scip_path = root.join("index.scip");
        std::fs::write(
            &scip_path,
            synthesize_binary_scip("rust-analyzer", "src/engine.rs"),
        )
        .unwrap();

        // First sync ingests binary protobuf SCIP edges at confidence 1.0.
        let first = sync_if_present(&root);
        match first {
            SyncOutcome::Ingested {
                edges,
                documents,
                tool,
                ..
            } => {
                assert_eq!(edges, 1);
                assert_eq!(documents, 1);
                assert_eq!(tool, "rust-analyzer");
            }
            other => panic!("expected Ingested, got {other:?}"),
        }

        let st = Store::open(&root).unwrap();
        let scip_edges = st.all_edges_by_source(EDGE_SOURCE_SCIP).unwrap();
        assert_eq!(scip_edges.len(), 1);
        assert_eq!(scip_edges[0].src_name, "engine::run_tick");
        assert_eq!(scip_edges[0].dst_name, "engine::Widget::compute");
        assert_eq!(scip_edges[0].line, 5);
        assert_eq!(scip_edges[0].source, EDGE_SOURCE_SCIP);
        assert!((scip_edges[0].confidence - CONFIDENCE_SCIP).abs() < 1e-9);

        // Second sync with unchanged manifests + index is an instant CacheHit.
        let second = sync_if_present(&root);
        assert!(
            matches!(second, SyncOutcome::CacheHit { edge_count: 1, .. }),
            "expected CacheHit, got {second:?}"
        );

        // Mutating Cargo.lock changes the manifest SHA and busts the fingerprint cache!
        std::fs::write(root.join("Cargo.lock"), "# lock v2 (bumped)\n").unwrap();
        let third = sync_if_present(&root);
        assert!(
            matches!(third, SyncOutcome::Ingested { edges: 1, .. }),
            "expected re-ingest after Cargo.lock change, got {third:?}"
        );

        // Mutating compile_commands.json also busts the fingerprint cache!
        std::fs::write(
            root.join("compile_commands.json"),
            r#"[{"file":"src/engine.cpp"}]"#,
        )
        .unwrap();
        let fourth = sync_if_present(&root);
        assert!(
            matches!(fourth, SyncOutcome::Ingested { edges: 1, .. }),
            "expected re-ingest after compile_commands.json change, got {fourth:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn json_scip_fixture_and_silent_fallback_on_missing_or_corrupt() {
        let _env = ScipTestEnv::new("json_fallback");
        let root = temp_dir("json_fallback");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/app.cpp"),
            "void leaf() {}\nvoid caller() { leaf(); }\n",
        )
        .unwrap();

        // Missing index -> silent fallback.
        assert_eq!(sync_if_present(&root), SyncOutcome::FallbackNoIndex);
        let health_fallback = format_health_section(&root);
        assert!(health_fallback.contains("fallback"), "{health_fallback}");
        assert!(health_fallback.contains("treesitter"), "{health_fallback}");

        // Corrupt index -> silent fallback (never panics or errors out).
        let scip_path = root.join("index.scip");
        std::fs::write(&scip_path, b"\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff").unwrap();
        assert!(
            matches!(sync_if_present(&root), SyncOutcome::FallbackError(_)),
            "corrupt index should fall back cleanly"
        );

        // Documented JSON fixture with both occurrences and direct edges -> ingests cleanly.
        let json_fixture = r#"{
            "metadata": { "tool_info": { "name": "scip-clang", "version": "0.3" } },
            "documents": [
                {
                    "relative_path": "src/app.cpp",
                    "language": "cpp",
                    "symbols": [
                        { "symbol": "scip-clang cxx . . $ app/leaf().", "kind": "function" },
                        { "symbol": "scip-clang cxx . . $ app/caller().", "kind": "function" }
                    ],
                    "occurrences": [
                        { "range": [0, 5, 9], "enclosing_range": [0, 0, 0, 14], "symbol": "scip-clang cxx . . $ app/leaf().", "symbol_roles": 1 },
                        { "range": [1, 5, 11], "enclosing_range": [1, 0, 1, 25], "symbol": "scip-clang cxx . . $ app/caller().", "symbol_roles": 1 },
                        { "range": [1, 16, 20], "symbol": "scip-clang cxx . . $ app/leaf().", "symbol_roles": 8 }
                    ]
                }
            ],
            "edges": [
                { "file": "src/app.cpp", "src_name": "app::boot", "dst_name": "app::caller", "line": 2, "kind": "call" }
            ]
        }"#;
        std::fs::write(&scip_path, json_fixture).unwrap();

        let outcome = sync_if_present(&root);
        assert!(
            matches!(outcome, SyncOutcome::Ingested { edges: 2, .. }),
            "expected 2 JSON SCIP edges, got {outcome:?}"
        );

        let health_active = format_health_section(&root);
        assert!(health_active.contains("active"), "{health_active}");
        assert!(health_active.contains("scip=2 (1.0)"), "{health_active}");
        assert!(health_active.contains("scip-clang"), "{health_active}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn detect_tools_finds_fake_binaries_on_custom_path() {
        let dir = temp_dir("detect_tools");
        let ra = dir.join("rust-analyzer");
        let sc = dir.join("scip-clang");
        let cd = dir.join("clangd");
        for p in [&ra, &sc, &cd] {
            std::fs::write(p, "#!/bin/sh\nexit 0\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perm = std::fs::metadata(p).unwrap().permissions();
                perm.set_mode(0o755);
                std::fs::set_permissions(p, perm).unwrap();
            }
        }
        let found = detect_tools_in_path(Some(dir.as_os_str()));
        assert_eq!(found.rust_analyzer.as_deref(), Some(ra.as_path()));
        assert_eq!(found.scip_clang.as_deref(), Some(sc.as_path()));
        assert_eq!(found.clangd.as_deref(), Some(cd.as_path()));

        let empty = temp_dir("detect_tools_empty");
        let none = detect_tools_in_path(Some(empty.as_os_str()));
        assert!(none.rust_analyzer.is_none());
        assert!(none.scip_clang.is_none());
        assert!(none.clangd.is_none());

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&empty);
    }
}
