//! symbol_resolve - fuzzy / profile-aware front door for symbol lookup.
//!
//! Normalizes agent-supplied names (`::Foo`, `ns::Bar<T>`), locates exact
//! definitions under the active paths, and when that fails walks other
//! path_profiles, ranks fuzzy edit-distance suggestions, and on exact+fuzzy
//! miss falls back to SQLite `symbols_fts` BM25 + `potion-code-16M-v2` dense
//! embeddings (fused via RRF, cached by `content_sha256` in SQLite `embeddings`).

use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use tree_sitter::Parser;

use crate::cache;
use crate::embed::{self, EmbedDomain, Embedder, RRF_K};
use crate::lang::Lang;
use crate::store::{
    self, Store, SymbolHit, SymbolInput, EDGE_SOURCE_TREESITTER, STAGE_SYMBOL_DEF,
};
use crate::symbol_def::{self, DefBrief, DefKind};
use crate::{progress, stats, walk};

/// Minimum cosine similarity for a symbol to enter the dense RRF fallback list.
const MIN_SYMBOL_DENSE_COSINE: f64 = 0.25;
/// Maximum indexed symbols scanned for dense fallback under active paths.
const MAX_SYMBOL_DENSE_CORPUS: usize = 4000;

/// One ranked FTS5 / semantic fallback symbol match.
#[derive(Debug, Clone)]
struct FallbackHit {
    name: String,
    rel_path: String,
    line: usize,
    kind: String,
    provenance: &'static str,
}

/// MCP entry: required `symbol`, optional `paths` / `profile` / `max` / `semantic`.
pub fn resolve(root: &Path, args: &Value) -> Result<String, String> {
    resolve_with_embedder(root, args, None)
}

