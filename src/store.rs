//! Per-workspace SQLite (WAL + FTS5) graph and document store.
//!
//! Replaces monolithic global JSON `DiskMap` caches (`call_graph.json`,
//! `repo_map-symbols.json`, `knowledge-chunks.json`, `symbol_resolve-defs.json`)
//! with a workspace-scoped SQLite database at:
//!
//!   `$XDG_CACHE_HOME/wordkeep/workspaces/<workspace-id>/index.sqlite3`
//!
//! Design highlights:
//! - WAL mode, `synchronous = NORMAL`, `foreign_keys = ON`, `recursive_triggers = ON`,
//!   `busy_timeout = 5000`, schema tracked via `PRAGMA user_version`.
//! - `files` is the root table; deleting or GC'ing a file cascades to `symbols`,
//!   `edges`, `doc_chunks`, `file_stages`, `observation_links`, and via FTS5
//!   triggers to `symbols_fts` and `doc_chunks_fts`.
//! - `edges` carry `source` (`scip`, `treesitter`, `name-match`) and `confidence`
//!   (`1.0`, `0.5`, `0.3`) indexed by `(dst_name, confidence DESC)` and `(src_symbol_id)`.
//! - `embeddings(key_sha256, model_tag, dim, vec_blob)` and `observation_links`
//!   are provisioned in schema v1 for subsequent v0.5 phases.
//! - Debounced `notify` watcher helper (`start_watcher`) for MCP mode.
//! - One-time cleanup notice for legacy `~/.cache/wordkeep/*.json` DiskMap blobs.

#![allow(dead_code)]

use notify::RecursiveMode;
use notify_debouncer_mini::{new_debouncer, DebounceEventResult, Debouncer};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::Duration;

use crate::{cache, walk, workspace};

/// Current SQLite schema version stored in `PRAGMA user_version`.
pub const SCHEMA_VERSION: u32 = 2;

/// Filename inside the per-workspace cache directory.
pub const STORE_FILENAME: &str = "index.sqlite3";

/// Edge provenance tags.
pub const EDGE_SOURCE_SCIP: &str = "scip";
pub const EDGE_SOURCE_TREESITTER: &str = "treesitter";
pub const EDGE_SOURCE_NAME_MATCH: &str = "name-match";

/// Edge confidence tiers.
pub const CONFIDENCE_SCIP: f64 = 1.0;
pub const CONFIDENCE_TREESITTER: f64 = 0.5;
pub const CONFIDENCE_NAME_MATCH: f64 = 0.3;

/// Extractor stage identifiers tracked in `file_stages`.
pub const STAGE_CALL_GRAPH: &str = "call_graph";
pub const STAGE_SYMBOL_DEF: &str = "symbol_def";
pub const STAGE_REPO_MAP: &str = "repo_map";
pub const STAGE_DOC_CHUNKS: &str = "doc_chunks_v2";
pub const STAGE_OBS_LINKS: &str = "obs_links_v1";

/// Legacy global JSON `DiskMap` files eligible for one-time cleanup notice.
/// Explicitly excludes `savings.json`, `semif-status.json`, etc.
pub const LEGACY_DISKMAP_FILES: &[&str] = &[
    "call_graph.json",
    "repo_map-symbols.json",
    "knowledge-chunks.json",
    "symbol_resolve-defs.json",
];

const META_LEGACY_NOTICE: &str = "legacy_json_notice_emitted";
const DOC_EXT: [&str; 3] = ["md", "mdc", "markdown"];
const MAX_WATCH_DIRS: usize = 4096;

static LEGACY_NOTICE_ONCE: Once = Once::new();

/// Stored metadata for an indexed workspace file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecord {
    pub id: i64,
    pub rel_path: String,
    pub mtime_ns: u64,
    pub size_bytes: u64,
    pub lang: String,
    pub indexed_at: u64,
}

/// Input payload for [`Store::upsert_symbol`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolInput<'a> {
    pub file_id: i64,
    pub name: &'a str,
    pub qual_name: &'a str,
    pub kind: &'a str,
    pub line: usize,
    pub end_line: usize,
    pub signature: &'a str,
    pub doc: &'a str,
    pub exported: bool,
    pub source: &'a str,
}

/// A symbol row returned from [`Store::search_symbols_fts`] or [`Store::symbols_for_file`].
#[derive(Debug, Clone, PartialEq)]
pub struct SymbolHit {
    pub id: i64,
    pub file_id: i64,
    pub rel_path: String,
    pub name: String,
    pub qual_name: String,
    pub kind: String,
    pub line: usize,
    pub end_line: usize,
    pub signature: String,
    pub doc: String,
    pub exported: bool,
    pub source: String,
    /// Positive relevance score (`-bm25(symbols_fts)`); `0.0` for non-FTS lookups.
    pub score: f64,
}

/// Input payload for [`Store::upsert_edge`].
#[derive(Debug, Clone, PartialEq)]
pub struct EdgeInput<'a> {
    pub file_id: i64,
    pub src_symbol_id: Option<i64>,
    pub src_name: &'a str,
    pub dst_name: &'a str,
    pub kind: &'a str,
    pub line: usize,
    pub site_text: &'a str,
    pub source: &'a str,
    pub confidence: f64,
}

/// A call/reference edge returned from the store.
#[derive(Debug, Clone, PartialEq)]
pub struct EdgeRecord {
    pub id: i64,
    pub file_id: i64,
    pub rel_path: String,
    pub src_symbol_id: Option<i64>,
    pub src_name: String,
    pub dst_name: String,
    pub kind: String,
    pub line: usize,
    pub site_text: String,
    pub source: String,
    pub confidence: f64,
}

/// Input payload for [`Store::upsert_doc_chunk`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocChunkInput<'a> {
    pub file_id: i64,
    pub chunk_idx: usize,
    pub heading: &'a str,
    pub body: &'a str,
    /// Optional precomputed SHA-256 hex digest; computed automatically when empty.
    pub sha256: &'a str,
}

/// A documentation chunk returned from [`Store::search_docs_fts`] or [`Store::doc_chunks_for_file`].
#[derive(Debug, Clone, PartialEq)]
pub struct DocChunkHit {
    pub id: i64,
    pub file_id: i64,
    pub rel_path: String,
    pub chunk_idx: usize,
    pub heading: String,
    pub body: String,
    pub sha256: String,
    /// Positive relevance score (`-bm25(doc_chunks_fts)`); `0.0` for non-FTS lookups.
    pub score: f64,
}

/// Summary returned by [`Store::gc_missing_files`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcReport {
    pub files_checked: usize,
    pub files_removed: usize,
}

/// Stored observation-to-code anchor row in `observation_links`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationLink {
    pub id: i64,
    pub obs_kind: String,
    pub obs_id: String,
    pub file_id: Option<i64>,
    pub symbol_name: String,
    pub target_rel: String,
    pub anchor_hash: String,
    pub created_at: u64,
}

/// Diverged observation anchor detected when on-disk code/file hash no longer
/// matches the `anchor_hash` recorded when the note/shrift/defect was indexed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleAnchor {
    pub obs_kind: String,
    pub obs_id: String,
    pub target_rel: String,
    pub symbol_name: String,
    pub recorded_hash: String,
    pub current_hash: Option<String>,
}

impl StaleAnchor {
    pub fn label(&self) -> String {
        if !self.symbol_name.is_empty() {
            self.symbol_name.clone()
        } else {
            self.target_rel.clone()
        }
    }
}

/// Handle keeping a debounced filesystem watcher alive. Dropping stops watching.
pub struct WatcherHandle {
    _debouncer: Debouncer<notify::RecommendedWatcher>,
    pub watched_dirs: usize,
}

/// Per-workspace SQLite WAL + FTS5 index store.
pub struct Store {
    conn: Connection,
    root: PathBuf,
    path: PathBuf,
}

/// Path to the workspace SQLite database:
/// `$XDG_CACHE_HOME/wordkeep/workspaces/<workspace-id>/index.sqlite3`.
pub fn db_path(root: &Path) -> PathBuf {
    workspace::workspace_file(root, STORE_FILENAME)
}

/// Normalize a path relative to `root` using forward slashes.
pub fn normalize_rel(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.to_string_lossy().replace('\\', "/")
}

