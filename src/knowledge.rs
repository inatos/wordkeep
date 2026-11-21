//! knowledge_search / knowledge_answer - 3-way Reciprocal Rank Fusion (RRF)
//! retrieval over project markdown.
//!
//! Indexes `.md` / `.mdc` files (chunked by heading) into the per-workspace
//! SQLite store (`doc_chunks` + `doc_chunks_fts`), then fuses up to three
//! independent ranked lists via Reciprocal Rank Fusion (`K = 60`):
//!   1. SQLite FTS5 BM25 (`doc_chunks_fts`, scoped to active roots)
//!   2. In-memory Okapi BM25 + exact heading/phrase affinity (also covers
//!      dynamically injected unresolved defects from `.wordkeep/defects.json`)
//!   3. Dense vector cosine similarity over all chunks in scope, cached by
//!      `(content_sha256, model_tag)` in the SQLite `embeddings` table
//!      (pure-Rust `model2vec-rs` `potion-retrieval-32M` under `--features
//!      semantic-m2v`, or legacy `fastembed` under `--features embeddings`).
//!
//! Defect boosting (`eyeball_fail` / `open` / `gated`) is applied post-RRF,
//! followed by optional Ereshkigal SemIf head reranking (`semif:true`).
//! `knowledge_answer` builds an extractive synthesis + citations from the same
//! pipeline.

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use wordkeep_knowledge::chunk_bodies;

use crate::cache;
use crate::continuation::{self, KIND_HIT_SKIP};
use crate::embed::{self, EmbedDomain, Embedder, RRF_K};
use crate::store::{self, Store, STAGE_DOC_CHUNKS};
use crate::{coeffects, config, defects, effect_journal, mas, progress, semif, stats, walk};

const DOC_EXT: [&str; 3] = ["md", "mdc", "markdown"];
/// Paths (relative to repo root) agents may write via `knowledge_upsert`.
const UPSERT_ROOTS: [&str; 3] = [".cursor/rules", "docs", ".wordkeep/notes"];
const K1: f64 = 1.5;
const B: f64 = 0.75;
/// RRF leg weights: SQLite FTS5 BM25 + in-memory Okapi BM25 + dense vectors.
const W_FTS5: f64 = 1.0;
const W_BM25: f64 = 1.0;
const W_DENSE: f64 = 1.2;
/// Minimum cosine similarity for a chunk to enter the dense RRF list.
const MIN_DOC_DENSE_COSINE: f64 = 0.20;
/// Maximum dense candidates contributed into RRF per query.
const DENSE_RRF_TOP: usize = 64;

/// Process-local warm doc corpus: pre-tokenized chunks + BM25 stats keyed by
/// workspace + roots + SQLite doc-stage fingerprint. Avoids N+1 SQLite loads and
/// full-corpus `term_freqs` reallocation on warm `knowledge_search` calls.
struct WarmDocCorpus {
    root: PathBuf,
    roots_key: String,
    fingerprint: String,
    chunks: Arc<Vec<Chunk>>,
    df: Arc<HashMap<String, u32>>,
    postings: Arc<HashMap<String, Vec<usize>>>,
    avgdl: f64,
    indexed_bytes: u64,
}

fn build_bm25_stats(
    chunks: &[Chunk],
) -> (HashMap<String, u32>, HashMap<String, Vec<usize>>, f64) {
    let n = chunks.len() as f64;
    let avgdl = if n > 0.0 {
        (chunks.iter().map(|c| c.len as f64).sum::<f64>() / n).max(1.0)
    } else {
        1.0
    };
    let mut df: HashMap<String, u32> = HashMap::new();
    let mut postings: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, c) in chunks.iter().enumerate() {
        for term in c.tf.keys() {
            *df.entry(term.clone()).or_insert(0) += 1;
            postings.entry(term.clone()).or_default().push(i);
        }
    }
    (df, postings, avgdl)
}

fn warm_corpus_cache() -> &'static Mutex<Option<WarmDocCorpus>> {
    static CACHE: OnceLock<Mutex<Option<WarmDocCorpus>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Merged docs+defects corpus so warm searches never re-clone ~2500 chunks.
struct MergedCorpus {
    root: PathBuf,
    roots_key: String,
    doc_fingerprint: String,
    defects_mtime_ns: u64,
    chunks: Arc<Vec<Chunk>>,
    defect_boosts: HashMap<usize, f64>,
    df: Arc<HashMap<String, u32>>,
    postings: Arc<HashMap<String, Vec<usize>>>,
    avgdl: f64,
}

fn merged_corpus_cache() -> &'static Mutex<Option<MergedCorpus>> {
    static CACHE: OnceLock<Mutex<Option<MergedCorpus>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

fn roots_cache_key(roots: &[String]) -> String {
    let mut parts: Vec<String> = roots
        .iter()
        .map(|r| r.replace('\\', "/").trim_matches('/').to_string())
        .collect();
    parts.sort();
    parts.join("\n")
}

pub(crate) fn invalidate_warm_corpus_cache() {
    if let Ok(mut guard) = warm_corpus_cache().lock() {
        *guard = None;
    }
    if let Ok(mut guard) = merged_corpus_cache().lock() {
        *guard = None;
    }
    embed::invalidate_embed_ram_cache();
}

fn merge_docs_with_defects(
    root: &Path,
    roots: &[String],
    warm: &WarmIndex,
    include_defects: bool,
    defects_mt: u64,
) -> (
    Arc<Vec<Chunk>>,
    HashMap<usize, f64>,
    Option<Arc<HashMap<String, u32>>>,
    Option<Arc<HashMap<String, Vec<usize>>>>,
    Option<f64>,
) {
    let doc_fp = warm.fingerprint.clone();
    let roots_key = roots_cache_key(roots);
    let canon = root
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf());
    let want_defects = include_defects;

    if !doc_fp.is_empty() {
        if let Ok(guard) = merged_corpus_cache().lock() {
            if let Some(cached) = guard.as_ref() {
                let defects_ok = if want_defects {
                    cached.defects_mtime_ns == defects_mt
                } else {
                    cached.defect_boosts.is_empty()
                };
                if cached.root == canon
                    && cached.roots_key == roots_key
                    && cached.doc_fingerprint == doc_fp
                    && defects_ok
                {
                    return (
                        Arc::clone(&cached.chunks),
                        cached.defect_boosts.clone(),
                        Some(Arc::clone(&cached.df)),
                        Some(Arc::clone(&cached.postings)),
                        Some(cached.avgdl),
                    );
                }
            }
        }
    }

    // Docs-only fast path: reuse warm Arc without cloning.
    if !want_defects {
        return (
            Arc::clone(&warm.chunks),
            HashMap::new(),
            warm.df.clone(),
            warm.postings.clone(),
            warm.avgdl,
        );
    }

    let mut defect_boosts: HashMap<usize, f64> = HashMap::new();
    let mut defect_chunks: Vec<Chunk> = Vec::new();
    for d in defects::unresolved(root) {
        let boost = defects::boost_for_status(&d.status);
        if boost <= 0.0 {
            continue;
        }
        let (path, heading, body) = defects::defect_chunk_text(&d);
        let mut tf = HashMap::new();
        for t in tokenize(&format!("{heading} {body}")) {
            *tf.entry(t).or_insert(0u32) += 1;
        }
        let len = tf.values().sum();
        let sha256 = store::sha256_hex(&format!("{heading}\n{body}"));
        defect_boosts.insert(warm.chunks.len() + defect_chunks.len(), boost);
        defect_chunks.push(Chunk {
            path,
            heading,
            body,
            sha256,
            tf,
            len,
        });
    }

    if defect_chunks.is_empty() {
        let chunks = Arc::clone(&warm.chunks);
        if !doc_fp.is_empty() {
            if let (Some(df), Some(postings), Some(avgdl)) =
                (warm.df.clone(), warm.postings.clone(), warm.avgdl)
            {
                if let Ok(mut guard) = merged_corpus_cache().lock() {
                    *guard = Some(MergedCorpus {
                        root: canon,
                        roots_key,
                        doc_fingerprint: doc_fp,
                        defects_mtime_ns: defects_mt,
                        chunks: Arc::clone(&chunks),
                        defect_boosts: HashMap::new(),
                        df: Arc::clone(&df),
                        postings: Arc::clone(&postings),
                        avgdl,
                    });
                }
                return (
                    chunks,
                    HashMap::new(),
                    Some(df),
                    Some(postings),
                    Some(avgdl),
                );
            }
        }
        return (
            chunks,
            HashMap::new(),
            warm.df.clone(),
            warm.postings.clone(),
            warm.avgdl,
        );
    }

    let mut merged = Vec::with_capacity(warm.chunks.len() + defect_chunks.len());
    merged.extend(warm.chunks.iter().cloned());
    merged.extend(defect_chunks);
    let (df, postings, avgdl) = build_bm25_stats(&merged);
    let df = Arc::new(df);
    let postings = Arc::new(postings);
    let chunks = Arc::new(merged);
    if !doc_fp.is_empty() {
        if let Ok(mut guard) = merged_corpus_cache().lock() {
            *guard = Some(MergedCorpus {
                root: canon,
                roots_key,
                doc_fingerprint: doc_fp,
                defects_mtime_ns: defects_mt,
                chunks: Arc::clone(&chunks),
                defect_boosts: defect_boosts.clone(),
                df: Arc::clone(&df),
                postings: Arc::clone(&postings),
                avgdl,
            });
        }
    }
    (chunks, defect_boosts, Some(df), Some(postings), Some(avgdl))
}