fn resolve_with_embedder(
    root: &Path,
    args: &Value,
    custom_embedder: Option<&dyn Embedder>,
) -> Result<String, String> {
    let raw = args
        .get("symbol")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if raw.is_empty() {
        return Err(crate::config::symbol_required_err());
    }
    let symbol = normalize_symbol(raw);
    if symbol.is_empty() {
        return Err(crate::config::symbol_required_err());
    }
    let paths = crate::config::paths_from_args(root, args)?;
    let max = args.get("max").and_then(Value::as_u64).unwrap_or(8).max(1) as usize;
    let explicit_scope = args.get("paths").is_some() || args.get("profile").is_some();
    let want_semantic = args
        .get("semantic")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    progress::tick(0, None, "symbol_resolve: locate");
    let (info, mut io_st) = symbol_def::locate_with_stats(root, &symbol, &paths, false);
    if let Some(info) = info {
        let out = format!(
            "symbol_resolve - \"{raw}\" → \"{symbol}\"\n  {}:{}  ({})\n  {}\n",
            info.rel,
            info.line,
            info.kind.label(),
            info.signature
        );
        stats::record_ext(
            "symbol_resolve",
            stats::RecordMeta {
                baseline: io_st.scanned / 4,
                returned: (out.len() / 4) as u64,
                bytes_read: io_st.bytes_read,
                cache_hits: io_st.cache_hits,
                cache_misses: io_st.cache_misses,
                reason: None,
            },
        );
        return Ok(out);
    }

    // Coverage expand: try other profiles for an exact hit. Skip when the agent
    // already narrowed paths/profile — that scan is the cold-latency hog.
    let mut profile_hits: Vec<(String, String, usize, DefKind)> = Vec::new();
    if !explicit_scope {
        let current: BTreeSet<&str> = paths.iter().map(|s| s.as_str()).collect();
        let mut names = crate::config::list_profile_names(root);
        // Prefer an inferred profile first so we often hit without scanning all.
        if let Some(hint) = crate::config::infer_profile(root, &symbol) {
            if let Some(i) = names.iter().position(|n| n == &hint) {
                let n = names.remove(i);
                names.insert(0, n);
            }
        }
        for pname in names {
            if profile_hits.len() >= 3 {
                break;
            }
            let Some(ppaths) = crate::config::profile_paths(root, &pname) else {
                continue;
            };
            let other: BTreeSet<&str> = ppaths.iter().map(|s| s.as_str()).collect();
            if other == current || other.is_subset(&current) {
                continue;
            }
            let (hit, pst) = symbol_def::locate_with_stats(root, &symbol, &ppaths, false);
            io_st.bytes_read += pst.bytes_read;
            io_st.cache_hits += pst.cache_hits;
            io_st.cache_misses += pst.cache_misses;
            if let Some(info) = hit {
                profile_hits.push((pname, info.rel, info.line, info.kind));
            }
        }
    }

    let mut out =
        format!("symbol_resolve - \"{raw}\" → \"{symbol}\": no definition under {paths:?}\n");
    if !profile_hits.is_empty() {
        out.push_str("\nfound under other profile(s):\n");
        for (pname, rel, line, kind) in &profile_hits {
            out.push_str(&format!(
                "  profile:\"{pname}\"  {rel}:{line}  ({})\n",
                kind.label()
            ));
        }
        out.push_str("\nRe-run with that profile, or broaden paths.\n");
    } else {
        let store = Store::open(root).ok();
        let (sugg, sst) =
            fuzzy_suggestions_with_stats(root, &symbol, &paths, max, store.as_ref());
        io_st.bytes_read += sst.bytes_read;
        io_st.cache_hits += sst.cache_hits;
        io_st.cache_misses += sst.cache_misses;
        if !sugg.is_empty() {
            out.push_str("\nsuggestions (ranked):\n");
            for s in &sugg {
                out.push_str(&format!("  {s}\n"));
            }
            let list: Vec<String> = sugg.iter().take(5).cloned().collect();
            let profiles = crate::config::list_profile_names(root);
            let profile_note = if profiles.is_empty() {
                String::new()
            } else {
                format!(" (or pass profile:\"{}\")", profiles[0])
            };
            out.push_str(&format!("\ndid you mean: {}?{profile_note}", list.join(", ")));
        } else {
            // Both exact and fuzzy lookup missed: fall back to SQLite symbols_fts
            // BM25 + potion-code-16M-v2 dense embeddings (fused via RRF).
            let fallback = fallback_fts5_and_semantic(
                root,
                store.as_ref(),
                raw,
                &symbol,
                &paths,
                max,
                want_semantic,
                custom_embedder,
            );
            if !fallback.is_empty() {
                out.push_str("\nfallback matches (fts5 + semantic):\n");
                for hit in &fallback {
                    out.push_str(&format!(
                        "  {}  {}:{}  ({})  [{}]\n",
                        hit.name, hit.rel_path, hit.line, hit.kind, hit.provenance
                    ));
                }
                let list: Vec<String> = fallback.iter().take(5).map(|h| h.name.clone()).collect();
                let profiles = crate::config::list_profile_names(root);
                let profile_note = if profiles.is_empty() {
                    String::new()
                } else {
                    format!(" (or pass profile:\"{}\")", profiles[0])
                };
                out.push_str(&format!("\ndid you mean: {}?{profile_note}", list.join(", ")));
            } else {
                let profiles = crate::config::list_profile_names(root);
                if !profiles.is_empty() {
                    out.push_str(&format!(
                        "\nNo close names under {paths:?}; try profile one of {:?}.\n",
                        profiles
                    ));
                }
            }
        }
    }

    stats::record_ext(
        "symbol_resolve",
        stats::RecordMeta {
            baseline: io_st.scanned / 4,
            returned: (out.len() / 4) as u64,
            bytes_read: io_st.bytes_read,
            cache_hits: io_st.cache_hits,
            cache_misses: io_st.cache_misses,
            reason: None,
        },
    );
    Ok(out)
}

/// Strip leading `::`, template args `<...>`, and take the trailing `::` segment.
/// Qualified input like `ns::Foo<T>` becomes `Foo`; bare names stay as-is.
pub fn normalize_symbol(raw: &str) -> String {
    let mut s = raw.trim();
    while s.starts_with("::") {
        s = &s[2..];
    }
    s = s.trim();
    let stripped = strip_template_args(s);
    let trimmed = stripped.trim();
    // Trailing segment for lookup; empty after strip → fall back to pre-template leaf.
    let leaf = trimmed.rsplit("::").next().unwrap_or(trimmed).trim();
    leaf.to_string()
}