/// Compute lowercase hex SHA-256 for arbitrary bytes.
pub fn sha256_bytes(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

/// Compute lowercase hex SHA-256 for `text`.
pub fn sha256_hex(text: &str) -> String {
    sha256_bytes(text.as_bytes())
}

fn rel_under_roots(rel: &str, roots: &[String]) -> bool {
    if roots.is_empty() {
        return true;
    }
    let norm = rel.replace('\\', "/");
    roots.iter().any(|r| {
        let rr = r.trim_matches('/').replace('\\', "/");
        if rr.is_empty() || rr == "." {
            return true;
        }
        norm == rr || norm.starts_with(&format!("{rr}/"))
    })
}

fn decode_embedding_blob(dim: i64, blob: &[u8]) -> Option<Vec<f32>> {
    if dim < 0 || blob.len() != (dim as usize) * 4 {
        return None;
    }
    let mut out = Vec::with_capacity(dim as usize);
    for chunk in blob.chunks_exact(4) {
        out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Some(out)
}

/// Compute a compact 16-hex-char (64-bit) SHA-256 ETag over length-prefixed parts.
pub fn etag_hex(parts: &[&[u8]]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update((p.len() as u64).to_le_bytes());
        h.update(p);
    }
    let digest = h.finalize();
    let mut out = String::with_capacity(16);
    for b in &digest[..8] {
        use std::fmt::Write;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

/// Normalize a caller-supplied ETag token (strips whitespace, optional `W/`,
/// surrounding quotes, and optional `etag=` prefix).
pub fn normalize_etag(raw: &str) -> &str {
    let mut s = raw.trim();
    if let Some(rest) = s.strip_prefix("W/").or_else(|| s.strip_prefix("w/")) {
        s = rest.trim();
    }
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s = &s[1..s.len() - 1];
    }
    let s = s.trim();
    s.strip_prefix("etag=").unwrap_or(s).trim()
}

/// Check whether an `if_none_match` argument matches `current` ETag.
/// Accepts bare hex, `etag=<hex>`, quoted `"<hex>"`, weak `W/"<hex>"`,
/// comma-separated lists, or `*`.
pub fn etag_matches(if_none_match: Option<&str>, current: &str) -> bool {
    let Some(raw) = if_none_match else {
        return false;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed == "*" {
        return true;
    }
    let cur = normalize_etag(current);
    trimmed
        .split(',')
        .map(normalize_etag)
        .any(|tok| !tok.is_empty() && (tok == "*" || tok.eq_ignore_ascii_case(cur)))
}

/// Canonical 1-line response when `if_none_match` matches.
pub fn unchanged_etag_line(etag: &str) -> String {
    format!("unchanged (etag={etag})\n")
}

impl Store {
    /// Open (or create + initialize) the per-workspace SQLite store for `root`.
    pub fn open(root: &Path) -> Result<Self, String> {
        let dir = workspace::ensure_workspace_dir(root)?;
        let path = dir.join(STORE_FILENAME);
        Self::open_at(root, &path)
    }

    /// Open (or create + initialize) a store at an explicit database path.
    pub fn open_at(root: &Path, path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .map_err(|e| format!("sqlite open {}: {e}", path.display()))?;
        Self::configure_and_init(conn, root.to_path_buf(), path.to_path_buf())
    }

    fn configure_and_init(conn: Connection, root: PathBuf, path: PathBuf) -> Result<Self, String> {
        // Set busy_timeout FIRST so concurrent opens wait cleanly before WAL/schema pragmas.
        conn.execute_batch(
            "PRAGMA busy_timeout = 5000;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA recursive_triggers = ON;
             PRAGMA temp_store = MEMORY;",
        )
        .map_err(|e| format!("sqlite pragmas {}: {e}", path.display()))?;

        let current_ver: u32 = conn
            .query_row("PRAGMA user_version;", [], |row| row.get(0))
            .map_err(|e| format!("sqlite read user_version: {e}"))?;

        if current_ver < SCHEMA_VERSION {
            init_schema(&conn)?;
        }

        Ok(Store { conn, root, path })
    }

    /// Filesystem path to the opened SQLite database.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Workspace root associated with this store.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Read `PRAGMA user_version`.
    pub fn schema_version(&self) -> Result<u32, String> {
        self.conn
            .query_row("PRAGMA user_version;", [], |row| row.get(0))
            .map_err(|e| format!("sqlite user_version: {e}"))
    }

    /// Read active `PRAGMA journal_mode` (lowercased, e.g. `"wal"`).
    pub fn journal_mode(&self) -> Result<String, String> {
        let mode: String = self
            .conn
            .query_row("PRAGMA journal_mode;", [], |row| row.get(0))
            .map_err(|e| format!("sqlite journal_mode: {e}"))?;
        Ok(mode.to_ascii_lowercase())
    }

    /// Read whether `PRAGMA foreign_keys` is enabled.
    pub fn foreign_keys_enabled(&self) -> Result<bool, String> {
        let on: i64 = self
            .conn
            .query_row("PRAGMA foreign_keys;", [], |row| row.get(0))
            .map_err(|e| format!("sqlite foreign_keys: {e}"))?;
        Ok(on != 0)
    }

    /// Execute `f` inside an `IMMEDIATE` transaction (deadlock-free in WAL mode).
    /// Rolls back automatically on error.
    pub fn with_immediate_tx<T, F>(&self, f: F) -> Result<T, String>
    where
        F: FnOnce(&Store) -> Result<T, String>,
    {
        self.conn
            .execute_batch("BEGIN IMMEDIATE;")
            .map_err(|e| format!("sqlite BEGIN IMMEDIATE: {e}"))?;
        match f(self) {
            Ok(val) => {
                self.conn
                    .execute_batch("COMMIT;")
                    .map_err(|e| format!("sqlite COMMIT: {e}"))?;
                Ok(val)
            }
            Err(err) => {
                let _ = self.conn.execute_batch("ROLLBACK;");
                Err(err)
            }
        }
    }

    /// Upsert a workspace file record and return its `files.id`.
    ///
    /// If the file already exists with a **different** `mtime_ns`, its stale
    /// `file_stages`, `symbols`, `edges`, and `doc_chunks` rows are cleared so
    /// subsequent stage writes rebuild cleanly without dangling FTS rows.
    /// If `mtime_ns` is unchanged, existing stage/symbol/edge/chunk rows are
    /// preserved so independent extractors (`call_graph`, `symbol_resolve`,
    /// `repo_map`) compose cleanly on the same file.
    pub fn upsert_file(
        &self,
        rel_path: &str,
        mtime_ns: u64,
        size_bytes: u64,
        lang: &str,
    ) -> Result<i64, String> {
        let now = workspace::now_secs() as i64;
        let mt = mtime_ns as i64;
        let sz = size_bytes as i64;

        let existing: Option<(i64, i64)> = self
            .conn
            .query_row(
                "SELECT id, mtime_ns FROM files WHERE rel_path = ?1",
                params![rel_path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| format!("sqlite select file {rel_path}: {e}"))?;

        if let Some((id, old_mt)) = existing {
            if old_mt != mt {
                self.conn
                    .execute(
                        "UPDATE files
                         SET mtime_ns = ?2, size_bytes = ?3,
                             lang = CASE WHEN ?4 != '' THEN ?4 ELSE lang END,
                             indexed_at = ?5
                         WHERE id = ?1",
                        params![id, mt, sz, lang, now],
                    )
                    .map_err(|e| format!("sqlite update file {rel_path}: {e}"))?;
                // File changed on disk: wipe stale derived rows (triggers keep FTS5 synced).
                self.conn
                    .execute("DELETE FROM file_stages WHERE file_id = ?1", params![id])
                    .map_err(|e| format!("sqlite clear file_stages {rel_path}: {e}"))?;
                self.conn
                    .execute("DELETE FROM edges WHERE file_id = ?1", params![id])
                    .map_err(|e| format!("sqlite clear edges {rel_path}: {e}"))?;
                self.conn
                    .execute("DELETE FROM symbols WHERE file_id = ?1", params![id])
                    .map_err(|e| format!("sqlite clear symbols {rel_path}: {e}"))?;
                self.conn
                    .execute("DELETE FROM doc_chunks WHERE file_id = ?1", params![id])
                    .map_err(|e| format!("sqlite clear doc_chunks {rel_path}: {e}"))?;
            } else if !lang.is_empty() || sz > 0 {
                self.conn
                    .execute(
                        "UPDATE files
                         SET size_bytes = CASE WHEN ?2 > 0 THEN ?2 ELSE size_bytes END,
                             lang = CASE WHEN ?3 != '' THEN ?3 ELSE lang END,
                             indexed_at = ?4
                         WHERE id = ?1",
                        params![id, sz, lang, now],
                    )
                    .map_err(|e| format!("sqlite touch file {rel_path}: {e}"))?;
            }
            return Ok(id);
        }

        self.conn
            .query_row(
                "INSERT INTO files (rel_path, mtime_ns, size_bytes, lang, indexed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 RETURNING id",
                params![rel_path, mt, sz, lang, now],
                |row| row.get(0),
            )
            .map_err(|e| format!("sqlite insert file {rel_path}: {e}"))
    }

    /// Fetch a file record by workspace-relative path.
    pub fn get_file(&self, rel_path: &str) -> Result<Option<FileRecord>, String> {
        self.conn
            .query_row(
                "SELECT id, rel_path, mtime_ns, size_bytes, lang, indexed_at
                 FROM files WHERE rel_path = ?1",
                params![rel_path],
                |row| {
                    Ok(FileRecord {
                        id: row.get(0)?,
                        rel_path: row.get(1)?,
                        mtime_ns: row.get::<_, i64>(2)? as u64,
                        size_bytes: row.get::<_, i64>(3)? as u64,
                        lang: row.get(4)?,
                        indexed_at: row.get::<_, i64>(5)? as u64,
                    })
                },
            )
            .optional()
            .map_err(|e| format!("sqlite get_file {rel_path}: {e}"))
    }

    /// Delete a file by workspace-relative path.
    /// Cascades to `symbols`, `edges`, `doc_chunks`, `file_stages`, `observation_links`,
    /// and (via `recursive_triggers = ON`) removes all corresponding FTS5 entries.
    pub fn delete_file(&self, rel_path: &str) -> Result<bool, String> {
        let changed = self
            .conn
            .execute("DELETE FROM files WHERE rel_path = ?1", params![rel_path])
            .map_err(|e| format!("sqlite delete_file {rel_path}: {e}"))?;
        Ok(changed > 0)
    }

    /// Drop stage watermarks for `rel_path` (forcing re-extraction on next query)
    /// without deleting the file row itself.
    pub fn invalidate_file_stages(&self, rel_path: &str) -> Result<bool, String> {
        let changed = self
            .conn
            .execute(
                "DELETE FROM file_stages
                 WHERE file_id IN (SELECT id FROM files WHERE rel_path = ?1)",
                params![rel_path],
            )
            .map_err(|e| format!("sqlite invalidate_file_stages {rel_path}: {e}"))?;
        Ok(changed > 0)
    }

    /// Return `(file_id, mtime_ns)` for a completed extractor stage on `rel_path`.
    pub fn file_stage_mtime(
        &self,
        rel_path: &str,
        stage: &str,
    ) -> Result<Option<(i64, u64)>, String> {
        self.conn
            .query_row(
                "SELECT fs.file_id, fs.mtime_ns
                 FROM file_stages fs
                 JOIN files f ON f.id = fs.file_id
                 WHERE f.rel_path = ?1 AND fs.stage = ?2",
                params![rel_path, stage],
                |row| Ok((row.get(0)?, row.get::<_, i64>(1)? as u64)),
            )
            .optional()
            .map_err(|e| format!("sqlite file_stage_mtime {rel_path}/{stage}: {e}"))
    }

    /// Mark an extractor stage complete for `file_id` at `mtime_ns`, with an
    /// optional compact payload string (e.g. `repo_map` display lines).
    pub fn set_file_stage(
        &self,
        file_id: i64,
        stage: &str,
        mtime_ns: u64,
        payload: Option<&str>,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO file_stages (file_id, stage, mtime_ns, payload)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(file_id, stage) DO UPDATE SET
                     mtime_ns = excluded.mtime_ns,
                     payload  = excluded.payload",
                params![file_id, stage, mtime_ns as i64, payload],
            )
            .map_err(|e| format!("sqlite set_file_stage {stage}: {e}"))?;
        Ok(())
    }

    /// Fetch the cached stage payload when `rel_path` has a matching `mtime_ns`.
    pub fn get_file_stage_payload(
        &self,
        rel_path: &str,
        stage: &str,
        mtime_ns: u64,
    ) -> Result<Option<String>, String> {
        let row: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT fs.payload
                 FROM file_stages fs
                 JOIN files f ON f.id = fs.file_id
                 WHERE f.rel_path = ?1 AND fs.stage = ?2 AND fs.mtime_ns = ?3",
                params![rel_path, stage, mtime_ns as i64],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| format!("sqlite get_file_stage_payload {rel_path}/{stage}: {e}"))?;
        Ok(row.flatten())
    }

    /// Upsert a symbol into `symbols` (automatically updating `symbols_fts`)
    /// and return its stable `symbols.id`.
    pub fn upsert_symbol(&self, sym: &SymbolInput<'_>) -> Result<i64, String> {
        let source = if sym.source.is_empty() {
            EDGE_SOURCE_TREESITTER
        } else {
            sym.source
        };
        let qual = if sym.qual_name.is_empty() {
            sym.name
        } else {
            sym.qual_name
        };
        let exported = i64::from(sym.exported);
        self.conn
            .query_row(
                "INSERT INTO symbols
                     (file_id, name, qual_name, kind, line, end_line, signature, doc, exported, source)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(file_id, name, qual_name, kind, line) DO UPDATE SET
                     end_line  = CASE WHEN excluded.end_line > 0 THEN excluded.end_line ELSE symbols.end_line END,
                     signature = CASE WHEN excluded.signature != '' THEN excluded.signature ELSE symbols.signature END,
                     doc       = CASE WHEN excluded.doc != '' THEN excluded.doc ELSE symbols.doc END,
                     exported  = CASE WHEN excluded.exported != 0 THEN excluded.exported ELSE symbols.exported END,
                     source    = excluded.source
                 RETURNING id",
                params![
                    sym.file_id,
                    sym.name,
                    qual,
                    sym.kind,
                    sym.line as i64,
                    sym.end_line as i64,
                    sym.signature,
                    sym.doc,
                    exported,
                    source,
                ],
                |row| row.get(0),
            )
            .map_err(|e| format!("sqlite upsert_symbol {}: {e}", sym.name))
    }

    /// Return all symbols stored for `file_id`, ordered by source line.
    pub fn symbols_for_file(&self, file_id: i64) -> Result<Vec<SymbolHit>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT s.id, s.file_id, f.rel_path, s.name, s.qual_name, s.kind,
                        s.line, s.end_line, s.signature, s.doc, s.exported, s.source
                 FROM symbols s
                 JOIN files f ON f.id = s.file_id
                 WHERE s.file_id = ?1
                 ORDER BY s.line ASC, s.id ASC",
            )
            .map_err(|e| format!("sqlite prepare symbols_for_file: {e}"))?;
        let rows = stmt
            .query_map(params![file_id], |row| {
                Ok(SymbolHit {
                    id: row.get(0)?,
                    file_id: row.get(1)?,
                    rel_path: row.get(2)?,
                    name: row.get(3)?,
                    qual_name: row.get(4)?,
                    kind: row.get(5)?,
                    line: row.get::<_, i64>(6)? as usize,
                    end_line: row.get::<_, i64>(7)? as usize,
                    signature: row.get(8)?,
                    doc: row.get(9)?,
                    exported: row.get::<_, i64>(10)? != 0,
                    source: row.get(11)?,
                    score: 0.0,
                })
            })
            .map_err(|e| format!("sqlite query symbols_for_file: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("sqlite collect symbols_for_file: {e}"))
    }

    /// Return symbols of a specific `kind` (e.g. `"function"`) stored for `file_id`.
    pub fn symbols_for_file_kind(
        &self,
        file_id: i64,
        kind: &str,
    ) -> Result<Vec<SymbolHit>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT s.id, s.file_id, f.rel_path, s.name, s.qual_name, s.kind,
                        s.line, s.end_line, s.signature, s.doc, s.exported, s.source
                 FROM symbols s
                 JOIN files f ON f.id = s.file_id
                 WHERE s.file_id = ?1 AND s.kind = ?2
                 ORDER BY s.line ASC, s.id ASC",
            )
            .map_err(|e| format!("sqlite prepare symbols_for_file_kind: {e}"))?;
        let rows = stmt
            .query_map(params![file_id, kind], |row| {
                Ok(SymbolHit {
                    id: row.get(0)?,
                    file_id: row.get(1)?,
                    rel_path: row.get(2)?,
                    name: row.get(3)?,
                    qual_name: row.get(4)?,
                    kind: row.get(5)?,
                    line: row.get::<_, i64>(6)? as usize,
                    end_line: row.get::<_, i64>(7)? as usize,
                    signature: row.get(8)?,
                    doc: row.get(9)?,
                    exported: row.get::<_, i64>(10)? != 0,
                    source: row.get(11)?,
                    score: 0.0,
                })
            })
            .map_err(|e| format!("sqlite query symbols_for_file_kind: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("sqlite collect symbols_for_file_kind: {e}"))
    }

    /// Fast projection of symbol names for `file_id` (used by `symbol_resolve`).
    pub fn symbol_names_for_file(&self, file_id: i64) -> Result<Vec<String>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT name FROM symbols WHERE file_id = ?1 ORDER BY line ASC, id ASC")
            .map_err(|e| format!("sqlite prepare symbol_names_for_file: {e}"))?;
        let rows = stmt
            .query_map(params![file_id], |row| row.get::<_, String>(0))
            .map_err(|e| format!("sqlite query symbol_names_for_file: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("sqlite collect symbol_names_for_file: {e}"))
    }

    /// Full-text search over `symbols_fts` (ranked by FTS5 BM25, best match first).
    pub fn search_symbols_fts(&self, query: &str, limit: usize) -> Result<Vec<SymbolHit>, String> {
        self.search_symbols_fts_scoped(query, &[], limit)
    }

    /// Scoped full-text search over `symbols_fts`, filtering `files.rel_path` to
    /// `paths` (exact file or directory prefix) before applying `LIMIT`.
    pub fn search_symbols_fts_scoped(
        &self,
        query: &str,
        paths: &[String],
        limit: usize,
    ) -> Result<Vec<SymbolHit>, String> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let Some(and_expr) = sanitize_fts5_query(query, true, false) else {
            return Ok(Vec::new());
        };
        let hits = self.run_symbols_fts(&and_expr, paths, limit)?;
        if !hits.is_empty() {
            return Ok(hits);
        }
        if let Some(or_expr) = sanitize_fts5_query(query, true, true) {
            if or_expr != and_expr {
                return self.run_symbols_fts(&or_expr, paths, limit);
            }
        }
        Ok(Vec::new())
    }

    fn run_symbols_fts(
        &self,
        match_expr: &str,
        paths: &[String],
        limit: usize,
    ) -> Result<Vec<SymbolHit>, String> {
        let scope = build_path_scope_clause(paths, 2);
        let limit_param = 2 + scope.as_ref().map_or(0, |(_, b)| b.len());
        let sql = match &scope {
            Some((clause, _)) => format!(
                "SELECT s.id, s.file_id, f.rel_path, s.name, s.qual_name, s.kind,
                        s.line, s.end_line, s.signature, s.doc, s.exported, s.source,
                        -bm25(symbols_fts) AS score
                 FROM symbols_fts
                 JOIN symbols s ON s.id = symbols_fts.rowid
                 JOIN files f   ON f.id = s.file_id
                 WHERE symbols_fts MATCH ?1 AND ({clause})
                 ORDER BY bm25(symbols_fts) ASC, s.id ASC
                 LIMIT ?{limit_param}"
            ),
            None => "SELECT s.id, s.file_id, f.rel_path, s.name, s.qual_name, s.kind,
                        s.line, s.end_line, s.signature, s.doc, s.exported, s.source,
                        -bm25(symbols_fts) AS score
                 FROM symbols_fts
                 JOIN symbols s ON s.id = symbols_fts.rowid
                 JOIN files f   ON f.id = s.file_id
                 WHERE symbols_fts MATCH ?1
                 ORDER BY bm25(symbols_fts) ASC, s.id ASC
                 LIMIT ?2"
                .to_string(),
        };
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| format!("sqlite prepare search_symbols_fts: {e}"))?;
        let mut owned: Vec<rusqlite::types::Value> = Vec::with_capacity(limit_param);
        owned.push(rusqlite::types::Value::Text(match_expr.to_string()));
        if let Some((_, binds)) = scope {
            for b in binds {
                owned.push(rusqlite::types::Value::Text(b));
            }
        }
        owned.push(rusqlite::types::Value::Integer(limit as i64));
        let rows = stmt
            .query_map(rusqlite::params_from_iter(owned.iter()), |row| {
                Ok(SymbolHit {
                    id: row.get(0)?,
                    file_id: row.get(1)?,
                    rel_path: row.get(2)?,
                    name: row.get(3)?,
                    qual_name: row.get(4)?,
                    kind: row.get(5)?,
                    line: row.get::<_, i64>(6)? as usize,
                    end_line: row.get::<_, i64>(7)? as usize,
                    signature: row.get(8)?,
                    doc: row.get(9)?,
                    exported: row.get::<_, i64>(10)? != 0,
                    source: row.get(11)?,
                    score: row.get(12)?,
                })
            })
            .map_err(|e| format!("sqlite query search_symbols_fts: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("sqlite collect search_symbols_fts: {e}"))
    }

    /// List stored symbols under `paths` (empty or `"."` = whole workspace), ordered
    /// deterministically by path and line, capped at `limit`.
    pub fn symbols_in_paths(
        &self,
        paths: &[String],
        limit: usize,
    ) -> Result<Vec<SymbolHit>, String> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let scope = build_path_scope_clause(paths, 1);
        let limit_param = 1 + scope.as_ref().map_or(0, |(_, b)| b.len());
        let sql = match &scope {
            Some((clause, _)) => format!(
                "SELECT s.id, s.file_id, f.rel_path, s.name, s.qual_name, s.kind,
                        s.line, s.end_line, s.signature, s.doc, s.exported, s.source
                 FROM symbols s
                 JOIN files f ON f.id = s.file_id
                 WHERE {clause}
                 ORDER BY f.rel_path ASC, s.line ASC, s.id ASC
                 LIMIT ?{limit_param}"
            ),
            None => "SELECT s.id, s.file_id, f.rel_path, s.name, s.qual_name, s.kind,
                        s.line, s.end_line, s.signature, s.doc, s.exported, s.source
                 FROM symbols s
                 JOIN files f ON f.id = s.file_id
                 ORDER BY f.rel_path ASC, s.line ASC, s.id ASC
                 LIMIT ?1"
                .to_string(),
        };
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| format!("sqlite prepare symbols_in_paths: {e}"))?;
        let mut owned: Vec<rusqlite::types::Value> = Vec::with_capacity(limit_param);
        if let Some((_, binds)) = scope {
            for b in binds {
                owned.push(rusqlite::types::Value::Text(b));
            }
        }
        owned.push(rusqlite::types::Value::Integer(limit as i64));
        let rows = stmt
            .query_map(rusqlite::params_from_iter(owned.iter()), |row| {
                Ok(SymbolHit {
                    id: row.get(0)?,
                    file_id: row.get(1)?,
                    rel_path: row.get(2)?,
                    name: row.get(3)?,
                    qual_name: row.get(4)?,
                    kind: row.get(5)?,
                    line: row.get::<_, i64>(6)? as usize,
                    end_line: row.get::<_, i64>(7)? as usize,
                    signature: row.get(8)?,
                    doc: row.get(9)?,
                    exported: row.get::<_, i64>(10)? != 0,
                    source: row.get(11)?,
                    score: 0.0,
                })
            })
            .map_err(|e| format!("sqlite query symbols_in_paths: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("sqlite collect symbols_in_paths: {e}"))
    }

    /// Upsert a confidence-tagged edge into `edges` and return its `edges.id`.
    pub fn upsert_edge(&self, edge: &EdgeInput<'_>) -> Result<i64, String> {
        let kind = if edge.kind.is_empty() {
            "call"
        } else {
            edge.kind
        };
        let source = if edge.source.is_empty() {
            EDGE_SOURCE_TREESITTER
        } else {
            edge.source
        };
        self.conn
            .query_row(
                "INSERT INTO edges
                     (file_id, src_symbol_id, src_name, dst_name, kind, line, site_text, source, confidence)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(file_id, src_name, dst_name, kind, line, source) DO UPDATE SET
                     src_symbol_id = COALESCE(excluded.src_symbol_id, edges.src_symbol_id),
                     site_text     = CASE WHEN excluded.site_text != '' THEN excluded.site_text ELSE edges.site_text END,
                     confidence    = MAX(edges.confidence, excluded.confidence)
                 RETURNING id",
                params![
                    edge.file_id,
                    edge.src_symbol_id,
                    edge.src_name,
                    edge.dst_name,
                    kind,
                    edge.line as i64,
                    edge.site_text,
                    source,
                    edge.confidence,
                ],
                |row| row.get(0),
            )
            .map_err(|e| format!("sqlite upsert_edge {}->{}: {e}", edge.src_name, edge.dst_name))
    }

    /// Delete all edges in `file_id` from a given provenance `source`
    /// (e.g. clear `"treesitter"` edges before re-writing without touching `"scip"`).
    pub fn clear_file_edges_by_source(&self, file_id: i64, source: &str) -> Result<usize, String> {
        self.conn
            .execute(
                "DELETE FROM edges WHERE file_id = ?1 AND source = ?2",
                params![file_id, source],
            )
            .map_err(|e| format!("sqlite clear_file_edges_by_source: {e}"))
    }

    /// Delete all edges across the entire workspace from a given provenance `source`
    /// (e.g. clear stale `"scip"` edges before re-ingesting a fresh `.scip` index
    /// without touching `"treesitter"` or `"name-match"` edges).
    pub fn clear_edges_by_source(&self, source: &str) -> Result<usize, String> {
        self.conn
            .execute("DELETE FROM edges WHERE source = ?1", params![source])
            .map_err(|e| format!("sqlite clear_edges_by_source: {e}"))
    }

    /// Return all edges across the workspace matching provenance `source`,
    /// ordered by `(rel_path, line, id)`.
    pub fn all_edges_by_source(&self, source: &str) -> Result<Vec<EdgeRecord>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT e.id, e.file_id, f.rel_path, e.src_symbol_id, e.src_name, e.dst_name,
                        e.kind, e.line, e.site_text, e.source, e.confidence
                 FROM edges e
                 JOIN files f ON f.id = e.file_id
                 WHERE e.source = ?1
                 ORDER BY f.rel_path ASC, e.line ASC, e.id ASC",
            )
            .map_err(|e| format!("sqlite prepare all_edges_by_source: {e}"))?;
        let rows = stmt
            .query_map(params![source], map_edge_row)
            .map_err(|e| format!("sqlite query all_edges_by_source: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("sqlite collect all_edges_by_source: {e}"))
    }

    /// Count stored edges grouped by provenance `source` (`scip`, `treesitter`, `name-match`, …).
    pub fn count_edges_by_source(&self) -> Result<BTreeMap<String, usize>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT source, COUNT(*) FROM edges GROUP BY source ORDER BY source ASC")
            .map_err(|e| format!("sqlite prepare count_edges_by_source: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                let src: String = row.get(0)?;
                let cnt: i64 = row.get(1)?;
                Ok((src, cnt.max(0) as usize))
            })
            .map_err(|e| format!("sqlite query count_edges_by_source: {e}"))?;
        let mut out = BTreeMap::new();
        for r in rows {
            let (src, cnt) = r.map_err(|e| format!("sqlite row count_edges_by_source: {e}"))?;
            out.insert(src, cnt);
        }
        Ok(out)
    }

    /// Return all edges recorded in `file_id`, ordered by source line then confidence DESC.
    pub fn edges_for_file(&self, file_id: i64) -> Result<Vec<EdgeRecord>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT e.id, e.file_id, f.rel_path, e.src_symbol_id, e.src_name, e.dst_name,
                        e.kind, e.line, e.site_text, e.source, e.confidence
                 FROM edges e
                 JOIN files f ON f.id = e.file_id
                 WHERE e.file_id = ?1
                 ORDER BY e.line ASC, e.confidence DESC, e.id ASC",
            )
            .map_err(|e| format!("sqlite prepare edges_for_file: {e}"))?;
        let rows = stmt
            .query_map(params![file_id], map_edge_row)
            .map_err(|e| format!("sqlite query edges_for_file: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("sqlite collect edges_for_file: {e}"))
    }

    /// Look up incoming edges targeting `dst_name`, ordered by `(confidence DESC, line ASC)`.
    /// Uses index `idx_edges_dst_conf(dst_name, confidence DESC)`.
    pub fn callers_by_dst(
        &self,
        dst_name: &str,
        min_confidence: f64,
        limit: usize,
    ) -> Result<Vec<EdgeRecord>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT e.id, e.file_id, f.rel_path, e.src_symbol_id, e.src_name, e.dst_name,
                        e.kind, e.line, e.site_text, e.source, e.confidence
                 FROM edges e
                 JOIN files f ON f.id = e.file_id
                 WHERE e.dst_name = ?1 AND e.confidence >= ?2
                 ORDER BY e.confidence DESC, f.rel_path ASC, e.line ASC
                 LIMIT ?3",
            )
            .map_err(|e| format!("sqlite prepare callers_by_dst: {e}"))?;
        let rows = stmt
            .query_map(params![dst_name, min_confidence, limit as i64], map_edge_row)
            .map_err(|e| format!("sqlite query callers_by_dst: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("sqlite collect callers_by_dst: {e}"))
    }

    /// Look up outgoing edges originating from `src_symbol_id`.
    /// Uses index `idx_edges_src_sym(src_symbol_id)`.
    pub fn callees_by_src_symbol(&self, src_symbol_id: i64) -> Result<Vec<EdgeRecord>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT e.id, e.file_id, f.rel_path, e.src_symbol_id, e.src_name, e.dst_name,
                        e.kind, e.line, e.site_text, e.source, e.confidence
                 FROM edges e
                 JOIN files f ON f.id = e.file_id
                 WHERE e.src_symbol_id = ?1
                 ORDER BY e.confidence DESC, e.line ASC, e.id ASC",
            )
            .map_err(|e| format!("sqlite prepare callees_by_src_symbol: {e}"))?;
        let rows = stmt
            .query_map(params![src_symbol_id], map_edge_row)
            .map_err(|e| format!("sqlite query callees_by_src_symbol: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("sqlite collect callees_by_src_symbol: {e}"))
    }

    /// Upsert a documentation chunk into `doc_chunks` (updating `doc_chunks_fts`)
    /// and return its `doc_chunks.id`.
    pub fn upsert_doc_chunk(&self, chunk: &DocChunkInput<'_>) -> Result<i64, String> {
        let owned_sha;
        let sha = if chunk.sha256.is_empty() {
            owned_sha = sha256_hex(&format!("{}\n{}", chunk.heading, chunk.body));
            owned_sha.as_str()
        } else {
            chunk.sha256
        };
        self.conn
            .query_row(
                "INSERT INTO doc_chunks (file_id, chunk_idx, heading, body, sha256)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(file_id, chunk_idx) DO UPDATE SET
                     heading = excluded.heading,
                     body    = excluded.body,
                     sha256  = excluded.sha256
                 RETURNING id",
                params![
                    chunk.file_id,
                    chunk.chunk_idx as i64,
                    chunk.heading,
                    chunk.body,
                    sha,
                ],
                |row| row.get(0),
            )
            .map_err(|e| format!("sqlite upsert_doc_chunk: {e}"))
    }

    /// Replace all doc chunks for `file_id` cleanly (keeps `doc_chunks_fts` in sync).
    pub fn replace_doc_chunks(
        &self,
        file_id: i64,
        chunks: &[(String, String)],
    ) -> Result<usize, String> {
        self.conn
            .execute("DELETE FROM doc_chunks WHERE file_id = ?1", params![file_id])
            .map_err(|e| format!("sqlite clear doc_chunks: {e}"))?;
        for (idx, (heading, body)) in chunks.iter().enumerate() {
            self.upsert_doc_chunk(&DocChunkInput {
                file_id,
                chunk_idx: idx,
                heading,
                body,
                sha256: "",
            })?;
        }
        Ok(chunks.len())
    }

    /// Fetch all stored doc chunks for `file_id`, ordered by `chunk_idx`.
    pub fn doc_chunks_for_file(&self, file_id: i64) -> Result<Vec<DocChunkHit>, String> {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT c.id, c.file_id, f.rel_path, c.chunk_idx, c.heading, c.body, c.sha256
                     FROM doc_chunks c
                     JOIN files f ON f.id = c.file_id
                     WHERE c.file_id = ?1
                     ORDER BY c.chunk_idx ASC",
                )
                .map_err(|e| format!("sqlite prepare doc_chunks_for_file: {e}"))?;
            let rows = stmt
                .query_map(params![file_id], |row| {
                    Ok(DocChunkHit {
                        id: row.get(0)?,
                        file_id: row.get(1)?,
                        rel_path: row.get(2)?,
                        chunk_idx: row.get::<_, i64>(3)? as usize,
                        heading: row.get(4)?,
                        body: row.get(5)?,
                        sha256: row.get(6)?,
                        score: 0.0,
                    })
                })
                .map_err(|e| format!("sqlite query doc_chunks_for_file: {e}"))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("sqlite collect doc_chunks_for_file: {e}"))
        }

    /// Lightweight doc-stage watermark for warm corpus cache keys: SHA-256 of
    /// `(rel_path, stage_mtime_ns)` under `roots`, without loading chunk bodies.
    pub fn doc_stage_fingerprint(
        &self,
        roots: &[String],
    ) -> Result<(Vec<(String, u64)>, String), String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT f.rel_path, fs.mtime_ns
                 FROM files f
                 JOIN file_stages fs ON fs.file_id = f.id AND fs.stage = ?1
                 ORDER BY f.rel_path ASC",
            )
            .map_err(|e| format!("sqlite prepare doc_stage_fingerprint: {e}"))?;
        let rows = stmt
            .query_map(params![STAGE_DOC_CHUNKS], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
            })
            .map_err(|e| format!("sqlite query doc_stage_fingerprint: {e}"))?;
        let mut file_stages: Vec<(String, u64)> = Vec::new();
        let mut fp_parts: Vec<String> = Vec::new();
        for row in rows {
            let (rel, stage_mt) =
                row.map_err(|e| format!("sqlite collect doc_stage_fingerprint: {e}"))?;
            if !rel_under_roots(&rel, roots) {
                continue;
            }
            fp_parts.push(format!("{rel}:{stage_mt}"));
            file_stages.push((rel, stage_mt));
        }
        fp_parts.sort();
        Ok((file_stages, sha256_hex(&fp_parts.join("\n"))))
    }

    /// Bulk-load every `doc_chunks` row whose file is under one of `roots` and
    /// has a matching `STAGE_DOC_CHUNKS` watermark. Returns `(chunks, fingerprint)`
    /// where fingerprint is a stable SHA-256 of `(rel_path, stage_mtime_ns)` pairs.
    /// Callers must still verify on-disk mtimes against `stage_mtime` before trusting
    /// the corpus as fully warm.
    pub fn doc_chunks_for_roots(
        &self,
        roots: &[String],
    ) -> Result<(Vec<DocChunkHit>, Vec<(String, u64)>, String), String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT c.id, c.file_id, f.rel_path, c.chunk_idx, c.heading, c.body, c.sha256,
                        fs.mtime_ns, f.size_bytes
                 FROM doc_chunks c
                 JOIN files f ON f.id = c.file_id
                 JOIN file_stages fs ON fs.file_id = f.id AND fs.stage = ?1
                 ORDER BY f.rel_path ASC, c.chunk_idx ASC",
            )
            .map_err(|e| format!("sqlite prepare doc_chunks_for_roots: {e}"))?;
        let rows = stmt
            .query_map(params![STAGE_DOC_CHUNKS], |row| {
                Ok((
                    DocChunkHit {
                        id: row.get(0)?,
                        file_id: row.get(1)?,
                        rel_path: row.get(2)?,
                        chunk_idx: row.get::<_, i64>(3)? as usize,
                        heading: row.get(4)?,
                        body: row.get(5)?,
                        sha256: row.get(6)?,
                        score: 0.0,
                    },
                    row.get::<_, i64>(7)? as u64,
                    row.get::<_, i64>(8)? as u64,
                ))
            })
            .map_err(|e| format!("sqlite query doc_chunks_for_roots: {e}"))?;

        let mut chunks = Vec::new();
        let mut file_stages: Vec<(String, u64)> = Vec::new();
        let mut seen_files: HashSet<String> = HashSet::new();
        let mut fp_parts: Vec<String> = Vec::new();

        for row in rows {
            let (hit, stage_mt, _size_bytes) =
                row.map_err(|e| format!("sqlite collect doc_chunks_for_roots: {e}"))?;
            if !rel_under_roots(&hit.rel_path, roots) {
                continue;
            }
            if seen_files.insert(hit.rel_path.clone()) {
                file_stages.push((hit.rel_path.clone(), stage_mt));
                fp_parts.push(format!("{}:{}", hit.rel_path, stage_mt));
            }
            chunks.push(hit);
        }
        fp_parts.sort();
        let fingerprint = sha256_hex(&fp_parts.join("\n"));
        Ok((chunks, file_stages, fingerprint))
    }

    /// Observation-scoped stale check: only hash anchors belonging to the given
    /// `(obs_kind, obs_id)` keys (the top-k search hits), not the whole workspace.
    pub fn stale_anchors_for_observations(
        &self,
        root: &Path,
        keys: &[(String, String)],
    ) -> Result<Vec<StaleAnchor>, String> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let mut wanted: HashSet<(String, String)> = HashSet::with_capacity(keys.len());
        for (k, id) in keys {
            wanted.insert((k.clone(), id.clone()));
        }
        let links = self.list_observation_links(None)?;
        let mut file_cache: HashMap<String, Option<String>> = HashMap::new();
        let mut sym_cache: HashMap<(String, String), Option<String>> = HashMap::new();
        let mut out = Vec::new();
        for link in links {
            if !wanted.contains(&(link.obs_kind.clone(), link.obs_id.clone())) {
                continue;
            }
            if link.anchor_hash.is_empty() {
                continue;
            }
            let current = if link.symbol_name.is_empty() {
                file_cache
                    .entry(link.target_rel.clone())
                    .or_insert_with(|| {
                        std::fs::read(root.join(&link.target_rel))
                            .ok()
                            .map(|b| sha256_bytes(&b))
                    })
                    .clone()
            } else {
                let key = (link.target_rel.clone(), link.symbol_name.clone());
                sym_cache
                    .entry(key)
                    .or_insert_with(|| {
                        compute_symbol_anchor_hash(root, &link.target_rel, &link.symbol_name)
                    })
                    .clone()
            };
            if current.as_deref() != Some(link.anchor_hash.as_str()) {
                out.push(StaleAnchor {
                    obs_kind: link.obs_kind,
                    obs_id: link.obs_id,
                    target_rel: link.target_rel,
                    symbol_name: link.symbol_name,
                    recorded_hash: link.anchor_hash,
                    current_hash: current,
                });
            }
        }
        Ok(out)
    }

    /// Full-text search over `doc_chunks_fts` (ranked by FTS5 BM25, best match first).
    pub fn search_docs_fts(&self, query: &str, limit: usize) -> Result<Vec<DocChunkHit>, String> {
        self.search_docs_fts_scoped(query, &[], limit)
    }

    /// Scoped full-text search over `doc_chunks_fts`, filtering `files.rel_path`
    /// to `roots` (exact file or directory prefix) before applying `LIMIT`.
    pub fn search_docs_fts_scoped(
        &self,
        query: &str,
        roots: &[String],
        limit: usize,
    ) -> Result<Vec<DocChunkHit>, String> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let Some(and_expr) = sanitize_fts5_query(query, false, false) else {
            return Ok(Vec::new());
        };
        let hits = self.run_docs_fts(&and_expr, roots, limit)?;
        if !hits.is_empty() {
            return Ok(hits);
        }
        if let Some(or_expr) = sanitize_fts5_query(query, false, true) {
            if or_expr != and_expr {
                return self.run_docs_fts(&or_expr, roots, limit);
            }
        }
        Ok(Vec::new())
    }

    fn run_docs_fts(
        &self,
        match_expr: &str,
        roots: &[String],
        limit: usize,
    ) -> Result<Vec<DocChunkHit>, String> {
        let scope = build_path_scope_clause(roots, 2);
        let limit_param = 2 + scope.as_ref().map_or(0, |(_, b)| b.len());
        let sql = match &scope {
            Some((clause, _)) => format!(
                "SELECT c.id, c.file_id, f.rel_path, c.chunk_idx, c.heading, c.body, c.sha256,
                        -bm25(doc_chunks_fts) AS score
                 FROM doc_chunks_fts
                 JOIN doc_chunks c ON c.id = doc_chunks_fts.rowid
                 JOIN files f      ON f.id = c.file_id
                 WHERE doc_chunks_fts MATCH ?1 AND ({clause})
                 ORDER BY bm25(doc_chunks_fts) ASC, c.id ASC
                 LIMIT ?{limit_param}"
            ),
            None => "SELECT c.id, c.file_id, f.rel_path, c.chunk_idx, c.heading, c.body, c.sha256,
                        -bm25(doc_chunks_fts) AS score
                 FROM doc_chunks_fts
                 JOIN doc_chunks c ON c.id = doc_chunks_fts.rowid
                 JOIN files f      ON f.id = c.file_id
                 WHERE doc_chunks_fts MATCH ?1
                 ORDER BY bm25(doc_chunks_fts) ASC, c.id ASC
                 LIMIT ?2"
                .to_string(),
        };
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| format!("sqlite prepare search_docs_fts: {e}"))?;
        let mut owned: Vec<rusqlite::types::Value> = Vec::with_capacity(limit_param);
        owned.push(rusqlite::types::Value::Text(match_expr.to_string()));
        if let Some((_, binds)) = scope {
            for b in binds {
                owned.push(rusqlite::types::Value::Text(b));
            }
        }
        owned.push(rusqlite::types::Value::Integer(limit as i64));
        let rows = stmt
            .query_map(rusqlite::params_from_iter(owned.iter()), |row| {
                Ok(DocChunkHit {
                    id: row.get(0)?,
                    file_id: row.get(1)?,
                    rel_path: row.get(2)?,
                    chunk_idx: row.get::<_, i64>(3)? as usize,
                    heading: row.get(4)?,
                    body: row.get(5)?,
                    sha256: row.get(6)?,
                    score: row.get(7)?,
                })
            })
            .map_err(|e| format!("sqlite query search_docs_fts: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("sqlite collect search_docs_fts: {e}"))
    }

    /// Store an embedding vector (`f32` little-endian blob) keyed by `(key_sha256, model_tag)`.
    pub fn upsert_embedding(
        &self,
        key_sha256: &str,
        model_tag: &str,
        vec: &[f32],
    ) -> Result<(), String> {
        let mut blob = Vec::with_capacity(vec.len() * 4);
        for &v in vec {
            blob.extend_from_slice(&v.to_le_bytes());
        }
        let now = workspace::now_secs() as i64;
        self.conn
            .execute(
                "INSERT INTO embeddings (key_sha256, model_tag, dim, vec_blob, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(key_sha256, model_tag) DO UPDATE SET
                     dim        = excluded.dim,
                     vec_blob   = excluded.vec_blob,
                     updated_at = excluded.updated_at",
                params![key_sha256, model_tag, vec.len() as i64, blob, now],
            )
            .map_err(|e| format!("sqlite upsert_embedding: {e}"))?;
        Ok(())
    }

    /// Retrieve an embedding vector for `(key_sha256, model_tag)`.
    pub fn get_embedding(
        &self,
        key_sha256: &str,
        model_tag: &str,
    ) -> Result<Option<Vec<f32>>, String> {
        let row: Option<(i64, Vec<u8>)> = self
            .conn
            .query_row(
                "SELECT dim, vec_blob FROM embeddings WHERE key_sha256 = ?1 AND model_tag = ?2",
                params![key_sha256, model_tag],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| format!("sqlite get_embedding: {e}"))?;
        let Some((dim, blob)) = row else {
            return Ok(None);
        };
        Ok(decode_embedding_blob(dim, &blob))
    }

    /// Bulk-fetch embeddings for many content hashes under one `model_tag`
    /// via chunked `IN (...)` queries (avoids N+1 and full-table scans).
    pub fn get_embeddings_batch(
        &self,
        key_sha256s: &[String],
        model_tag: &str,
    ) -> Result<HashMap<String, Vec<f32>>, String> {
        let mut wanted: Vec<&str> = Vec::with_capacity(key_sha256s.len());
        let mut seen: HashSet<&str> = HashSet::with_capacity(key_sha256s.len());
        for sha in key_sha256s {
            if seen.insert(sha.as_str()) {
                wanted.push(sha.as_str());
            }
        }
        if wanted.is_empty() {
            return Ok(HashMap::new());
        }
        let mut out = HashMap::with_capacity(wanted.len());
        const CHUNK: usize = 400;
        for chunk in wanted.chunks(CHUNK) {
            let mut sql = String::from(
                "SELECT key_sha256, dim, vec_blob FROM embeddings WHERE model_tag = ?1 AND key_sha256 IN (",
            );
            for i in 0..chunk.len() {
                if i > 0 {
                    sql.push(',');
                }
                sql.push('?');
            }
            sql.push(')');
            let mut stmt = self
                .conn
                .prepare(&sql)
                .map_err(|e| format!("sqlite prepare get_embeddings_batch: {e}"))?;
            let mut params: Vec<rusqlite::types::Value> =
                Vec::with_capacity(1 + chunk.len());
            params.push(model_tag.to_string().into());
            for sha in chunk {
                params.push((*sha).to_string().into());
            }
            let rows = stmt
                .query_map(rusqlite::params_from_iter(params), |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Vec<u8>>(2)?,
                    ))
                })
                .map_err(|e| format!("sqlite query get_embeddings_batch: {e}"))?;
            for row in rows {
                let (k, dim, blob) =
                    row.map_err(|e| format!("sqlite collect get_embeddings_batch: {e}"))?;
                if let Some(vec) = decode_embedding_blob(dim, &blob) {
                    out.insert(k, vec);
                }
            }
        }
        Ok(out)
    }

    /// Remove rows from `files` whose paths no longer exist on disk under `self.root`.
    /// `ON DELETE CASCADE` + `recursive_triggers = ON` cleans up all dependent
    /// `symbols`, `edges`, `doc_chunks`, `file_stages`, `observation_links`, and FTS5 entries.
    pub fn gc_missing_files(&self) -> Result<GcReport, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, rel_path FROM files")
            .map_err(|e| format!("sqlite prepare gc_missing_files: {e}"))?;
        let rows: Vec<(i64, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| format!("sqlite query gc_missing_files: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("sqlite collect gc_missing_files: {e}"))?;
        drop(stmt);

        let files_checked = rows.len();
        let missing_ids: Vec<i64> = rows
            .into_iter()
            .filter_map(|(id, rel)| {
                let abs = self.root.join(&rel);
                if abs.exists() {
                    None
                } else {
                    Some(id)
                }
            })
            .collect();

        if missing_ids.is_empty() {
            return Ok(GcReport {
                files_checked,
                files_removed: 0,
            });
        }

        let removed = self.with_immediate_tx(|s| {
            let mut del = s
                .conn
                .prepare("DELETE FROM files WHERE id = ?1")
                .map_err(|e| format!("sqlite prepare gc delete: {e}"))?;
            let mut count = 0usize;
            for id in &missing_ids {
                count += del
                    .execute(params![id])
                    .map_err(|e| format!("sqlite gc delete file {id}: {e}"))?;
            }
            Ok(count)
        })?;

        Ok(GcReport {
            files_checked,
            files_removed: removed,
        })
    }

    /// Run SQLite FTS5 internal integrity check on both virtual tables.
    pub fn check_fts_integrity(&self) -> Result<(), String> {
        self.conn
            .execute_batch(
                "INSERT INTO symbols_fts(symbols_fts) VALUES('integrity-check');
                 INSERT INTO doc_chunks_fts(doc_chunks_fts) VALUES('integrity-check');",
            )
            .map_err(|e| format!("sqlite fts5 integrity-check failed: {e}"))
    }

    /// Read a value from the internal `meta` table.
    pub fn get_meta(&self, key: &str) -> Result<Option<String>, String> {
        self.conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("sqlite get_meta {key}: {e}"))
    }

    /// Write a value into the internal `meta` table.
    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map_err(|e| format!("sqlite set_meta {key}: {e}"))?;
        Ok(())
    }

    /// Emit a one-time notice to `stderr` when legacy monolithic JSON DiskMaps
    /// still exist in `$XDG_CACHE_HOME/wordkeep/`. Persisted in `meta` so it
    /// notifies once per workspace (never spamming MCP stderr or touching stdout).
    pub fn maybe_emit_legacy_cleanup_notice(&self) -> Option<String> {
        self.maybe_emit_legacy_cleanup_notice_in(&cache::dir())
    }

    /// Testable variant of [`Self::maybe_emit_legacy_cleanup_notice`] with an
    /// explicit cache directory (avoids mutating process-wide `XDG_CACHE_HOME`).
    pub fn maybe_emit_legacy_cleanup_notice_in(&self, cache_dir: &Path) -> Option<String> {
        if self
            .get_meta(META_LEGACY_NOTICE)
            .ok()
            .flatten()
            .is_some()
        {
            return None;
        }
        let blobs = detect_legacy_json_blobs(cache_dir);
        let notice = format_legacy_cleanup_notice(&blobs)?;
        let _ = self.set_meta(META_LEGACY_NOTICE, &workspace::now_secs().to_string());
        LEGACY_NOTICE_ONCE.call_once(|| {
            eprintln!("{notice}");
        });
        Some(notice)
    }

    /// Count total rows in `symbols`.
    pub fn symbol_count(&self) -> Result<usize, String> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
            .map_err(|e| format!("sqlite count symbols: {e}"))?;
        Ok(n.max(0) as usize)
    }

    /// Count total rows in `observation_links`.
    pub fn observation_link_count(&self) -> Result<usize, String> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM observation_links", [], |row| row.get(0))
            .map_err(|e| format!("sqlite count observation_links: {e}"))?;
        Ok(n.max(0) as usize)
    }

    /// Look up the workspace-relative defining file path for a symbol name or trailing `::` segment.
    pub fn lookup_symbol_file(&self, seg: &str) -> Result<Option<String>, String> {
        let suffix = format!("%::{seg}");
        self.conn
            .query_row(
                "SELECT f.rel_path
                 FROM symbols s
                 JOIN files f ON f.id = s.file_id
                 WHERE s.name = ?1 OR s.qual_name = ?1 OR s.name LIKE ?2
                 ORDER BY CASE WHEN s.name = ?1 THEN 0 ELSE 1 END, s.id ASC
                 LIMIT 1",
                params![seg, suffix],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("sqlite lookup_symbol_file {seg}: {e}"))
    }

    /// Extract backticked symbol and file anchors from `text` (plus `explicit_anchors`),
    /// compute their current SHA-256 content/definition hashes, and replace the
    /// `observation_links` rows for `(obs_kind, obs_id)`.
    pub fn sync_observation_links(
        &self,
        root: &Path,
        obs_kind: &str,
        obs_id: &str,
        file_id: Option<i64>,
        text: &str,
        explicit_anchors: &[String],
    ) -> Result<usize, String> {
        let mut raw_tokens: Vec<String> = Vec::new();
        let mut seen_raw: HashSet<String> = HashSet::new();
        for a in explicit_anchors {
            let t = a.trim();
            if !t.is_empty() && seen_raw.insert(t.to_string()) {
                raw_tokens.push(t.to_string());
            }
        }
        for tok in extract_backtick_tokens(text) {
            if seen_raw.insert(tok.clone()) {
                raw_tokens.push(tok);
            }
        }

        let mut warmed_symbols = false;
        let mut resolved: Vec<(String, String, String)> = Vec::new(); // (target_rel, symbol_name, hash)
        let mut seen_resolved: HashSet<(String, String)> = HashSet::new();

        for raw in raw_tokens {
            if resolved.len() >= 24 {
                break;
            }
            if let Some(rel_cand) = parse_file_anchor_candidate(&raw) {
                if rel_cand != obs_id {
                    let abs = root.join(&rel_cand);
                    if let Ok(bytes) = std::fs::read(&abs) {
                        if seen_resolved.insert((rel_cand.clone(), String::new())) {
                            resolved.push((rel_cand, String::new(), sha256_bytes(&bytes)));
                        }
                        continue;
                    }
                }
            }
            if let Some(sym_seg) = parse_symbol_anchor_candidate(&raw) {
                if !warmed_symbols {
                    crate::symbol_resolve::ensure_symbols_warm(root, self);
                    warmed_symbols = true;
                }
                if let Ok(Some(def_rel)) = self.lookup_symbol_file(&sym_seg) {
                    if let Some(hash) = compute_symbol_anchor_hash(root, &def_rel, &sym_seg) {
                        if seen_resolved.insert((def_rel.clone(), sym_seg.clone())) {
                            resolved.push((def_rel, sym_seg, hash));
                        }
                    }
                }
            }
        }

        let now = workspace::now_secs() as i64;
        self.conn
            .execute(
                "DELETE FROM observation_links WHERE obs_kind = ?1 AND obs_id = ?2",
                params![obs_kind, obs_id],
            )
            .map_err(|e| format!("sqlite clear observation_links {obs_kind}:{obs_id}: {e}"))?;

        for (target_rel, symbol_name, anchor_hash) in &resolved {
            self.conn
                .execute(
                    "INSERT INTO observation_links
                        (obs_kind, obs_id, file_id, symbol_name, target_rel, anchor_hash, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        obs_kind,
                        obs_id,
                        file_id,
                        symbol_name,
                        target_rel,
                        anchor_hash,
                        now
                    ],
                )
                .map_err(|e| format!("sqlite insert observation_link: {e}"))?;
        }
        Ok(resolved.len())
    }

    /// List stored `observation_links` rows (optionally filtered by `obs_kind`).
    pub fn list_observation_links(
        &self,
        filter_kind: Option<&str>,
    ) -> Result<Vec<ObservationLink>, String> {
        let sql = if filter_kind.is_some() {
            "SELECT id, obs_kind, obs_id, file_id, symbol_name, target_rel, anchor_hash, created_at
             FROM observation_links
             WHERE obs_kind = ?1
             ORDER BY id ASC"
        } else {
            "SELECT id, obs_kind, obs_id, file_id, symbol_name, target_rel, anchor_hash, created_at
             FROM observation_links
             ORDER BY id ASC"
        };
        let mut stmt = self
            .conn
            .prepare(sql)
            .map_err(|e| format!("sqlite prepare list_observation_links: {e}"))?;
        let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<ObservationLink> {
            Ok(ObservationLink {
                id: row.get(0)?,
                obs_kind: row.get(1)?,
                obs_id: row.get(2)?,
                file_id: row.get(3)?,
                symbol_name: row.get(4)?,
                target_rel: row.get(5)?,
                anchor_hash: row.get(6)?,
                created_at: row.get::<_, i64>(7)?.max(0) as u64,
            })
        };
        let rows = if let Some(k) = filter_kind {
            stmt.query_map(params![k], map_row)
        } else {
            stmt.query_map([], map_row)
        }
        .map_err(|e| format!("sqlite query list_observation_links: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("sqlite collect list_observation_links: {e}"))
    }

    /// Check stored `observation_links` against live on-disk files/symbols and return
    /// all anchors whose SHA-256 hash has diverged (or whose target disappeared).
    pub fn stale_anchors(
        &self,
        root: &Path,
        filter_kind: Option<&str>,
    ) -> Result<Vec<StaleAnchor>, String> {
        let links = self.list_observation_links(filter_kind)?;
        if links.is_empty() {
            return Ok(Vec::new());
        }
        let mut file_cache: std::collections::HashMap<String, Option<String>> =
            std::collections::HashMap::new();
        let mut sym_cache: std::collections::HashMap<(String, String), Option<String>> =
            std::collections::HashMap::new();
        let mut out = Vec::new();

        for link in links {
            if link.anchor_hash.is_empty() {
                continue;
            }
            let current = if link.symbol_name.is_empty() {
                file_cache
                    .entry(link.target_rel.clone())
                    .or_insert_with(|| {
                        std::fs::read(root.join(&link.target_rel))
                            .ok()
                            .map(|b| sha256_bytes(&b))
                    })
                    .clone()
            } else {
                let key = (link.target_rel.clone(), link.symbol_name.clone());
                sym_cache
                    .entry(key)
                    .or_insert_with(|| {
                        compute_symbol_anchor_hash(root, &link.target_rel, &link.symbol_name)
                    })
                    .clone()
            };
            if current.as_deref() != Some(link.anchor_hash.as_str()) {
                out.push(StaleAnchor {
                    obs_kind: link.obs_kind,
                    obs_id: link.obs_id,
                    target_rel: link.target_rel,
                    symbol_name: link.symbol_name,
                    recorded_hash: link.anchor_hash,
                    current_hash: current,
                });
            }
        }
        Ok(out)
    }

    /// Lightweight mtime-based cache tag over distinct `target_rel` paths in `observation_links`
    /// so `index_health`'s in-memory TTL cache invalidates immediately when anchored code changes.
    pub fn quick_obs_cache_tag(&self, root: &Path) -> String {
        let mut stmt = match self
            .conn
            .prepare("SELECT COUNT(*), GROUP_CONCAT(DISTINCT target_rel) FROM observation_links")
        {
            Ok(s) => s,
            Err(_) => return "obs=0".into(),
        };
        let Ok((count, joined)) = stmt.query_row([], |r| {
            Ok((
                r.get::<_, i64>(0).unwrap_or(0),
                r.get::<_, Option<String>>(1).unwrap_or(None),
            ))
        }) else {
            return "obs=0".into();
        };
        let mut max_mt = 0u64;
        let mut present = 0usize;
        if let Some(list) = joined {
            for rel in list.split(',').filter(|s| !s.is_empty()) {
                if let Ok(m) = std::fs::metadata(root.join(rel)) {
                    present += 1;
                    max_mt = max_mt.max(cache::mtime_ns(&m));
                }
            }
        }
        format!("obs={count}:{present}:{max_mt}")
    }
}

/// Extract single-backtick inline tokens from Markdown, skipping fenced code blocks.
pub fn extract_backtick_tokens(markdown: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut in_fence = false;

    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let bytes = line.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i] == b'`' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'`' {
                    while i < bytes.len() && bytes[i] == b'`' {
                        i += 1;
                    }
                    continue;
                }
                let start = i + 1;
                let mut end = start;
                while end < bytes.len() && bytes[end] != b'`' {
                    end += 1;
                }
                if end < bytes.len() && end > start {
                    let span = line[start..end].trim();
                    if !span.is_empty()
                        && span.len() <= 120
                        && !span.chars().any(char::is_whitespace)
                        && seen.insert(span.to_string())
                    {
                        out.push(span.to_string());
                    }
                    i = end + 1;
                    continue;
                }
            }
            i += 1;
        }
    }
    out
}

