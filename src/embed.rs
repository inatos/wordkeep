//! Dense embedding backends, SQLite vector caching, and Reciprocal Rank Fusion (RRF).
//!
//! Backends (all opt-in via Cargo features so default builds stay lean/offline):
//! - `semantic-m2v`: pure-Rust `model2vec-rs` (zero ONNX Runtime). Uses
//!   `minishlab/potion-retrieval-32M` for markdown docs/rules/notes
//!   ([`EmbedDomain::Docs`]) and `minishlab/potion-code-16M-v2` for code symbols
//!   ([`EmbedDomain::Code`]).
//! - `embeddings-fastembed` (legacy alias `embeddings`): compatibility ONNX
//!   Runtime backend (`fastembed`, AllMiniLM-L6-v2) kept until evals verify
//!   `semantic-m2v` parity.
//!
//! Vectors are cached in the per-workspace SQLite `embeddings` table keyed by
//! `(key_sha256, model_tag)` via [`Store::get_embedding`] /
//! [`Store::upsert_embedding`]. Queries themselves are embedded fresh and never
//! persisted.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use crate::store::Store;

/// Process-local embedding RAM cache: `model_tag → (sha → vec)`. Nested maps avoid
/// allocating a `(tag, sha)` key on every warm dense lookup.
fn embed_ram_cache() -> &'static Mutex<HashMap<String, HashMap<String, Arc<Vec<f32>>>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, HashMap<String, Arc<Vec<f32>>>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Drop the process-local embedding RAM cache (doc corpus invalidation).
pub fn invalidate_embed_ram_cache() {
    if let Ok(mut guard) = embed_ram_cache().lock() {
        guard.clear();
    }
}

/// Default Model2Vec repo for markdown docs, rules, design notes, and defects.
#[cfg_attr(not(feature = "semantic-m2v"), allow(dead_code))]
pub const MODEL_M2V_DOCS_DEFAULT: &str = "minishlab/potion-retrieval-32M";
/// Default Model2Vec repo for source code symbols, signatures, and docstrings.
#[cfg_attr(not(feature = "semantic-m2v"), allow(dead_code))]
pub const MODEL_M2V_CODE_DEFAULT: &str = "minishlab/potion-code-16M-v2";
/// SQLite `model_tag` for the default docs Model2Vec model.
#[cfg_attr(not(feature = "semantic-m2v"), allow(dead_code))]
pub const TAG_M2V_DOCS_DEFAULT: &str = "potion-retrieval-32M";
/// SQLite `model_tag` for the default code Model2Vec model.
#[cfg_attr(not(feature = "semantic-m2v"), allow(dead_code))]
pub const TAG_M2V_CODE_DEFAULT: &str = "potion-code-16M-v2";
/// SQLite `model_tag` for the legacy `fastembed` AllMiniLM-L6-v2 model.
#[cfg_attr(not(feature = "embeddings-fastembed"), allow(dead_code))]
pub const TAG_FASTEMBED_LEGACY: &str = "allminilml6v2";
/// Standard Cormack et al. Reciprocal Rank Fusion dampening constant.
pub const RRF_K: f64 = 60.0;

/// Selects the preferred static embedding model for a retrieval domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedDomain {
    /// Markdown docs, rules, design notes, and defects (`potion-retrieval-32M`).
    Docs,
    /// Source code symbols, signatures, and doc comments (`potion-code-16M-v2`).
    Code,
}

/// Dense text encoder abstraction used by `knowledge` and `symbol_resolve`.
pub trait Embedder: Send + Sync {
    /// Stable model identifier stored as `embeddings.model_tag`.
    fn model_tag(&self) -> &str;

    /// Embed a batch of texts into dense `f32` vectors aligned 1:1 with `texts`.
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String>;

    /// Embed a single query string (never persisted to SQLite).
    fn embed_one(&self, text: &str) -> Result<Vec<f32>, String> {
        let mut out = self.embed_batch(&[text.to_string()])?;
        out.pop()
            .ok_or_else(|| "embedder returned empty batch".to_string())
    }

    /// Optional per-query cap on newly embedded uncached corpus items.
    /// `None` means full-corpus embedding (fast static Model2Vec / stubs);
    /// `Some(n)` bounds cold latency for heavier legacy ONNX models.
    fn max_uncached_per_query(&self) -> Option<usize> {
        None
    }
}