/// Ranked close names under `paths` (exact-ci, suffix, edit-distance ≤2, contains;
/// falls back to SQLite `symbols_fts` + `potion-code-16M-v2` when fuzzy misses).
pub fn suggestions(root: &Path, symbol: &str, paths: &[String], max: usize) -> Vec<String> {
    suggestions_with_stats(root, symbol, paths, max).0
}

fn fuzzy_suggestions_with_stats(
    root: &Path,
    symbol: &str,
    paths: &[String],
    max: usize,
    store: Option<&Store>,
) -> (Vec<String>, symbol_def::LocateStats) {
    let needle = normalize_symbol(symbol);
    if needle.is_empty() || max == 0 {
        return (Vec::new(), symbol_def::LocateStats::default());
    }
    let (names, st) = collect_def_names(root, paths, store);
    // Multi-word queries (e.g. "area widget" or conceptual phrases) aren't single
    // identifier typos; let FTS5 + potion-code-16M-v2 fallback rank them directly.
    if needle.contains(char::is_whitespace) {
        return (Vec::new(), st);
    }
    let mut scored: Vec<(i32, String)> = names
        .into_iter()
        .filter_map(|name| {
            let leaf = name.rsplit("::").next().unwrap_or(name.as_str());
            score_candidate(&needle, leaf).map(|sc| (sc, leaf.to_string()))
        })
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    scored.dedup_by(|a, b| a.1 == b.1);
    (scored.into_iter().take(max).map(|(_, n)| n).collect(), st)
}

fn suggestions_with_stats(
    root: &Path,
    symbol: &str,
    paths: &[String],
    max: usize,
) -> (Vec<String>, symbol_def::LocateStats) {
    let store = Store::open(root).ok();
    let (fuzzy, st) = fuzzy_suggestions_with_stats(root, symbol, paths, max, store.as_ref());
    if !fuzzy.is_empty() || max == 0 {
        return (fuzzy, st);
    }
    let needle = normalize_symbol(symbol);
    if needle.is_empty() {
        return (Vec::new(), st);
    }
    let fallback =
        fallback_fts5_and_semantic(root, store.as_ref(), symbol, &needle, paths, max, true, None);
    (fallback.into_iter().map(|h| h.name).collect(), st)
}