struct WarmHit {
    chunks: Arc<Vec<Chunk>>,
    df: Arc<HashMap<String, u32>>,
    postings: Arc<HashMap<String, Vec<usize>>>,
    avgdl: f64,
    indexed_bytes: u64,
}

fn lookup_warm_corpus(root: &Path, roots: &[String], fingerprint: &str) -> Option<WarmHit> {
    let Ok(guard) = warm_corpus_cache().lock() else {
        return None;
    };
    let cached = guard.as_ref()?;
    let canon = root
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf());
    if cached.root != canon {
        return None;
    }
    if cached.roots_key != roots_cache_key(roots) {
        return None;
    }
    if cached.fingerprint != fingerprint {
        return None;
    }
    Some(WarmHit {
        chunks: Arc::clone(&cached.chunks),
        df: Arc::clone(&cached.df),
        postings: Arc::clone(&cached.postings),
        avgdl: cached.avgdl,
        indexed_bytes: cached.indexed_bytes,
    })
}

fn store_warm_corpus(
    root: &Path,
    roots: &[String],
    fingerprint: String,
    chunks: Vec<Chunk>,
    indexed_bytes: u64,
) -> WarmHit {
    let (df, postings, avgdl) = build_bm25_stats(&chunks);
    let arc = Arc::new(chunks);
    let df = Arc::new(df);
    let postings = Arc::new(postings);
    let canon = root
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf());
    let hit = WarmHit {
        chunks: Arc::clone(&arc),
        df: Arc::clone(&df),
        postings: Arc::clone(&postings),
        avgdl,
        indexed_bytes,
    };
    if let Ok(mut guard) = warm_corpus_cache().lock() {
        *guard = Some(WarmDocCorpus {
            root: canon,
            roots_key: roots_cache_key(roots),
            fingerprint,
            chunks: arc,
            df,
            postings,
            avgdl,
            indexed_bytes,
        });
    }
    hit
}

/// Drop cached chunking for a markdown file in a known workspace root
/// (spatial coeffect on agent writes). Deletes the file row from the workspace
/// SQLite store so `doc_chunks`, `doc_chunks_fts`, and `file_stages` stay fresh.
pub fn invalidate_markdown_chunks_in(root: &Path, rel: &str) {
    let norm = rel.replace('\\', "/");
    invalidate_warm_corpus_cache();
    if let Ok(store) = Store::open(root) {
        let _ = store.delete_file(&norm);
    }
}

/// Drop cached chunking for a markdown file given only its path.
/// Walks ancestors for `.wordkeep` or `.git` to find the workspace root,
/// falling back to `path.parent()`.
#[allow(dead_code)]
pub fn invalidate_markdown_chunks(path: &Path) {
    let root = infer_root_for_path(path);
    let rel = crate::store::normalize_rel(&root, path);
    invalidate_markdown_chunks_in(&root, &rel);
}

#[allow(dead_code)]
fn infer_root_for_path(path: &Path) -> std::path::PathBuf {
    let start = path.parent().unwrap_or(path);
    for anc in start.ancestors() {
        if anc.join(".wordkeep").is_dir() || anc.join(".git").exists() {
            return anc.to_path_buf();
        }
    }
    start.to_path_buf()
}

const STOP: [&str; 22] = [
    "the", "and", "for", "with", "that", "this", "from", "are", "was", "but", "not", "you", "your",
    "use", "using", "into", "when", "which", "have", "has", "its", "can",
];

struct Chunk {
    path: String,
    heading: String,
    body: String,
    sha256: String,
    tf: HashMap<String, u32>,
    len: u32,
}

impl Clone for Chunk {
    fn clone(&self) -> Self {
        Chunk {
            path: self.path.clone(),
            heading: self.heading.clone(),
            body: self.body.clone(),
            sha256: self.sha256.clone(),
            tf: self.tf.clone(),
            len: self.len,
        }
    }
}

/// One 3-way RRF (and optionally SemIf-reranked) hit with path/heading/body.
struct ScoredHit {
    score: f64,
    path: String,
    heading: String,
    body: String,
}

#[derive(Clone, Copy, Default)]
struct DocIndexStats {
    indexed_bytes: u64,
    bytes_read: u64,
    cache_hits: u64,
    cache_misses: u64,
}

/// Shared retrieval result for `search` and `answer`.
struct Retrieval {
    query: String,
    q_terms: Vec<String>,
    hits: Vec<ScoredHit>,
    io: DocIndexStats,
    /// True when the index (docs + optional defects) had zero chunks.
    empty_index: bool,
}

/// Index + 3-way RRF (SQLite FTS5 BM25 + lexical Okapi BM25 + SQLite-cached
/// dense vectors) + defect boost + optional SemIf rerank. Shared by `search`
/// and `answer`.
fn retrieve(root: &Path, args: &Value) -> Result<Retrieval, String> {
    retrieve_with_embedder(root, args, None)
}