/// Cache telemetry from one [`embed_cached`] pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EmbedCacheStats {
    pub hits: usize,
    pub misses: usize,
    pub persisted: usize,
}

/// True when any real dense embedding backend was compiled into this binary.
pub const fn has_compiled_backend() -> bool {
    cfg!(any(feature = "semantic-m2v", feature = "embeddings-fastembed"))
}

/// Human-readable label for compiled embedding features.
#[allow(dead_code)]
pub const fn compiled_backend_name() -> &'static str {
    if cfg!(all(
        feature = "semantic-m2v",
        feature = "embeddings-fastembed"
    )) {
        "semantic-m2v+fastembed"
    } else if cfg!(feature = "semantic-m2v") {
        "semantic-m2v"
    } else if cfg!(feature = "embeddings-fastembed") {
        "fastembed"
    } else {
        "none"
    }
}

/// Resolve the active embedder for `domain` (or `Ok(None)` when disabled / no
/// semantic feature compiled in).
///
/// Environment overrides:
/// - `WORDKEEP_EMBED_BACKEND`: `auto` (default, prefers `semantic-m2v` over
///   legacy `fastembed`), `m2v` / `model2vec`, `fastembed`, or `none` / `off`.
/// - `WORDKEEP_M2V_DOCS_MODEL` / `WORDKEEP_M2V_CODE_MODEL`: local directory or
///   HuggingFace repo ID overriding `potion-retrieval-32M` / `potion-code-16M-v2`.
/// - `WORDKEEP_EMBED_OFFLINE=1` / `WORDKEEP_M2V_OFFLINE=1` / `HF_HUB_OFFLINE=1`:
///   forbid network downloads (still loads local dirs and existing HF cache snapshots).
pub fn resolve_embedder(
    root: &Path,
    domain: EmbedDomain,
) -> Result<Option<Arc<dyn Embedder>>, String> {
    let backend = std::env::var("WORDKEEP_EMBED_BACKEND")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match backend.as_str() {
        "none" | "off" | "0" | "disabled" => return Ok(None),
        "m2v" | "model2vec" => {
            #[cfg(feature = "semantic-m2v")]
            {
                return m2v::load(root, domain);
            }
            #[cfg(not(feature = "semantic-m2v"))]
            {
                let _ = (root, domain);
                return Err("WORDKEEP_EMBED_BACKEND=m2v requires --features semantic-m2v".into());
            }
        }
        "fastembed" => {
            #[cfg(feature = "embeddings-fastembed")]
            {
                let _ = (root, domain);
                return fastembed_compat::load().map(|e| Some(e as Arc<dyn Embedder>));
            }
            #[cfg(not(feature = "embeddings-fastembed"))]
            {
                let _ = (root, domain);
                return Err(
                    "WORDKEEP_EMBED_BACKEND=fastembed requires --features embeddings-fastembed"
                        .into(),
                );
            }
        }
        _ => {}
    }

    #[cfg(feature = "semantic-m2v")]
    {
        match m2v::load(root, domain) {
            Ok(Some(emb)) => return Ok(Some(emb)),
            Ok(None) => return Ok(None),
            Err(m2v_err) => {
                #[cfg(feature = "embeddings-fastembed")]
                {
                    if let Ok(fb) = fastembed_compat::load() {
                        return Ok(Some(fb));
                    }
                }
                return Err(m2v_err);
            }
        }
    }

    #[cfg(all(not(feature = "semantic-m2v"), feature = "embeddings-fastembed"))]
    {
        let _ = (root, domain);
        return fastembed_compat::load().map(|e| Some(e as Arc<dyn Embedder>));
    }

    #[cfg(not(any(feature = "semantic-m2v", feature = "embeddings-fastembed")))]
    {
        let _ = (root, domain);
        Ok(None)
    }
}