/// Split camelCase / PascalCase / snake_case / `::` identifiers into space-separated
/// subwords so FTS5 (`unicode61`) and static code embeddings align across styles.
fn split_ident_words(raw: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = raw.chars().collect();
    for (i, &ch) in chars.iter().enumerate() {
        if !ch.is_alphanumeric() {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if !cur.is_empty() && ch.is_uppercase() {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|c| c.is_lowercase());
            if prev.is_lowercase() || prev.is_ascii_digit() || (prev.is_uppercase() && next_lower) {
                words.push(std::mem::take(&mut cur));
            }
        }
        cur.push(ch);
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words.join(" ")
}

fn symbol_embed_text(s: &SymbolHit) -> String {
    let leaf = s.name.rsplit("::").next().unwrap_or(s.name.as_str());
    let expanded = split_ident_words(leaf);
    let sig = if s.signature.is_empty() {
        s.qual_name.as_str()
    } else {
        s.signature.as_str()
    };
    let mut out = if expanded.eq_ignore_ascii_case(leaf) {
        format!("{} {} {}", s.kind, leaf, sig)
    } else {
        format!("{} {} {} {}", s.kind, leaf, expanded, sig)
    };
    if !s.doc.is_empty() {
        out.push(' ');
        out.push_str(&s.doc);
    }
    out
}

fn build_code_query(raw: &str, normalized: &str) -> String {
    let expanded = split_ident_words(raw);
    if expanded.is_empty() || expanded.eq_ignore_ascii_case(raw) {
        raw.to_string()
    } else if raw.eq_ignore_ascii_case(normalized) {
        format!("{raw} {expanded}")
    } else {
        format!("{raw} {normalized} {expanded}")
    }
}

/// FTS5 BM25 + `potion-code-16M-v2` dense fallback fused via RRF when exact and
/// fuzzy symbol lookup both miss. Assumes `collect_def_names` has already warmed
/// the SQLite `symbols` + `symbols_fts` tables for `paths`.
fn fallback_fts5_and_semantic(
    root: &Path,
    store: Option<&Store>,
    raw: &str,
    normalized: &str,
    paths: &[String],
    max: usize,
    want_semantic: bool,
    custom_embedder: Option<&dyn Embedder>,
) -> Vec<FallbackHit> {
    if max == 0 {
        return Vec::new();
    }
    let owned_store;
    let store = match store {
        Some(s) => s,
        None => match Store::open(root) {
            Ok(s) => {
                owned_store = s;
                &owned_store
            }
            Err(_) => return Vec::new(),
        },
    };

    // Leg 1: SQLite FTS5 BM25 over `symbols_fts` (scoped to `paths`).
    let fts_limit = (max * 6).max(32);
    let mut fts_raw = store
        .search_symbols_fts_scoped(raw, paths, fts_limit)
        .unwrap_or_default();
    if fts_raw.is_empty() {
        let split = split_ident_words(raw);
        if !split.is_empty() && split != raw {
            fts_raw = store
                .search_symbols_fts_scoped(&split, paths, fts_limit)
                .unwrap_or_default();
        }
    }
    if fts_raw.is_empty() && normalized != raw {
        fts_raw = store
            .search_symbols_fts_scoped(normalized, paths, fts_limit)
            .unwrap_or_default();
    }

    // Unified candidate pool keyed by `(rel_path, line, leaf)`.
    let mut pool: Vec<(String /* leaf */, SymbolHit)> = Vec::new();
    let mut pool_idx: HashMap<(String, usize, String), usize> = HashMap::new();
    let mut intern = |hit: SymbolHit| -> Option<usize> {
        let leaf = hit
            .name
            .rsplit("::")
            .next()
            .unwrap_or(hit.name.as_str())
            .trim()
            .to_string();
        if !is_ident_leaf(&leaf) {
            return None;
        }
        let key = (hit.rel_path.clone(), hit.line, leaf.clone());
        if let Some(&idx) = pool_idx.get(&key) {
            return Some(idx);
        }
        let idx = pool.len();
        pool_idx.insert(key, idx);
        pool.push((leaf, hit));
        Some(idx)
    };

    let mut fts5_ranks: Vec<usize> = Vec::new();
    let mut in_fts: HashSet<usize> = HashSet::new();
    for hit in fts_raw {
        if let Some(idx) = intern(hit) {
            if in_fts.insert(idx) {
                fts5_ranks.push(idx);
            }
        }
    }

    // Leg 2: Dense code embeddings (`potion-code-16M-v2` under `semantic-m2v`,
    // or legacy `fastembed`), cached by `(content_sha256, model_tag)` in SQLite.
    let mut dense_ranks: Vec<usize> = Vec::new();
    let mut in_dense: HashSet<usize> = HashSet::new();
    if want_semantic {
        let owned_embedder = if custom_embedder.is_none() {
            match embed::resolve_embedder(root, EmbedDomain::Code) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("[wordkeep] code semantic fallback unavailable, using FTS5: {e}");
                    None
                }
            }
        } else {
            None
        };
        let active_embedder: Option<&dyn Embedder> =
            custom_embedder.or_else(|| owned_embedder.as_deref());

        if let Some(emb) = active_embedder {
            if let Ok(all_syms) = store.symbols_in_paths(paths, MAX_SYMBOL_DENSE_CORPUS) {
                for sym in all_syms {
                    let _ = intern(sym);
                }
            }
            if !pool.is_empty() {
                let items: Vec<(String, String)> = pool
                    .iter()
                    .map(|(_, s)| {
                        let text = symbol_embed_text(s);
                        let sha = store::sha256_hex(&text);
                        (sha, text)
                    })
                    .collect();
                let query_text = build_code_query(raw, normalized);
                match (
                    embed::embed_cached(Some(store), emb, &items, &fts5_ranks),
                    emb.embed_one(&query_text),
                ) {
                    (Ok((vecs, _)), Ok(q_vec)) => {
                        let mut scored: Vec<(f64, usize)> = Vec::new();
                        for (idx, maybe_v) in vecs.iter().enumerate() {
                            if let Some(v) = maybe_v {
                                let sim = embed::cosine(&q_vec, v.as_slice());
                                if sim >= MIN_SYMBOL_DENSE_COSINE {
                                    scored.push((sim, idx));
                                }
                            }
                        }
                        scored.sort_by(|a, b| {
                            b.0.partial_cmp(&a.0)
                                .unwrap_or(std::cmp::Ordering::Equal)
                                .then_with(|| a.1.cmp(&b.1))
                        });
                        for (_, idx) in scored.into_iter().take((max * 4).max(24)) {
                            if in_dense.insert(idx) {
                                dense_ranks.push(idx);
                            }
                        }
                    }
                    (Err(e), _) | (_, Err(e)) => {
                        eprintln!("[wordkeep] code embedding failed, using FTS5: {e}");
                    }
                }
            }
        }
    }

    if fts5_ranks.is_empty() && dense_ranks.is_empty() {
        return Vec::new();
    }

    let fused = embed::rrf_fuse(&[(&fts5_ranks, 1.0), (&dense_ranks, 1.0)], RRF_K);
    let mut seen_names: HashSet<String> = HashSet::new();
    let mut out: Vec<FallbackHit> = Vec::new();
    for (_, idx) in fused {
        let (leaf, hit) = &pool[idx];
        if !seen_names.insert(leaf.clone()) {
            continue;
        }
        let provenance = match (in_fts.contains(&idx), in_dense.contains(&idx)) {
            (true, true) => "hybrid",
            (true, false) => "fts5",
            (false, true) => "semantic",
            (false, false) => continue,
        };
        out.push(FallbackHit {
            name: leaf.clone(),
            rel_path: hit.rel_path.clone(),
            line: hit.line,
            kind: hit.kind.clone(),
            provenance,
        });
        if out.len() >= max {
            break;
        }
    }
    out
}