fn retrieve_with_embedder(
    root: &Path,
    args: &Value,
    custom_embedder: Option<&dyn Embedder>,
) -> Result<Retrieval, String> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if query.is_empty() {
        return Err("query is required".into());
    }
    let roots = config::doc_roots_from_args(root, args)?;
    let include_defects = args
        .get("include_defects")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    let store = Store::open(root).ok();
    let warm = index_with_store(root, &roots, store.as_ref());
    let io = warm.io;
    let defects_mt = if include_defects {
        std::fs::metadata(root.join(".wordkeep/defects.json"))
            .ok()
            .map(|m| cache::mtime_ns(&m))
            .unwrap_or(0)
    } else {
        0
    };
    if include_defects {
        if let Some(st) = store.as_ref() {
            defects::ensure_defect_links_indexed(root, st);
        }
    }
    let (chunks_arc, defect_boosts, cached_df, cached_postings, cached_avgdl) =
        merge_docs_with_defects(root, &roots, &warm, include_defects, defects_mt);
    let chunks: &[Chunk] = chunks_arc.as_slice();

    let q_terms = tokenize(&query);
    if q_terms.is_empty() {
        return Err("query has no searchable terms".into());
    }

    if chunks.is_empty() {
        return Ok(Retrieval {
            query,
            q_terms,
            hits: Vec::new(),
            io,
            empty_index: true,
        });
    }

    // Leg 1: SQLite FTS5 BM25 over `doc_chunks_fts` (scoped to active `roots`).
    let mut by_path_sha: HashMap<(&str, &str), Vec<usize>> = HashMap::with_capacity(chunks.len());
    let mut by_path_heading: HashMap<(&str, &str), Vec<usize>> =
        HashMap::with_capacity(chunks.len());
    for (i, c) in chunks.iter().enumerate() {
        if defect_boosts.contains_key(&i) {
            continue;
        }
        by_path_sha
            .entry((c.path.as_str(), c.sha256.as_str()))
            .or_default()
            .push(i);
        by_path_heading
            .entry((c.path.as_str(), c.heading.as_str()))
            .or_default()
            .push(i);
    }

    let mut fts5_ranks: Vec<usize> = Vec::new();
    if let Some(st) = store.as_ref() {
        let fts_limit = chunks.len().clamp(64, 512);
        if let Ok(fts_hits) = st.search_docs_fts_scoped(&query, &roots, fts_limit) {
            let mut seen_fts: HashSet<usize> = HashSet::with_capacity(fts_hits.len());
            for hit in &fts_hits {
                let candidates = by_path_sha
                    .get(&(hit.rel_path.as_str(), hit.sha256.as_str()))
                    .or_else(|| by_path_heading.get(&(hit.rel_path.as_str(), hit.heading.as_str())));
                if let Some(indices) = candidates {
                    if let Some(&idx) = indices.iter().find(|&&i| seen_fts.insert(i)) {
                        fts5_ranks.push(idx);
                    }
                }
            }
        }
    }

    // Leg 2: In-memory Okapi BM25 + exact heading/phrase affinity over all
    // chunks (including unresolved defects injected from `.wordkeep/defects.json`).
    let n = chunks.len() as f64;
    let (df_arc, postings_arc, avgdl): (
        Arc<HashMap<String, u32>>,
        Option<Arc<HashMap<String, Vec<usize>>>>,
        f64,
    ) = if let (Some(df), Some(avg)) = (cached_df.as_ref(), cached_avgdl) {
        (Arc::clone(df), cached_postings.clone(), avg)
    } else {
        let (df, postings, avg) = build_bm25_stats(chunks);
        (Arc::new(df), Some(Arc::new(postings)), avg)
    };
    let query_lower = query.to_ascii_lowercase();

    // Score only chunks that contain at least one query term (postings union).
    let mut cand: HashSet<usize> = HashSet::new();
    if let Some(ref postings) = postings_arc {
        for qt in &q_terms {
            if let Some(ids) = postings.get(qt.as_str()) {
                cand.extend(ids.iter().copied());
            }
        }
    } else {
        cand.extend(0..chunks.len());
    }
    let mut bm25_scored: Vec<(f64, usize)> = Vec::new();
    for i in cand {
        let c = &chunks[i];
        let dl = c.len as f64;
        let mut score = 0.0;
        for qt in &q_terms {
            let f = *c.tf.get(qt).unwrap_or(&0) as f64;
            if f == 0.0 {
                continue;
            }
            let n_qi = *df_arc.get(qt.as_str()).unwrap_or(&0) as f64;
            let idf = ((n - n_qi + 0.5) / (n_qi + 0.5) + 1.0).ln();
            score += idf * (f * (K1 + 1.0)) / (f + K1 * (1.0 - B + B * dl / avgdl));
        }
        if score > 0.0 {
            let heading_lower = c.heading.to_ascii_lowercase();
            if heading_lower.contains(&query_lower) {
                score *= 1.25;
            } else if q_terms.len() > 1
                && q_terms
                    .iter()
                    .all(|qt| heading_lower.contains(qt.as_str()))
            {
                score *= 1.15;
            }
            bm25_scored.push((score, i));
        }
    }
    bm25_scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.cmp(&b.1))
    });
    let bm25_ranks: Vec<usize> = bm25_scored.iter().map(|&(_, idx)| idx).collect();
    let bm25_rank_of: HashMap<usize, usize> = bm25_ranks
        .iter()
        .enumerate()
        .map(|(r, &idx)| (idx, r + 1))
        .collect();
    // Leg 3: Dense vector cosine similarity over all chunks in scope, cached by
    // `(content_sha256, model_tag)` in SQLite `embeddings` table.
    let want_semantic = args
        .get("semantic")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let mut dense_ranks: Vec<usize> = Vec::new();
    if want_semantic {
        let owned_embedder = if custom_embedder.is_none() {
            match embed::resolve_embedder(root, EmbedDomain::Docs) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("[wordkeep] dense retrieval unavailable, using lexical RRF: {e}");
                    None
                }
            }
        } else {
            None
        };
        let active_embedder: Option<&dyn Embedder> =
            custom_embedder.or_else(|| owned_embedder.as_deref());

        if let Some(emb) = active_embedder {
            // Warm corpus: skip reallocating every chunk body; SQLite/RAM hits
            // only need the content sha. Fill text for cold misses below.
            let warm_dense = io.cache_hits > 0;
            let mut items: Vec<(String, String)> = chunks
                .iter()
                .map(|c| {
                    if warm_dense {
                        (c.sha256.clone(), String::new())
                    } else {
                        (c.sha256.clone(), format!("{}\n{}", c.heading, c.body))
                    }
                })
                .collect();
            if warm_dense {
                // Ensure priority candidates have text if they miss the RAM/SQL cache.
                for &idx in fts5_ranks.iter().chain(bm25_ranks.iter()).take(DENSE_RRF_TOP) {
                    if let Some(c) = chunks.get(idx) {
                        if let Some(slot) = items.get_mut(idx) {
                            if slot.1.is_empty() {
                                slot.1 = format!("{}\n{}", c.heading, c.body);
                            }
                        }
                    }
                }
            }
            let mut priority: Vec<usize> = Vec::with_capacity(fts5_ranks.len() + bm25_ranks.len());
            priority.extend_from_slice(&fts5_ranks);
            priority.extend_from_slice(&bm25_ranks);

            match (
                embed::embed_cached(store.as_ref(), emb, &items, &priority),
                emb.embed_one(&query),
            ) {
                (Ok((vecs, _cache_stats)), Ok(q_vec)) => {
                    let mut dense_scored: Vec<(f64, usize)> = Vec::new();
                    for (idx, maybe_vec) in vecs.iter().enumerate() {
                        if let Some(v) = maybe_vec {
                            let sim = embed::cosine(&q_vec, v.as_slice());
                            if sim >= MIN_DOC_DENSE_COSINE {
                                dense_scored.push((sim, idx));
                            }
                        }
                    }
                    dense_scored.sort_by(|a, b| {
                        b.0.partial_cmp(&a.0)
                            .unwrap_or(std::cmp::Ordering::Equal)
                            .then_with(|| a.1.cmp(&b.1))
                    });
                    dense_ranks = dense_scored
                        .into_iter()
                        .take(DENSE_RRF_TOP)
                        .map(|(_, idx)| idx)
                        .collect();
                }
                (Err(e), _) | (_, Err(e)) => {
                    eprintln!("[wordkeep] dense embedding failed, using lexical RRF: {e}");
                }
            }
        }
    }

    // True 3-way Reciprocal Rank Fusion over FTS5 BM25 + lexical Okapi BM25 + dense vectors.
    let mut scored = embed::rrf_fuse(
        &[
            (&fts5_ranks, W_FTS5),
            (&bm25_ranks, W_BM25),
            (&dense_ranks, W_DENSE),
        ],
        RRF_K,
    );

    // Preserve unresolved defect boosting (`eyeball_fail` 8x, `open` 4x, `gated` 2x).
    // Defects are injected from `.wordkeep/defects.json` rather than `doc_chunks_fts`,
    // so grant FTS5-leg parity when they matched lexically before scaling by boost.
    if !defect_boosts.is_empty() && !scored.is_empty() {
        let fts_active = !fts5_ranks.is_empty();
        for (score, idx) in &mut scored {
            if let Some(&boost) = defect_boosts.get(idx) {
                if fts_active {
                    if let Some(&r) = bm25_rank_of.get(idx) {
                        *score += (W_FTS5 * (RRF_K + 1.0)) / (RRF_K + r as f64);
                    }
                }
                *score *= boost;
            }
        }
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.cmp(&b.1))
        });
    }

    // Optional Ereshkigal SemIf head reranking (`semif:true`, distinct from dense
    // embeddings). Also retained as a soft fallback when `semantic:true` is
    // explicitly requested in a build with no compiled embedding backend.
    let want_semif = args
        .get("semif")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || (!embed::has_compiled_backend()
            && custom_embedder.is_none()
            && args
                .get("semantic")
                .and_then(Value::as_bool)
                .unwrap_or(false));
    let scored = if want_semif {
        let candidates: Vec<(String, String)> = chunks
            .iter()
            .map(|c| (c.heading.clone(), c.body.clone()))
            .collect();
        let before = scored.clone();
        match semif::rerank_knowledge(root, &query, &candidates, scored) {
            Ok((reranked, _fallback)) => reranked,
            Err(e) => {
                eprintln!("[wordkeep] semif knowledge rerank unavailable, keeping RRF: {e}");
                before
            }
        }
    } else {
        scored
    };

    let hits: Vec<ScoredHit> = scored
        .into_iter()
        .map(|(score, idx)| {
            let c = &chunks[idx];
            ScoredHit {
                score,
                path: c.path.clone(),
                heading: c.heading.clone(),
                body: c.body.clone(),
            }
        })
        .collect();

    Ok(Retrieval {
        query,
        q_terms,
        hits,
        io,
        empty_index: false,
    })
}