/// Fetch cached embeddings from SQLite `embeddings(key_sha256, model_tag)` and
/// batch-embed only cache misses via `embedder`, persisting new vectors in one
/// `BEGIN IMMEDIATE` transaction.
///
/// `items` is a slice of `(content_sha256, text)` pairs. `priority_order` lists
/// preferred indices to embed first when `embedder.max_uncached_per_query()`
/// caps cold work (e.g., legacy ONNX `fastembed`). For fast static Model2Vec
/// (`max_uncached_per_query() == None`), every uncached item is embedded.
pub fn embed_cached(
    store: Option<&Store>,
    embedder: &dyn Embedder,
    items: &[(String, String)],
    priority_order: &[usize],
) -> Result<(Vec<Option<Arc<Vec<f32>>>>, EmbedCacheStats), String> {
    if items.is_empty() {
        return Ok((Vec::new(), EmbedCacheStats::default()));
    }
    let tag = embedder.model_tag();
    let mut cached_by_sha: HashMap<&str, Option<Arc<Vec<f32>>>> =
        HashMap::with_capacity(items.len());
    let mut stats = EmbedCacheStats::default();

    // Process RAM cache first; bulk-SQLite only the remaining misses.
    let mut need_sql: Vec<String> = Vec::new();
    {
        let ram = embed_ram_cache().lock().ok();
        let by_tag = ram.as_ref().and_then(|g| g.get(tag));
        for (sha, _) in items {
            if cached_by_sha.contains_key(sha.as_str()) {
                continue;
            }
            if let Some(v) = by_tag.and_then(|m| m.get(sha.as_str())) {
                cached_by_sha.insert(sha.as_str(), Some(Arc::clone(v)));
                continue;
            }
            need_sql.push(sha.clone());
        }
    }
    need_sql.sort();
    need_sql.dedup();
    if !need_sql.is_empty() {
        if let Some(st) = store {
            if let Ok(batch) = st.get_embeddings_batch(&need_sql, tag) {
                if let Ok(mut ram) = embed_ram_cache().lock() {
                    let slot = ram.entry(tag.to_string()).or_default();
                    for (k, v) in &batch {
                        if !v.is_empty() {
                            slot.insert(k.clone(), Arc::new(v.clone()));
                        }
                    }
                }
                for (sha, _) in items {
                    if cached_by_sha.contains_key(sha.as_str()) {
                        continue;
                    }
                    let found = batch
                        .get(sha)
                        .filter(|v| !v.is_empty())
                        .map(|v| Arc::new(v.clone()));
                    cached_by_sha.insert(sha.as_str(), found);
                }
            }
        }
    }
    for (sha, _) in items {
        if !cached_by_sha.contains_key(sha.as_str()) {
            cached_by_sha.insert(sha.as_str(), None);
        }
    }

    let mut out: Vec<Option<Arc<Vec<f32>>>> = vec![None; items.len()];
    for (idx, (sha, _)) in items.iter().enumerate() {
        if let Some(Some(vec)) = cached_by_sha.get(sha.as_str()) {
            out[idx] = Some(Arc::clone(vec));
            stats.hits += 1;
        }
    }

    // Collect distinct uncached content hashes in priority order, then scan order.
    let mut miss_shas: Vec<(&str, &str)> = Vec::new();
    let mut seen_miss: HashSet<&str> = HashSet::new();
    let cap = embedder.max_uncached_per_query();

    let mut push_miss = |idx: usize| {
        if idx >= items.len() {
            return;
        }
        if let Some(limit) = cap {
            if miss_shas.len() >= limit {
                return;
            }
        }
        if out[idx].is_some() {
            return;
        }
        let (sha, text) = &items[idx];
        // Empty text means "cache probe only" (warm path); skip re-embed.
        if text.is_empty() {
            return;
        }
        if seen_miss.insert(sha.as_str()) {
            miss_shas.push((sha.as_str(), text.as_str()));
        }
    };

    for &idx in priority_order {
        push_miss(idx);
    }
    for idx in 0..items.len() {
        push_miss(idx);
    }

    if !miss_shas.is_empty() {
        let batch_texts: Vec<String> = miss_shas.iter().map(|(_, t)| (*t).to_string()).collect();
        let computed = embedder.embed_batch(&batch_texts)?;
        if computed.len() != miss_shas.len() {
            return Err(format!(
                "embedder {tag} returned {} vectors for {} inputs",
                computed.len(),
                miss_shas.len()
            ));
        }
        stats.misses = miss_shas.len();

        let mut newly_computed: HashMap<&str, Arc<Vec<f32>>> =
            HashMap::with_capacity(miss_shas.len());
        for ((sha, _), vec) in miss_shas.iter().zip(computed.into_iter()) {
            if !vec.is_empty() {
                newly_computed.insert(*sha, Arc::new(vec));
            }
        }

        if let Some(st) = store {
            let persist_ok = st.with_immediate_tx(|s| {
                for (sha, vec) in &newly_computed {
                    s.upsert_embedding(sha, tag, vec.as_slice())?;
                }
                Ok(())
            });
            if persist_ok.is_ok() {
                stats.persisted = newly_computed.len();
            }
        }
        if let Ok(mut ram) = embed_ram_cache().lock() {
            let slot = ram.entry(tag.to_string()).or_default();
            for (sha, vec) in &newly_computed {
                slot.insert((*sha).to_string(), Arc::clone(vec));
            }
        }

        for (idx, (sha, _)) in items.iter().enumerate() {
            if out[idx].is_none() {
                if let Some(vec) = newly_computed.get(sha.as_str()) {
                    out[idx] = Some(Arc::clone(vec));
                }
            }
        }
    }

    Ok((out, stats))
}

