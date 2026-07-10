//! Optional semantic reranking for knowledge_search (feature = "embeddings").
//!
//! Embeds the query and the BM25 top candidates with a small local model
//! (fastembed / ONNX Runtime), then blends normalized BM25 with cosine
//! similarity. The model is lazily loaded once and reused for the process
//! lifetime. BM25 remains the fallback if the model can't initialize.
//!
//! Chunk embeddings are cached by content hash in memory and on disk, so an
//! embeddings build skips re-embedding unchanged chunks across queries and across
//! cold starts. The query itself is always embedded fresh (never cached).

use super::Chunk;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

static MODEL: OnceLock<Mutex<Option<TextEmbedding>>> = OnceLock::new();
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

const HEAD: usize = 24;
/// Identifies the model in the cache key/filename so a model swap can't mix dims.
const MODEL_TAG: &str = "allminilml6v2";

fn ensure_model() -> Result<&'static Mutex<Option<TextEmbedding>>, String> {
    let cell = MODEL.get_or_init(|| Mutex::new(None));
    {
        let mut guard = cell.lock().map_err(|_| "model lock poisoned".to_string())?;
        if guard.is_none() {
            let model = TextEmbedding::try_new(InitOptions::new(EmbeddingModel::AllMiniLML6V2))
                .map_err(|e| format!("model init: {e}"))?;
            *guard = Some(model);
        }
    }
    Ok(cell)
}