pub fn search(root: &Path, args: &Value) -> Result<String, String> {
    search_with_embedder(root, args, None)
}

fn search_with_embedder(
    root: &Path,
    args: &Value,
    custom_embedder: Option<&dyn Embedder>,
) -> Result<String, String> {
    const FP_KEYS: &[&str] = &[
        "query",
        "k",
        "roots",
        "include_defects",
        "semantic",
        "semif",
        "optical",
        "optical_max_ratio",
        "optical_mode",
        "optical_gpu",
    ];
    let k = args.get("k").and_then(Value::as_u64).unwrap_or(5).max(1) as usize;
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(1200) as usize;
    let args_fp = continuation::args_fingerprint(args, FP_KEYS);
    let resume_at =
        continuation::resume_offset(args, "knowledge_search", FP_KEYS, KIND_HIT_SKIP)? as usize;

    progress::tick(0, None, "knowledge_search: retrieve");
    let retrieval = retrieve_with_embedder(root, args, custom_embedder)?;
    if retrieval.hits.is_empty() {
        stats::record_ext(
            "knowledge_search",
            stats::RecordMeta {
                baseline: retrieval.io.indexed_bytes / 4,
                returned: 0,
                bytes_read: retrieval.io.bytes_read,
                cache_hits: retrieval.io.cache_hits,
                cache_misses: retrieval.io.cache_misses,
                reason: None,
            },
        );
        if retrieval.empty_index {
            return Ok("knowledge_search: no documents indexed".into());
        }
        return Ok(format!(
            "knowledge_search: no matches for {:?}",
            retrieval.query
        ));
    }

    let end = k.min(retrieval.hits.len());
    let start = resume_at.min(end);
    let mut out = format!(
        "knowledge_search - query {:?}, top {} of {} matches",
        retrieval.query,
        end,
        retrieval.hits.len()
    );
    if resume_at > 0 {
        out.push_str(&format!(" [continuation from hit #{resume_at}]"));
    }
    out.push('\n');
    let mut obs_keys: Vec<(String, String)> = Vec::new();
    let mut seen_obs: HashSet<(String, String)> = HashSet::new();
    for rank in start..end {
        let hit = &retrieval.hits[rank];
        if hit.path == ".wordkeep/defects.json" {
            if let Some(rest) = hit.heading.strip_prefix("DEFECT [") {
                if let Some((_, defect_id)) = rest.split_once("] ") {
                    let key = ("defect".to_string(), defect_id.trim().to_string());
                    if seen_obs.insert(key.clone()) {
                        obs_keys.push(key);
                    }
                }
            }
        } else if let Some((obs_kind, obs_id)) = obs_kind_and_id_for_note_rel(&hit.path) {
            let key = (obs_kind.to_string(), obs_id);
            if seen_obs.insert(key.clone()) {
                obs_keys.push(key);
            }
        }
    }
    let mut stale_lookup: HashMap<(String, String), Vec<String>> = HashMap::new();
    if !obs_keys.is_empty() {
        if let Ok(st) = Store::open(root) {
            if let Ok(stale) = st.stale_anchors_for_observations(root, &obs_keys) {
                for s in stale {
                    let entry = stale_lookup
                        .entry((s.obs_kind.clone(), s.obs_id.clone()))
                        .or_default();
                    let lbl = s.label();
                    if !entry.contains(&lbl) {
                        entry.push(lbl);
                    }
                }
            }
        }
    }

    let mut used = out.len() / 4;
    let mut truncated_at: Option<usize> = None;
    let mut shown = 0usize;
    let mut shown_hits: Vec<(String, String, String)> = Vec::new();
    for rank in start..end {
        let hit = &retrieval.hits[rank];
        let stale_tag = stale_badge_for_hit(&hit.path, &hit.heading, &stale_lookup);
        let snippet = snippet_for(&hit.body, &retrieval.q_terms, 280);
        let block = format!(
            "\n[{}] {} - {}{}  (score {:.2})\n{}\n",
            rank + 1,
            hit.path,
            hit.heading,
            stale_tag,
            hit.score,
            snippet
        );
        let block_tokens = block.len() / 4;
        if used + block_tokens > budget && shown > 0 {
            truncated_at = Some(rank);
            break;
        }
        used += block_tokens;
        shown += 1;
        out.push_str(&block);
        shown_hits.push((hit.path.clone(), hit.heading.clone(), hit.body.clone()));
    }
    if let Some(rank) = truncated_at {
        let omitted = end.saturating_sub(rank);
        continuation::append_footer(
            &mut out,
            "knowledge_search",
            args_fp,
            rank as u64,
            KIND_HIT_SKIP,
            &format!("+{omitted} further matches"),
        );
    }

    // Opt-in Optical Compression Tier (OCT): PNG artifact + OCR-decoded skim.
    let optical_policy = crate::optical::OpticalPolicy::from_args(root, args);
    if optical_policy.enabled && !shown_hits.is_empty() {
        let approx_chars: usize = shown_hits.iter().map(|(_, _, b)| b.len()).sum();
        let est_ratio = crate::optical::compression_ratio(
            (approx_chars as u64).div_ceil(4).max(1),
            optical_policy
                .mode
                .unwrap_or(crate::optical::VisionMode::Base)
                .vision_tokens(),
        );
        let _ = crate::semif::optical_policy_hint(root, est_ratio, false);
        progress::tick(shown as u64, Some(end as u64), "knowledge_search: optical");
        match crate::optical::compress_hits(root, &shown_hits, &optical_policy) {
            Ok(bundle) => {
                let section = crate::optical::format_optical_section(&bundle);
                let section_tokens = section.len() / 4;
                if used + section_tokens <= budget || shown == 0 {
                    out.push_str(&section);
                } else {
                    out.push_str("\n---\noptical: omitted (token_budget); re-call with higher budget or optical:false\n");
                }
            }
            Err(e) => {
                out.push_str(&format!(
                    "\n---\noptical: fallback exact (error: {e})\n"
                ));
            }
        }
    }

    progress::tick(shown as u64, Some(end as u64), "knowledge_search: done");
    stats::record_ext(
        "knowledge_search",
        stats::RecordMeta {
            baseline: retrieval.io.indexed_bytes / 4,
            returned: (out.len() / 4) as u64,
            bytes_read: retrieval.io.bytes_read,
            cache_hits: retrieval.io.cache_hits,
            cache_misses: retrieval.io.cache_misses,
            reason: None,
        },
    );
    Ok(out)
}