fn parse_file_anchor_candidate(raw: &str) -> Option<String> {
    let mut s = raw.trim().trim_matches('"').trim_matches('\'');
    if s.is_empty() || s.contains("://") || s.starts_with('-') {
        return None;
    }
    if let Some((before_hash, _)) = s.split_once('#') {
        s = before_hash;
    }
    // Strip trailing `:line` or `:line:col` (preserve `::` C++/Rust scope!).
    if !s.contains("::") {
        if let Some((path_part, rest)) = s.split_once(':') {
            if rest.chars().all(|c| c.is_ascii_digit() || c == ':' || c == '-') {
                s = path_part;
            }
        }
    }
    let norm = s.replace('\\', "/");
    let norm = norm.trim_start_matches("./").trim_matches('/');
    if norm.is_empty() || norm.contains("..") {
        return None;
    }
    const ANCHOR_EXTS: &[&str] = &[
        "rs", "cpp", "cc", "cxx", "c", "h", "hpp", "hh", "py", "pyi", "ts", "tsx", "svelte",
        "cs", "glsl", "vert", "frag", "comp", "das", "sh", "toml", "json", "md", "mdc",
    ];
    let has_ext = Path::new(norm)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ANCHOR_EXTS.iter().any(|k| ext.eq_ignore_ascii_case(k)));
    if has_ext {
        Some(norm.to_string())
    } else {
        None
    }
}

