//! knowledge_search - BM25 lexical retrieval over project markdown.
//!
//! Indexes `.md` / `.mdc` files (chunked by heading), then ranks chunks for a query
//! with Okapi BM25. Zero dependencies, fully deterministic, no model download - a
//! strong offline baseline. With `--features embeddings`, the BM25 head is reranked
//! by a small local embedding model (see embed.rs); BM25 stays the fallback.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use crate::cache::{self, DiskMap};
use crate::{stats, walk};

#[cfg(feature = "embeddings")]
mod embed;

const DOC_EXT: [&str; 3] = ["md", "mdc", "markdown"];
/// Paths (relative to repo root) agents may write via `knowledge_upsert`.
const UPSERT_ROOTS: [&str; 3] = [".cursor/rules", "docs", ".wordkeep/notes"];
const K1: f64 = 1.5;
const B: f64 = 0.75;

/// Chunk cache keyed by absolute path; the stored mtime invalidates the entry
/// when the document changes. Backed by a JSON file under the shared cache dir,
/// so a cold spawn over a large docs tree reuses the previous run's chunking.
static CHUNK_CACHE: OnceLock<Mutex<DiskMap>> = OnceLock::new();

const STOP: [&str; 22] = [
    "the", "and", "for", "with", "that", "this", "from", "are", "was", "but", "not", "you", "your",
    "use", "using", "into", "when", "which", "have", "has", "its", "can",
];

struct Chunk {
    path: String,
    heading: String,
    body: String,
    tf: HashMap<String, u32>,
    len: u32,
}

impl Clone for Chunk {
    fn clone(&self) -> Self {
        Chunk {
            path: self.path.clone(),
            heading: self.heading.clone(),
            body: self.body.clone(),
            tf: self.tf.clone(),
            len: self.len,
        }
    }
}

pub fn search(root: &Path, args: &Value) -> Result<String, String> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if query.is_empty() {
        return Err("query is required".into());
    }
    let k = args.get("k").and_then(Value::as_u64).unwrap_or(5).max(1) as usize;
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(1200) as usize;
    let roots = crate::config::doc_roots_from_args(args)?;

    let (chunks, indexed_bytes) = index(root, &roots);
    if chunks.is_empty() {
        stats::record("knowledge_search", indexed_bytes / 4, 0);
        return Ok("knowledge_search: no documents indexed".into());
    }

    let n = chunks.len() as f64;
    let avgdl = chunks.iter().map(|c| c.len as f64).sum::<f64>() / n;
    let mut df: HashMap<&str, u32> = HashMap::new();
    for c in &chunks {
        for term in c.tf.keys() {
            *df.entry(term.as_str()).or_insert(0) += 1;
        }
    }

    let q_terms = tokenize(query);
    if q_terms.is_empty() {
        return Err("query has no searchable terms".into());
    }

    let mut scored: Vec<(f64, usize)> = Vec::new();
    for (i, c) in chunks.iter().enumerate() {
        let dl = c.len as f64;
        let mut score = 0.0;
        for qt in &q_terms {
            let f = *c.tf.get(qt).unwrap_or(&0) as f64;
            if f == 0.0 {
                continue;
            }
            let n_qi = *df.get(qt.as_str()).unwrap_or(&0) as f64;
            let idf = ((n - n_qi + 0.5) / (n_qi + 0.5) + 1.0).ln();
            score += idf * (f * (K1 + 1.0)) / (f + K1 * (1.0 - B + B * dl / avgdl));
        }
        if score > 0.0 {
            scored.push((score, i));
        }
    }
    if scored.is_empty() {
        stats::record("knowledge_search", indexed_bytes / 4, 0);
        return Ok(format!("knowledge_search: no matches for {query:?}"));
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    // Optional semantic rerank of the BM25 head (opt-in `embeddings` feature).
    #[cfg(feature = "embeddings")]
    let scored = if args
        .get("semantic")
        .and_then(Value::as_bool)
        .unwrap_or(true)
    {
        match embed::rerank(query, &chunks, scored.clone()) {
            Ok(reranked) => reranked,
            Err(e) => {
                eprintln!("[wordkeep] semantic rerank unavailable, using BM25: {e}");
                scored
            }
        }
    } else {
        scored
    };

    let shown = k.min(scored.len());
    let mut out = format!(
        "knowledge_search - query {:?}, top {} of {} matches\n",
        query,
        shown,
        scored.len()
    );
    let mut used = out.len() / 4;
    for (rank, (score, idx)) in scored.iter().take(k).enumerate() {
        let c = &chunks[*idx];
        let snippet = snippet_for(&c.body, &q_terms, 280);
        let block = format!(
            "\n[{}] {} - {}  (score {:.2})\n{}\n",
            rank + 1,
            c.path,
            c.heading,
            score,
            snippet
        );
        let block_tokens = block.len() / 4;
        if used + block_tokens > budget && rank > 0 {
            out.push_str("\n… (further matches omitted by token_budget)\n");
            break;
        }
        used += block_tokens;
        out.push_str(&block);
    }
    stats::record(
        "knowledge_search",
        indexed_bytes / 4,
        (out.len() / 4) as u64,
    );
    Ok(out)
}