/// Extractive synthesis from top BM25 hits (no LLM) — concatenated snippets + citations.
pub fn answer(root: &Path, args: &Value) -> Result<String, String> {
    let max_citations = args
        .get("max_citations")
        .and_then(Value::as_u64)
        .unwrap_or(5)
        .max(1) as usize;
    let k = args
        .get("k")
        .and_then(Value::as_u64)
        .unwrap_or(max_citations as u64)
        .max(1) as usize;
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(1200) as usize;

    let retrieval = retrieve(root, args)?;
    if retrieval.hits.is_empty() {
        stats::record_ext(
            "knowledge_answer",
            stats::RecordMeta {
                baseline: retrieval.io.indexed_bytes / 4,
                returned: 0,
                bytes_read: retrieval.io.bytes_read,
                cache_hits: retrieval.io.cache_hits,
                cache_misses: retrieval.io.cache_misses,
                reason: None,
            },
        );
        return Ok(format!(
            "knowledge_answer - query {:?}\nanswer: (no matching documents)\ncitations:\n",
            retrieval.query
        ));
    }

    let cite_n = max_citations.min(k).min(retrieval.hits.len());
    let hits = &retrieval.hits[..cite_n];

    let mut answer_parts: Vec<String> = Vec::new();
    for hit in hits {
        let snip = snippet_for(&hit.body, &retrieval.q_terms, 220);
        let cleaned = cleanup_snippet(&snip);
        if cleaned.is_empty() {
            continue;
        }
        // Prefer sentence-like fragments; ensure trailing punctuation for synthesis.
        let mut s = cleaned;
        if !s.ends_with('.') && !s.ends_with('!') && !s.ends_with('?') {
            s.push('.');
        }
        answer_parts.push(s);
        if answer_parts.len() >= 5 {
            break;
        }
    }
    if answer_parts.len() < 2 && hits.len() > answer_parts.len() {
        // Pad with heading-context if we only got one fragment.
        for hit in hits.iter().skip(answer_parts.len()) {
            let extra = cleanup_snippet(&snippet_for(&hit.body, &retrieval.q_terms, 160));
            if extra.is_empty() {
                continue;
            }
            let mut s = extra;
            if !s.ends_with('.') && !s.ends_with('!') && !s.ends_with('?') {
                s.push('.');
            }
            answer_parts.push(s);
            if answer_parts.len() >= 2 {
                break;
            }
        }
    }
    let answer_text = if answer_parts.is_empty() {
        "(no usable snippets)".to_string()
    } else {
        answer_parts.join(" ")
    };

    let mut out = format!(
        "knowledge_answer - query {:?}\nanswer: {}\ncitations:\n",
        retrieval.query, answer_text
    );
    for (rank, hit) in hits.iter().enumerate() {
        let snippet = snippet_for(&hit.body, &retrieval.q_terms, 200);
        let block = format!(
            "  [{}] {} - {} (score {:.2})\n      {}\n",
            rank + 1,
            hit.path,
            hit.heading,
            hit.score,
            cleanup_snippet(&snippet)
        );
        if out.len() / 4 + block.len() / 4 > budget && rank > 0 {
            out.push_str("  … (further citations omitted by token_budget)\n");
            break;
        }
        out.push_str(&block);
    }

    // Clip whole output to token_budget (chars ≈ tokens * 4).
    let max_chars = budget.saturating_mul(4);
    if out.len() > max_chars {
        out.truncate(max_chars);
        out.push_str("\n… (truncated by token_budget)\n");
    }

    stats::record_ext(
        "knowledge_answer",
        stats::RecordMeta {
            baseline: retrieval.io.indexed_bytes / 4,
            returned: (out.len() / 4) as u64,
            bytes_read: retrieval.io.bytes_read,
            cache_hits: retrieval.io.cache_hits,
            cache_misses: retrieval.io.cache_misses,
            reason: None,
        },
    );
    Ok(out)
}

/// Light cleanup for extractive synthesis: collapse whitespace, strip fence noise.
fn cleanup_snippet(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = true;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out.trim()
        .trim_matches(|c| c == '`' || c == '*' || c == '#' || c == '|')
        .trim()
        .to_string()
}