/// Cosine similarity in `[-1.0, 1.0]` (`0.0` for empty or zero-norm vectors).
pub fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f64, 0.0f64, 0.0f64);
    for i in 0..n {
        let (x, y) = (a[i] as f64, b[i] as f64);
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// Weighted Reciprocal Rank Fusion over N ranked candidate index lists.
///
/// Each entry in `lists` is `(ranked_indices, weight)` where `ranked_indices[0]`
/// has rank 1. Duplicate indices inside a single list are ignored after their
/// best rank. Returns `(scaled_rrf_score, idx)` sorted descending by score with
/// deterministic ascending `idx` tie-breaking.
///
/// Scores are multiplied by `(k + 1.0)` (strictly monotone scaling) so a
/// single-list rank-1 hit with weight `1.0` reports `1.00`, keeping `{:.2}`
/// output readable while preserving exact RRF ordering.
pub fn rrf_fuse(lists: &[(&[usize], f64)], k: f64) -> Vec<(f64, usize)> {
    let k = k.max(1.0);
    let scale = k + 1.0;
    let mut acc: HashMap<usize, f64> = HashMap::new();
    for (ranked, weight) in lists {
        if *weight <= 0.0 {
            continue;
        }
        let mut seen: HashSet<usize> = HashSet::with_capacity(ranked.len());
        let mut rank = 0usize;
        for &idx in *ranked {
            if !seen.insert(idx) {
                continue;
            }
            rank += 1;
            *acc.entry(idx).or_insert(0.0) += (*weight * scale) / (k + rank as f64);
        }
    }
    let mut out: Vec<(f64, usize)> = acc
        .into_iter()
        .filter(|(_, s)| *s > 0.0)
        .map(|(idx, s)| (s, idx))
        .collect();
    out.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.cmp(&b.1))
    });
    out
}

#[cfg(any(feature = "semantic-m2v", feature = "embeddings-fastembed"))]
fn offline_requested() -> bool {
    for key in [
        "WORDKEEP_EMBED_OFFLINE",
        "WORDKEEP_M2V_OFFLINE",
        "HF_HUB_OFFLINE",
    ] {
        if let Ok(v) = std::env::var(key) {
            let trimmed = v.trim();
            if matches!(trimmed, "1" | "true" | "TRUE" | "yes" | "YES") {
                return true;
            }
        }
    }
    false
}