/// Rerank the BM25-sorted `scored` list by blending normalized BM25 with embedding
/// cosine similarity over the top `HEAD` candidates. Returns a new ordering; the
/// tail beyond `HEAD` keeps its BM25 order.
pub fn rerank(
    query: &str,
    chunks: &[Chunk],
    scored: Vec<(f64, usize)>,
) -> Result<Vec<(f64, usize)>, String> {
    if scored.len() <= 1 {
        return Ok(scored);
    }
    let head_n = scored.len().min(HEAD);

    let cell = ensure_model()?;
    let mut guard = cell.lock().map_err(|_| "model lock poisoned".to_string())?;
    let model = guard.as_mut().ok_or("model unavailable")?;

    // Candidate texts and their content-hash keys.
    let mut texts: Vec<String> = Vec::with_capacity(head_n);
    let mut keys: Vec<u64> = Vec::with_capacity(head_n);
    for (_, idx) in scored.iter().take(head_n) {
        let c = &chunks[*idx];
        let t = format!("{}\n{}", c.heading, c.body);
        keys.push(fnv1a(&[MODEL_TAG, &t]));
        texts.push(t);
    }

    let cache = CACHE.get_or_init(|| Mutex::new(Cache::load(cache_path())));
    let mut store = cache
        .lock()
        .map_err(|_| "embed cache lock poisoned".to_string())?;

    // Only the cache misses need embedding; the query is always embedded fresh.
    let misses: Vec<usize> = (0..head_n).filter(|&i| !store.contains(keys[i])).collect();
    let mut batch: Vec<String> = Vec::with_capacity(misses.len() + 1);
    batch.push(query.to_string());
    for &i in &misses {
        batch.push(texts[i].clone());
    }
    let embs = model
        .embed(batch, None)
        .map_err(|e| format!("embed: {e}"))?;
    let q = embs[0].clone();
    for (j, &i) in misses.iter().enumerate() {
        store.insert(keys[i], embs[j + 1].clone());
    }
    store.save();

    let max_bm = scored[..head_n]
        .iter()
        .map(|(s, _)| *s)
        .fold(f64::MIN, f64::max)
        .max(1e-9);

    let mut head: Vec<(f64, usize)> = Vec::with_capacity(head_n);
    for (i, (bm, idx)) in scored.iter().take(head_n).enumerate() {
        let sim = store.get(keys[i]).map(|e| cosine(&q, e)).unwrap_or(0.0);
        let sim01 = ((sim + 1.0) / 2.0).clamp(0.0, 1.0);
        let bm01 = (bm / max_bm).clamp(0.0, 1.0);
        head.push((0.5 * bm01 + 0.5 * sim01, *idx));
    }
    head.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut out = head;
    out.extend(scored.into_iter().skip(head_n));
    Ok(out)
}

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let (mut dot, mut na, mut nb) = (0f64, 0f64, 0f64);
    for i in 0..a.len().min(b.len()) {
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

/// 64-bit FNV-1a over the given parts (with a separator), used for stable,
/// process-independent cache keys.
fn fnv1a(parts: &[&str]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in p.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Where the chunk-embedding cache lives: the shared cache dir, with a filename
/// tagged by the model so embedding dimensions never mix.
fn cache_path() -> PathBuf {
    crate::cache::dir().join(format!("emb-{MODEL_TAG}.cbe"))
}

/// content-hash → embedding cache, persisted as a small fixed-width binary blob.
struct Cache {
    dim: usize,
    map: HashMap<u64, Vec<f32>>,
    path: PathBuf,
    dirty: bool,
}

impl Cache {
    /// Best-effort load; any IO/format error yields an empty (in-memory) cache.
    fn load(path: PathBuf) -> Cache {
        let mut c = Cache {
            dim: 0,
            map: HashMap::new(),
            path,
            dirty: false,
        };
        let Ok(bytes) = std::fs::read(&c.path) else {
            return c;
        };
        if bytes.len() < 12 || &bytes[0..4] != b"CBE1" {
            return c;
        }
        let dim = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
        let count = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
        if dim == 0 {
            return c;
        }
        let rec = 8 + dim * 4;
        let mut off = 12;
        for _ in 0..count {
            if off + rec > bytes.len() {
                break;
            }
            let key = u64::from_le_bytes(bytes[off..off + 8].try_into().unwrap());
            let mut v = Vec::with_capacity(dim);
            let mut o = off + 8;
            for _ in 0..dim {
                v.push(f32::from_le_bytes(bytes[o..o + 4].try_into().unwrap()));
                o += 4;
            }
            c.map.insert(key, v);
            off += rec;
        }
        c.dim = dim;
        c
    }

    fn contains(&self, key: u64) -> bool {
        self.map.contains_key(&key)
    }

    fn get(&self, key: u64) -> Option<&Vec<f32>> {
        self.map.get(&key)
    }

    fn insert(&mut self, key: u64, v: Vec<f32>) {
        if self.dim == 0 {
            self.dim = v.len();
        }
        if v.len() != self.dim {
            return; // dim mismatch (stale model) - skip rather than corrupt the file
        }
        self.map.insert(key, v);
        self.dirty = true;
    }

    /// Persist atomically (temp file + rename) when there's something new to write.
    fn save(&mut self) {
        if !self.dirty || self.dim == 0 {
            return;
        }
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut buf = Vec::with_capacity(12 + self.map.len() * (8 + self.dim * 4));
        buf.extend_from_slice(b"CBE1");
        buf.extend_from_slice(&(self.dim as u32).to_le_bytes());
        buf.extend_from_slice(&(self.map.len() as u32).to_le_bytes());
        for (k, v) in &self.map {
            buf.extend_from_slice(&k.to_le_bytes());
            for x in v {
                buf.extend_from_slice(&x.to_le_bytes());
            }
        }
        let tmp = self.path.with_extension("cbe.tmp");
        if std::fs::write(&tmp, &buf).is_ok() && std::fs::rename(&tmp, &self.path).is_ok() {
            self.dirty = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a_is_stable_and_separates_parts() {
        // Deterministic across calls.
        assert_eq!(fnv1a(&["a", "b"]), fnv1a(&["a", "b"]));
        // The separator prevents `["ab",""]` colliding with `["a","b"]`.
        assert_ne!(fnv1a(&["ab", ""]), fnv1a(&["a", "b"]));
        // Model tag participates in the key.
        assert_ne!(fnv1a(&["m1", "text"]), fnv1a(&["m2", "text"]));
    }

    #[test]
    fn cosine_basics() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-9);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-9);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn cache_round_trips_through_disk() {
        let path = std::env::temp_dir()
            .join(format!("cbtest_emb_{}", std::process::id()))
            .join("emb.cbe");
        let _ = std::fs::remove_file(&path);

        let mut c = Cache::load(path.clone());
        assert!(!c.contains(7));
        c.insert(7, vec![0.1, 0.2, 0.3]);
        c.insert(9, vec![0.4, 0.5, 0.6]);
        c.save();

        let reloaded = Cache::load(path.clone());
        assert_eq!(reloaded.dim, 3);
        assert!(reloaded.contains(7));
        assert_eq!(reloaded.get(9), Some(&vec![0.4, 0.5, 0.6]));

        // Dimension mismatch is rejected, not corrupting the store.
        let mut c2 = Cache::load(path.clone());
        c2.insert(11, vec![1.0, 2.0]); // wrong dim → ignored
        assert!(!c2.contains(11));

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