/// Write or update a markdown section so the next `knowledge_search` can retrieve it.
/// Restricted to `.cursor/rules/`, `docs/`, `.wordkeep/notes/`, root `README.md`,
/// and any configured `knowledge_write_roots`.
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

    if mode != "upsert_section"
        && mode != "append_section"
        && mode != "replace_file"
        && mode != "pitfall"
    {
        return Err(format!(
            "unknown mode {mode:?}; use upsert_section, append_section, replace_file, or pitfall"
        ));
    }

    let (heading, body) = if mode == "pitfall" {
        let mut missing = Vec::new();
        let symptom = args
            .get("symptom")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if symptom.is_none() {
            missing.push("symptom");
        }
        let root_cause = args
            .get("root_cause")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if root_cause.is_none() {
            missing.push("root_cause");
        }
        let fix = args
            .get("fix")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if fix.is_none() {
            missing.push("fix");
        }
        if !missing.is_empty() {
            return Err(format!("mode pitfall requires: {}", missing.join(", ")));
        }
        let symptom = symptom.unwrap();
        let root_cause = root_cause.unwrap();
        let fix = fix.unwrap();
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
                config::test_filter_hint(root, tag)
            ));
        }
        (heading, body)
    } else {
        let mut missing = Vec::new();
        let heading = args
            .get("heading")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from);
        if heading.is_none() {
            missing.push("heading");
        }
        let body = args
            .get("body")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from);
        if body.is_none() {
            missing.push("body");
        }
        if !missing.is_empty() {
            return Err(format!("mode {mode} requires: {}", missing.join(", ")));
        }
        (heading.unwrap(), body.unwrap())
    };
    let write_mode = if mode == "pitfall" {
        "upsert_section"
    } else {
        mode
    };

    let full = resolve_upsert_path(root, rel)?;
    let rel_key = full
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| rel.to_string());
    let existed = full.exists();
    let mut src = if existed {
        std::fs::read_to_string(&full).map_err(|e| format!("read {}: {e}", full.display()))?
    } else {
        String::new()
    };
    let before_content = if existed { Some(src.clone()) } else { None };

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

    if let Some(effect_sess) = args
        .get("effect_session")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        mas::require_open_session(root, effect_sess)?;
        effect_journal::record_write(root, effect_sess, &rel_key, before_content, src.clone())?;
    }

    write_atomic(&full, &src)?;
    if let Some((obs_kind, obs_id)) = obs_kind_and_id_for_note_rel(&rel_key) {
        if let Ok(st) = Store::open(root) {
            let (mt, sz) = std::fs::metadata(&full)
                .map(|m| (cache::mtime_ns(&m), m.len()))
                .unwrap_or((0, src.len() as u64));
            let fid = if mt > 0 {
                st.upsert_file(&rel_key, mt, sz, "markdown").ok()
            } else {
                None
            };
            if st
                .sync_observation_links(root, obs_kind, &obs_id, fid, &src, &[])
                .is_ok()
            {
                if let Some(fid) = fid {
                    let _ = st.set_file_stage(fid, store::STAGE_OBS_LINKS, mt, None);
                }
            }
        }
    }
    let effect_sess = args
        .get("effect_session")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    coeffects::notify(
        root,
        "knowledge_upsert",
        coeffects::NotifyCtx {
            rel_paths: &[rel_key.as_str()],
            session: effect_sess,
        },
    );
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
    let norm = config::validate_rel_path(rel.trim().trim_start_matches(['/', '\\']))?;
    let extra = config::knowledge_write_roots(root);
    let mut allowed_roots: Vec<String> = UPSERT_ROOTS.iter().map(|s| (*s).to_string()).collect();
    allowed_roots.extend(extra);
    // Root README.md is always writable (not arbitrary **/README.md).
    let allowed = norm.eq_ignore_ascii_case("README.md")
        || allowed_roots
            .iter()
            .any(|p| norm == *p || norm.starts_with(&format!("{p}/")));
    if !allowed {
        return Err(format!(
            "path must be README.md or under one of: {}",
            allowed_roots.join(", ")
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

/// Try to satisfy a warm query from the process-local corpus cache or a single
/// bulk SQLite load (no walk / N+1). Returns `None` when any staged file's
/// on-disk mtime has drifted.
struct WarmIndex {
    chunks: Arc<Vec<Chunk>>,
    df: Option<Arc<HashMap<String, u32>>>,
    postings: Option<Arc<HashMap<String, Vec<usize>>>>,
    avgdl: Option<f64>,
    fingerprint: String,
    io: DocIndexStats,
}

fn try_warm_doc_corpus(root: &Path, roots: &[String], store: &Store) -> Option<WarmIndex> {
    let (file_stages, fingerprint) = store.doc_stage_fingerprint(roots).ok()?;
    if file_stages.is_empty() {
        return None;
    }
    // Process-cache hit: trust SQLite fingerprint + invalidation coeffects
    // (skip per-file disk stats — that was the warm-path multi-hundred-ms tax).
    if let Some(hit) = lookup_warm_corpus(root, roots, &fingerprint) {
        let n = hit.chunks.len() as u64;
        return Some(WarmIndex {
            chunks: hit.chunks,
            df: Some(hit.df),
            postings: Some(hit.postings),
            avgdl: Some(hit.avgdl),
            fingerprint,
            io: DocIndexStats {
                indexed_bytes: hit.indexed_bytes,
                bytes_read: 0,
                cache_hits: n,
                cache_misses: 0,
            },
        });
    }

    // Process-cache miss: verify on-disk mtimes once, then bulk-load + tokenize.
    let mut indexed_bytes = 0u64;
    for (rel, stage_mt) in &file_stages {
        let meta = std::fs::metadata(root.join(rel)).ok()?;
        if cache::mtime_ns(&meta) != *stage_mt {
            return None;
        }
        indexed_bytes = indexed_bytes.saturating_add(meta.len());
    }
    let (hits, _, fp2) = store.doc_chunks_for_roots(roots).ok()?;
    if hits.is_empty() || fp2 != fingerprint {
        return None;
    }
    let mut chunks: Vec<Chunk> = Vec::with_capacity(hits.len());
    for hit in hits {
        let mut tf = HashMap::new();
        for t in tokenize(&format!("{} {}", hit.heading, hit.body)) {
            *tf.entry(t).or_insert(0u32) += 1;
        }
        let len = tf.values().sum();
        chunks.push(Chunk {
            path: hit.rel_path,
            heading: hit.heading,
            body: hit.body,
            sha256: hit.sha256,
            tf,
            len,
        });
    }
    let n = chunks.len() as u64;
    let hit = store_warm_corpus(root, roots, fingerprint.clone(), chunks, indexed_bytes);
    Some(WarmIndex {
        chunks: hit.chunks,
        df: Some(hit.df),
        postings: Some(hit.postings),
        avgdl: Some(hit.avgdl),
        fingerprint,
        io: DocIndexStats {
            indexed_bytes,
            bytes_read: 0,
            cache_hits: n,
            cache_misses: 0,
        },
    })
}

/// Index the configured roots into chunks, returning the chunks and the total
/// bytes of document text scanned (the baseline an agent would otherwise read).
/// Skips pruned directories and persists newly chunked files into the
/// per-workspace SQLite store (`doc_chunks` + `doc_chunks_fts`).
fn index_with_store(root: &Path, roots: &[String], store: Option<&Store>) -> WarmIndex {
    if let Some(st) = store {
        // Warm hit: skip note-link re-walk; links were synced when chunks were written.
        if let Some(warm) = try_warm_doc_corpus(root, roots, st) {
            return warm;
        }
    }

    let mut chunks = Vec::new();
    let mut io = DocIndexStats::default();
    let prune = walk::prune_set(root);
    let mut pending: Vec<(String, u64, u64, Vec<(String, String)>)> = Vec::new();

    for r in roots {
        let base = root.join(r);
        if base.is_file() {
            let rel = base
                .strip_prefix(root)
                .unwrap_or(&base)
                .to_string_lossy()
                .replace('\\', "/");
            chunks.extend(cached_chunks_in(
                store,
                &mut pending,
                &base,
                &rel,
                &mut io,
            ));
            continue;
        }
        for path_owned in walk::files(&base, &prune) {
            let path = path_owned.as_path();
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
            chunks.extend(cached_chunks_in(
                store,
                &mut pending,
                path,
                &rel,
                &mut io,
            ));
        }
    }

    if !pending.is_empty() {
        invalidate_warm_corpus_cache();
        if let Some(st) = store {
            let _ = st.with_immediate_tx(|s| {
                for (rel, mtime_ns, len, pairs) in &pending {
                    let file_id = s.upsert_file(rel, *mtime_ns, *len, "markdown")?;
                    s.replace_doc_chunks(file_id, pairs)?;
                    s.set_file_stage(file_id, STAGE_DOC_CHUNKS, *mtime_ns, None)?;
                    if let Some((obs_kind, obs_id)) = obs_kind_and_id_for_note_rel(rel) {
                        if let Ok(full_src) = std::fs::read_to_string(root.join(rel)) {
                            if s.sync_observation_links(
                                root,
                                obs_kind,
                                &obs_id,
                                Some(file_id),
                                &full_src,
                                &[],
                            )
                            .is_ok()
                            {
                                let _ = s.set_file_stage(
                                    file_id,
                                    store::STAGE_OBS_LINKS,
                                    *mtime_ns,
                                    None,
                                );
                            }
                        }
                    }
                }
                Ok(())
            });
        }
    }
    if let Some(st) = store {
        let touches_notes = roots.iter().any(|r| {
            let n = r.replace('\\', "/");
            let n = n.trim_matches('/');
            n.is_empty()
                || n == "."
                || n == ".wordkeep"
                || n == ".wordkeep/notes"
                || n.starts_with(".wordkeep/notes/")
        });
        if touches_notes {
            ensure_note_obs_links_indexed(root, st);
        }
        // Always seed the process-local warm cache after a walk — even when some
        // files were cold — so the next call hits Arc corpus instead of re-walking.
        if !chunks.is_empty() {
            let fingerprint = st
                .doc_stage_fingerprint(roots)
                .map(|(_, fp)| fp)
                .unwrap_or_default();
            if !fingerprint.is_empty() {
                let hit =
                    store_warm_corpus(root, roots, fingerprint.clone(), chunks, io.indexed_bytes);
                return WarmIndex {
                    chunks: hit.chunks,
                    df: Some(hit.df),
                    postings: Some(hit.postings),
                    avgdl: Some(hit.avgdl),
                    fingerprint,
                    io,
                };
            }
        }
    }
    WarmIndex {
        chunks: Arc::new(chunks),
        df: None,
        postings: None,
        avgdl: None,
        fingerprint: String::new(),
        io,
    }
}

fn obs_kind_and_id_for_note_rel(rel: &str) -> Option<(&'static str, String)> {
    let norm = rel.replace('\\', "/");
    if norm == ".wordkeep/notes/shrift-ledger.md" {
        return None;
    }
    if let Some(rest) = norm.strip_prefix(".wordkeep/notes/shrift/") {
        let slug = Path::new(rest)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(rest);
        if !slug.is_empty() {
            return Some(("shrift", slug.to_string()));
        }
    }
    if norm.starts_with(".wordkeep/notes/") {
        return Some(("note", norm));
    }
    None
}

const META_NOTES_OBS_SCAN: &str = "obs_notes_dir_scan_mtime_ns";

/// Ensure `.wordkeep/notes/**` Markdown files have `STAGE_OBS_LINKS` populated
/// even when their `doc_chunks` were already warm-cached prior to v0.5 Phase 6.
pub fn ensure_note_obs_links_indexed(root: &Path, store: &Store) {
    let notes_dir = root.join(".wordkeep/notes");
    if !notes_dir.is_dir() {
        return;
    }
    // Directory mtime watermark: skip the walk when notes tree is unchanged.
    if let Ok(dir_meta) = notes_dir.metadata() {
        let dir_mt = cache::mtime_ns(&dir_meta);
        if dir_mt > 0 {
            let want = dir_mt.to_string();
            if store
                .get_meta(META_NOTES_OBS_SCAN)
                .ok()
                .flatten()
                .as_deref()
                == Some(want.as_str())
            {
                return;
            }
        }
    }
    let prune = walk::prune_set(root);
    let mut cold: Vec<(String, &'static str, String, u64, u64, String)> = Vec::new();
    for path_owned in walk::files(&notes_dir, &prune) {
        let path = path_owned.as_path();
        let ok = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| DOC_EXT.contains(&e));
        if !ok {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let Some((obs_kind, obs_id)) = obs_kind_and_id_for_note_rel(&rel) else {
            continue;
        };
        let Ok(meta) = path.metadata() else {
            continue;
        };
        let mt = cache::mtime_ns(&meta);
        if mt == 0 {
            continue;
        }
        if let Ok(Some((_, stored_mt))) = store.file_stage_mtime(&rel, store::STAGE_OBS_LINKS) {
            if stored_mt == mt {
                continue;
            }
        }
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        cold.push((rel, obs_kind, obs_id, mt, meta.len(), src));
    }
    let dir_mt = notes_dir
        .metadata()
        .ok()
        .map(|m| cache::mtime_ns(&m))
        .unwrap_or(0);
    if cold.is_empty() {
        if dir_mt > 0 {
            let _ = store.set_meta(META_NOTES_OBS_SCAN, &dir_mt.to_string());
        }
        return;
    }
    crate::symbol_resolve::ensure_symbols_warm(root, store);
    let _ = store.with_immediate_tx(|s| {
        for (rel, obs_kind, obs_id, mt, sz, src) in &cold {
            if let Ok(fid) = s.upsert_file(rel, *mt, *sz, "markdown") {
                if s.sync_observation_links(root, obs_kind, obs_id, Some(fid), src, &[])
                    .is_ok()
                {
                    let _ = s.set_file_stage(fid, store::STAGE_OBS_LINKS, *mt, None);
                }
            }
        }
        if dir_mt > 0 {
            let _ = s.set_meta(META_NOTES_OBS_SCAN, &dir_mt.to_string());
        }
        Ok(())
    });
}

fn stale_badge_for_hit(
    path: &str,
    heading: &str,
    stale_lookup: &HashMap<(String, String), Vec<String>>,
) -> String {
    if stale_lookup.is_empty() {
        return String::new();
    }
    if path == ".wordkeep/defects.json" {
        if let Some(rest) = heading.strip_prefix("DEFECT [") {
            if let Some((_, defect_id)) = rest.split_once("] ") {
                if let Some(targets) =
                    stale_lookup.get(&("defect".to_string(), defect_id.trim().to_string()))
                {
                    if !targets.is_empty() {
                        return format!(" [stale-anchor: {}]", targets.join(", "));
                    }
                }
            }
        }
        return String::new();
    }
    if let Some((obs_kind, obs_id)) = obs_kind_and_id_for_note_rel(path) {
        if let Some(targets) = stale_lookup.get(&(obs_kind.to_string(), obs_id)) {
            if !targets.is_empty() {
                return format!(" [stale-anchor: {}]", targets.join(", "));
            }
        }
    }
    String::new()
}

/// Return a document's chunks, reusing the SQLite `doc_chunks` cache when the
/// on-disk mtime is unchanged. Updates `io`. Queues cold parses
/// into `pending` for batch persistence into `doc_chunks` + `doc_chunks_fts`.
fn cached_chunks_in(
    store: Option<&Store>,
    pending: &mut Vec<(String, u64, u64, Vec<(String, String)>)>,
    path: &Path,
    rel: &str,
    io: &mut DocIndexStats,
) -> Vec<Chunk> {
    let (mtime_ns, len) = match std::fs::metadata(path) {
        Ok(m) => (cache::mtime_ns(&m), m.len()),
        Err(_) => (0, 0),
    };
    io.indexed_bytes += len;
    if mtime_ns > 0 {
        if let Some(st) = store {
            if let Ok(Some((file_id, stored_mt))) = st.file_stage_mtime(rel, STAGE_DOC_CHUNKS) {
                if stored_mt == mtime_ns {
                    if let Ok(rows) = st.doc_chunks_for_file(file_id) {
                        io.cache_hits += 1;
                        let mut out = Vec::with_capacity(rows.len());
                        for r in rows {
                            push_chunk_with_sha(
                                &mut out,
                                &r.rel_path,
                                &r.heading,
                                &r.body,
                                if r.sha256.is_empty() {
                                    None
                                } else {
                                    Some(r.sha256.as_str())
                                },
                            );
                        }
                        return out;
                    }
                }
            }
        }
    }
    let Ok(src) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    io.cache_misses += 1;
    io.bytes_read += src.len() as u64;
    let pairs = chunk_bodies(rel, &src);
    let mut chunks = Vec::new();
    let mut stored_pairs = Vec::new();
    for (heading, body) in pairs {
        let trimmed = body.trim();
        if !trimmed.is_empty() {
            let before = chunks.len();
            push_chunk(&mut chunks, rel, &heading, trimmed);
            if chunks.len() > before {
                stored_pairs.push((heading, trimmed.to_string()));
            }
        }
    }
    if mtime_ns > 0 {
        pending.push((rel.to_string(), mtime_ns, len, stored_pairs));
    }
    chunks
}

/// Single-file convenience wrapper over [`cached_chunks_in`] (flushes immediately).
#[cfg(test)]
fn cached_chunks(path: &Path, rel: &str, bytes: &mut u64) -> Vec<Chunk> {
    let root = infer_root_for_path(path);
    let store = Store::open(&root).ok();
    let mut pending = Vec::new();
    let mut io = DocIndexStats::default();
    let out = cached_chunks_in(store.as_ref(), &mut pending, path, rel, &mut io);
    *bytes += io.indexed_bytes;
    if !pending.is_empty() {
        if let Some(st) = store.as_ref() {
            let _ = st.with_immediate_tx(|s| {
                for (r, mt, len, pairs) in &pending {
                    let fid = s.upsert_file(r, *mt, *len, "markdown")?;
                    s.replace_doc_chunks(fid, pairs)?;
                    s.set_file_stage(fid, STAGE_DOC_CHUNKS, *mt, None)?;
                }
                Ok(())
            });
        }
    }
    out
}

fn push_chunk(out: &mut Vec<Chunk>, path: &str, heading: &str, buf: &str) {
    push_chunk_with_sha(out, path, heading, buf, None);
}

fn push_chunk_with_sha(
    out: &mut Vec<Chunk>,
    path: &str,
    heading: &str,
    buf: &str,
    known_sha: Option<&str>,
) {
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
    let sha256 = match known_sha {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => store::sha256_hex(&text),
    };
    out.push(Chunk {
        path: path.to_string(),
        heading: heading.to_string(),
        body: body.to_string(),
        sha256,
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
    fn shared_chunker_strips_frontmatter_and_splits_headings() {
        let src = "---\ntitle: rule\n---\n# Alpha\nbody one\n## Beta\nbody two\n";
        let out = chunk_bodies("r.md", src);
        let headings: Vec<&str> = out.iter().map(|(heading, _)| heading.as_str()).collect();
        assert!(headings.contains(&"Alpha"), "{headings:?}");
        assert!(headings.contains(&"Beta"), "{headings:?}");
        // frontmatter key should not leak into any chunk body
        assert!(
            out.iter().all(|(_, body)| !body.contains("title")),
            "frontmatter leaked"
        );
    }

    #[test]
    fn shared_chunker_does_not_split_cpp_include_inside_fence() {
        let src = "# Build\n```cpp\n#include <vector>\n```\ncompiler notes\n";
        let out = chunk_bodies("build.md", src);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].0, "Build");
        assert!(out[0].1.contains("#include <vector>"));
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
    fn answer_synthesizes_from_top_hits() {
        let dir = std::env::temp_dir().join(format!("cbtest_kb_ans_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let docs = dir.join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(
            docs.join("phys.md"),
            "# Physics\nThe physics step runs PhysicsStep each tick with fixed dt.\n",
        )
        .unwrap();
        std::fs::write(
            docs.join("net.md"),
            "# Netcode\nRollback compares same-binary runs for determinism.\n",
        )
        .unwrap();
        let out = answer(
            &dir,
            &json!({
                "query": "physics step",
                "roots": ["docs"],
                "max_citations": 3,
                "include_defects": false
            }),
        )
        .unwrap();
        assert!(out.starts_with("knowledge_answer - query"), "{out}");
        assert!(out.contains("answer:"), "{out}");
        assert!(out.contains("citations:"), "{out}");
        assert!(out.contains("phys.md"), "{out}");
        assert!(out.contains("[1]"), "{out}");
        // Physics should cite ahead of unrelated netcode.
        let phys = out.find("phys.md").unwrap();
        if let Some(net) = out.find("net.md") {
            assert!(phys < net, "{out}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalidate_markdown_chunks_evicts_disk_entry() {
        let _env = crate::cache::test_env_lock();
        let dir = std::env::temp_dir().join(format!("cbtest_kbinv_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("n.md");
        std::fs::write(&f, "# One\nalpha content\n").unwrap();
        let mut bytes = 0u64;
        let _ = cached_chunks(&f, "n.md", &mut bytes);
        let mtime = cache::mtime_ns(&std::fs::metadata(&f).unwrap());
        let st = Store::open(&dir).unwrap();
        assert_eq!(
            st.file_stage_mtime("n.md", STAGE_DOC_CHUNKS)
                .unwrap()
                .map(|(_, mt)| mt),
            Some(mtime)
        );
        assert_eq!(st.search_docs_fts("alpha", 5).unwrap().len(), 1);
        invalidate_markdown_chunks(&f);
        assert!(st.file_stage_mtime("n.md", STAGE_DOC_CHUNKS).unwrap().is_none());
        assert!(st.search_docs_fts("alpha", 5).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upsert_notifies_coeffects_for_search() {
        let dir = std::env::temp_dir().join(format!("cbtest_kb_coeffect_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let docs = dir.join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        let rel = "docs/agent-note.md";
        std::fs::write(
            docs.join("agent-note.md"),
            "# Agent\nInitial searchable phrase xyzzy.\n",
        )
        .unwrap();
        upsert(
            &dir,
            &json!({
                "path": rel,
                "heading": "Agent",
                "body": "Updated searchable phrase xyzzy.",
                "mode": "upsert_section"
            }),
        )
        .unwrap();
        let out = search(&dir, &json!({"query": "xyzzy", "roots": ["docs"]})).unwrap();
        assert!(
            out.contains("Updated searchable"),
            "search should see post-upsert body: {out}"
        );
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
        assert!(
            err2.contains("must be under") || err2.contains("README.md"),
            "{err2}"
        );
    }

    #[test]
    fn upsert_reports_all_missing_mode_fields() {
        let dir = std::env::temp_dir().join(format!("cbtest_kbup_miss_{}", std::process::id()));
        let err = upsert(&dir, &json!({"path": "docs/x.md", "mode": "pitfall"})).unwrap_err();
        assert!(err.contains("symptom"), "{err}");
        assert!(err.contains("root_cause"), "{err}");
        assert!(err.contains("fix"), "{err}");
        let err2 = upsert(
            &dir,
            &json!({"path": "docs/x.md", "mode": "upsert_section"}),
        )
        .unwrap_err();
        assert!(err2.contains("heading"), "{err2}");
        assert!(err2.contains("body"), "{err2}");
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

    #[test]
    fn search_boosts_unresolved_defects_and_respects_include_defects_false() {
        let _env = crate::cache::test_env_lock();
        let dir = std::env::temp_dir().join(format!("cbtest_kb_defect_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let docs = dir.join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(
            docs.join("water.md"),
            "# Water Foam\nWater foam shader renders foam crest on water waves.\n",
        )
        .unwrap();
        defects::upsert(
            &dir,
            &json!({
                "id": "eyeball-water-foam",
                "status": "eyeball_fail",
                "subsystem": "water",
                "summary": "Water foam shader clips shoreline"
            }),
        )
        .unwrap();

        let with_defects = search(
            &dir,
            &json!({
                "query": "water foam shader",
                "roots": ["docs"],
                "include_defects": true,
                "semantic": false
            }),
        )
        .unwrap();
        let def_pos = with_defects.find("DEFECT [eyeball_fail] eyeball-water-foam");
        let doc_pos = with_defects.find("water.md");
        assert!(def_pos.is_some(), "defect missing: {with_defects}");
        assert!(
            doc_pos.map_or(true, |d| def_pos.unwrap() < d),
            "eyeball_fail defect must rank above normal doc: {with_defects}"
        );

        let without_defects = search(
            &dir,
            &json!({
                "query": "water foam shader",
                "roots": ["docs"],
                "include_defects": false,
                "semantic": false
            }),
        )
        .unwrap();
        assert!(
            !without_defects.contains("DEFECT [eyeball_fail]"),
            "include_defects:false leaked defect: {without_defects}"
        );
        assert!(without_defects.contains("water.md"), "{without_defects}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_preserves_semif_head_rerank() {
        let _env = crate::cache::test_env_lock();
        let dir = std::env::temp_dir().join(format!("cbtest_kb_semif_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let docs = dir.join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(
            docs.join("phys.md"),
            "# Physics Step\nPhysics step runs deterministic fixed timestep simulation.\n",
        )
        .unwrap();
        std::fs::write(
            docs.join("audio.md"),
            "# Audio Step\nAudio step mixes emitters on audio thread.\n",
        )
        .unwrap();
        let out = search(
            &dir,
            &json!({
                "query": "physics step deterministic",
                "roots": ["docs"],
                "include_defects": false,
                "semantic": false,
                "semif": true
            }),
        )
        .unwrap();
        assert!(out.contains("phys.md"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rrf_three_way_retrieves_zero_overlap_semantic_hit_and_caches_in_sqlite() {
        let _env = crate::cache::test_env_lock();
        let dir = std::env::temp_dir().join(format!("cbtest_kb_rrf_zero_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let docs = dir.join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(
            docs.join("slab.md"),
            "# Memory Pool\nFixed-size slab recycler avoids heap fragmentation.\n",
        )
        .unwrap();
        std::fs::write(
            docs.join("audio.md"),
            "# Audio Mixer\nAudio voice mixer streams sound channels.\n",
        )
        .unwrap();

        let stub = crate::embed::test_helpers::KeywordStubEmbedder::new(
            crate::embed::TAG_M2V_DOCS_DEFAULT,
            &[
                &["arena", "bump", "allocator", "slab", "recycler", "pool"],
                &["audio", "mixer", "voice", "sound"],
            ],
        );

        // Zero lexical token overlap with `docs/slab.md`!
        let out = search_with_embedder(
            &dir,
            &json!({
                "query": "arena bump allocator",
                "roots": ["docs"],
                "include_defects": false,
                "semantic": true
            }),
            Some(&stub),
        )
        .unwrap();
        assert!(
            out.contains("slab.md"),
            "zero-overlap semantic hit missing: {out}"
        );
        assert!(
            !out.contains("audio.md"),
            "orthogonal doc should not pass dense threshold: {out}"
        );

        // Verify the chunk vector was cached in SQLite `embeddings` by content_sha256.
        let store = Store::open(&dir).unwrap();
        let slab_sha = store::sha256_hex(
            "Memory Pool\nFixed-size slab recycler avoids heap fragmentation.",
        );
        assert!(
            store
                .get_embedding(&slab_sha, crate::embed::TAG_M2V_DOCS_DEFAULT)
                .unwrap()
                .is_some(),
            "chunk embedding must be cached by content_sha256 in sqlite embeddings table"
        );

        // Second query must reuse SQLite-cached chunk vectors (only embedding the 1 query).
        let before_texts = stub.embedded_count();
        let out2 = search_with_embedder(
            &dir,
            &json!({
                "query": "bump arena",
                "roots": ["docs"],
                "include_defects": false,
                "semantic": true
            }),
            Some(&stub),
        )
        .unwrap();
        assert!(out2.contains("slab.md"), "{out2}");
        assert_eq!(
            stub.embedded_count() - before_texts,
            1,
            "warm query should embed only the query, not cached chunks"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rrf_preserves_defect_boost_and_semif_with_dense_leg() {
        let _env = crate::cache::test_env_lock();
        let dir = std::env::temp_dir().join(format!("cbtest_kb_rrf_def_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let docs = dir.join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(
            docs.join("water.md"),
            "# Water Foam\nWater foam crest rendering shader pipeline.\n",
        )
        .unwrap();
        defects::upsert(
            &dir,
            &json!({
                "id": "eyeball-water-crest",
                "status": "eyeball_fail",
                "subsystem": "water",
                "summary": "Water foam crest shader clips near shore"
            }),
        )
        .unwrap();

        let stub = crate::embed::test_helpers::KeywordStubEmbedder::new(
            crate::embed::TAG_M2V_DOCS_DEFAULT,
            &[&["water", "foam", "crest", "shader"]],
        );

        let out = search_with_embedder(
            &dir,
            &json!({
                "query": "water foam crest shader",
                "roots": ["docs"],
                "include_defects": true,
                "semantic": true,
                "semif": true
            }),
            Some(&stub),
        )
        .unwrap();
        let def_pos = out.find("DEFECT [eyeball_fail] eyeball-water-crest");
        let doc_pos = out.find("water.md");
        assert!(def_pos.is_some(), "defect missing: {out}");
        assert!(
            doc_pos.map_or(true, |d| def_pos.unwrap() < d),
            "eyeball_fail defect must stay ahead of normal doc under 3-way RRF + semif: {out}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "semantic-m2v")]
    #[test]
    fn knowledge_search_zero_overlap_with_real_model2vec() {
        let _env = crate::cache::test_env_lock();
        let dir = std::env::temp_dir().join(format!("cbtest_kb_m2v_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let docs = dir.join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(
            docs.join("slab.md"),
            "# Memory Pool\nFixed-size slab recycler eliminates fragmentation.\n",
        )
        .unwrap();
        std::fs::write(
            docs.join("audio.md"),
            "# Audio Mixer\nMixer blends voices per frame.\n",
        )
        .unwrap();

        let model_dir = dir
            .join(".wordkeep/models")
            .join(crate::embed::TAG_M2V_DOCS_DEFAULT);
        crate::embed::test_helpers::write_synthetic_m2v_model(
            &model_dir,
            &[
                ("arena", &[1.0, 0.0]),
                ("allocator", &[1.0, 0.0]),
                ("slab", &[1.0, 0.0]),
                ("recycler", &[1.0, 0.0]),
                ("audio", &[0.0, 1.0]),
                ("mixer", &[0.0, 1.0]),
            ],
        )
        .unwrap();

        let out = search(
            &dir,
            &json!({
                "query": "arena allocator",
                "roots": ["docs"],
                "include_defects": false,
                "semantic": true
            }),
        )
        .unwrap();
        assert!(
            out.contains("slab.md"),
            "real model2vec-rs must retrieve zero-overlap doc: {out}"
        );
        assert!(
            !out.contains("audio.md"),
            "orthogonal doc should not match: {out}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