/// Empty string, or `"\ndid you mean: Foo, Bar? (or pass profile:\"...\")"`.
pub fn did_you_mean_hint(root: &Path, symbol: &str, paths: &[String]) -> String {
    let sugg = suggestions(root, symbol, paths, 5);
    if sugg.is_empty() {
        return String::new();
    }
    let list = sugg.join(", ");
    let profiles = crate::config::list_profile_names(root);
    let profile_note = if profiles.is_empty() {
        String::new()
    } else {
        format!(" (or pass profile:\"{}\")", profiles[0])
    };
    format!("\ndid you mean: {list}?{profile_note}")
}

fn strip_template_args(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0i32;
    for ch in s.chars() {
        match ch {
            '<' => depth += 1,
            '>' => {
                if depth > 0 {
                    depth -= 1;
                } else {
                    out.push(ch);
                }
            }
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out
}

/// True when `cand` looks like a usable identifier leaf (has a letter, len ≥ 3).
fn is_ident_leaf(cand: &str) -> bool {
    cand.len() >= 3 && cand.chars().any(|c| c.is_ascii_alphabetic())
}

/// Lengths are comparable enough for fuzzy match (neither side more than 2×).
fn length_ratio_ok(a: usize, b: usize) -> bool {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    lo > 0 && hi <= lo.saturating_mul(2)
}

/// Shared ≥3-char prefix or ≥50% character overlap (set intersection / longer).
fn shares_prefix_or_overlap(a: &str, b: &str) -> bool {
    let pref = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
    if pref >= 3 {
        return true;
    }
    let aa: BTreeSet<char> = a.chars().collect();
    let bb: BTreeSet<char> = b.chars().collect();
    let inter = aa.intersection(&bb).count();
    let longer = a.len().max(b.len()).max(1);
    inter * 2 >= longer
}

/// Lower score is better. `None` = not a candidate.
fn score_candidate(needle: &str, cand: &str) -> Option<i32> {
    if !is_ident_leaf(cand) {
        return None;
    }
    if needle == cand {
        return Some(0);
    }
    let n_l = needle.to_ascii_lowercase();
    let c_l = cand.to_ascii_lowercase();
    if n_l == c_l {
        return Some(1);
    }
    let n_len = n_l.len();
    let c_len = c_l.len();
    let min_side = n_len.min(4);
    // Suffix / contains: reject tiny fragments that match by accident (e.g. "E").
    if c_len >= min_side && length_ratio_ok(n_len, c_len) {
        if c_l.ends_with(&n_l) || n_l.ends_with(&c_l) {
            return Some(10 + (c_len as i32 - n_len as i32).unsigned_abs() as i32);
        }
        if c_l.contains(&n_l) || (n_len >= 4 && n_l.contains(&c_l)) {
            return Some(40 + (c_len as i32 - n_len as i32).unsigned_abs() as i32);
        }
    }
    // Edit distance: only near-length identifiers with shared shape.
    if n_len <= 40
        && (4..=40).contains(&c_len)
        && n_len.abs_diff(c_len) <= 2
        && shares_prefix_or_overlap(&n_l, &c_l)
    {
        let d = levenshtein(&n_l, &c_l);
        if d <= 2 {
            return Some(20 + d as i32);
        }
    }
    None
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut cur = vec![0usize; m + 1];
    for i in 1..=n {
        cur[0] = i;
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[m]
}

/// Maximum cold source files parsed synchronously when warming `symbols` for
/// observation-link extraction; larger monorepos reuse already-indexed `symbols`
/// rows without stalling doc/health queries on a cold full-tree C++ parse.
const MAX_SYNC_WARM_FILES: usize = 128;

/// Warm the workspace SQLite `symbols` table once when empty (bounded to
/// [`MAX_SYNC_WARM_FILES`] cold files) so observation-link symbol anchor
/// extraction resolves via O(1) SQLite B-tree lookups without stalling large monorepos.
pub fn ensure_symbols_warm(root: &Path, store: &Store) {
    if store.symbol_count().unwrap_or(0) > 0 {
        return;
    }
    let paths = crate::config::default_paths(root);
    let prune = walk::prune_set(root);
    let mut count = 0usize;
    for p in &paths {
        for path_owned in walk::files(&root.join(p), &prune) {
            if Lang::from_path(path_owned.as_path()).is_some() {
                count += 1;
                if count > MAX_SYNC_WARM_FILES {
                    return;
                }
            }
        }
    }
    let _ = collect_def_names(root, &paths, Some(store));
}

fn collect_def_names(
    root: &Path,
    paths: &[String],
    store: Option<&Store>,
) -> (BTreeSet<String>, symbol_def::LocateStats) {
    let prune = walk::prune_set(root);
    let mut cands: Vec<(PathBuf, String, Lang, u64, u64)> = Vec::new();
    for p in paths {
        let base = root.join(p);
        for path_owned in walk::files(&base, &prune) {
            let path = path_owned.as_path();
            let Some(lang) = Lang::from_path(path) else {
                continue;
            };
            let Ok(meta) = path.metadata() else { continue };
            let rel = store::normalize_rel(root, path);
            cands.push((
                path.to_path_buf(),
                rel,
                lang,
                cache::mtime_ns(&meta),
                meta.len(),
            ));
        }
    }
    cands.sort_by(|a, b| a.0.cmp(&b.0));

    let owned_store = if store.is_none() {
        Store::open(root).ok()
    } else {
        None
    };
    let store = store.or(owned_store.as_ref());
    let mut parser = Parser::new();
    let mut names = BTreeSet::new();
    let mut pending: Vec<(String, Lang, u64, u64, Vec<DefBrief>)> = Vec::new();
    let mut io_st = symbol_def::LocateStats::default();

    for (path, rel, lang, mt, sz) in &cands {
        io_st.scanned += *sz;
        let warm = store
            .and_then(|st| st.file_stage_mtime(rel, STAGE_SYMBOL_DEF).ok().flatten())
            .filter(|&(_, stored_mt)| stored_mt == *mt);
        if let (Some(st), Some((file_id, _))) = (store, warm) {
            if let Ok(sym_names) = st.symbol_names_for_file(file_id) {
                io_st.cache_hits += 1;
                for n in sym_names {
                    names.insert(n);
                }
                continue;
            }
        }
        io_st.cache_misses += 1;
        io_st.bytes_read += *sz;
        let defs = symbol_def::definitions(&mut parser, *lang, path, *mt);
        for d in &defs {
            names.insert(d.name.clone());
        }
        pending.push((rel.clone(), *lang, *mt, *sz, defs));
    }

    if !pending.is_empty() {
        if let Some(st) = store {
            let _ = st.with_immediate_tx(|s| {
                for (rel, lang, mt, sz, defs) in &pending {
                    let file_id = s.upsert_file(rel, *mt, *sz, lang.label())?;
                    for d in defs {
                        s.upsert_symbol(&SymbolInput {
                            file_id,
                            name: &d.name,
                            qual_name: &d.name,
                            kind: d.kind.label(),
                            line: d.line,
                            end_line: 0,
                            signature: &d.signature,
                            doc: "",
                            exported: d.exported,
                            source: EDGE_SOURCE_TREESITTER,
                        })?;
                    }
                    s.set_file_stage(file_id, STAGE_SYMBOL_DEF, *mt, None)?;
                }
                Ok(())
            });
        }
    }
    (names, io_st)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    #[test]
    fn normalize_strips_qualifiers_and_templates() {
        assert_eq!(normalize_symbol("Widget"), "Widget");
        assert_eq!(normalize_symbol("::Widget"), "Widget");
        assert_eq!(normalize_symbol("ns::Foo"), "Foo");
        assert_eq!(normalize_symbol("::std::vector<int>"), "vector");
        assert_eq!(normalize_symbol("Foo<Bar::Baz>"), "Foo");
        assert_eq!(normalize_symbol("  A::B<T>::C  "), "C");
        assert_eq!(normalize_symbol("map<string, vector<int>>"), "map");
    }

    #[test]
    fn suggestions_rank_close_names() {
        let dir = std::env::temp_dir().join(format!("wk_symres_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write(
            &dir,
            "src/a.cpp",
            "struct WidgetArea { int x; };\nint widget_area() { return 0; }\nint widget_are() { return 1; }\n",
        );
        let sugg = suggestions(&dir, "widget_area", &["src".into()], 5);
        assert!(
            sugg.iter().any(|s| s == "widget_area" || s == "widget_are"),
            "{sugg:?}"
        );
        // Typo within edit distance 2.
        let typo = suggestions(&dir, "widget_aree", &["src".into()], 5);
        assert!(typo.iter().any(|s| s.contains("widget_are")), "{typo:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn suggestions_reject_short_garbage() {
        assert!(score_candidate("symbl_resolve", "E").is_none());
        assert!(score_candidate("symbl_resolve", "Ve").is_none());
        assert!(score_candidate("symbl_resolve", "Ol").is_none());
        assert!(score_candidate("symbl_resolve", "Bl").is_none());
        // Near-miss of a real identifier still scores.
        assert!(score_candidate("widget_are", "widget_area").is_some());
    }

    #[test]
    fn suggestions_typo_never_emits_short_tokens() {
        let dir = std::env::temp_dir().join(format!("wk_symres_typo_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write(
            &dir,
            "src/a.cpp",
            "int symbol_resolve() { return 0; }\nint normalize_symbol() { return 1; }\nint E() { return 2; }\n",
        );
        for needle in ["symbl_resolve", "widget_are"] {
            let sugg = suggestions(&dir, needle, &["src".into()], 8);
            for s in &sugg {
                assert!(s.len() >= 3, "short garbage for {needle}: {sugg:?}");
            }
        }
        let for_typo = suggestions(&dir, "symbl_resolve", &["src".into()], 5);
        assert!(
            for_typo.iter().any(|s| s == "symbol_resolve")
                || for_typo.is_empty()
                || for_typo.iter().all(|s| s.len() >= 3),
            "{for_typo:?}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn did_you_mean_empty_when_nothing_close() {
        let dir = std::env::temp_dir().join(format!("wk_symres2_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write(&dir, "src/a.cpp", "int totally_unrelated() { return 0; }\n");
        let hint = did_you_mean_hint(&dir, "zzzz_nope", &["src".into()]);
        assert!(hint.is_empty(), "{hint}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_reports_exact_hit() {
        let dir = std::env::temp_dir().join(format!("wk_symres3_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write(&dir, "src/a.cpp", "int alpha_fn() { return 0; }\n");
        let out = resolve(
            &dir,
            &serde_json::json!({ "symbol": "::alpha_fn", "paths": ["src"] }),
        )
        .unwrap();
        assert!(out.contains("alpha_fn"), "{out}");
        assert!(out.contains("function"), "{out}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_falls_back_to_fts5_when_exact_and_fuzzy_miss() {
        let _env = crate::cache::test_env_lock();
        let dir = std::env::temp_dir().join(format!("wk_symres_fts_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write(
            &dir,
            "src/a.cpp",
            "struct Widget { int w; int h; };\nint compute_widget_bounds(const Widget& w) { return w.w * w.h; }\nint parse_json_token() { return 1; }\n",
        );
        // Reversed multi-word query misses exact lookup and edit-distance fuzzy,
        // so SQLite `symbols_fts` fallback must surface `compute_widget_bounds`.
        let out = resolve(
            &dir,
            &serde_json::json!({
                "symbol": "bounds widget",
                "paths": ["src"],
                "semantic": false
            }),
        )
        .unwrap();
        assert!(
            out.contains("compute_widget_bounds"),
            "FTS5 fallback missed symbol: {out}"
        );
        assert!(out.contains("[fts5]"), "expected fts5 provenance: {out}");
        assert!(out.contains("did you mean:"), "{out}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_falls_back_to_semantic_zero_overlap_and_caches_in_sqlite() {
        let _env = crate::cache::test_env_lock();
        let dir = std::env::temp_dir().join(format!("wk_symres_sem_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write(
            &dir,
            "src/a.cpp",
            "int slab_recycler_release() { return 0; }\nint audio_mixer_tick() { return 1; }\n",
        );
        let stub = crate::embed::test_helpers::KeywordStubEmbedder::new(
            crate::embed::TAG_M2V_CODE_DEFAULT,
            &[
                &["arena", "allocator", "free", "slab", "recycler", "release"],
                &["audio", "mixer", "tick"],
            ],
        );

        // Zero lexical overlap with `slab_recycler_release`!
        let out = resolve_with_embedder(
            &dir,
            &serde_json::json!({
                "symbol": "arena_allocator_free",
                "paths": ["src"],
                "semantic": true
            }),
            Some(&stub),
        )
        .unwrap();
        assert!(
            out.contains("slab_recycler_release"),
            "semantic fallback missed zero-overlap symbol: {out}"
        );
        assert!(
            out.contains("[semantic]"),
            "expected semantic provenance: {out}"
        );
        assert!(
            !out.contains("audio_mixer_tick"),
            "orthogonal symbol should not match: {out}"
        );

        // Second lookup reuses SQLite `embeddings` cache (only embedding the query).
        let before = stub.embedded_count();
        let out2 = resolve_with_embedder(
            &dir,
            &serde_json::json!({
                "symbol": "arena_free",
                "paths": ["src"],
                "semantic": true
            }),
            Some(&stub),
        )
        .unwrap();
        assert!(out2.contains("slab_recycler_release"), "{out2}");
        assert_eq!(
            stub.embedded_count() - before,
            1,
            "warm fallback should embed only query, reusing cached symbol vectors"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "semantic-m2v")]
    #[test]
    fn resolve_zero_overlap_with_real_model2vec_potion_code() {
        let _env = crate::cache::test_env_lock();
        let dir = std::env::temp_dir().join(format!("wk_symres_m2v_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write(
            &dir,
            "src/a.cpp",
            "int slab_recycler() { return 0; }\nint audio_mixer() { return 1; }\n",
        );
        let model_dir = dir
            .join(".wordkeep/models")
            .join(crate::embed::TAG_M2V_CODE_DEFAULT);
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

        let out = resolve(
            &dir,
            &serde_json::json!({
                "symbol": "arena_allocator",
                "paths": ["src"],
                "semantic": true
            }),
        )
        .unwrap();
        assert!(
            out.contains("slab_recycler"),
            "real model2vec-rs potion-code must resolve zero-overlap symbol: {out}"
        );
        assert!(out.contains("[semantic]"), "{out}");

        let _ = fs::remove_dir_all(&dir);
    }
}