fn parse_symbol_anchor_candidate(raw: &str) -> Option<String> {
    let mut s = raw.trim();
    if let Some(stripped) = s.strip_suffix("()") {
        s = stripped;
    }
    if s.contains('/') || s.contains('.') && !s.contains("::") {
        return None;
    }
    if let Some((head, _)) = s.split_once('<') {
        s = head;
    }
    let seg = s.rsplit("::").next().unwrap_or(s).trim();
    if seg.len() < 3 || seg.len() > 80 {
        return None;
    }
    let mut chars = seg.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let lower = seg.to_ascii_lowercase();
    const NOISE: &[&str] = &[
        "true", "false", "null", "none", "some", "ok", "err", "self", "super", "crate",
        "main", "new", "test", "tests", "todo", "note", "done", "open", "gated", "blocked",
        "auto", "full", "core", "json", "text", "utf8", "head",
    ];
    if NOISE.contains(&lower.as_str()) {
        return None;
    }
    // Require snake_case (`_`), CamelCase (uppercase letter), or qualified `::` to avoid
    // anchoring plain lowercase English words in backticks unless qualified.
    let structured =
        seg.contains('_') || seg.chars().any(|c| c.is_ascii_uppercase()) || raw.contains("::");
    if !structured {
        return None;
    }
    Some(seg.to_string())
}