#[cfg(feature = "semantic-m2v")]
mod m2v {
    use super::*;
    use model2vec_rs::model::StaticModel;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};

    static MODELS: OnceLock<Mutex<HashMap<String, Arc<Model2VecEmbedder>>>> = OnceLock::new();

    pub struct Model2VecEmbedder {
        model: StaticModel,
        tag: String,
    }

    impl Embedder for Model2VecEmbedder {
        fn model_tag(&self) -> &str {
            &self.tag
        }

        fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
            if texts.is_empty() {
                return Ok(Vec::new());
            }
            Ok(self.model.encode(texts))
        }
    }

    fn tag_from_source(source: &str, default_tag: &str) -> String {
        let trimmed = source.trim().trim_end_matches('/');
        if trimmed == MODEL_M2V_DOCS_DEFAULT {
            return TAG_M2V_DOCS_DEFAULT.to_string();
        }
        if trimmed == MODEL_M2V_CODE_DEFAULT {
            return TAG_M2V_CODE_DEFAULT.to_string();
        }
        let leaf = Path::new(trimmed)
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(default_tag);
        if Path::new(trimmed).exists() {
            format!("m2v:{leaf}")
        } else {
            leaf.to_string()
        }
    }

    fn find_cached_hf_snapshot(repo_id: &str) -> Option<PathBuf> {
        let hub_root = std::env::var_os("HF_HOME")
            .map(|h| PathBuf::from(h).join("hub"))
            .or_else(|| {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache/huggingface/hub"))
            })?;
        let dir_name = format!("models--{}", repo_id.replace('/', "--"));
        let snapshots = hub_root.join(dir_name).join("snapshots");
        let entries = std::fs::read_dir(snapshots).ok()?;
        for entry in entries.flatten() {
            let p = entry.path();
            if p.join("tokenizer.json").is_file() && p.join("model.safetensors").is_file() {
                return Some(p);
            }
        }
        None
    }

    fn resolve_source(root: &Path, domain: EmbedDomain) -> Option<(String, String)> {
        let (env_key, default_repo, short_name) = match domain {
            EmbedDomain::Docs => (
                "WORDKEEP_M2V_DOCS_MODEL",
                MODEL_M2V_DOCS_DEFAULT,
                TAG_M2V_DOCS_DEFAULT,
            ),
            EmbedDomain::Code => (
                "WORDKEEP_M2V_CODE_MODEL",
                MODEL_M2V_CODE_DEFAULT,
                TAG_M2V_CODE_DEFAULT,
            ),
        };
        if let Ok(raw) = std::env::var(env_key) {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                if matches!(
                    trimmed.to_ascii_lowercase().as_str(),
                    "none" | "off" | "0" | "disabled"
                ) {
                    return None;
                }
                let tag = tag_from_source(trimmed, short_name);
                return Some((trimmed.to_string(), tag));
            }
        }
        let ws_local = root.join(".wordkeep/models").join(short_name);
        if ws_local.is_dir() {
            return Some((ws_local.to_string_lossy().into_owned(), short_name.into()));
        }
        let cache_local = crate::cache::dir().join("models").join(short_name);
        if cache_local.is_dir() {
            return Some((
                cache_local.to_string_lossy().into_owned(),
                short_name.into(),
            ));
        }
        Some((default_repo.to_string(), short_name.to_string()))
    }

    pub fn load(root: &Path, domain: EmbedDomain) -> Result<Option<Arc<dyn Embedder>>, String> {
        let Some((source, tag)) = resolve_source(root, domain) else {
            return Ok(None);
        };
        let quantize_i8 = std::env::var("WORDKEEP_M2V_QUANTIZE_INT8")
            .ok()
            .map(|v| matches!(v.trim(), "1" | "true" | "TRUE"))
            .unwrap_or(false);
        let cache_key = format!("{source}|q8={quantize_i8}");

        let map = MODELS.get_or_init(|| Mutex::new(HashMap::new()));
        {
            let guard = map.lock().map_err(|_| "m2v cache lock poisoned".to_string())?;
            if let Some(existing) = guard.get(&cache_key) {
                return Ok(Some(Arc::clone(existing) as Arc<dyn Embedder>));
            }
        }

        let load_path = if Path::new(&source).exists() {
            source.clone()
        } else if offline_requested() {
            if let Some(snap) = find_cached_hf_snapshot(&source) {
                snap.to_string_lossy().into_owned()
            } else {
                return Err(format!(
                    "model2vec offline and '{source}' not found in local paths or HF cache"
                ));
            }
        } else {
            source.clone()
        };

        let hf_token = std::env::var("HF_TOKEN").ok().filter(|t| !t.trim().is_empty());
        let mut model = StaticModel::from_pretrained(&load_path, hf_token.as_deref(), None, None)
            .map_err(|e| format!("model2vec load '{source}': {e}"))?;
        if quantize_i8 {
            model = model.quantize_int8();
        }
        let emb = Arc::new(Model2VecEmbedder { model, tag });
        let mut guard = map.lock().map_err(|_| "m2v cache lock poisoned".to_string())?;
        guard.insert(cache_key, Arc::clone(&emb));
        Ok(Some(emb as Arc<dyn Embedder>))
    }
}

#[cfg(feature = "embeddings-fastembed")]
mod fastembed_compat {
    use super::*;
    use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
    use std::sync::{Mutex, OnceLock};

    static MODEL: OnceLock<Mutex<Option<TextEmbedding>>> = OnceLock::new();
    static SINGLETON: OnceLock<Arc<FastEmbedder>> = OnceLock::new();

    pub struct FastEmbedder;

    impl Embedder for FastEmbedder {
        fn model_tag(&self) -> &str {
            TAG_FASTEMBED_LEGACY
        }

        fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
            if texts.is_empty() {
                return Ok(Vec::new());
            }
            let cell = ensure_model()?;
            let mut guard = cell
                .lock()
                .map_err(|_| "fastembed model lock poisoned".to_string())?;
            let model = guard.as_mut().ok_or("fastembed model unavailable")?;
            model
                .embed(texts.to_vec(), None)
                .map_err(|e| format!("fastembed embed: {e}"))
        }

        fn max_uncached_per_query(&self) -> Option<usize> {
            Some(48)
        }
    }

    fn ensure_model() -> Result<&'static Mutex<Option<TextEmbedding>>, String> {
        let cell = MODEL.get_or_init(|| Mutex::new(None));
        let mut guard = cell
            .lock()
            .map_err(|_| "fastembed model lock poisoned".to_string())?;
        if guard.is_none() {
            if offline_requested() {
                return Err("fastembed disabled under offline mode".into());
            }
            let model = TextEmbedding::try_new(InitOptions::new(EmbeddingModel::AllMiniLML6V2))
                .map_err(|e| format!("fastembed init: {e}"))?;
            *guard = Some(model);
        }
        Ok(cell)
    }

    pub fn load() -> Result<Arc<FastEmbedder>, String> {
        let _ = ensure_model()?;
        Ok(Arc::clone(
            SINGLETON.get_or_init(|| Arc::new(FastEmbedder)),
        ))
    }
}

#[cfg(test)]
pub(crate) mod test_helpers {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Deterministic offline stub embedder for unit tests.
    ///
    /// Maps synonym clusters (`clusters[d]` -> basis axis `d`) into unit-length
    /// vectors so zero-overlap synonyms share cosine similarity `1.0` while
    /// unrelated texts remain orthogonal (`0.0`). Tracks `embed_calls` to verify
    /// SQLite `embeddings` cache hits.
    pub struct KeywordStubEmbedder {
        tag: String,
        clusters: Vec<Vec<String>>,
        pub embed_calls: AtomicUsize,
        pub texts_embedded: AtomicUsize,
    }

    impl KeywordStubEmbedder {
        pub fn new(tag: &str, clusters: &[&[&str]]) -> Self {
            Self {
                tag: tag.to_string(),
                clusters: clusters
                    .iter()
                    .map(|g| g.iter().map(|s| s.to_ascii_lowercase()).collect())
                    .collect(),
                embed_calls: AtomicUsize::new(0),
                texts_embedded: AtomicUsize::new(0),
            }
        }

        pub fn calls(&self) -> usize {
            self.embed_calls.load(Ordering::Relaxed)
        }

        pub fn embedded_count(&self) -> usize {
            self.texts_embedded.load(Ordering::Relaxed)
        }