/// Write or update a markdown section so the next `knowledge_search` can retrieve it.
/// Restricted to `.cursor/rules/`, `docs/`, and `.wordkeep/notes/` under the repo root.
pub fn upsert(root: &Path, args: &Value) -> Result<String, String> {
    let rel = args
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("path is required")?;
    let mode = args
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("upsert_section");

    let (heading, body) = if mode == "pitfall" {
        let symptom = args
            .get("symptom")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or("symptom is required for mode pitfall")?;
        let root_cause = args
            .get("root_cause")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or("root_cause is required for mode pitfall")?;
        let fix = args
            .get("fix")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or("fix is required for mode pitfall")?;
        let heading = args
            .get("heading")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .unwrap_or_else(|| pitfall_heading_from_symptom(symptom));
        let mut body =
            format!("**Symptom:** {symptom}\n\n**Root cause:** {root_cause}\n\n**Fix:** {fix}\n");
        if let Some(tag) = args
            .get("test_tag")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let tag = tag.trim_start_matches('[').trim_end_matches(']');
            body.push_str(&format!(
                "\n**Verify:** `{}`\n",
                crate::config::test_filter_hint(root, tag)
            ));
        }
        (heading, body)
    } else {
        let heading = args
            .get("heading")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or("heading is required")?
            .to_string();
        let body = args
            .get("body")
            .and_then(Value::as_str)
            .ok_or("body is required")?
            .trim()
            .to_string();
        if body.is_empty() {
            return Err("body is required".into());
        }
        (heading, body)
    };

    if mode != "upsert_section"
        && mode != "append_section"
        && mode != "replace_file"
        && mode != "pitfall"
    {
        return Err(format!(
            "unknown mode {mode:?}; use upsert_section, append_section, replace_file, or pitfall"
        ));
    }
    let write_mode = if mode == "pitfall" {
        "upsert_section"
    } else {
        mode
    };

    let full = resolve_upsert_path(root, rel)?;
    let existed = full.exists();
    let mut src = if existed {
        std::fs::read_to_string(&full).map_err(|e| format!("read {}: {e}", full.display()))?
    } else {
        String::new()
    };

    let action = match write_mode {
        "replace_file" => {
            src = if body.starts_with("---") || body.starts_with('#') {
                format!("{body}\n")
            } else {
                build_new_file(rel, &heading, &body, args)
            };
            "replaced file"
        }
        "append_section" => {
            if !existed {
                src = build_new_file(rel, &heading, &body, args);
                "created file"
            } else {
                src.push_str(&format!("\n\n# {heading}\n\n{body}\n"));
                "appended section"
            }
        }
        _ => {
            if !existed {
                src = build_new_file(rel, &heading, &body, args);
                "created file"
            } else if replace_section(&mut src, &heading, &body) {
                "updated section"
            } else {
                src.push_str(&format!("\n\n# {heading}\n\n{body}\n"));
                "appended section"
            }
        }
    };
    let action = if mode == "pitfall" {
        format!("pitfall {action}")
    } else {
        action.to_string()
    };

    write_atomic(&full, &src)?;
    stats::record("knowledge_upsert", (src.len() / 4) as u64, 32);

    Ok(format!(
        "knowledge_upsert - {action}: {rel} (heading {heading:?}, {} bytes)",
        src.len()
    ))
}