fn compute_symbol_anchor_hash(root: &Path, def_rel: &str, sym_seg: &str) -> Option<String> {
    let scope = vec![def_rel.to_string()];
    let (info, _) = crate::symbol_def::locate_with_stats(root, sym_seg, &scope, true);
    let info = info?;
    let canonical = format!("{}\n{}", info.signature.trim(), info.body.trim());
    Some(sha256_hex(&canonical))
}

/// Detect legacy monolithic JSON DiskMap blobs under `cache_dir`.
pub fn detect_legacy_json_blobs(cache_dir: &Path) -> Vec<(PathBuf, u64)> {
    let mut found = Vec::new();
    for name in LEGACY_DISKMAP_FILES {
        let p = cache_dir.join(name);
        if let Ok(meta) = std::fs::metadata(&p) {
            if meta.is_file() {
                found.push((p, meta.len()));
            }
        }
    }
    found
}

/// Format a concise, safe cleanup notice for detected legacy DiskMap blobs.
pub fn format_legacy_cleanup_notice(blobs: &[(PathBuf, u64)]) -> Option<String> {
    if blobs.is_empty() {
        return None;
    }
    let total_bytes: u64 = blobs.iter().map(|(_, b)| *b).sum();
    let total_mb = total_bytes as f64 / (1024.0 * 1024.0);
    let names: Vec<String> = blobs
        .iter()
        .filter_map(|(p, b)| {
            let n = p.file_name()?.to_string_lossy();
            let mb = *b as f64 / (1024.0 * 1024.0);
            Some(format!("{n} ({mb:.1} MB)"))
        })
        .collect();
    let rm_args: Vec<String> = blobs
        .iter()
        .map(|(p, _)| format!("'{}'", p.display()))
        .collect();
    Some(format!(
        "[wordkeep] SQLite workspace store active; found {} legacy JSON cache blob(s) totaling {total_mb:.1} MB [{}]. Safe to reclaim space: rm -f {}",
        blobs.len(),
        names.join(", "),
        rm_args.join(" ")
    ))
}

/// Start a debounced filesystem watcher for `root` that keeps the per-workspace
/// SQLite store up to date in MCP mode.
///
/// - Walks non-pruned directories via `ignore::WalkBuilder` (skipping `.git`,
///   `build*`, `target*`, `.venv`, `.worktrees`, `node_modules`, etc.).
/// - On file deletion: removes the `files` row (cascading symbols/edges/doc_chunks/FTS5).
/// - On Markdown edit: eagerly re-chunks into `doc_chunks` + `doc_chunks_fts`.
/// - On source edit: invalidates `file_stages` so the next tool query re-parses cleanly.
pub fn start_watcher(root: &Path, debounce: Duration) -> Result<WatcherHandle, String> {
    start_watcher_with_hook(root, debounce, |_| {})
}

