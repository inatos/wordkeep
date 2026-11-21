use crate::config::{ResolvedSearchBackend, WikiConfig};
use crate::indexer::{
    collect_docs, id_slug, is_markdown_path, kind_for_path, metadata_mtime_ns, sha256_hex,
    sqlite_db_path, ChunkDocument,
};
use crate::meili::MeiliClient;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use wordkeep_knowledge::{parse_markdown, slugify};

const STAGE_DOC_CHUNKS: &str = "doc_chunks_v2";
const STAGE_WIKI_META: &str = "wiki_meta";

#[derive(Clone, Debug, Default)]
pub(crate) struct SearchParams {
    pub q: String,
    pub kind: Option<String>,
    pub root: Option<String>,
    pub tag: Option<String>,
    pub limit: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct WikiSectionMeta {
    anchor: String,
    heading_path: Vec<String>,
    heading_level: u8,
    start_line: usize,
    end_line: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct WikiFileMeta {
    tags: Vec<String>,
    sections: Vec<WikiSectionMeta>,
}

#[derive(Clone)]
pub(crate) enum SearchBackend {
    Sqlite(SqliteSearchBackend),
    Meili(MeiliClient),
}

impl SearchBackend {
    pub fn from_config(root: &Path, config: &WikiConfig) -> Result<Self, String> {
        match config.resolved_search_backend() {
            ResolvedSearchBackend::Sqlite => Ok(Self::Sqlite(SqliteSearchBackend::new(root))),
            ResolvedSearchBackend::Meili => Ok(Self::Meili(MeiliClient::new(
                config.meili_url.clone(),
                config.meili_key.clone(),
                config.index_uid.clone(),
            )?)),
        }
    }

    pub fn backend_name(&self) -> &'static str {
        match self {
            Self::Sqlite(_) => "sqlite",
            Self::Meili(_) => "meili",
        }
    }

    pub async fn ensure_index(&self, root: &Path, config: &WikiConfig) -> Result<(), String> {
        match self {
            Self::Sqlite(sqlite) => sqlite.ensure_index(root, config),
            Self::Meili(meili) => meili.ensure_index().await,
        }
    }

    pub async fn upsert_file_chunks(
        &self,
        rel_path: &str,
        mtime_ns: u64,
        size_bytes: u64,
        tags: &[String],
        old_chunk_ids: &[String],
        chunks: &[ChunkDocument],
    ) -> Result<(), String> {
        match self {
            Self::Sqlite(sqlite) => {
                sqlite.upsert_file_chunks(rel_path, mtime_ns, size_bytes, tags, chunks)
            }
            Self::Meili(meili) => {
                if !old_chunk_ids.is_empty() {
                    meili.delete_documents(old_chunk_ids).await?;
                }
                meili.upsert_documents(chunks).await
            }
        }
    }

    pub async fn delete_file_chunks(
        &self,
        rel_path: &str,
        chunk_ids: &[String],
    ) -> Result<(), String> {
        match self {
            Self::Sqlite(sqlite) => sqlite.delete_file(rel_path),
            Self::Meili(meili) => meili.delete_documents(chunk_ids).await,
        }
    }

    pub async fn search(
        &self,
        root: &Path,
        config: &WikiConfig,
        params: &SearchParams,
    ) -> Result<Value, String> {
        match self {
            Self::Sqlite(sqlite) => sqlite.search(root, config, params),
            Self::Meili(meili) => {
                let mut filters = Vec::new();
                if let Some(kind) = params.kind.as_deref().filter(|s| !s.trim().is_empty()) {
                    filters.push(format!("kind = \"{}\"", filter_value(kind)));
                }
                if let Some(r) = params.root.as_deref().filter(|s| !s.trim().is_empty()) {
                    filters.push(format!("root = \"{}\"", filter_value(r)));
                }
                if let Some(tag) = params.tag.as_deref().filter(|s| !s.trim().is_empty()) {
                    filters.push(format!("tags = \"{}\"", filter_value(tag)));
                }
                let mut request = json!({
                    "q": params.q,
                    "limit": params.limit.clamp(1, 100),
                    "attributesToHighlight": ["content", "heading", "body"],
                    "highlightPreTag": "<em>",
                    "highlightPostTag": "</em>",
                    "attributesToCrop": ["content", "body"],
                    "cropLength": 40
                });
                if !filters.is_empty() {
                    request["filter"] = Value::String(filters.join(" AND "));
                }
                let mut resp = meili.search(request).await?;
                if let Some(obj) = resp.as_object_mut() {
                    obj.entry("backend".to_string())
                        .or_insert_with(|| json!("meili"));
                }
                Ok(resp)
            }
        }
    }

    pub async fn health(&self) -> Result<Value, String> {
        match self {
            Self::Sqlite(sqlite) => sqlite.health(),
            Self::Meili(meili) => meili.health().await,
        }
    }

    pub async fn index_stats(&self) -> Result<Value, String> {
        match self {
            Self::Sqlite(sqlite) => sqlite.index_stats(),
            Self::Meili(meili) => meili.index_stats().await,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SqliteSearchBackend {
    db_path: PathBuf,
}

impl SqliteSearchBackend {
    pub fn new(root: &Path) -> Self {
        Self {
            db_path: sqlite_db_path(root),
        }
    }

    #[cfg(test)]
    pub fn new_at(db_path: PathBuf) -> Self {
        Self { db_path }
    }

    fn open_conn(&self) -> Result<Connection, String> {
        if let Some(parent) = self.db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        let conn = Connection::open(&self.db_path)
            .map_err(|e| format!("open sqlite {}: {e}", self.db_path.display()))?;
        conn.execute_batch(
            "PRAGMA busy_timeout = 5000;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA recursive_triggers = ON;
             PRAGMA temp_store = MEMORY;",
        )
        .map_err(|e| format!("sqlite pragmas {}: {e}", self.db_path.display()))?;

        // Ensure shared doc tables exist without advancing PRAGMA user_version so
        // `wordkeep` MCP's Store::init_schema still runs its full schema migration cleanly.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS files (
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
             END;",
        )
        .map_err(|e| format!("sqlite init doc tables {}: {e}", self.db_path.display()))?;
        Ok(conn)
    }

    pub fn ensure_index(&self, root: &Path, config: &WikiConfig) -> Result<(), String> {
        let conn = self.open_conn()?;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM doc_chunks", [], |row| row.get(0))
            .map_err(|e| format!("count doc_chunks: {e}"))?;
        if count > 0 {
            return Ok(());
        }
        drop(conn);
        // Cold store: populate doc_chunks from configured doc_roots so desktop search
        // works immediately even before wordkeep MCP has indexed the workspace.
        let docs = collect_docs(root, config)?;
        for doc in docs.values() {
            let Ok(meta) = std::fs::metadata(&doc.absolute) else {
                continue;
            };
            let mtime_ns = metadata_mtime_ns(&meta);
            let Ok(source) = std::fs::read_to_string(&doc.absolute) else {
                continue;
            };
            let content_hash = sha256_hex(source.as_bytes());
            let parsed = parse_markdown(&doc.relative, &source);
            let kind = kind_for_path(&doc.relative);
            let updated_at = now_secs();
            let mut chunks = Vec::with_capacity(parsed.sections.len());
            for section in parsed.sections {
                let id = id_slug(&format!("{}#{}", doc.relative, section.anchor));
                let content = if section.body.is_empty() {
                    section.heading.clone()
                } else {
                    format!("{}\n{}", section.heading, section.body)
                };
                chunks.push(ChunkDocument {
                    id,
                    path: doc.relative.clone(),
                    heading: section.heading,
                    heading_path: section.heading_path,
                    heading_level: section.heading_level,
                    anchor: section.anchor,
                    body: section.body,
                    content,
                    kind: kind.clone(),
                    root: doc.root_label.clone(),
                    tags: parsed.tags.clone(),
                    start_line: section.start_line,
                    end_line: section.end_line,
                    mtime_ns,
                    content_hash: content_hash.clone(),
                    updated_at,
                });
            }
            self.upsert_file_chunks(&doc.relative, mtime_ns, meta.len(), &parsed.tags, &chunks)?;
        }
        Ok(())
    }

    pub fn upsert_file_chunks(
        &self,
        rel_path: &str,
        mtime_ns: u64,
        size_bytes: u64,
        tags: &[String],
        chunks: &[ChunkDocument],
    ) -> Result<(), String> {
        let conn = self.open_conn()?;
        conn.execute_batch("BEGIN IMMEDIATE;")
            .map_err(|e| format!("sqlite BEGIN IMMEDIATE: {e}"))?;

        let result = (|| -> Result<(), String> {
            let now = now_secs() as i64;
            let mt = mtime_ns as i64;
            let sz = size_bytes as i64;

            let existing: Option<i64> = conn
                .query_row(
                    "SELECT id FROM files WHERE rel_path = ?1",
                    params![rel_path],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| format!("select file {rel_path}: {e}"))?;

            let file_id = if let Some(id) = existing {
                conn.execute(
                    "UPDATE files
                     SET mtime_ns = ?2, size_bytes = ?3, lang = 'markdown', indexed_at = ?4
                     WHERE id = ?1",
                    params![id, mt, sz, now],
                )
                .map_err(|e| format!("update file {rel_path}: {e}"))?;
                conn.execute("DELETE FROM doc_chunks WHERE file_id = ?1", params![id])
                    .map_err(|e| format!("clear doc_chunks {rel_path}: {e}"))?;
                id
            } else {
                conn.query_row(
                    "INSERT INTO files (rel_path, mtime_ns, size_bytes, lang, indexed_at)
                     VALUES (?1, ?2, ?3, 'markdown', ?4)
                     RETURNING id",
                    params![rel_path, mt, sz, now],
                    |row| row.get(0),
                )
                .map_err(|e| format!("insert file {rel_path}: {e}"))?
            };

            let mut sections_meta = Vec::with_capacity(chunks.len());
            for (idx, chunk) in chunks.iter().enumerate() {
                let sha = sha256_hex(format!("{}\n{}", chunk.heading, chunk.body).as_bytes());
                conn.execute(
                    "INSERT INTO doc_chunks (file_id, chunk_idx, heading, body, sha256)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![file_id, idx as i64, chunk.heading, chunk.body, sha],
                )
                .map_err(|e| format!("insert doc_chunk {rel_path}#{idx}: {e}"))?;
                sections_meta.push(WikiSectionMeta {
                    anchor: chunk.anchor.clone(),
                    heading_path: chunk.heading_path.clone(),
                    heading_level: chunk.heading_level,
                    start_line: chunk.start_line,
                    end_line: chunk.end_line,
                });
            }

            let meta_payload = serde_json::to_string(&WikiFileMeta {
                tags: tags.to_vec(),
                sections: sections_meta,
            })
            .unwrap_or_else(|_| "{}".to_string());

            conn.execute(
                "INSERT INTO file_stages (file_id, stage, mtime_ns, payload)
                 VALUES (?1, ?2, ?3, NULL)
                 ON CONFLICT(file_id, stage) DO UPDATE SET mtime_ns = excluded.mtime_ns, payload = NULL",
                params![file_id, STAGE_DOC_CHUNKS, mt],
            )
            .map_err(|e| format!("set stage {STAGE_DOC_CHUNKS}: {e}"))?;

            conn.execute(
                "INSERT INTO file_stages (file_id, stage, mtime_ns, payload)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(file_id, stage) DO UPDATE SET mtime_ns = excluded.mtime_ns, payload = excluded.payload",
                params![file_id, STAGE_WIKI_META, mt, meta_payload],
            )
            .map_err(|e| format!("set stage {STAGE_WIKI_META}: {e}"))?;

            Ok(())
        })();

        match result {
            Ok(()) => {
                conn.execute_batch("COMMIT;")
                    .map_err(|e| format!("sqlite COMMIT: {e}"))?;
                Ok(())
            }
            Err(err) => {
                let _ = conn.execute_batch("ROLLBACK;");
                Err(err)
            }
        }
    }

    pub fn delete_file(&self, rel_path: &str) -> Result<(), String> {
        let conn = self.open_conn()?;
        conn.execute("DELETE FROM files WHERE rel_path = ?1", params![rel_path])
            .map_err(|e| format!("delete file {rel_path}: {e}"))?;
        Ok(())
    }

    pub fn counts(&self) -> Result<(usize, usize), String> {
        let conn = self.open_conn()?;
        let chunks: i64 = conn
            .query_row("SELECT COUNT(*) FROM doc_chunks", [], |row| row.get(0))
            .map_err(|e| format!("count doc_chunks: {e}"))?;
        let files: i64 = conn
            .query_row(
                "SELECT COUNT(DISTINCT file_id) FROM doc_chunks",
                [],
                |row| row.get(0),
            )
            .map_err(|e| format!("count doc files: {e}"))?;
        Ok((files as usize, chunks as usize))
    }

    pub fn health(&self) -> Result<Value, String> {
        let conn = self.open_conn()?;
        let _: i64 = conn
            .query_row("SELECT COUNT(*) FROM doc_chunks", [], |row| row.get(0))
            .map_err(|e| format!("sqlite health query: {e}"))?;
        Ok(json!({
            "status": "available",
            "backend": "sqlite",
            "db_path": self.db_path
        }))
    }

    pub fn index_stats(&self) -> Result<Value, String> {
        let (files, chunks) = self.counts()?;
        Ok(json!({
            "backend": "sqlite",
            "numberOfDocuments": chunks,
            "files": files,
            "isIndexing": false,
            "db_path": self.db_path
        }))
    }

    pub fn search(
        &self,
        root: &Path,
        config: &WikiConfig,
        params: &SearchParams,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let conn = self.open_conn()?;
        let limit = params.limit.clamp(1, 100);
        let q_trimmed = params.q.trim();
        let query_tokens = extract_tokens(q_trimmed);

        let raw_rows = if q_trimmed.is_empty() {
            query_recent_chunks(&conn, (limit * 8).max(200))?
        } else {
            let mut rows = Vec::new();
            if let Some(and_prefix) = build_fts5_query(&query_tokens, true, false) {
                rows = query_fts_chunks(&conn, &and_prefix, (limit * 8).max(200))?;
            }
            if rows.is_empty() && query_tokens.len() > 1 {
                if let Some(or_prefix) = build_fts5_query(&query_tokens, true, true) {
                    rows = query_fts_chunks(&conn, &or_prefix, (limit * 8).max(200))?;
                }
            }
            rows
        };

        let want_kind = params
            .kind
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let want_root = params
            .root
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let want_tag = params
            .tag
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let mut disk_meta_cache: HashMap<i64, Option<WikiFileMeta>> = HashMap::new();
        let mut hits = Vec::new();
        let mut total_matched = 0usize;

        for row in raw_rows {
            if !is_markdown_path(Path::new(&row.rel_path)) {
                continue;
            }
            let kind = kind_for_path(&row.rel_path);
            if let Some(wk) = want_kind {
                if kind != wk {
                    continue;
                }
            }
            let root_label = config.root_label_for(&row.rel_path).unwrap_or_default();
            if let Some(wr) = want_root {
                if root_label != wr {
                    continue;
                }
            }

            let file_meta = if let Some(payload) = row.meta_payload.as_deref() {
                serde_json::from_str::<WikiFileMeta>(payload).ok()
            } else {
                disk_meta_cache
                    .entry(row.file_id)
                    .or_insert_with(|| load_disk_meta(root, &row.rel_path))
                    .clone()
            };

            let tags = file_meta
                .as_ref()
                .map(|m| m.tags.clone())
                .unwrap_or_default();
            if let Some(wt) = want_tag {
                if !tags.iter().any(|t| t == wt) {
                    continue;
                }
            }

            total_matched += 1;
            if hits.len() >= limit {
                continue;
            }

            let sec = file_meta
                .as_ref()
                .and_then(|m| m.sections.get(row.chunk_idx));
            let anchor = sec
                .map(|s| s.anchor.clone())
                .filter(|a| !a.is_empty())
                .unwrap_or_else(|| slugify(&row.heading));
            let heading_path = sec
                .map(|s| s.heading_path.clone())
                .unwrap_or_else(|| vec![row.heading.clone()]);
            let heading_level = sec.map(|s| s.heading_level).unwrap_or(1);
            let start_line = sec.map(|s| s.start_line).unwrap_or(1);
            let end_line = sec.map(|s| s.end_line).unwrap_or(start_line);

            let id = id_slug(&format!("{}#{}", row.rel_path, anchor));
            let content = if row.body.is_empty() {
                row.heading.clone()
            } else {
                format!("{}\n{}", row.heading, row.body)
            };

            let fmt_heading = highlight_escaped(&row.heading, &query_tokens, 160);
            let fmt_body = highlight_escaped(&row.body, &query_tokens, 260);
            let fmt_content = if row.body.is_empty() {
                fmt_heading.clone()
            } else {
                fmt_body.clone()
            };

            hits.push(json!({
                "id": id,
                "path": row.rel_path,
                "heading": row.heading,
                "heading_path": heading_path,
                "heading_level": heading_level,
                "anchor": anchor,
                "body": row.body,
                "content": content,
                "kind": kind,
                "root": root_label,
                "tags": tags,
                "start_line": start_line,
                "end_line": end_line,
                "mtime_ns": row.mtime_ns,
                "_formatted": {
                    "heading": fmt_heading,
                    "body": fmt_body,
                    "content": fmt_content
                }
            }));
        }

        let processing_ms = started.elapsed().as_millis() as u64;
        Ok(json!({
            "hits": hits,
            "query": params.q,
            "processingTimeMs": processing_ms,
            "limit": limit,
            "estimatedTotalHits": total_matched,
            "backend": "sqlite"
        }))
    }
}

struct RawChunkRow {
    file_id: i64,
    rel_path: String,
    chunk_idx: usize,
    heading: String,
    body: String,
    mtime_ns: u64,
    meta_payload: Option<String>,
}

fn query_fts_chunks(
    conn: &Connection,
    match_expr: &str,
    limit: usize,
) -> Result<Vec<RawChunkRow>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT c.file_id, f.rel_path, c.chunk_idx, c.heading, c.body, f.mtime_ns, s.payload
             FROM doc_chunks_fts
             JOIN doc_chunks c ON c.id = doc_chunks_fts.rowid
             JOIN files f      ON f.id = c.file_id
             LEFT JOIN file_stages s ON s.file_id = c.file_id AND s.stage = 'wiki_meta'
             WHERE doc_chunks_fts MATCH ?1
             ORDER BY bm25(doc_chunks_fts, 2.0, 1.0) ASC, c.id ASC
             LIMIT ?2",
        )
        .map_err(|e| format!("sqlite prepare fts search: {e}"))?;
    let rows = stmt
        .query_map(params![match_expr, limit as i64], |row| {
            Ok(RawChunkRow {
                file_id: row.get(0)?,
                rel_path: row.get(1)?,
                chunk_idx: row.get::<_, i64>(2)?.max(0) as usize,
                heading: row.get(3)?,
                body: row.get(4)?,
                mtime_ns: row.get::<_, i64>(5)?.max(0) as u64,
                meta_payload: row.get(6)?,
            })
        })
        .map_err(|e| format!("sqlite run fts search: {e}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("sqlite collect fts search: {e}"))
}

fn query_recent_chunks(conn: &Connection, limit: usize) -> Result<Vec<RawChunkRow>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT c.file_id, f.rel_path, c.chunk_idx, c.heading, c.body, f.mtime_ns, s.payload
             FROM doc_chunks c
             JOIN files f ON f.id = c.file_id
             LEFT JOIN file_stages s ON s.file_id = c.file_id AND s.stage = 'wiki_meta'
             ORDER BY f.mtime_ns DESC, c.id ASC
             LIMIT ?1",
        )
        .map_err(|e| format!("sqlite prepare recent chunks: {e}"))?;
    let rows = stmt
        .query_map(params![limit as i64], |row| {
            Ok(RawChunkRow {
                file_id: row.get(0)?,
                rel_path: row.get(1)?,
                chunk_idx: row.get::<_, i64>(2)?.max(0) as usize,
                heading: row.get(3)?,
                body: row.get(4)?,
                mtime_ns: row.get::<_, i64>(5)?.max(0) as u64,
                meta_payload: row.get(6)?,
            })
        })
        .map_err(|e| format!("sqlite run recent chunks: {e}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("sqlite collect recent chunks: {e}"))
}

fn load_disk_meta(root: &Path, rel_path: &str) -> Option<WikiFileMeta> {
    let src = std::fs::read_to_string(root.join(rel_path)).ok()?;
    let parsed = parse_markdown(rel_path, &src);
    let sections = parsed
        .sections
        .into_iter()
        .map(|s| WikiSectionMeta {
            anchor: s.anchor,
            heading_path: s.heading_path,
            heading_level: s.heading_level,
            start_line: s.start_line,
            end_line: s.end_line,
        })
        .collect();
    Some(WikiFileMeta {
        tags: parsed.tags,
        sections,
    })
}

fn extract_tokens(raw: &str) -> Vec<String> {
    raw.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .take(24)
        .map(|t| t.to_ascii_lowercase())
        .collect()
}

fn build_fts5_query(tokens: &[String], prefix_last: bool, join_or: bool) -> Option<String> {
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

/// Crop `text` around the first matching token, HTML-escape all source characters,
/// and wrap matched alphanumeric tokens in `<em>...</em>` for safe `{@html}` rendering.
pub(crate) fn highlight_escaped(text: &str, tokens: &[String], max_chars: usize) -> String {
    if text.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = text.chars().collect();
    let lower_text = text.to_ascii_lowercase();
    let first_match_char = tokens
        .iter()
        .filter(|t| !t.is_empty())
        .filter_map(|tok| lower_text.find(tok.as_str()))
        .min()
        .map(|byte_idx| text[..byte_idx].chars().count())
        .unwrap_or(0);

    let start = if chars.len() <= max_chars {
        0
    } else {
        first_match_char.saturating_sub(max_chars / 4).min(chars.len() - max_chars)
    };
    let end = (start + max_chars).min(chars.len());
    let slice: String = chars[start..end].iter().collect();

    let mut out = String::with_capacity(slice.len() + 32);
    if start > 0 {
        out.push('…');
    }

    let mut idx = 0usize;
    while idx < slice.len() {
        let ch = slice[idx..].chars().next().unwrap();
        if ch.is_alphanumeric() {
            let word_end = idx
                + slice[idx..]
                    .chars()
                    .take_while(|c| c.is_alphanumeric())
                    .map(char::len_utf8)
                    .sum::<usize>();
            let word = &slice[idx..word_end];
            let word_lower = word.to_ascii_lowercase();
            let matched = tokens.iter().enumerate().any(|(ti, tok)| {
                if ti + 1 == tokens.len() && tok.len() >= 2 {
                    word_lower.starts_with(tok)
                } else {
                    word_lower == *tok
                }
            });
            if matched {
                out.push_str("<em>");
                push_html_escaped(&mut out, word);
                out.push_str("</em>");
            } else {
                push_html_escaped(&mut out, word);
            }
            idx = word_end;
        } else {
            push_html_char(&mut out, ch);
            idx += ch.len_utf8();
        }
    }

    if end < chars.len() {
        out.push('…');
    }
    out
}

fn push_html_escaped(out: &mut String, s: &str) {
    for ch in s.chars() {
        push_html_char(out, ch);
    }
}

fn push_html_char(out: &mut String, ch: char) {
    match ch {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        '"' => out.push_str("&quot;"),
        '\'' => out.push_str("&#39;"),
        c => out.push(c),
    }
}

fn filter_value(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SearchBackendMode;

    #[test]
    fn sqlite_reads_shared_mcp_doc_chunks_without_wiki_reindex_and_escapes_html() {
        let dir = std::env::temp_dir().join(format!(
            "wk_wiki_sqlite_shared_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::create_dir_all(dir.join(".wordkeep/notes")).unwrap();
        std::fs::write(
            dir.join("docs/arch.md"),
            "---\ntags: [architecture, ecs]\n---\n\n# Query Cache\n\nUse cached queries instead of <script>alert(1)</script> ad-hoc scans.\n",
        )
        .unwrap();
        std::fs::write(
            dir.join(".wordkeep/notes/perf.md"),
            "# Hitch Note\n\nCached queries avoid table churn.\n",
        )
        .unwrap();

        let db_path = dir.join("shared_index.sqlite3");
        let backend = SqliteSearchBackend::new_at(db_path.clone());

        // Simulate wordkeep MCP populating files + doc_chunks directly (no wiki_meta stage!).
        {
            let conn = backend.open_conn().unwrap();
            conn.execute(
                "INSERT INTO files (rel_path, mtime_ns, size_bytes, lang, indexed_at)
                 VALUES ('docs/arch.md', 100, 120, 'markdown', 1),
                        ('.wordkeep/notes/perf.md', 200, 80, 'markdown', 1)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO doc_chunks (file_id, chunk_idx, heading, body, sha256)
                 VALUES (1, 0, 'Query Cache', 'Use cached queries instead of <script>alert(1)</script> ad-hoc scans.', 'h1'),
                        (2, 0, 'Hitch Note', 'Cached queries avoid table churn.', 'h2')",
                [],
            )
            .unwrap();
        }

        let cfg = WikiConfig {
            doc_roots: vec!["docs".into(), ".wordkeep/notes".into()],
            search_backend: SearchBackendMode::Sqlite,
            meili_configured: false,
            meili_url: String::new(),
            meili_key: None,
            index_uid: "wiki_chunks".into(),
            bind: "127.0.0.1:8787".into(),
            retain_search_queries: false,
            project_name: "Demo".into(),
            read_only: false,
        };

        let res = backend
            .search(
                &dir,
                &cfg,
                &SearchParams {
                    q: "cached queries".into(),
                    kind: None,
                    root: None,
                    tag: None,
                    limit: 10,
                },
            )
            .unwrap();
        assert_eq!(res["backend"], "sqlite");
        assert_eq!(res["estimatedTotalHits"], 2);
        let hits = res["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 2);

        // Verify XSS escaping + <em> highlight in _formatted.
        let arch_hit = hits.iter().find(|h| h["path"] == "docs/arch.md").unwrap();
        let formatted_body = arch_hit["_formatted"]["body"].as_str().unwrap();
        assert!(formatted_body.contains("<em>cached</em>"));
        assert!(formatted_body.contains("&lt;script&gt;"));
        assert!(!formatted_body.contains("<script>"));
        assert_eq!(arch_hit["kind"], "doc");
        assert_eq!(arch_hit["root"], "docs");

        // Filter by tag resolved from frontmatter on disk even without wiki_meta stage.
        let tag_res = backend
            .search(
                &dir,
                &cfg,
                &SearchParams {
                    q: "cached".into(),
                    kind: None,
                    root: None,
                    tag: Some("ecs".into()),
                    limit: 10,
                },
            )
            .unwrap();
        assert_eq!(tag_res["estimatedTotalHits"], 1);
        assert_eq!(tag_res["hits"][0]["path"], "docs/arch.md");

        // Filter by kind ("note").
        let kind_res = backend
            .search(
                &dir,
                &cfg,
                &SearchParams {
                    q: "cached".into(),
                    kind: Some("note".into()),
                    root: None,
                    tag: None,
                    limit: 10,
                },
            )
            .unwrap();
        assert_eq!(kind_res["estimatedTotalHits"], 1);
        assert_eq!(kind_res["hits"][0]["path"], ".wordkeep/notes/perf.md");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cold_ensure_index_populates_sqlite_and_supports_prefix_search() {
        let dir = std::env::temp_dir().join(format!(
            "wk_wiki_sqlite_cold_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::write(
            dir.join("docs/guide.md"),
            "# Rollback Determinism\n\nDeterministic snapshots keep netcode stable.\n",
        )
        .unwrap();

        let db_path = dir.join("cold_index.sqlite3");
        let backend = SqliteSearchBackend::new_at(db_path);
        let cfg = WikiConfig {
            doc_roots: vec!["docs".into()],
            search_backend: SearchBackendMode::Auto,
            meili_configured: false,
            meili_url: String::new(),
            meili_key: None,
            index_uid: "wiki_chunks".into(),
            bind: "127.0.0.1:8787".into(),
            retain_search_queries: false,
            project_name: "Demo".into(),
            read_only: false,
        };

        backend.ensure_index(&dir, &cfg).unwrap();
        let (files, chunks) = backend.counts().unwrap();
        assert_eq!(files, 1);
        assert_eq!(chunks, 1);

        // Live-typing prefix match ("determ" -> "Determinism" / "Deterministic").
        let res = backend
            .search(
                &dir,
                &cfg,
                &SearchParams {
                    q: "rollback determ".into(),
                    limit: 10,
                    ..SearchParams::default()
                },
            )
            .unwrap();
        assert_eq!(res["estimatedTotalHits"], 1);
        assert_eq!(res["hits"][0]["heading"], "Rollback Determinism");

        let _ = std::fs::remove_dir_all(dir);
    }
}