fn pitfall_heading_from_symptom(symptom: &str) -> String {
    let one_line = symptom.lines().next().unwrap_or(symptom).trim();
    let mut h = one_line.to_string();
    if h.len() > 72 {
        h.truncate(69);
        h.push_str("...");
    }
    h
}

fn resolve_upsert_path(root: &Path, rel: &str) -> Result<std::path::PathBuf, String> {
    let norm = crate::config::validate_rel_path(rel.trim().trim_start_matches(['/', '\\']))?;
    let allowed = UPSERT_ROOTS
        .iter()
        .any(|p| norm == *p || norm.starts_with(&format!("{p}/")));
    if !allowed {
        return Err(format!(
            "path must be under one of: {}",
            UPSERT_ROOTS.join(", ")
        ));
    }
    let ext_ok = norm
        .rsplit('.')
        .next()
        .map(|e| DOC_EXT.contains(&e))
        .unwrap_or(false);
    if !ext_ok {
        return Err("path must end with .md, .mdc, or .markdown".into());
    }
    Ok(root.join(&norm))
}

fn build_new_file(rel: &str, heading: &str, body: &str, args: &Value) -> String {
    let mut out = String::new();
    if rel.ends_with(".mdc") {
        let desc = args
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("Agent-maintained project notes")
            .trim();
        let always = args
            .get("always_apply")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        out.push_str("---\n");
        out.push_str(&format!("description: {desc}\n"));
        out.push_str(&format!("alwaysApply: {always}\n"));
        out.push('\n');
        out.push_str("---\n\n");
    }
    out.push_str(&format!("# {heading}\n\n{body}\n"));
    out
}

/// Replace the body under `heading` (first `#+ heading` match). Returns true if found.
fn replace_section(src: &mut String, heading: &str, body: &str) -> bool {
    let lines: Vec<&str> = src.lines().collect();
    let target = heading.to_lowercase();
    let mut start: Option<(usize, usize)> = None;
    for (i, line) in lines.iter().enumerate() {
        if let Some(level) = heading_level(line) {
            let text = line[level..].trim().to_lowercase();
            if text == target {
                start = Some((i, level));
                break;
            }
        }
    }
    let Some((start_idx, _level)) = start else {
        return false;
    };
    let mut end_idx = lines.len();
    for (i, line) in lines.iter().enumerate().skip(start_idx + 1) {
        if heading_level(line).is_some() {
            end_idx = i;
            break;
        }
    }
    let mut rebuilt = String::new();
    for line in &lines[..start_idx] {
        rebuilt.push_str(line);
        rebuilt.push('\n');
    }
    rebuilt.push_str(&format!("# {heading}\n\n{body}\n"));
    if end_idx < lines.len() {
        if !rebuilt.ends_with('\n') {
            rebuilt.push('\n');
        }
        for line in &lines[end_idx..] {
            rebuilt.push_str(line);
            rebuilt.push('\n');
        }
    }
    *src = rebuilt.trim_end().to_string();
    if !src.ends_with('\n') {
        src.push('\n');
    }
    true
}

fn heading_level(line: &str) -> Option<usize> {
    if !line.starts_with('#') {
        return None;
    }
    let hashes = line.chars().take_while(|&c| c == '#').count();
    if hashes == 0 || hashes >= line.len() {
        return None;
    }
    if !line
        .as_bytes()
        .get(hashes)
        .is_some_and(|b| b.is_ascii_whitespace())
    {
        return None;
    }
    Some(hashes)
}

fn write_atomic(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let tmp = path.with_extension("wordkeep-tmp");
    std::fs::write(&tmp, contents).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename {}: {e}", path.display()))
}