/// Variant of [`start_watcher`] that also invokes `on_batch` with the filtered
/// changed paths after updating the store (useful for diagnostics and tests).
pub fn start_watcher_with_hook<F>(
    root: &Path,
    debounce: Duration,
    on_batch: F,
) -> Result<WatcherHandle, String>
where
    F: Fn(&[PathBuf]) + Send + 'static,
{
    let canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let prune = walk::prune_set(&canon);
    let cb_root = canon.clone();
    let cb_prune = prune.clone();

    let mut debouncer = new_debouncer(debounce, move |res: DebounceEventResult| {
        let Ok(events) = res else {
            return;
        };
        let mut unique = BTreeSet::new();
        for ev in events {
            if !is_watcher_path_pruned(&cb_root, &ev.path, &cb_prune) {
                unique.insert(ev.path);
            }
        }
        if unique.is_empty() {
            return;
        }
        let batch: Vec<PathBuf> = unique.into_iter().collect();
        apply_watcher_batch(&cb_root, &batch);
        on_batch(&batch);
    })
    .map_err(|e| format!("notify debouncer init: {e}"))?;

    // Always watch root itself non-recursively.
    debouncer
        .watcher()
        .watch(&canon, RecursiveMode::NonRecursive)
        .map_err(|e| format!("watch {}: {e}", canon.display()))?;
    let mut watched_dirs = 1usize;

    // Register non-pruned subdirectories via `ignore::WalkBuilder` so we never
    // burn inotify watches on build/, target-*/, .venv/, .git/, or .worktrees/.
    let prune_walk = prune.clone();
    let walker = ignore::WalkBuilder::new(&canon)
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .require_git(false)
        .parents(true)
        .filter_entry(move |entry| {
            if entry.depth() == 0 {
                return true;
            }
            if entry.file_type().is_some_and(|t| t.is_dir()) {
                if let Some(name) = entry.file_name().to_str() {
                    return !walk::is_pruned_name(name, &prune_walk);
                }
            }
            true
        })
        .build();

    for entry in walker.flatten() {
        if watched_dirs >= MAX_WATCH_DIRS {
            break;
        }
        if entry.depth() == 0 {
            continue;
        }
        if entry.file_type().is_some_and(|t| t.is_dir()) {
            if debouncer
                .watcher()
                .watch(entry.path(), RecursiveMode::NonRecursive)
                .is_ok()
            {
                watched_dirs += 1;
            }
        }
    }

    Ok(WatcherHandle {
        _debouncer: debouncer,
        watched_dirs,
    })
}

fn is_watcher_path_pruned(root: &Path, path: &Path, prune: &HashSet<String>) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return true;
    };
    if rel.as_os_str().is_empty() {
        return true;
    }
    for comp in rel.components() {
        let std::path::Component::Normal(os) = comp else {
            continue;
        };
        if let Some(s) = os.to_str() {
            if walk::is_pruned_name(s, prune) {
                return true;
            }
        }
    }
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if name.ends_with(".tmp")
            || name.ends_with(".swp")
            || name.ends_with('~')
            || name.ends_with(".min.js")
            || name.ends_with(".min.css")
            || name.ends_with(".map")
        {
            return true;
        }
    }
    false
}

fn apply_watcher_batch(root: &Path, paths: &[PathBuf]) {
    let Ok(store) = Store::open(root) else {
        return;
    };
    // Doc edits invalidate the process-local warm knowledge corpus.
    if paths.iter().any(|p| {
        p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| DOC_EXT.iter().any(|x| x.eq_ignore_ascii_case(e)))
            || !p.exists()
    }) {
        crate::knowledge::invalidate_warm_corpus_cache();
    }
    let _ = store.with_immediate_tx(|s| {
        for path in paths {
            let rel = normalize_rel(root, path);
            if rel.is_empty() {
                continue;
            }
            if !path.exists() {
                let _ = s.delete_file(&rel);
                continue;
            }
            if !path.is_file() {
                continue;
            }
            let is_doc = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| DOC_EXT.iter().any(|x| x.eq_ignore_ascii_case(e)));
            if is_doc {
                if let (Ok(meta), Ok(src)) = (std::fs::metadata(path), std::fs::read_to_string(path))
                {
                    let mt = cache::mtime_ns(&meta);
                    let file_id = s.upsert_file(&rel, mt, meta.len(), "markdown")?;
                    let chunks = wordkeep_knowledge::chunk_bodies(&rel, &src);
                    s.replace_doc_chunks(file_id, &chunks)?;
                    s.set_file_stage(file_id, STAGE_DOC_CHUNKS, mt, None)?;
                }
            } else {
                let _ = s.invalidate_file_stages(&rel);
            }
        }
        Ok(())
    });
}

fn map_edge_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EdgeRecord> {
    Ok(EdgeRecord {
        id: row.get(0)?,
        file_id: row.get(1)?,
        rel_path: row.get(2)?,
        src_symbol_id: row.get(3)?,
        src_name: row.get(4)?,
        dst_name: row.get(5)?,
        kind: row.get(6)?,
        line: row.get::<_, i64>(7)? as usize,
        site_text: row.get(8)?,
        source: row.get(9)?,
        confidence: row.get(10)?,
    })
}

/// Sanitize arbitrary user/agent input into a safe SQLite FTS5 `MATCH` expression.
/// Splits on non-alphanumeric boundaries (matching `unicode61`), quotes each term
/// so FTS5 keywords (`AND`/`OR`/`NOT`/`NEAR`) never trigger syntax errors, and
/// optionally applies prefix matching (`"tok"*`) to the final token.
pub fn sanitize_fts5_query(raw: &str, prefix_last: bool, join_or: bool) -> Option<String> {
    let tokens: Vec<&str> = raw
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .take(24)
        .collect();
    if tokens.is_empty() {
        return None;
    }
    let last_idx = tokens.len() - 1;
    let parts: Vec<String> = tokens
        .iter()
        .enumerate()
        .map(|(i, tok)| {
            if prefix_last && i == last_idx && tok.len() >= 2 {
                format!("\"{tok}\"*")
            } else {
                format!("\"{tok}\"")
            }
        })
        .collect();
    let sep = if join_or { " OR " } else { " " };
    Some(parts.join(sep))
}

fn normalize_scope_prefix(raw: &str) -> String {
    let mut s = raw.trim().replace('\\', "/");
    while let Some(rest) = s.strip_prefix("./") {
        s = rest.to_string();
    }
    s.trim_end_matches('/').to_string()
}

fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Build a parameterized SQL predicate matching `f.rel_path` against exact file
/// paths or directory prefixes in `roots`. Returns `None` when unscoped (empty
/// list or includes `"."`).
fn build_path_scope_clause(roots: &[String], start_param: usize) -> Option<(String, Vec<String>)> {
    if roots.is_empty() {
        return None;
    }
    let mut clauses = Vec::new();
    let mut binds = Vec::new();
    let mut next_idx = start_param;
    for raw in roots {
        let p = normalize_scope_prefix(raw);
        if p.is_empty() || p == "." {
            return None;
        }
        let eq_i = next_idx;
        let like_i = next_idx + 1;
        next_idx += 2;
        clauses.push(format!(
            "(f.rel_path = ?{eq_i} OR f.rel_path LIKE ?{like_i} ESCAPE '\\')"
        ));
        binds.push(p.clone());
        binds.push(format!("{}/%", escape_like(&p)));
    }
    if clauses.is_empty() {
        None
    } else {
        Some((clauses.join(" OR "), binds))
    }
}