        fn encode_text(&self, text: &str) -> Vec<f32> {
            let lower = text.to_ascii_lowercase();
            let mut v = vec![0.0f32; self.clusters.len().max(1)];
            for (axis, words) in self.clusters.iter().enumerate() {
                let mut hits = 0.0f32;
                for w in words {
                    if lower.contains(w.as_str()) {
                        hits += 1.0;
                    }
                }
                v[axis] = hits;
            }
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                for x in &mut v {
                    *x /= norm;
                }
            }
            v
        }
    }

    impl Embedder for KeywordStubEmbedder {
        fn model_tag(&self) -> &str {
            &self.tag
        }

        fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
            self.embed_calls.fetch_add(1, Ordering::Relaxed);
            self.texts_embedded.fetch_add(texts.len(), Ordering::Relaxed);
            Ok(texts.iter().map(|t| self.encode_text(t)).collect())
        }
    }

    /// Write a tiny (~1 KB) valid local Model2Vec directory (`config.json`,
    /// WordPiece `tokenizer.json`, and `model.safetensors`) loadable directly by
    /// `model2vec_rs::model::StaticModel::from_pretrained` with zero network I/O.
    #[cfg(feature = "semantic-m2v")]
    pub fn write_synthetic_m2v_model(
        dir: &Path,
        vocab_vectors: &[(&str, &[f32])],
    ) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let dim = vocab_vectors
            .first()
            .map(|(_, v)| v.len())
            .filter(|&d| d > 0)
            .ok_or("vocab_vectors must be non-empty with dim > 0")?;

        // Build vocab with `[UNK]` at ID 0.
        let mut vocab_map = serde_json::Map::new();
        vocab_map.insert("[UNK]".to_string(), serde_json::json!(0));
        let mut raw_floats: Vec<f32> = vec![0.0; dim];
        for (i, (tok, vec)) in vocab_vectors.iter().enumerate() {
            if vec.len() != dim {
                return Err(format!("dim mismatch for token {tok}"));
            }
            vocab_map.insert(tok.to_ascii_lowercase(), serde_json::json!(i + 1));
            raw_floats.extend_from_slice(vec);
        }
        let rows = vocab_vectors.len() + 1;

        let cfg = serde_json::json!({
            "normalize": true,
            "max_length": 512
        });
        std::fs::write(
            dir.join("config.json"),
            serde_json::to_vec(&cfg).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;

        let tok_json = serde_json::json!({
            "added_tokens": [],
            "normalizer": {
                "type": "BertNormalizer",
                "clean_text": true,
                "handle_chinese_chars": true,
                "strip_accents": true,
                "lowercase": true
            },
            "pre_tokenizer": { "type": "BertPreTokenizer" },
            "model": {
                "type": "WordPiece",
                "unk_token": "[UNK]",
                "continuing_subword_prefix": "##",
                "max_input_chars_per_word": 100,
                "vocab": vocab_map
            }
        });
        std::fs::write(
            dir.join("tokenizer.json"),
            serde_json::to_vec(&tok_json).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;

        let byte_len = raw_floats.len() * 4;
        let header = serde_json::json!({
            "embeddings": {
                "dtype": "F32",
                "shape": [rows, dim],
                "data_offsets": [0, byte_len]
            }
        });
        let header_bytes = serde_json::to_vec(&header).map_err(|e| e.to_string())?;
        let mut st_bytes = Vec::with_capacity(8 + header_bytes.len() + byte_len);
        st_bytes.extend_from_slice(&(header_bytes.len() as u64).to_le_bytes());
        st_bytes.extend_from_slice(&header_bytes);
        for f in raw_floats {
            st_bytes.extend_from_slice(&f.to_le_bytes());
        }
        std::fs::write(dir.join("model.safetensors"), st_bytes).map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::sha256_hex;

    #[test]
    fn cosine_basics() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-9);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-9);
        assert!((cosine(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-9);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
        assert_eq!(cosine(&[], &[1.0]), 0.0);
    }

    #[test]
    fn rrf_fuse_combines_three_lists_and_surfaces_zero_overlap_dense_hit() {
        let fts = [0usize, 1];
        let bm25 = [1usize, 0];
        let dense = [2usize, 1]; // doc 2 has zero lexical overlap!
        let fused = rrf_fuse(&[(&fts, 1.0), (&bm25, 1.0), (&dense, 1.2)], RRF_K);
        // Doc 1 appears in all 3 lists -> ranks #1.
        assert_eq!(fused[0].1, 1, "{fused:?}");
        // Doc 0 appears in 2 lexical lists -> ranks #2.
        assert_eq!(fused[1].1, 0, "{fused:?}");
        // Doc 2 appears ONLY in dense -> still surfaced!
        assert_eq!(fused[2].1, 2, "{fused:?}");
        assert!(fused[2].0 > 1.0, "scaled RRF score should be readable: {fused:?}");

        // Pure zero-overlap case: both lexical lists empty, only dense returns hit 7.
        let empty: [usize; 0] = [];
        let only_dense = [7usize];
        let zero_ov = rrf_fuse(&[(&empty, 1.0), (&empty, 1.0), (&only_dense, 1.2)], RRF_K);
        assert_eq!(zero_ov.len(), 1);
        assert_eq!(zero_ov[0].1, 7);
    }

    #[test]
    fn embed_cached_persists_by_sha256_and_skips_reembedding_on_warm_call() {
        let _env = crate::cache::test_env_lock();
        let dir = std::env::temp_dir().join(format!("wk_emb_cache_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir).expect("store");
        let stub = test_helpers::KeywordStubEmbedder::new(
            TAG_M2V_DOCS_DEFAULT,
            &[&["arena", "slab"], &["audio", "mixer"]],
        );

        let items = vec![
            (sha256_hex("slab allocator"), "slab allocator".to_string()),
            (sha256_hex("audio mixer"), "audio mixer".to_string()),
        ];

        // Cold pass: embeds both and persists them to SQLite embeddings table.
        let (v1, st1) = embed_cached(Some(&store), &stub, &items, &[]).unwrap();
        assert_eq!(st1.hits, 0);
        assert_eq!(st1.misses, 2);
        assert_eq!(st1.persisted, 2);
        assert_eq!(stub.calls(), 1);
        assert!(v1[0].is_some() && v1[1].is_some());

        // Direct store lookup confirms content_sha256 + model_tag key.
        let stored = store
            .get_embedding(&items[0].0, TAG_M2V_DOCS_DEFAULT)
            .unwrap()
            .expect("cached embedding in sqlite");
        assert_eq!(stored, v1[0].as_ref().unwrap().as_slice());

        // Warm pass: zero calls to embedder, 100% SQLite hits.
        let (v2, st2) = embed_cached(Some(&store), &stub, &items, &[]).unwrap();
        assert_eq!(st2.hits, 2);
        assert_eq!(st2.misses, 0);
        assert_eq!(stub.calls(), 1, "warm pass must not re-embed");
        assert_eq!(v1, v2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "semantic-m2v")]
    #[test]
    fn model2vec_loads_local_synthetic_model_and_encodes() {
        let dir = std::env::temp_dir().join(format!("wk_m2v_synth_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let model_dir = dir.join(".wordkeep/models").join(TAG_M2V_DOCS_DEFAULT);
        test_helpers::write_synthetic_m2v_model(
            &model_dir,
            &[
                ("arena", &[1.0, 0.0, 0.0]),
                ("allocator", &[1.0, 0.0, 0.0]),
                ("slab", &[0.95, 0.05, 0.0]),
                ("recycler", &[0.95, 0.05, 0.0]),
                ("audio", &[0.0, 1.0, 0.0]),
                ("mixer", &[0.0, 1.0, 0.0]),
            ],
        )
        .unwrap();

        let emb = m2v::load(&dir, EmbedDomain::Docs)
            .expect("load synthetic local m2v")
            .expect("embedder present");
        assert_eq!(emb.model_tag(), TAG_M2V_DOCS_DEFAULT);
        let q = emb.embed_one("arena allocator").unwrap();
        let slab = emb.embed_one("slab recycler").unwrap();
        let audio = emb.embed_one("audio mixer").unwrap();
        assert!(
            cosine(&q, &slab) > 0.95,
            "zero-overlap synonyms must align: {}",
            cosine(&q, &slab)
        );
        assert!(
            cosine(&q, &audio).abs() < 0.05,
            "unrelated topics must stay orthogonal: {}",
            cosine(&q, &audio)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Optional real HuggingFace potion model smoke test (skipped unless
    /// `WORDKEEP_TEST_REAL_M2V=1` is set in the environment).
    #[cfg(feature = "semantic-m2v")]
    #[test]
    fn model2vec_real_potion_models_when_env_enabled() {
        if std::env::var("WORDKEEP_TEST_REAL_M2V").ok().as_deref() != Some("1") {
            return;
        }
        let dir = std::env::temp_dir().join(format!("wk_m2v_real_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let docs_emb = resolve_embedder(&dir, EmbedDomain::Docs)
            .expect("load potion-retrieval-32M")
            .expect("docs embedder");
        assert_eq!(docs_emb.model_tag(), TAG_M2V_DOCS_DEFAULT);
        let q = docs_emb.embed_one("frame rate stutter hitch").unwrap();
        let rel = docs_emb.embed_one("rendering latency spike perf").unwrap();
        let unr = docs_emb.embed_one("chocolate cake baking recipe").unwrap();
        assert!(
            cosine(&q, &rel) > cosine(&q, &unr),
            "potion-retrieval-32M should prefer perf over cake"
        );

        let code_emb = resolve_embedder(&dir, EmbedDomain::Code)
            .expect("load potion-code-16M-v2")
            .expect("code embedder");
        assert_eq!(code_emb.model_tag(), TAG_M2V_CODE_DEFAULT);
        let cq = code_emb.embed_one("calculate rectangle area").unwrap();
        let crel = code_emb
            .embed_one("function widget_area int widget_area(int w, int h)")
            .unwrap();
        let cunr = code_emb
            .embed_one("function parse_json_escape const char* src")
            .unwrap();
        assert!(
            cosine(&cq, &crel) > cosine(&cq, &cunr),
            "potion-code-16M-v2 should prefer area calc over json escape"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