/// Index the configured roots into chunks, returning the chunks and the total
/// bytes of document text scanned (the baseline an agent would otherwise read).
/// Skips pruned directories and persists newly chunked files to disk.
fn index(root: &Path, roots: &[String]) -> (Vec<Chunk>, u64) {
    let mut chunks = Vec::new();
    let mut bytes = 0u64;
    let prune = walk::prune_set(root);
    for r in roots {
        let base = root.join(r);
        if base.is_file() {
            let rel = base
                .strip_prefix(root)
                .unwrap_or(&base)
                .to_string_lossy()
                .replace('\\', "/");
            chunks.extend(cached_chunks(&base, &rel, &mut bytes));
            continue;
        }
        for entry in walk::files(&base, &prune) {
            let path = entry.path();
            let ok = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| DOC_EXT.contains(&e))
                .unwrap_or(false);
            if !ok {
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            chunks.extend(cached_chunks(path, &rel, &mut bytes));
        }
    }
    if let Some(c) = CHUNK_CACHE.get() {
        if let Ok(mut store) = c.lock() {
            store.save();
        }
    }
    (chunks, bytes)
}

/// Return a document's chunks, reusing the cached chunking when the on-disk mtime
/// is unchanged. Adds the file size to `bytes` (the read-it-yourself baseline).
/// Falls back to a fresh read+`chunk_file` on cache miss or stat error.
fn cached_chunks(path: &Path, rel: &str, bytes: &mut u64) -> Vec<Chunk> {
    let key = path.to_string_lossy();
    let (mtime_ns, len) = match std::fs::metadata(path) {
        Ok(m) => (cache::mtime_ns(&m), m.len()),
        Err(_) => (0, 0),
    };
    *bytes += len;
    let cell = CHUNK_CACHE.get_or_init(|| Mutex::new(DiskMap::load("knowledge-chunks.json")));
    if let Ok(store) = cell.lock() {
        if let Some(v) = store.get(key.as_ref(), mtime_ns) {
            if let Some(arr) = v.as_array() {
                return arr.iter().filter_map(chunk_from_value).collect();
            }
        }
    }
    let Ok(src) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut chunks = Vec::new();
    chunk_file(rel, &src, &mut chunks);
    if let Ok(mut store) = cell.lock() {
        let payload = Value::Array(
            chunks
                .iter()
                .map(|c| json!({ "path": c.path, "heading": c.heading, "body": c.body }))
                .collect(),
        );
        store.put(key.as_ref(), mtime_ns, payload);
    }
    chunks
}

/// Rebuild a `Chunk` from its cached `{path, heading, body}`, recomputing term
/// frequencies/length so BM25 scoring is identical to a fresh parse.
fn chunk_from_value(v: &Value) -> Option<Chunk> {
    let path = v.get("path").and_then(Value::as_str)?.to_string();
    let heading = v.get("heading").and_then(Value::as_str)?.to_string();
    let body = v.get("body").and_then(Value::as_str)?.to_string();
    let text = format!("{heading}\n{body}");
    let tf = term_freqs(&text);
    let len: u32 = tf.values().sum();
    if len == 0 {
        return None;
    }
    Some(Chunk {
        path,
        heading,
        body,
        tf,
        len,
    })
}