fn init_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch("BEGIN IMMEDIATE;")
        .map_err(|e| format!("sqlite schema BEGIN IMMEDIATE: {e}"))?;

    let ver: u32 = conn
        .query_row("PRAGMA user_version;", [], |row| row.get(0))
        .unwrap_or(0);
    if ver >= SCHEMA_VERSION {
        let _ = conn.execute_batch("COMMIT;");
        return Ok(());
    }

    let ddl = format!(
        "CREATE TABLE IF NOT EXISTS meta (
             key   TEXT PRIMARY KEY,
             value TEXT NOT NULL
         );

         CREATE TABLE IF NOT EXISTS files (
             id         INTEGER PRIMARY KEY AUTOINCREMENT,
             rel_path   TEXT NOT NULL UNIQUE,
             mtime_ns   INTEGER NOT NULL,
             size_bytes INTEGER NOT NULL DEFAULT 0,
             lang       TEXT NOT NULL DEFAULT '',
             indexed_at INTEGER NOT NULL DEFAULT 0
         );

         CREATE TABLE IF NOT EXISTS file_stages (
             file_id  INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
             stage    TEXT NOT NULL,
             mtime_ns INTEGER NOT NULL,
             payload  TEXT,
             PRIMARY KEY (file_id, stage)
         );

         CREATE TABLE IF NOT EXISTS symbols (
             id        INTEGER PRIMARY KEY AUTOINCREMENT,
             file_id   INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
             name      TEXT NOT NULL,
             qual_name TEXT NOT NULL DEFAULT '',
             kind      TEXT NOT NULL DEFAULT '',
             line      INTEGER NOT NULL DEFAULT 0,
             end_line  INTEGER NOT NULL DEFAULT 0,
             signature TEXT NOT NULL DEFAULT '',
             doc       TEXT NOT NULL DEFAULT '',
             exported  INTEGER NOT NULL DEFAULT 0,
             source    TEXT NOT NULL DEFAULT 'treesitter',
             UNIQUE(file_id, name, qual_name, kind, line)
         );
         CREATE INDEX IF NOT EXISTS idx_symbols_file      ON symbols(file_id);
         CREATE INDEX IF NOT EXISTS idx_symbols_name      ON symbols(name);
         CREATE INDEX IF NOT EXISTS idx_symbols_qual_name ON symbols(qual_name);

         CREATE VIRTUAL TABLE IF NOT EXISTS symbols_fts USING fts5(
             name,
             qual_name,
             signature,
             doc,
             content='symbols',
             content_rowid='id',
             tokenize='unicode61'
         );

         CREATE TRIGGER IF NOT EXISTS symbols_ai AFTER INSERT ON symbols BEGIN
             INSERT INTO symbols_fts(rowid, name, qual_name, signature, doc)
             VALUES (new.id, new.name, new.qual_name, new.signature, new.doc);
         END;
         CREATE TRIGGER IF NOT EXISTS symbols_ad AFTER DELETE ON symbols BEGIN
             INSERT INTO symbols_fts(symbols_fts, rowid, name, qual_name, signature, doc)
             VALUES ('delete', old.id, old.name, old.qual_name, old.signature, old.doc);
         END;
         CREATE TRIGGER IF NOT EXISTS symbols_au AFTER UPDATE ON symbols BEGIN
             INSERT INTO symbols_fts(symbols_fts, rowid, name, qual_name, signature, doc)
             VALUES ('delete', old.id, old.name, old.qual_name, old.signature, old.doc);
             INSERT INTO symbols_fts(rowid, name, qual_name, signature, doc)
             VALUES (new.id, new.name, new.qual_name, new.signature, new.doc);
         END;

         CREATE TABLE IF NOT EXISTS edges (
             id            INTEGER PRIMARY KEY AUTOINCREMENT,
             file_id       INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
             src_symbol_id INTEGER REFERENCES symbols(id) ON DELETE CASCADE,
             src_name      TEXT NOT NULL,
             dst_name      TEXT NOT NULL,
             kind          TEXT NOT NULL DEFAULT 'call',
             line          INTEGER NOT NULL DEFAULT 0,
             site_text     TEXT NOT NULL DEFAULT '',
             source        TEXT NOT NULL DEFAULT 'treesitter',
             confidence    REAL NOT NULL DEFAULT 0.5,
             UNIQUE(file_id, src_name, dst_name, kind, line, source)
         );
         CREATE INDEX IF NOT EXISTS idx_edges_dst_conf ON edges(dst_name, confidence DESC);
         CREATE INDEX IF NOT EXISTS idx_edges_src_sym  ON edges(src_symbol_id);
         CREATE INDEX IF NOT EXISTS idx_edges_file     ON edges(file_id);
         CREATE INDEX IF NOT EXISTS idx_edges_src_name ON edges(src_name);

         CREATE TABLE IF NOT EXISTS doc_chunks (
             id        INTEGER PRIMARY KEY AUTOINCREMENT,
             file_id   INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
             chunk_idx INTEGER NOT NULL DEFAULT 0,
             heading   TEXT NOT NULL DEFAULT '',
             body      TEXT NOT NULL,
             sha256    TEXT NOT NULL DEFAULT '',
             UNIQUE(file_id, chunk_idx)
         );
         CREATE INDEX IF NOT EXISTS idx_doc_chunks_file ON doc_chunks(file_id);

         CREATE VIRTUAL TABLE IF NOT EXISTS doc_chunks_fts USING fts5(
             heading,
             body,
             content='doc_chunks',
             content_rowid='id',
             tokenize='unicode61'
         );

         CREATE TRIGGER IF NOT EXISTS doc_chunks_ai AFTER INSERT ON doc_chunks BEGIN
             INSERT INTO doc_chunks_fts(rowid, heading, body)
             VALUES (new.id, new.heading, new.body);
         END;
         CREATE TRIGGER IF NOT EXISTS doc_chunks_ad AFTER DELETE ON doc_chunks BEGIN
             INSERT INTO doc_chunks_fts(doc_chunks_fts, rowid, heading, body)
             VALUES ('delete', old.id, old.heading, old.body);
         END;
         CREATE TRIGGER IF NOT EXISTS doc_chunks_au AFTER UPDATE ON doc_chunks BEGIN
             INSERT INTO doc_chunks_fts(doc_chunks_fts, rowid, heading, body)
             VALUES ('delete', old.id, old.heading, old.body);
             INSERT INTO doc_chunks_fts(rowid, heading, body)
             VALUES (new.id, new.heading, new.body);
         END;

         CREATE TABLE IF NOT EXISTS embeddings (
             key_sha256 TEXT NOT NULL,
             model_tag  TEXT NOT NULL,
             dim        INTEGER NOT NULL,
             vec_blob   BLOB NOT NULL,
             updated_at INTEGER NOT NULL DEFAULT 0,
             PRIMARY KEY (key_sha256, model_tag)
         );

         -- Provisioned for v0.5 staleness / defect / note links (ON DELETE CASCADE on files).
         CREATE TABLE IF NOT EXISTS observation_links (
             id          INTEGER PRIMARY KEY AUTOINCREMENT,
             obs_kind    TEXT NOT NULL,
             obs_id      TEXT NOT NULL,
             file_id     INTEGER REFERENCES files(id) ON DELETE CASCADE,
             symbol_name TEXT NOT NULL DEFAULT '',
             target_rel  TEXT NOT NULL DEFAULT '',
             anchor_hash TEXT NOT NULL DEFAULT '',
             created_at  INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS idx_obs_links_file ON observation_links(file_id);
         CREATE INDEX IF NOT EXISTS idx_obs_links_obs  ON observation_links(obs_kind, obs_id);"
    );

    if let Err(e) = conn.execute_batch(&ddl) {
        let _ = conn.execute_batch("ROLLBACK;");
        return Err(format!("sqlite init_schema: {e}"));
    }

    // Upgrade v1 observation_links tables in-place before setting user_version = 2.
    if ver == 1 {
        let cols: HashSet<String> = conn
            .prepare("PRAGMA table_info(observation_links);")
            .and_then(|mut s| {
                let rows = s.query_map([], |r| r.get::<_, String>(1))?;
                Ok(rows.flatten().collect())
            })
            .unwrap_or_default();
        if !cols.contains("target_rel") {
            if let Err(e) = conn.execute_batch(
                "ALTER TABLE observation_links ADD COLUMN target_rel TEXT NOT NULL DEFAULT '';",
            ) {
                let _ = conn.execute_batch("ROLLBACK;");
                return Err(format!("sqlite migrate observation_links.target_rel: {e}"));
            }
        }
        if !cols.contains("anchor_hash") {
            if let Err(e) = conn.execute_batch(
                "ALTER TABLE observation_links ADD COLUMN anchor_hash TEXT NOT NULL DEFAULT '';",
            ) {
                let _ = conn.execute_batch("ROLLBACK;");
                return Err(format!("sqlite migrate observation_links.anchor_hash: {e}"));
            }
        }
    }

    if let Err(e) = conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};")) {
        let _ = conn.execute_batch("ROLLBACK;");
        return Err(format!("sqlite set user_version: {e}"));
    }
    conn.execute_batch("COMMIT;")
        .map_err(|e| format!("sqlite schema COMMIT: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Condvar, Mutex};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_workspace(tag: &str) -> PathBuf {
        let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "wk_store_{tag}_{}_{}",
            std::process::id(),
            n
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn open_initializes_wal_pragmas_and_schema() {
        let _g = crate::cache::test_env_lock();
        let root = temp_workspace("open");

        let store = Store::open(&root).unwrap();
        let expected = workspace::workspace_dir(&root).join(STORE_FILENAME);
        assert_eq!(store.path(), expected.as_path());
        assert!(expected.exists());
        assert_eq!(store.journal_mode().unwrap(), "wal");
        assert!(store.foreign_keys_enabled().unwrap());
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);

        // Verify all required tables and edge indexes exist.
        let tables: Vec<String> = {
            let mut stmt = store
                .conn
                .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
                .unwrap();
            stmt.query_map([], |r| r.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        for req in [
            "files",
            "file_stages",
            "symbols",
            "symbols_fts",
            "edges",
            "doc_chunks",
            "doc_chunks_fts",
            "embeddings",
            "observation_links",
            "meta",
        ] {
            assert!(
                tables.iter().any(|t| t == req),
                "missing table {req} in {tables:?}"
            );
        }

        let indexes: Vec<String> = {
            let mut stmt = store
                .conn
                .prepare("SELECT name FROM sqlite_master WHERE type = 'index'")
                .unwrap();
            stmt.query_map([], |r| r.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        assert!(indexes.iter().any(|i| i == "idx_edges_dst_conf"));
        assert!(indexes.iter().any(|i| i == "idx_edges_src_sym"));
        store.check_fts_integrity().unwrap();

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn upsert_symbols_edges_and_fts_query() {
        let root = temp_workspace("upsert");
        let db = root.join("test.sqlite3");
        let store = Store::open_at(&root, &db).unwrap();

        let f1 = store.upsert_file("src/water.cpp", 100, 512, "cpp").unwrap();
        let sym_tick = store
            .upsert_symbol(&SymbolInput {
                file_id: f1,
                name: "tick_volumetric_water",
                qual_name: "water::tick_volumetric_water",
                kind: "function",
                line: 42,
                end_line: 88,
                signature: "void water::tick_volumetric_water(float dt)",
                doc: "Advance cellular voxel water simulation by one fixed step.",
                exported: true,
                source: EDGE_SOURCE_TREESITTER,
            })
            .unwrap();
        let sym_mesh = store
            .upsert_symbol(&SymbolInput {
                file_id: f1,
                name: "remesh_chunk",
                qual_name: "water::remesh_chunk",
                kind: "function",
                line: 95,
                end_line: 140,
                signature: "void water::remesh_chunk(ChunkId id)",
                doc: "Rebuild surface triangles after voxel edit.",
                exported: false,
                source: EDGE_SOURCE_TREESITTER,
            })
            .unwrap();

        // Confidence-tagged edges (treesitter=0.5, name-match=0.3, scip=1.0).
        store
            .upsert_edge(&EdgeInput {
                file_id: f1,
                src_symbol_id: Some(sym_tick),
                src_name: "tick_volumetric_water",
                dst_name: "remesh_chunk",
                kind: "call",
                line: 60,
                site_text: "remesh_chunk(cid);",
                source: EDGE_SOURCE_TREESITTER,
                confidence: CONFIDENCE_TREESITTER,
            })
            .unwrap();
        store
            .upsert_edge(&EdgeInput {
                file_id: f1,
                src_symbol_id: Some(sym_tick),
                src_name: "tick_volumetric_water",
                dst_name: "remesh_chunk",
                kind: "call",
                line: 61,
                site_text: "remesh_chunk(cid2);",
                source: EDGE_SOURCE_NAME_MATCH,
                confidence: CONFIDENCE_NAME_MATCH,
            })
            .unwrap();
        store
            .upsert_edge(&EdgeInput {
                file_id: f1,
                src_symbol_id: Some(sym_tick),
                src_name: "tick_volumetric_water",
                dst_name: "remesh_chunk",
                kind: "call",
                line: 62,
                site_text: "remesh_chunk(cid3);",
                source: EDGE_SOURCE_SCIP,
                confidence: CONFIDENCE_SCIP,
            })
            .unwrap();

        // Verify callers_by_dst orders by confidence DESC.
        let callers = store.callers_by_dst("remesh_chunk", 0.0, 10).unwrap();
        assert_eq!(callers.len(), 3);
        assert_eq!(callers[0].source, EDGE_SOURCE_SCIP);
        assert!((callers[0].confidence - CONFIDENCE_SCIP).abs() < 1e-9);
        assert_eq!(callers[1].source, EDGE_SOURCE_TREESITTER);
        assert!((callers[1].confidence - CONFIDENCE_TREESITTER).abs() < 1e-9);
        assert_eq!(callers[2].source, EDGE_SOURCE_NAME_MATCH);
        assert!((callers[2].confidence - CONFIDENCE_NAME_MATCH).abs() < 1e-9);

        // Callees by src_symbol_id.
        let callees = store.callees_by_src_symbol(sym_tick).unwrap();
        assert_eq!(callees.len(), 3);
        assert_eq!(callees[0].dst_name, "remesh_chunk");

        // FTS5 symbol search (including punctuation / C++ qualifiers / prefix).
        let hits = store.search_symbols_fts("water::tick_volu", 5).unwrap();
        assert!(!hits.is_empty(), "expected prefix FTS hit");
        assert_eq!(hits[0].id, sym_tick);
        assert_eq!(hits[0].rel_path, "src/water.cpp");

        let hits2 = store.search_symbols_fts("triangles voxel", 5).unwrap();
        assert_eq!(hits2.len(), 1);
        assert_eq!(hits2[0].id, sym_mesh);

        // Embeddings round-trip.
        let key = sha256_hex("water::tick_volumetric_water");
        store
            .upsert_embedding(&key, "potion-code-16M-v2", &[0.25, -0.5, 1.0])
            .unwrap();
        let got = store
            .get_embedding(&key, "potion-code-16M-v2")
            .unwrap()
            .unwrap();
        assert_eq!(got, vec![0.25, -0.5, 1.0]);

        store.check_fts_integrity().unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn doc_chunks_fts_and_gc_missing_files_cascade() {
        let root = temp_workspace("gc");
        let db = root.join("test.sqlite3");
        let store = Store::open_at(&root, &db).unwrap();

        // Create two real files on disk.
        let keep_p = root.join("docs/keep.md");
        let drop_p = root.join("docs/drop.md");
        std::fs::create_dir_all(keep_p.parent().unwrap()).unwrap();
        std::fs::write(&keep_p, "# Keep\nRollback determinism guide.\n").unwrap();
        std::fs::write(&drop_p, "# Drop\nEphemeral xylophone notes.\n").unwrap();

        let f_keep = store.upsert_file("docs/keep.md", 10, 40, "markdown").unwrap();
        let f_drop = store.upsert_file("docs/drop.md", 10, 40, "markdown").unwrap();

        store
            .upsert_doc_chunk(&DocChunkInput {
                file_id: f_keep,
                chunk_idx: 0,
                heading: "Keep",
                body: "Rollback determinism guide for physics ticks.",
                sha256: "",
            })
            .unwrap();
        store
            .upsert_doc_chunk(&DocChunkInput {
                file_id: f_drop,
                chunk_idx: 0,
                heading: "Drop",
                body: "Ephemeral xylophone notes to be garbage collected.",
                sha256: "",
            })
            .unwrap();
        let sym_drop = store
            .upsert_symbol(&SymbolInput {
                file_id: f_drop,
                name: "xylophone_helper",
                qual_name: "xylophone_helper",
                kind: "function",
                line: 1,
                end_line: 3,
                signature: "fn xylophone_helper()",
                doc: "",
                exported: false,
                source: EDGE_SOURCE_TREESITTER,
            })
            .unwrap();
        store
            .upsert_edge(&EdgeInput {
                file_id: f_drop,
                src_symbol_id: Some(sym_drop),
                src_name: "xylophone_helper",
                dst_name: "foo",
                kind: "call",
                line: 2,
                site_text: "foo()",
                source: EDGE_SOURCE_TREESITTER,
                confidence: CONFIDENCE_TREESITTER,
            })
            .unwrap();

        // Both docs are searchable before deletion.
        assert_eq!(store.search_docs_fts("xylophone", 5).unwrap().len(), 1);
        assert_eq!(store.search_docs_fts("determinism", 5).unwrap().len(), 1);
        assert_eq!(store.search_symbols_fts("xylophone", 5).unwrap().len(), 1);

        // Delete one file from disk and run GC.
        std::fs::remove_file(&drop_p).unwrap();
        let rep = store.gc_missing_files().unwrap();
        assert_eq!(rep.files_checked, 2);
        assert_eq!(rep.files_removed, 1);

        // Dropped file, its chunks, symbols, edges, AND FTS5 rows are gone!
        assert!(store.get_file("docs/drop.md").unwrap().is_none());
        assert!(store.get_file("docs/keep.md").unwrap().is_some());
        assert!(store.search_docs_fts("xylophone", 5).unwrap().is_empty());
        assert!(store.search_symbols_fts("xylophone", 5).unwrap().is_empty());
        assert!(store.callers_by_dst("foo", 0.0, 5).unwrap().is_empty());
        assert_eq!(store.search_docs_fts("determinism", 5).unwrap().len(), 1);

        // FTS5 internal integrity check passes after cascade deletes.
        store.check_fts_integrity().unwrap();

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn legacy_cleanup_notice_emits_once_and_skips_non_diskmap_json() {
        let _g = crate::cache::test_env_lock();
        let root = temp_workspace("legacy_root");
        let fake_cache = temp_workspace("legacy_cache");

        std::fs::write(fake_cache.join("call_graph.json"), vec![b'x'; 2048]).unwrap();
        // savings.json must NOT be listed for deletion.
        std::fs::write(fake_cache.join("savings.json"), b"{}").unwrap();

        let blobs = detect_legacy_json_blobs(&fake_cache);
        assert_eq!(blobs.len(), 1);
        assert!(blobs[0].0.ends_with("call_graph.json"));

        let store = Store::open(&root).unwrap();
        let first = store.maybe_emit_legacy_cleanup_notice_in(&fake_cache);
        assert!(first.is_some());
        let msg = first.unwrap();
        assert!(msg.contains("call_graph.json"), "{msg}");
        assert!(!msg.contains("savings.json"), "{msg}");

        // Second call (even on a freshly opened handle) returns None (one-time).
        let store2 = Store::open(&root).unwrap();
        assert!(store2
            .maybe_emit_legacy_cleanup_notice_in(&fake_cache)
            .is_none());

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&fake_cache);
    }

    #[test]
    fn fts5_query_sanitizer_handles_special_chars_and_operators() {
        assert_eq!(
            sanitize_fts5_query("ns::Widget<int>", true, false).as_deref(),
            Some("\"ns\" \"Widget\" \"int\"*")
        );
        assert_eq!(
            sanitize_fts5_query("OR AND NOT -foo", false, false).as_deref(),
            Some("\"OR\" \"AND\" \"NOT\" \"foo\"")
        );
        assert_eq!(sanitize_fts5_query("   ::: ---  ", false, false), None);
    }

    #[test]
    fn watcher_updates_markdown_and_deletes_removed_files() {
        let _g = crate::cache::test_env_lock();
        let root = temp_workspace("watch_root");

        let docs = root.join("docs");
        std::fs::create_dir_all(&docs).unwrap();

        let seen = Arc::new((Mutex::new(Vec::<PathBuf>::new()), Condvar::new()));
        let seen_cb = Arc::clone(&seen);
        let _handle =
            start_watcher_with_hook(&root, Duration::from_millis(50), move |batch| {
                let (lock, cv) = &*seen_cb;
                if let Ok(mut g) = lock.lock() {
                    g.extend(batch.iter().cloned());
                    cv.notify_all();
                }
            })
            .unwrap();

        // Create a markdown file and wait for the debounced batch.
        let note = docs.join("live.md");
        std::fs::write(&note, "# Live\nZeppelin telemetry indexed live.\n").unwrap();

        let (lock, cv) = &*seen;
        let mut g = lock.lock().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !g.iter().any(|p| p.ends_with("live.md")) {
            let rem = deadline.saturating_duration_since(std::time::Instant::now());
            if rem.is_zero() {
                break;
            }
            let (ng, res) = cv.wait_timeout(g, rem).unwrap();
            g = ng;
            if res.timed_out() {
                break;
            }
        }
        assert!(
            g.iter().any(|p| p.ends_with("live.md")),
            "watcher did not observe docs/live.md: {g:?}"
        );
        g.clear();
        drop(g);

        let store = Store::open(&root).unwrap();
        let hits = store.search_docs_fts("zeppelin", 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].rel_path, "docs/live.md");

        // Remove the file and verify watcher cascades deletion out of FTS5.
        std::fs::remove_file(&note).unwrap();
        let mut g = lock.lock().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !g.iter().any(|p| p.ends_with("live.md")) {
            let rem = deadline.saturating_duration_since(std::time::Instant::now());
            if rem.is_zero() {
                break;
            }
            let (ng, res) = cv.wait_timeout(g, rem).unwrap();
            g = ng;
            if res.timed_out() {
                break;
            }
        }
        drop(g);

        assert!(
            store.search_docs_fts("zeppelin", 5).unwrap().is_empty(),
            "watcher should remove deleted file from FTS5"
        );
        store.check_fts_integrity().unwrap();

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn migrated_consumers_populate_and_warm_read_sqlite_store() {
        let _g = crate::cache::test_env_lock();
        let root = temp_workspace("consumers_root");
        let xdg = temp_workspace("consumers_xdg");
        let prev_xdg = std::env::var_os("XDG_CACHE_HOME");
        std::env::set_var("XDG_CACHE_HOME", &xdg);

        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(
            root.join("src/engine.cpp"),
            "struct EngineCore { int tick; };\n\
             void helper_step() {}\n\
             void run_frame() { helper_step(); }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("docs/guide.md"),
            "# Architecture\nVolumetric water uses deterministic fixed steps.\n",
        )
        .unwrap();

        let paths = vec!["src".to_string()];

        // 1. call_graph populates symbols + confidence-tagged edges and warms on repeat.
        let hop1 = crate::call_graph::one_hop(&root, "helper_step", &paths);
        assert_eq!(hop1.callers.len(), 1);
        assert_eq!(hop1.callers[0].0, "run_frame");

        let st = Store::open(&root).unwrap();
        let edges = st.callers_by_dst("helper_step", 0.0, 10).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].source, EDGE_SOURCE_TREESITTER);
        assert!((edges[0].confidence - CONFIDENCE_TREESITTER).abs() < 1e-9);
        assert!(edges[0].src_symbol_id.is_some());

        let hop2 = crate::call_graph::one_hop(&root, "helper_step", &paths);
        assert_eq!(hop2.callers, hop1.callers);

        // 2. symbol_resolve populates full def set (including EngineCore) into symbols.
        let sugg = crate::symbol_resolve::suggestions(&root, "EngineCor", &paths, 5);
        assert!(sugg.iter().any(|s| s == "EngineCore"), "{sugg:?}");
        assert!(!st.search_symbols_fts("EngineCore", 5).unwrap().is_empty());

        // 3. repo_map caches display lines in SQLite file_stages.
        let map_out = crate::repo_map::build(
            &root,
            &serde_json::json!({ "paths": ["src"], "mode": "symbols" }),
        )
        .unwrap();
        assert!(map_out.contains("EngineCore"), "{map_out}");
        assert!(st
            .file_stage_mtime("src/engine.cpp", STAGE_REPO_MAP)
            .unwrap()
            .is_some());

        // 4. knowledge populates doc_chunks + doc_chunks_fts.
        let kb_out = crate::knowledge::search(
            &root,
            &serde_json::json!({ "query": "volumetric water", "roots": ["docs"] }),
        )
        .unwrap();
        assert!(kb_out.contains("Volumetric water"), "{kb_out}");
        let fts_hits = st.search_docs_fts("deterministic", 5).unwrap();
        assert_eq!(fts_hits.len(), 1);
        assert_eq!(fts_hits[0].rel_path, "docs/guide.md");

        // Confirm no legacy monolithic DiskMap JSON blobs were created in this workspace's cache root.
        for legacy in LEGACY_DISKMAP_FILES {
            assert!(
                !cache::dir().join(legacy).exists(),
                "legacy DiskMap {legacy} should not be written"
            );
        }

        st.check_fts_integrity().unwrap();
        if let Some(prev) = prev_xdg {
            std::env::set_var("XDG_CACHE_HOME", prev);
        }
        let _ = std::fs::remove_dir_all(&xdg);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn edge_source_counts_and_clear_by_source_isolate_overlays() {
        let _g = crate::cache::test_env_lock();
        let root = temp_workspace("edge_sources");
        let st = Store::open(&root).unwrap();
        let fid = st.upsert_file("src/a.rs", 100, 40, "rust").unwrap();

        st.upsert_edge(&EdgeInput {
            file_id: fid,
            src_symbol_id: None,
            src_name: "caller_a",
            dst_name: "target",
            kind: "call",
            line: 5,
            site_text: "target();",
            source: EDGE_SOURCE_TREESITTER,
            confidence: CONFIDENCE_TREESITTER,
        })
        .unwrap();
        st.upsert_edge(&EdgeInput {
            file_id: fid,
            src_symbol_id: None,
            src_name: "caller_b",
            dst_name: "target",
            kind: "call",
            line: 9,
            site_text: "target();",
            source: EDGE_SOURCE_NAME_MATCH,
            confidence: CONFIDENCE_NAME_MATCH,
        })
        .unwrap();
        st.upsert_edge(&EdgeInput {
            file_id: fid,
            src_symbol_id: None,
            src_name: "mod::caller_a",
            dst_name: "mod::target",
            kind: "call",
            line: 5,
            site_text: "target();",
            source: EDGE_SOURCE_SCIP,
            confidence: CONFIDENCE_SCIP,
        })
        .unwrap();

        let counts = st.count_edges_by_source().unwrap();
        assert_eq!(counts.get(EDGE_SOURCE_TREESITTER), Some(&1));
        assert_eq!(counts.get(EDGE_SOURCE_NAME_MATCH), Some(&1));
        assert_eq!(counts.get(EDGE_SOURCE_SCIP), Some(&1));

        let scip_edges = st.all_edges_by_source(EDGE_SOURCE_SCIP).unwrap();
        assert_eq!(scip_edges.len(), 1);
        assert!((scip_edges[0].confidence - CONFIDENCE_SCIP).abs() < 1e-9);

        let cleared = st.clear_edges_by_source(EDGE_SOURCE_SCIP).unwrap();
        assert_eq!(cleared, 1);
        let after = st.count_edges_by_source().unwrap();
        assert_eq!(after.get(EDGE_SOURCE_SCIP), None);
        assert_eq!(after.get(EDGE_SOURCE_TREESITTER), Some(&1));
        assert_eq!(after.get(EDGE_SOURCE_NAME_MATCH), Some(&1));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn etag_hex_and_matches_handle_variants() {
        let a = etag_hex(&[b"outline", b"src/a.rs", b"fn foo() {}"]);
        let b = etag_hex(&[b"outline", b"src/a.rs", b"fn foo() {}"]);
        let c = etag_hex(&[b"outline", b"src/a.rs", b"fn bar() {}"]);
        assert_eq!(a.len(), 16);
        assert_eq!(a, b);
        assert_ne!(a, c);

        assert!(etag_matches(Some(&a), &a));
        assert!(etag_matches(Some(&format!("\"{a}\"")), &a));
        assert!(etag_matches(Some(&format!("W/\"{a}\"")), &a));
        assert!(etag_matches(Some(&format!("etag={a}")), &a));
        assert!(etag_matches(Some(&format!("deadbeef, \"{a}\"")), &a));
        assert!(etag_matches(Some("*"), &a));
        assert!(!etag_matches(Some(&c), &a));
        assert!(!etag_matches(None, &a));
        assert!(!etag_matches(Some("   "), &a));
        assert_eq!(unchanged_etag_line(&a), format!("unchanged (etag={a})\n"));
    }

    #[test]
    fn scoped_fts_and_symbols_in_paths_respect_roots() {
        let _env = crate::cache::test_env_lock();
        let root = temp_workspace("scoped_fts");
        let st = Store::open(&root).expect("open store");
        let f_docs = st.upsert_file("docs/phys.md", 1, 10, "markdown").unwrap();
        let f_notes = st
            .upsert_file(".wordkeep/notes/phys.md", 1, 10, "markdown")
            .unwrap();
        st.replace_doc_chunks(f_docs, &[("Physics".into(), "shared term alpha".into())])
            .unwrap();
        st.replace_doc_chunks(
            f_notes,
            &[("Notes".into(), "shared term alpha note".into())],
        )
        .unwrap();

        let only_notes = st
            .search_docs_fts_scoped("shared alpha", &[".wordkeep/notes".into()], 10)
            .unwrap();
        assert_eq!(only_notes.len(), 1);
        assert_eq!(only_notes[0].rel_path, ".wordkeep/notes/phys.md");

        let f_src = st.upsert_file("src/a.cpp", 1, 20, "cpp").unwrap();
        let f_other = st.upsert_file("tools/b.cpp", 1, 20, "cpp").unwrap();
        st.upsert_symbol(&SymbolInput {
            file_id: f_src,
            name: "widget_area",
            qual_name: "widget_area",
            kind: "function",
            line: 5,
            end_line: 10,
            signature: "int widget_area()",
            doc: "",
            exported: true,
            source: EDGE_SOURCE_TREESITTER,
        })
        .unwrap();
        st.upsert_symbol(&SymbolInput {
            file_id: f_other,
            name: "widget_area_tool",
            qual_name: "widget_area_tool",
            kind: "function",
            line: 3,
            end_line: 6,
            signature: "int widget_area_tool()",
            doc: "",
            exported: true,
            source: EDGE_SOURCE_TREESITTER,
        })
        .unwrap();

        let sym_fts = st
            .search_symbols_fts_scoped("widget area", &["src".into()], 10)
            .unwrap();
        assert_eq!(sym_fts.len(), 1);
        assert_eq!(sym_fts[0].name, "widget_area");

        let syms = st.symbols_in_paths(&["src".into()], 10).unwrap();
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].rel_path, "src/a.cpp");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn observation_links_migrate_v1_and_surface_stale_anchors_across_four_tools() {
        let _env = crate::cache::test_env_lock();
        let root = temp_workspace("obs_links_root");
        let xdg = temp_workspace("obs_links_xdg");
        let prev_xdg = std::env::var_os("XDG_CACHE_HOME");
        std::env::set_var("XDG_CACHE_HOME", &xdg);

        // 1. Verify v1 -> v2 schema migration adds target_rel + anchor_hash cleanly.
        let v1_db = root.join("legacy_v1.sqlite3");
        {
            let raw = Connection::open(&v1_db).unwrap();
            raw.execute_batch(
                "CREATE TABLE observation_links (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     obs_kind TEXT NOT NULL,
                     obs_id TEXT NOT NULL,
                     file_id INTEGER,
                     symbol_name TEXT NOT NULL DEFAULT '',
                     created_at INTEGER NOT NULL DEFAULT 0
                 );
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        }
        let migrated = Store::open_at(&root, &v1_db).unwrap();
        assert_eq!(migrated.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(migrated.observation_link_count().unwrap(), 0);
        drop(migrated);

        // 2. Build workspace with source symbol `compute_flux` + `unrelated_fn`,
        //    plus a note, a shrift idea, and a defect anchoring them.
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".wordkeep/notes")).unwrap();
        std::fs::write(
            root.join(".wordkeep/config.json"),
            r#"{"default_paths":["src"],"default_doc_roots":[".wordkeep/notes"]}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("src/flux.cpp"),
            "int compute_flux(int x) {\n    return x * 2;\n}\nint unrelated_fn() {\n    return 7;\n}\n",
        )
        .unwrap();
        std::fs::write(root.join("src/constants.h"), "#define FLUX_CAP 64\n").unwrap();

        // Note anchors symbol `compute_flux`; shrift anchors file `src/constants.h`;
        // defect anchors `src/flux.cpp:1` + `compute_flux`.
        std::fs::write(
            root.join(".wordkeep/notes/flux-design.md"),
            "# Flux Design\n\nThe core solver calls `compute_flux` every fixed step.\n",
        )
        .unwrap();
        crate::shrift::upsert(
            &root,
            &serde_json::json!({
                "title": "Tune flux cap",
                "body": "Raise limit in `src/constants.h` after bench."
            }),
        )
        .unwrap();
        crate::defects::upsert(
            &root,
            &serde_json::json!({
                "id": "flux-overflow",
                "subsystem": "sim",
                "summary": "Clamp `compute_flux` when input spikes",
                "anchors": ["src/flux.cpp:1"]
            }),
        )
        .unwrap();

        // Initial queries: index observation_links; zero stale anchors before code diverges.
        let kb0 = crate::knowledge::search(
            &root,
            &serde_json::json!({ "query": "Flux Design solver", "roots": [".wordkeep/notes"] }),
        )
        .unwrap();
        assert!(!kb0.contains("[stale-anchor"), "unexpected stale before edit:\n{kb0}");

        let sr0 = crate::shrift::review(&root, &serde_json::json!({})).unwrap();
        assert!(!sr0.contains("[stale-anchor"), "unexpected stale before edit:\n{sr0}");

        let dl0 = crate::defects::list(&root, &serde_json::json!({})).unwrap();
        assert!(!dl0.contains("[stale-anchor"), "unexpected stale before edit:\n{dl0}");

        // 3. Edit ONLY `unrelated_fn` in src/flux.cpp:
        //    - Note (`compute_flux` symbol anchor) stays FRESH because `compute_flux` body/sig is unchanged!
        //    - Defect (`src/flux.cpp` file anchor) becomes STALE because file bytes changed!
        std::thread::sleep(Duration::from_millis(15));
        std::fs::write(
            root.join("src/flux.cpp"),
            "int compute_flux(int x) {\n    return x * 2;\n}\nint unrelated_fn() {\n    return 99;\n}\n",
        )
        .unwrap();
        let kb_unrel = crate::knowledge::search(
            &root,
            &serde_json::json!({ "query": "Flux Design solver", "include_defects": false, "roots": [".wordkeep/notes"] }),
        )
        .unwrap();
        assert!(
            !kb_unrel.contains("[stale-anchor"),
            "unrelated fn edit should not mark compute_flux note stale:\n{kb_unrel}"
        );

        // 4. Now edit `compute_flux` body AND `src/constants.h`:
        //    ALL FOUR surfaces (`knowledge_search`, `shrift_review`, `defect_list`, `index_health`)
        //    must emit `[stale-anchor`!
        std::thread::sleep(Duration::from_millis(15));
        std::fs::write(
            root.join("src/flux.cpp"),
            "int compute_flux(int x) {\n    return x * 4 + 1;\n}\nint unrelated_fn() {\n    return 99;\n}\n",
        )
        .unwrap();
        std::fs::write(root.join("src/constants.h"), "#define FLUX_CAP 128\n").unwrap();

        let kb1 = crate::knowledge::search(
            &root,
            &serde_json::json!({ "query": "Flux Design solver", "roots": [".wordkeep/notes"] }),
        )
        .unwrap();
        assert!(
            kb1.contains("[stale-anchor: compute_flux]"),
            "knowledge_search must surface [stale-anchor] on note:\n{kb1}"
        );

        let sr1 = crate::shrift::review(&root, &serde_json::json!({})).unwrap();
        assert!(
            sr1.contains("[stale-anchor: src/constants.h]"),
            "shrift_review must surface [stale-anchor] on idea:\n{sr1}"
        );

        let dl1 = crate::defects::list(&root, &serde_json::json!({})).unwrap();
        assert!(
            dl1.contains("[stale-anchor:"),
            "defect_list must surface [stale-anchor] on defect:\n{dl1}"
        );

        let ih1 = crate::index_stale::health(&root, &serde_json::json!({})).unwrap();
        assert!(
            ih1.contains("[stale-anchor]"),
            "index_health must surface [stale-anchor]:\n{ih1}"
        );

        match prev_xdg {
            Some(v) => std::env::set_var("XDG_CACHE_HOME", v),
            None => std::env::remove_var("XDG_CACHE_HOME"),
        }
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&xdg);
    }
}