/// Split a markdown file into heading-delimited chunks, skipping any leading
/// YAML frontmatter (`---` … `---`) used by `.mdc` rule files.
fn chunk_file(path: &str, src: &str, out: &mut Vec<Chunk>) {
    let mut heading = String::from("(intro)");
    let mut buf = String::new();
    let mut in_frontmatter = false;
    let mut saw_nonempty = false;

    for line in src.lines() {
        if !saw_nonempty {
            if line.trim().is_empty() {
                continue;
            }
            saw_nonempty = true;
            if line.trim() == "---" {
                in_frontmatter = true;
                continue;
            }
        }
        if in_frontmatter {
            if line.trim() == "---" {
                in_frontmatter = false;
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix('#') {
            push_chunk(out, path, &heading, &buf);
            buf.clear();
            let h = rest.trim_start_matches('#').trim();
            heading = if h.is_empty() {
                "(section)".into()
            } else {
                h.to_string()
            };
        } else {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    push_chunk(out, path, &heading, &buf);
}

fn push_chunk(out: &mut Vec<Chunk>, path: &str, heading: &str, buf: &str) {
    let body = buf.trim();
    if body.is_empty() {
        return;
    }
    let text = format!("{heading}\n{body}");
    let tf = term_freqs(&text);
    let len: u32 = tf.values().sum();
    if len == 0 {
        return;
    }
    out.push(Chunk {
        path: path.to_string(),
        heading: heading.to_string(),
        body: body.to_string(),
        tf,
        len,
    });
}

fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 2)
        .map(str::to_lowercase)
        .filter(|t| !STOP.contains(&t.as_str()))
        .collect()
}

fn term_freqs(text: &str) -> HashMap<String, u32> {
    let mut m = HashMap::new();
    for t in tokenize(text) {
        *m.entry(t).or_insert(0) += 1;
    }
    m
}

/// First body line containing any query term, trimmed to `max_chars`.
fn snippet_for(body: &str, q_terms: &[String], max_chars: usize) -> String {
    let lower = body.to_lowercase();
    let mut pos: Option<usize> = None;
    for qt in q_terms {
        if let Some(p) = lower.find(qt.as_str()) {
            pos = Some(pos.map_or(p, |b| b.min(p)));
        }
    }
    let start = pos.unwrap_or(0);
    let line_start = body[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let slice = &body[line_start..];
    let snippet: String = slice.chars().take(max_chars).collect();
    let snippet = snippet.replace('\n', " ").trim().to_string();
    if slice.chars().count() > max_chars {
        format!("{snippet} …")
    } else {
        snippet
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tokenize_drops_short_and_stopwords() {
        let toks = tokenize("The Physics step USES Rapier a b");
        assert!(toks.contains(&"physics".to_string()));
        assert!(toks.contains(&"step".to_string()));
        assert!(toks.contains(&"rapier".to_string()));
        assert!(
            !toks.contains(&"the".to_string()),
            "stopword kept: {toks:?}"
        );
        assert!(!toks.contains(&"a".to_string()), "1-char kept: {toks:?}");
    }

    #[test]
    fn term_freqs_counts() {
        let m = term_freqs("rapier rapier physics");
        assert_eq!(m.get("rapier"), Some(&2));
        assert_eq!(m.get("physics"), Some(&1));
    }

    #[test]
    fn chunk_file_strips_frontmatter_and_splits_headings() {
        let src = "---\ntitle: rule\n---\n# Alpha\nbody one\n## Beta\nbody two\n";
        let mut out = Vec::new();
        chunk_file("r.md", src, &mut out);
        let headings: Vec<&str> = out.iter().map(|c| c.heading.as_str()).collect();
        assert!(headings.contains(&"Alpha"), "{headings:?}");
        assert!(headings.contains(&"Beta"), "{headings:?}");
        // frontmatter key should not leak into any chunk body
        assert!(
            out.iter().all(|c| !c.body.contains("title")),
            "frontmatter leaked"
        );
    }

    #[test]
    fn search_ranks_relevant_chunk_first() {
        let dir = std::env::temp_dir().join(format!("cbtest_kb_{}", std::process::id()));
        let docs = dir.join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(
            docs.join("phys.md"),
            "# Physics\nThe physics step runs PhysicsStep each tick.\n",
        )
        .unwrap();
        std::fs::write(
            docs.join("audio.md"),
            "# Audio\nAudio emitters mix sound buffers.\n",
        )
        .unwrap();
        let out = search(&dir, &json!({"query": "physics step", "roots": ["docs"]})).unwrap();
        let phys = out.find("phys.md").unwrap();
        let audio = out.find("audio.md");
        // Physics doc must appear and rank ahead of any audio match.
        assert!(audio.map_or(true, |a| phys < a), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cached_chunks_reuses_until_mtime_changes() {
        let dir = std::env::temp_dir().join(format!("cbtest_kbcache_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("n.md");
        std::fs::write(&f, "# One\nalpha content\n").unwrap();
        let mut bytes = 0u64;
        let c1 = cached_chunks(&f, "n.md", &mut bytes);
        assert!(c1.iter().any(|c| c.heading == "One"));
        let again = cached_chunks(&f, "n.md", &mut bytes);
        assert_eq!(again.len(), c1.len());
        assert_eq!(again[0].heading, c1[0].heading);
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&f, "# Two\nbeta content\n").unwrap();
        let c2 = cached_chunks(&f, "n.md", &mut bytes);
        assert!(c2.iter().any(|c| c.heading == "Two"), "stale cache");
        assert!(bytes > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upsert_rejects_escape_paths() {
        let dir = std::env::temp_dir().join(format!("cbtest_kbup_{}", std::process::id()));
        let err = upsert(
            &dir,
            &json!({"path": "../evil.md", "heading": "X", "body": "y"}),
        )
        .unwrap_err();
        assert!(err.contains(".."), "{err}");
        let err2 = upsert(
            &dir,
            &json!({"path": "src/main.cpp", "heading": "X", "body": "y"}),
        )
        .unwrap_err();
        assert!(err2.contains("must be under"), "{err2}");
    }

    #[test]
    fn upsert_section_create_and_replace() {
        let dir = std::env::temp_dir().join(format!("cbtest_kbup2_{}", std::process::id()));
        let rules = dir.join(".cursor/rules");
        std::fs::create_dir_all(&rules).unwrap();
        let rel = ".cursor/rules/agent-note.mdc";
        upsert(
            &dir,
            &json!({
                "path": rel,
                "heading": "Perf",
                "body": "first note",
                "description": "test notes"
            }),
        )
        .unwrap();
        let text = std::fs::read_to_string(dir.join(rel)).unwrap();
        assert!(text.contains("alwaysApply: false"));
        assert!(text.contains("# Perf"));
        assert!(text.contains("first note"));

        upsert(
            &dir,
            &json!({"path": rel, "heading": "Perf", "body": "updated note"}),
        )
        .unwrap();
        let text2 = std::fs::read_to_string(dir.join(rel)).unwrap();
        assert!(text2.contains("updated note"));
        assert!(!text2.contains("first note"));

        let found = search(
            &dir,
            &json!({"query": "updated note perf", "roots": [".cursor/rules"]}),
        )
        .unwrap();
        assert!(found.contains("agent-note"), "{found}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pitfall_mode_formats_structured_entry() {
        let dir = std::env::temp_dir().join(format!("cbtest_kbup_pit_{}", std::process::id()));
        let notes = dir.join(".wordkeep/notes");
        std::fs::create_dir_all(&notes).unwrap();
        let rel = ".wordkeep/notes/pitfall-test.md";
        upsert(
            &dir,
            &json!({
                "path": rel,
                "mode": "pitfall",
                "symptom": "Chunk hitches at 60 Hz",
                "root_cause": "CA jitter on uneven terrain",
                "fix": "Gate remesh on SETTLE_EPS",
                "test_tag": "unit"
            }),
        )
        .unwrap();
        let text = std::fs::read_to_string(dir.join(rel)).unwrap();
        assert!(text.contains("**Symptom:**"));
        assert!(text.contains("SETTLE_EPS"));
        assert!(text.contains("[unit]"));
        let out = upsert(
            &dir,
            &json!({
                "path": rel,
                "mode": "pitfall",
                "symptom": "Chunk hitches at 60 Hz",
                "root_cause": "CA jitter",
                "fix": "Use settle epsilon"
            }),
        )
        .unwrap();
        assert!(out.contains("pitfall"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replace_section_swaps_body_until_next_heading() {
        let src = "# Alpha\nold\n## Beta\nkeep\n# Gamma\ntail\n";
        let mut s = src.to_string();
        assert!(replace_section(&mut s, "Alpha", "new"));
        assert!(s.contains("# Alpha\n\nnew"));
        assert!(s.contains("## Beta"));
        assert!(s.contains("tail"));
        assert!(!s.contains("old"));
    }
}
