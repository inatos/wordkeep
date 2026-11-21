//! repo_map - a token-budgeted structural overview of a source tree.
//!
//! Uses tree-sitter for exact extraction of top-level namespaces, record types,
//! enums, and function signatures (free + member), returned in source order,
//! across C/C++, GLSL, Rust, Python, and TypeScript/TSX/Svelte; Daslang via a
//! line scanner (or its vendored grammar under `--features daslang`).
//!
//! Progressive by default (`mode: auto`): bare walks return files-first (path +
//! symbol count); pass `expand` or `mode: "symbols"` (also auto when `pattern`
//! is set) for the full per-file signature dump. Token-budgeted so an agent can
//! see the shape of a directory without reading whole files. The per-file
//! extractor is shared with `outline` (single file, with line numbers).

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use tree_sitter::{Node, Parser};

use crate::cache;
use crate::call_graph;
use crate::continuation::{self, KIND_FILE_SKIP};
use crate::lang::Lang;
use crate::store::{Store, STAGE_REPO_MAP};
use crate::{progress, stats, walk};

const PER_FILE_CAP: usize = 40;
/// Minimum matched files in a truncated `alpha` walk before suggesting `rank:"pagerank"`.
const WIDE_TREE_MIN_FILES: usize = 8;

/// Output density: files-first (paths + counts) vs full per-file symbol dumps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MapMode {
    Files,
    Symbols,
}

/// File ordering mode (`alpha` default for deterministic tests; `pagerank` for PPR + git recency).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RankMode {
    Alpha,
    PageRank,
}

fn resolve_rank(args: &Value) -> Result<RankMode, String> {
    match args
        .get("rank")
        .and_then(Value::as_str)
        .unwrap_or("alpha")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "" | "alpha" => Ok(RankMode::Alpha),
        "pagerank" | "pr" => Ok(RankMode::PageRank),
        other => Err(format!(
            "invalid repo_map rank '{other}'; expected \"alpha\" or \"pagerank\""
        )),
    }
}

fn normalize_rel(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_string()
}

fn resolve_mode(args: &Value, pattern: &Option<String>) -> MapMode {
    match args.get("mode").and_then(Value::as_str).unwrap_or("auto") {
        "files" => MapMode::Files,
        "symbols" => MapMode::Symbols,
        // auto: pattern → symbols (targeted drill-down); bare walk → files-first
        _ if pattern.is_some() => MapMode::Symbols,
        _ => MapMode::Files,
    }
}

fn expand_set(args: &Value) -> HashSet<String> {
    args.get("expand")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(normalize_rel))
                .collect()
        })
        .unwrap_or_default()
}

fn format_symbol_block(rel: &str, symbols: &[String]) -> String {
    let mut block = format!("\n{rel}\n");
    for (i, s) in symbols.iter().enumerate() {
        if i == PER_FILE_CAP {
            block.push_str(&format!("  … (+{} more)\n", symbols.len() - PER_FILE_CAP));
            break;
        }
        block.push_str(&format!("  {s}\n"));
    }
    block
}

/// Unfiltered maps (no pattern, no expand, no explicit budget) stay small.
/// Targeted queries keep the 8,000-token default.
fn map_budget(args: &Value, unfiltered: bool) -> usize {
    if let Some(budget) = args.get("token_budget").and_then(Value::as_u64) {
        return budget as usize;
    }
    if unfiltered { 1500 } else { 8000 }
}

pub fn build(root: &Path, args: &Value) -> Result<String, String> {
    const FP_KEYS: &[&str] = &["paths", "profile", "mode", "expand", "pattern", "rank"];
    let paths = crate::config::paths_from_args(root, args)?;
    let pattern = args
        .get("pattern")
        .and_then(Value::as_str)
        .map(str::to_lowercase);
    let mode = resolve_mode(args, &pattern);
    let rank = resolve_rank(args)?;
    let expand = expand_set(args);
    let budget = map_budget(args, pattern.is_none() && expand.is_empty());
    let args_fp = continuation::args_fingerprint(args, FP_KEYS);
    let resume_at = continuation::resume_offset(args, "repo_map", FP_KEYS, KIND_FILE_SKIP)?;

    // The grammar is selected per file inside `extract`, so the shared parser
    // starts language-less and is retargeted as the walk crosses languages.
    let mut parser = Parser::new();

    let mut collected: Vec<(String, Vec<String>, u64)> = Vec::new();
    let mut scanned_bytes = 0u64;
    let mut bytes_read = 0u64;
    let mut cache_hits = 0u64;
    let mut cache_misses = 0u64;

    let prune = walk::prune_set(root);
    let store = Store::open(root).ok();
    let mut pending: Vec<(String, Lang, u64, u64, Vec<String>)> = Vec::new();
    progress::tick(0, None, "repo_map: scanning");
    for p in &paths {
        let base = root.join(p);
        for path_owned in walk::files(&base, &prune) {
            let path = path_owned.as_path();
            let Some(lang) = Lang::from_path(path) else {
                continue;
            };
            let rel = normalize_rel(&path.strip_prefix(root).unwrap_or(path).to_string_lossy());
            if let Some(pat) = &pattern {
                if !rel.to_lowercase().contains(pat) {
                    continue;
                }
            }
            let Ok(meta) = path.metadata() else { continue };
            let sz = meta.len();
            let mt = cache::mtime_ns(&meta);
            scanned_bytes += sz;
            let (symbols, hit, disk_bytes) = cached_symbols_in(
                store.as_ref(),
                &mut pending,
                &mut parser,
                lang,
                path,
                &rel,
                sz,
                mt,
            );
            bytes_read += disk_bytes;
            if hit {
                cache_hits += 1;
            } else {
                cache_misses += 1;
            }
            if symbols.is_empty() {
                continue;
            }
            collected.push((rel, symbols, mt));
        }
    }

    // Persist any newly extracted symbols into the per-workspace SQLite store.
    if !pending.is_empty() {
        if let Some(st) = store.as_ref() {
            let _ = st.with_immediate_tx(|s| {
                for (rel, lang, mt, sz, syms) in &pending {
                    let file_id = s.upsert_file(rel, *mt, *sz, lang.label())?;
                    let payload = serde_json::to_string(syms).unwrap_or_else(|_| "[]".into());
                    s.set_file_stage(file_id, STAGE_REPO_MAP, *mt, Some(&payload))?;
                }
                Ok(())
            });
        }
    }

    let pr_scores: Option<HashMap<String, f64>> = match rank {
        RankMode::Alpha => {
            collected.sort_by(|(rel_a, _, _), (rel_b, _, _)| rel_a.cmp(rel_b));
            None
        }
        RankMode::PageRank if !collected.is_empty() => {
            let scores = compute_personalized_pagerank(root, &paths, &collected, &expand);
            collected.sort_by(|(rel_a, _, _), (rel_b, _, _)| {
                let sa = scores.get(rel_a).copied().unwrap_or(0.0);
                let sb = scores.get(rel_b).copied().unwrap_or(0.0);
                sb.partial_cmp(&sa)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| rel_a.cmp(rel_b))
            });
            Some(scores)
        }
        RankMode::PageRank => None,
    };

    let mut body = String::new();
    let mut used_tokens = 0usize;
    let mut files = 0usize;
    let mut skipped_files = 0usize;
    let mut truncated = false;
    let mut any_expanded = false;
    let mut walk_index = 0u64;
    let mut resume_offset_out = 0u64;
    let total_matched = collected.len();

    for (rel, symbols, _) in &collected {
        if walk_index < resume_at {
            walk_index += 1;
            continue;
        }
        if truncated {
            skipped_files += 1;
            walk_index += 1;
            continue;
        }

        let expand_this = expand.contains(rel);
        let dump_symbols = mode == MapMode::Symbols || expand_this;
        if expand_this {
            any_expanded = true;
        }

        let block = if dump_symbols {
            format_symbol_block(rel, symbols)
        } else if let Some(ref scores) = pr_scores {
            let sc = scores.get(rel).copied().unwrap_or(0.0);
            format!("\n{rel}  [pr={sc:.4}] ({} symbols)\n", symbols.len())
        } else {
            format!("\n{rel}  ({} symbols)\n", symbols.len())
        };

        let block_tokens = block.len() / 4; // rough chars-per-token estimate
        if used_tokens + block_tokens > budget && files > 0 {
            truncated = true;
            resume_offset_out = walk_index;
            skipped_files += 1;
            walk_index += 1;
            continue;
        }
        used_tokens += block_tokens;
        files += 1;
        walk_index += 1;
        body.push_str(&block);
        if files % 32 == 0 {
            progress::tick(files as u64, None, &format!("repo_map: {files} files"));
        }
    }
    progress::tick(
        files as u64,
        Some(files as u64 + skipped_files as u64),
        "repo_map: done",
    );

    if body.is_empty() && !truncated {
        stats::record_ext(
            "repo_map",
            stats::RecordMeta {
                baseline: scanned_bytes / 4,
                returned: 0,
                bytes_read,
                cache_hits,
                cache_misses,
                reason: None,
            },
        );
        return Ok(format!("repo_map: no source symbols found under {paths:?}"));
    }
    let mode_label = match mode {
        MapMode::Files => "files",
        MapMode::Symbols => "symbols",
    };
    let rank_note = match rank {
        RankMode::Alpha => "",
        RankMode::PageRank => ", rank=pagerank",
    };
    let page_note = if resume_at > 0 {
        format!(" [continuation from file #{resume_at}]")
    } else {
        String::new()
    };
    let note = if truncated {
        format!(" (truncated by token_budget; +{skipped_files} file(s) omitted)")
    } else {
        String::new()
    };
    let mut header = format!(
        "repo_map - {files} file(s), ~{used_tokens} tokens [{mode_label} mode{rank_note}]{page_note}{note}\nroots: {paths:?}"
    );
    if let Some(pat) = &pattern {
        header.push_str(&format!("\npattern: {pat}"));
    }
    if mode == MapMode::Files && !any_expanded {
        body.push_str(
            "\nhint: pass expand:[\"path/to/file\"] or mode:\"symbols\" + pattern to see signatures\n",
        );
    }

    let mut out = format!("{header}\n{body}");
    if truncated {
        continuation::append_footer(
            &mut out,
            "repo_map",
            args_fp,
            resume_offset_out,
            KIND_FILE_SKIP,
            &format!("+{skipped_files} file(s) omitted"),
        );
        if rank == RankMode::Alpha && total_matched >= WIDE_TREE_MIN_FILES {
            out.push_str(
                "hint: wide tree truncated — pass rank:\"pagerank\" to surface high-centrality + recently-touched files first\n",
            );
        }
    }
    stats::record_ext(
        "repo_map",
        stats::RecordMeta {
            baseline: scanned_bytes / 4,
            returned: (out.len() / 4) as u64,
            bytes_read,
            cache_hits,
            cache_misses,
            reason: None,
        },
    );
    Ok(out)
}

/// Compute Personalized PageRank (damping 0.85) over file-level call edges,
/// personalized by git-recency (recent commits + dirty working-tree files) and
/// any explicit `expand` seed files.
fn compute_personalized_pagerank(
    root: &Path,
    paths: &[String],
    files: &[(String, Vec<String>, u64)],
    expand_seeds: &HashSet<String>,
) -> HashMap<String, f64> {
    let n = files.len();
    let mut idx_by_rel: HashMap<&str, usize> = HashMap::with_capacity(n);
    for (i, (rel, _, _)) in files.iter().enumerate() {
        idx_by_rel.insert(rel.as_str(), i);
    }

    let (commit_ts_map, dirty_set) = git_recency_signals(root);
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let max_mtime = files.iter().map(|(_, _, mt)| *mt).max().unwrap_or(0);

    // Build personalization vector p (sums to 1.0).
    let mut p = vec![0.0f64; n];
    for (i, (rel, _, mt)) in files.iter().enumerate() {
        let mut w = if call_graph::is_test_path(rel) {
            0.45
        } else {
            1.0
        };
        if dirty_set.contains(rel.as_str()) {
            w *= 3.5;
        } else if let Some(&ts) = commit_ts_map.get(rel.as_str()) {
            let age_days = (now_secs.saturating_sub(ts) as f64) / 86_400.0;
            w *= 1.0 + 1.5 * (-age_days / 14.0).exp();
        } else if commit_ts_map.is_empty() && max_mtime > 0 && *mt > 0 {
            // Non-git fallback: relative mtime recency within 14-day window.
            let age_days = (max_mtime.saturating_sub(*mt) as f64) / (86_400.0 * 1_000_000_000.0);
            w *= 1.0 + 0.5 * (-age_days / 14.0).exp();
        }
        if expand_seeds.contains(rel.as_str()) {
            w *= 3.0;
        }
        p[i] = w;
    }
    let p_sum: f64 = p.iter().sum();
    if p_sum > 0.0 {
        for x in &mut p {
            *x /= p_sum;
        }
    } else {
        let uniform = 1.0 / (n as f64);
        p.fill(uniform);
    }

    // Build weighted directed file->file transition edges from call_graph adjacency.
    let adj = call_graph::adjacency(root, paths);
    let mut outgoing: Vec<HashMap<usize, f64>> = vec![HashMap::new(); n];
    for (callee_seg, (dst_rel, _)) in &adj.sites {
        let dst_norm = normalize_rel(dst_rel);
        let Some(&dst_idx) = idx_by_rel.get(dst_norm.as_str()) else {
            continue;
        };
        if let Some(callers) = adj.caller_sites.get(callee_seg) {
            for cs in callers {
                let src_norm = normalize_rel(&cs.rel);
                let Some(&src_idx) = idx_by_rel.get(src_norm.as_str()) else {
                    continue;
                };
                if src_idx == dst_idx {
                    continue;
                }
                let weight = cs.confidence * if cs.is_test { 0.35 } else { 1.0 };
                *outgoing[src_idx].entry(dst_idx).or_insert(0.0) += weight;
            }
        }
    }

    let out_totals: Vec<f64> = outgoing.iter().map(|m| m.values().sum()).collect();
    let damping = 0.85f64;
    let mut ranks = p.clone();
    let mut next = vec![0.0f64; n];

    for _ in 0..25 {
        let mut dangling_mass = 0.0f64;
        for u in 0..n {
            if out_totals[u] <= 0.0 {
                dangling_mass += ranks[u];
            }
        }
        let base_factor = (1.0 - damping) + damping * dangling_mass;
        for v in 0..n {
            next[v] = base_factor * p[v];
        }
        for u in 0..n {
            let tot = out_totals[u];
            if tot <= 0.0 {
                continue;
            }
            let share = damping * ranks[u] / tot;
            for (&v, &w) in &outgoing[u] {
                next[v] += share * w;
            }
        }
        let delta: f64 = ranks
            .iter()
            .zip(next.iter())
            .map(|(a, b)| (a - b).abs())
            .sum();
        std::mem::swap(&mut ranks, &mut next);
        if delta < 1e-7 {
            break;
        }
    }

    let mut out = HashMap::with_capacity(n);
    for (i, (rel, _, _)) in files.iter().enumerate() {
        out.insert(rel.clone(), ranks[i]);
    }
    out
}

/// Collect per-file latest git commit epoch seconds (capped at 200 recent commits
/// in one subprocess) plus dirty tracked paths from `git diff --name-only HEAD`.
fn git_recency_signals(root: &Path) -> (HashMap<String, u64>, HashSet<String>) {
    let mut commits: HashMap<String, u64> = HashMap::new();
    let mut dirty: HashSet<String> = HashSet::new();

    let top = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| std::path::PathBuf::from(s.trim()));
    let Some(top_dir) = top else {
        return (commits, dirty);
    };
    let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let canon_top = top_dir.canonicalize().unwrap_or(top_dir);
    let prefix = canon_root
        .strip_prefix(&canon_top)
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();

    let to_root_rel = |repo_rel: &str| -> Option<String> {
        let clean = repo_rel.trim().replace('\\', "/");
        if clean.is_empty() {
            return None;
        }
        if prefix.is_empty() {
            Some(clean)
        } else {
            clean
                .strip_prefix(&prefix)
                .and_then(|s| s.strip_prefix('/'))
                .map(String::from)
        }
    };

    if let Ok(out) = Command::new("git")
        .args(["log", "-n", "200", "--format=COMMIT:%ct", "--name-only"])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    {
        if out.status.success() {
            let mut cur_ts: Option<u64> = None;
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                let t = line.trim();
                if t.is_empty() {
                    continue;
                }
                if let Some(rest) = t.strip_prefix("COMMIT:") {
                    cur_ts = rest.parse::<u64>().ok();
                } else if let Some(ts) = cur_ts {
                    if let Some(rel) = to_root_rel(t) {
                        commits.entry(rel).or_insert(ts);
                    }
                }
            }
        }
    }

    if let Ok(out) = Command::new("git")
        .args(["diff", "--name-only", "HEAD"])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    {
        if out.status.success() {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                if let Some(rel) = to_root_rel(line) {
                    dirty.insert(rel);
                }
            }
        }
    }

    (commits, dirty)
}

/// Return `(symbols, cache_hit, bytes_read_from_disk)`, reusing the SQLite
/// `file_stages` payload when the on-disk mtime is unchanged. Queues misses
/// into `pending` for batch upsert.
#[allow(clippy::too_many_arguments)]
fn cached_symbols_in(
    store: Option<&Store>,
    pending: &mut Vec<(String, Lang, u64, u64, Vec<String>)>,
    parser: &mut Parser,
    lang: Lang,
    path: &Path,
    rel: &str,
    size_bytes: u64,
    mtime_ns: u64,
) -> (Vec<String>, bool, u64) {
    if mtime_ns > 0 {
        if let Some(st) = store {
            if let Ok(Some(payload)) = st.get_file_stage_payload(rel, STAGE_REPO_MAP, mtime_ns) {
                if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(&payload) {
                    let syms = arr
                        .into_iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect();
                    return (syms, true, 0);
                }
            }
        }
    }
    let Ok(src) = std::fs::read_to_string(path) else {
        return (Vec::new(), false, 0);
    };
    let read_len = src.len() as u64;
    let syms = extract(parser, lang, &src);
    if mtime_ns > 0 {
        pending.push((rel.to_string(), lang, mtime_ns, size_bytes, syms.clone()));
    }
    (syms, false, read_len)
}

/// Single-file convenience wrapper over [`cached_symbols_in`] for unit tests.
#[cfg(test)]
fn cached_symbols(parser: &mut Parser, lang: Lang, path: &Path, mtime_ns: u64) -> Vec<String> {
    let root = path.parent().unwrap_or(path);
    let rel = normalize_rel(&path.strip_prefix(root).unwrap_or(path).to_string_lossy());
    let sz = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let store = Store::open(root).ok();
    let mut pending = Vec::new();
    let (syms, _, _) = cached_symbols_in(
        store.as_ref(),
        &mut pending,
        parser,
        lang,
        path,
        &rel,
        sz,
        mtime_ns,
    );
    if !pending.is_empty() {
        if let Some(st) = store.as_ref() {
            let _ = st.with_immediate_tx(|s| {
                for (r, l, mt, s_bytes, list) in &pending {
                    let fid = s.upsert_file(r, *mt, *s_bytes, l.label())?;
                    let payload = serde_json::to_string(list).unwrap_or_else(|_| "[]".into());
                    s.set_file_stage(fid, STAGE_REPO_MAP, *mt, Some(&payload))?;
                }
                Ok(())
            });
        }
    }
    syms
}

/// Exact top-level structure per language with each symbol's start byte offset,
/// source-ordered and de-duplicated (first occurrence wins). tree-sitter drives
/// C/C++, GLSL, Rust, Python, and TypeScript/TSX/Svelte; a small line scanner
/// handles Daslang without the grammar. Svelte is reduced to its `<script>`
/// blocks first (newlines preserved, so byte offsets still map to the file).
/// `repo_map` drops the offset; `outline` turns it into a line number. The
/// shared parser is retargeted to the file's grammar on each call.
pub fn symbols_located(parser: &mut Parser, lang: Lang, src: &str) -> Vec<(usize, String)> {
    let prepared = lang.preprocess(src);
    let mut found: Vec<(usize, String)> = match lang.ts_language() {
        Some(ts) => {
            if parser.set_language(&ts).is_err() {
                return Vec::new();
            }
            let Some(tree) = parser.parse(prepared.as_ref(), None) else {
                return Vec::new();
            };
            let bytes = prepared.as_bytes();
            let mut f = Vec::new();
            match lang {
                Lang::Rust => collect_rust(tree.root_node(), bytes, &mut f),
                Lang::Python => collect_python(tree.root_node(), bytes, &mut f),
                Lang::Ts | Lang::Tsx | Lang::Svelte => collect_ts(tree.root_node(), bytes, &mut f),
                Lang::CSharp => collect_csharp(tree.root_node(), bytes, &mut f),
                #[cfg(feature = "daslang")]
                Lang::Daslang => collect_das(tree.root_node(), bytes, &mut f),
                // C/C++ and GLSL (a tree-sitter-c fork) share the C node kinds.
                _ => collect_cpp(tree.root_node(), bytes, &mut f),
            }
            f
        }
        // Only Daslang reaches here, and only without the `daslang` feature;
        // every other language resolves to a tree-sitter grammar above.
        None => {
            #[cfg(not(feature = "daslang"))]
            {
                das_symbols(prepared.as_ref())
            }
            #[cfg(feature = "daslang")]
            {
                Vec::new()
            }
        }
    };
    found.sort_by_key(|(b, _)| *b);

    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for (b, s) in found {
        if seen.insert(s.clone()) {
            out.push((b, s));
        }
    }
    out
}

/// Structural symbols for `repo_map` - [`symbols_located`] with the byte offset
/// dropped.
fn extract(parser: &mut Parser, lang: Lang, src: &str) -> Vec<String> {
    symbols_located(parser, lang, src)
        .into_iter()
        .map(|(_, s)| s)
        .collect()
}

/// Recurse a C/C++ tree, emitting `(start_byte, symbol)` for the constructs we
/// map. Descends into namespace/record bodies so member functions are captured.
fn collect_cpp(node: Node, src: &[u8], out: &mut Vec<(usize, String)>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "namespace_definition" => {
                if let Some(name) = child.child_by_field_name("name") {
                    out.push((child.start_byte(), format!("namespace {}", text(name, src))));
                }
                if let Some(b) = child.child_by_field_name("body") {
                    collect_cpp(b, src, out);
                }
            }
            "struct_specifier" | "class_specifier" | "union_specifier" => {
                let kw = match child.kind() {
                    "struct_specifier" => "struct",
                    "class_specifier" => "class",
                    _ => "union",
                };
                // Require a body so forward declarations are skipped.
                if let (Some(name), Some(b)) = (
                    child.child_by_field_name("name"),
                    child.child_by_field_name("body"),
                ) {
                    out.push((child.start_byte(), format!("{kw} {}", text(name, src))));
                    collect_cpp(b, src, out);
                }
            }
            "enum_specifier" => {
                if let Some(name) = child.child_by_field_name("name") {
                    out.push((child.start_byte(), format!("enum {}", text(name, src))));
                }
            }
            "function_definition" => {
                if let Some(sig) = fn_signature(child, src) {
                    out.push((child.start_byte(), sig));
                }
            }
            "declaration" | "field_declaration" => {
                if let Some(d) = child.child_by_field_name("declarator") {
                    if d.kind() == "function_declarator" {
                        let raw = text(child, src).trim().trim_end_matches(';').trim();
                        out.push((child.start_byte(), squeeze(raw)));
                    }
                }
            }
            "template_declaration" | "linkage_specification" => collect_cpp(child, src, out),
            _ => {}
        }
    }
}

/// Recurse a Rust tree: modules, records, traits, impls, and fn signatures.
/// Descends into `mod`/`trait`/`impl` bodies so methods are captured.
fn collect_rust(node: Node, src: &[u8], out: &mut Vec<(usize, String)>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "mod_item" => {
                if let Some(n) = child.child_by_field_name("name") {
                    out.push((child.start_byte(), format!("mod {}", text(n, src))));
                }
                if let Some(b) = child.child_by_field_name("body") {
                    collect_rust(b, src, out);
                }
            }
            "struct_item" | "enum_item" | "union_item" => {
                let kw = match child.kind() {
                    "struct_item" => "struct",
                    "enum_item" => "enum",
                    _ => "union",
                };
                if let Some(n) = child.child_by_field_name("name") {
                    out.push((child.start_byte(), format!("{kw} {}", text(n, src))));
                }
            }
            "trait_item" => {
                if let Some(n) = child.child_by_field_name("name") {
                    out.push((child.start_byte(), format!("trait {}", text(n, src))));
                }
                if let Some(b) = child.child_by_field_name("body") {
                    collect_rust(b, src, out);
                }
            }
            "impl_item" => {
                out.push((child.start_byte(), header(child, src)));
                if let Some(b) = child.child_by_field_name("body") {
                    collect_rust(b, src, out);
                }
            }
            "function_item" | "function_signature_item" => {
                out.push((child.start_byte(), header(child, src)));
            }
            "type_item" => {
                if let Some(n) = child.child_by_field_name("name") {
                    out.push((child.start_byte(), format!("type {}", text(n, src))));
                }
            }
            "macro_definition" => {
                if let Some(n) = child.child_by_field_name("name") {
                    out.push((child.start_byte(), format!("macro_rules! {}", text(n, src))));
                }
            }
            _ => {}
        }
    }
}

/// Recurse a Python tree: classes and `def`s (sync + async). Descends into class
/// bodies so methods are captured; unwraps decorators.
fn collect_python(node: Node, src: &[u8], out: &mut Vec<(usize, String)>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "class_definition" => {
                out.push((child.start_byte(), header(child, src)));
                if let Some(b) = child.child_by_field_name("body") {
                    collect_python(b, src, out);
                }
            }
            "function_definition" => {
                out.push((child.start_byte(), header(child, src)));
            }
            "decorated_definition" => collect_python(child, src, out),
            _ => {}
        }
    }
}

/// Recurse a TypeScript/TSX/Svelte tree: classes (+ methods), interfaces, type
/// aliases, enums, function declarations, and top-level `const f = () => {}`
/// bindings. Descends class bodies for methods and unwraps `export`.
fn collect_ts(node: Node, src: &[u8], out: &mut Vec<(usize, String)>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "class_declaration" | "abstract_class_declaration" => {
                if let Some(n) = child.child_by_field_name("name") {
                    out.push((child.start_byte(), format!("class {}", text(n, src))));
                }
                if let Some(b) = child.child_by_field_name("body") {
                    collect_ts(b, src, out);
                }
            }
            "interface_declaration" => {
                if let Some(n) = child.child_by_field_name("name") {
                    out.push((child.start_byte(), format!("interface {}", text(n, src))));
                }
            }
            "type_alias_declaration" => {
                if let Some(n) = child.child_by_field_name("name") {
                    out.push((child.start_byte(), format!("type {}", text(n, src))));
                }
            }
            "enum_declaration" => {
                if let Some(n) = child.child_by_field_name("name") {
                    out.push((child.start_byte(), format!("enum {}", text(n, src))));
                }
            }
            "function_declaration" | "generator_function_declaration" | "method_definition" => {
                out.push((child.start_byte(), header(child, src)));
            }
            // `export` wraps a declaration; recurse so the inner one is captured.
            "export_statement" => collect_ts(child, src, out),
            // Top-level `const f = () => {}` / `let g = function () {}`.
            "lexical_declaration" | "variable_declaration" => {
                let mut c2 = child.walk();
                for d in child.children(&mut c2) {
                    if d.kind() == "variable_declarator" {
                        let is_fn = d
                            .child_by_field_name("value")
                            .map(|v| {
                                matches!(
                                    v.kind(),
                                    "arrow_function" | "function" | "function_expression"
                                )
                            })
                            .unwrap_or(false);
                        if is_fn {
                            if let Some(n) = d.child_by_field_name("name") {
                                out.push((d.start_byte(), format!("const {}()", text(n, src))));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Recurse a C# tree: namespaces, records, classes, and member declarations.
fn collect_csharp(node: Node, src: &[u8], out: &mut Vec<(usize, String)>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "namespace_declaration" | "file_scoped_namespace_declaration" => {
                if let Some(name) = child.child_by_field_name("name") {
                    out.push((child.start_byte(), format!("namespace {}", text(name, src))));
                }
                if let Some(b) = child.child_by_field_name("body") {
                    collect_csharp(b, src, out);
                }
            }
            "class_declaration"
            | "struct_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "record_struct_declaration" => {
                let kw = match child.kind() {
                    "class_declaration" => "class",
                    "struct_declaration" => "struct",
                    "interface_declaration" => "interface",
                    "enum_declaration" => "enum",
                    _ => "record",
                };
                if let Some(n) = child.child_by_field_name("name") {
                    out.push((child.start_byte(), format!("{kw} {}", text(n, src))));
                }
                if let Some(b) = child.child_by_field_name("body") {
                    collect_csharp(b, src, out);
                }
            }
            "method_declaration"
            | "constructor_declaration"
            | "destructor_declaration"
            | "property_declaration"
            | "indexer_declaration"
            | "delegate_declaration" => {
                out.push((child.start_byte(), header(child, src)));
                if let Some(b) = child.child_by_field_name("body") {
                    collect_csharp(b, src, out);
                }
            }
            _ => {}
        }
    }
}

/// A declaration header: text from the node start to its `body` (or end), with
/// whitespace squeezed and a trailing `;` trimmed. Works for Rust fn/impl and
/// Python def/class (keeps the trailing `:`).
fn header(node: Node, src: &[u8]) -> String {
    let end = node
        .child_by_field_name("body")
        .map(|b| b.start_byte())
        .unwrap_or_else(|| node.end_byte());
    let s = std::str::from_utf8(src.get(node.start_byte()..end).unwrap_or(b"")).unwrap_or("");
    squeeze(s.trim().trim_end_matches(';').trim())
}

/// Daslang (`.das`) extraction via the vendored tree-sitter grammar (enabled by
/// the `daslang` feature). Recurses the whole tree, emitting a label for each
/// outline node kind defined in the grammar's `outline_rules.yml`.
#[cfg(feature = "daslang")]
fn collect_das(node: Node, src: &[u8], out: &mut Vec<(usize, String)>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(label) = das_label(child, src) {
            out.push((child.start_byte(), label));
        }
        collect_das(child, src, out);
    }
}

/// Label for a Daslang outline node, or `None` if it is not one we map.
/// `function_declaration` keeps its full signature (up to the body); the others
/// are `keyword name` from the node's `name` field.
#[cfg(feature = "daslang")]
fn das_label(node: Node, src: &[u8]) -> Option<String> {
    let name = || {
        node.child_by_field_name("name")
            .map(|n| text(n, src).to_string())
    };
    match node.kind() {
        "function_declaration" => Some(header(node, src)),
        "structure_declaration" => {
            let kw = node
                .child_by_field_name("kind")
                .map(|k| text(k, src))
                .unwrap_or("struct");
            name().map(|n| format!("{kw} {n}"))
        }
        "enum_declaration" => name().map(|n| format!("enum {n}")),
        "variant_alias_declaration" => name().map(|n| format!("variant {n}")),
        "typedef_statement" => name().map(|n| format!("typedef {n}")),
        "bitfield_alias_declaration" => name().map(|n| format!("bitfield {n}")),
        _ => None,
    }
}

/// Daslang fallback used when the `daslang` feature is off: scan top-level
/// declarations by leading keyword - `def` (whole signature line),
/// `struct`/`class`/`enum`/`variant` (keyword + name). Indentation-based bodies
/// are ignored. With the feature on, `collect_das` uses the real grammar.
#[cfg(not(feature = "daslang"))]
fn das_symbols(src: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut off = 0usize;
    for line in src.lines() {
        let t = line.trim_start();
        if after_kw(t, "def").is_some() {
            out.push((off, squeeze(t)));
        } else if let Some(r) = after_kw(t, "struct") {
            out.push((off, format!("struct {}", ident(r))));
        } else if let Some(r) = after_kw(t, "class") {
            out.push((off, format!("class {}", ident(r))));
        } else if let Some(r) = after_kw(t, "enum") {
            out.push((off, format!("enum {}", ident(r))));
        } else if let Some(r) = after_kw(t, "variant") {
            out.push((off, format!("variant {}", ident(r))));
        }
        off += line.len() + 1; // +1 for the newline `lines()` stripped
    }
    out
}

/// The remainder after a leading keyword that is followed by whitespace, else
/// `None` (so `definitely` does not match the keyword `def`).
#[cfg(not(feature = "daslang"))]
fn after_kw<'a>(line: &'a str, kw: &str) -> Option<&'a str> {
    let r = line.strip_prefix(kw)?;
    if r.starts_with([' ', '\t']) {
        Some(r.trim_start())
    } else {
        None
    }
}

/// The leading identifier (`[A-Za-z0-9_]+`) of a string.
#[cfg(not(feature = "daslang"))]
fn ident(s: &str) -> &str {
    let end = s
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(s.len());
    &s[..end]
}

/// Signature of a function definition: text from its start up to the body `{`.
fn fn_signature(node: Node, src: &[u8]) -> Option<String> {
    let end = node
        .child_by_field_name("body")
        .map(|b| b.start_byte())
        .unwrap_or_else(|| node.end_byte());
    let s = std::str::from_utf8(src.get(node.start_byte()..end)?).ok()?;
    Some(squeeze(s.trim()))
}

fn text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    std::str::from_utf8(&src[node.byte_range()]).unwrap_or("")
}

/// Collapse interior whitespace to single spaces and cap the length.
fn squeeze(s: &str) -> String {
    let one = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= 110 {
        one
    } else {
        let t: String = one.chars().take(109).collect();
        format!("{t}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser() -> Parser {
        let mut p = Parser::new();
        p.set_language(&tree_sitter_cpp::LANGUAGE.into()).unwrap();
        p
    }

    #[test]
    fn unfiltered_map_defaults_to_small_budget() {
        let args = serde_json::json!({});
        assert_eq!(map_budget(&args, true), 1500);
        assert_eq!(map_budget(&args, false), 8000);
        let explicit = serde_json::json!({"token_budget": 4000});
        assert_eq!(map_budget(&explicit, true), 4000);
    }

    #[test]
    fn extract_finds_namespace_record_enum_and_fns() {
        let src = r#"
namespace game {
struct Transform { float x; float y; };
enum class State { Idle, Run };
void update(Transform& t);
int Transform_area(const Transform& t) { return 0; }
}
"#;
        let syms = extract(&mut parser(), Lang::Cpp, src);
        assert!(syms.iter().any(|s| s == "namespace game"), "{syms:?}");
        assert!(syms.iter().any(|s| s == "struct Transform"), "{syms:?}");
        assert!(syms.iter().any(|s| s == "enum State"), "{syms:?}");
        assert!(syms.iter().any(|s| s.contains("update(")), "{syms:?}");
        assert!(
            syms.iter().any(|s| s.contains("Transform_area(")),
            "{syms:?}"
        );
    }

    #[test]
    fn extract_skips_forward_declarations() {
        // No body → not a real definition, should be omitted.
        let syms = extract(&mut parser(), Lang::Cpp, "struct OnlyForward;\n");
        assert!(!syms.iter().any(|s| s == "struct OnlyForward"), "{syms:?}");
    }

    #[test]
    fn extract_is_source_ordered_and_deduped() {
        let src = "struct A {};\nstruct B {};\nstruct A {};\n";
        let syms = extract(&mut parser(), Lang::Cpp, src);
        let a = syms.iter().position(|s| s == "struct A");
        let b = syms.iter().position(|s| s == "struct B");
        assert!(a.is_some() && b.is_some());
        assert!(a.unwrap() < b.unwrap(), "source order: {syms:?}");
        assert_eq!(syms.iter().filter(|s| *s == "struct A").count(), 1);
    }

    #[test]
    fn extract_rust_items_and_methods() {
        let src = "pub mod net {\n    pub struct Peer { id: u32 }\n    pub enum State { Up, Down }\n    impl Peer {\n        pub fn ping(&self) -> bool { true }\n    }\n    pub fn connect(addr: &str) -> Peer { todo!() }\n}\n";
        let syms = extract(&mut parser(), Lang::Rust, src);
        assert!(syms.iter().any(|s| s == "mod net"), "{syms:?}");
        assert!(syms.iter().any(|s| s == "struct Peer"), "{syms:?}");
        assert!(syms.iter().any(|s| s == "enum State"), "{syms:?}");
        assert!(syms.iter().any(|s| s.starts_with("impl Peer")), "{syms:?}");
        assert!(syms.iter().any(|s| s.contains("fn ping")), "{syms:?}");
        assert!(syms.iter().any(|s| s.contains("fn connect")), "{syms:?}");
    }

    #[test]
    fn extract_python_classes_and_defs() {
        let src = "import os\n\nclass Atlas:\n    def bake(self, path):\n        return path\n\n@cache\ndef build(name):\n    return name\n";
        let syms = extract(&mut parser(), Lang::Python, src);
        assert!(
            syms.iter().any(|s| s.starts_with("class Atlas")),
            "{syms:?}"
        );
        assert!(syms.iter().any(|s| s.contains("def bake")), "{syms:?}");
        // decorated def is unwrapped and captured
        assert!(syms.iter().any(|s| s.contains("def build")), "{syms:?}");
    }

    #[test]
    fn extract_typescript_items_and_methods() {
        let src = "export class Atlas {\n  bake(path: string): string { return path; }\n}\ninterface Shape { area(): number }\ntype Id = number;\nenum Color { Red, Green }\nexport function build(name: string): string { return name; }\nconst make = (n: number) => n + 1;\n";
        let syms = extract(&mut parser(), Lang::Ts, src);
        assert!(syms.iter().any(|s| s == "class Atlas"), "{syms:?}");
        assert!(syms.iter().any(|s| s.contains("bake")), "{syms:?}");
        assert!(syms.iter().any(|s| s == "interface Shape"), "{syms:?}");
        assert!(syms.iter().any(|s| s == "type Id"), "{syms:?}");
        assert!(syms.iter().any(|s| s == "enum Color"), "{syms:?}");
        assert!(
            syms.iter().any(|s| s.contains("function build")),
            "{syms:?}"
        );
        assert!(syms.iter().any(|s| s.contains("const make")), "{syms:?}");
    }

    #[test]
    fn extract_csharp_classes_and_methods() {
        let src = "namespace App.Exporter {\n  public class Plugin {\n    public void Awake() { Logger.LogInfo(\"hi\"); }\n  }\n  public static class BundleWriter {\n    public static void Write(string dir) {}\n  }\n}\n";
        let syms = extract(&mut parser(), Lang::CSharp, src);
        assert!(
            syms.iter().any(|s| s.contains("namespace App.Exporter")),
            "{syms:?}"
        );
        assert!(syms.iter().any(|s| s == "class Plugin"), "{syms:?}");
        assert!(syms.iter().any(|s| s.contains("Awake")), "{syms:?}");
        assert!(syms.iter().any(|s| s == "class BundleWriter"), "{syms:?}");
        assert!(syms.iter().any(|s| s.contains("Write")), "{syms:?}");
    }

    #[test]
    fn extract_svelte_reads_script_only() {
        let src = "<script lang=\"ts\">\nexport function onMount() {}\nconst label = \"hi\";\n</script>\n\n<h1>{label}</h1>\n";
        let syms = extract(&mut parser(), Lang::Svelte, src);
        assert!(
            syms.iter().any(|s| s.contains("function onMount")),
            "{syms:?}"
        );
        // markup is not parsed as code
        assert!(!syms.iter().any(|s| s.contains("h1")), "{syms:?}");
    }

    // Default build: the line scanner handles indentation-style Daslang
    // (def name + indented bodies, no braces).
    #[cfg(not(feature = "daslang"))]
    #[test]
    fn extract_daslang_indentation_via_scanner() {
        let src = "require app_core\n\n[export]\ndef tick_frozen(t : float; dt : float) : float\n    return t - dt\n\nstruct Spark\n    energy : float\n\nenum Element\n    Fire\n";
        let syms = extract(&mut parser(), Lang::Daslang, src);
        assert!(
            syms.iter().any(|s| s.contains("def tick_frozen(")),
            "{syms:?}"
        );
        assert!(syms.iter().any(|s| s == "struct Spark"), "{syms:?}");
        assert!(syms.iter().any(|s| s == "enum Element"), "{syms:?}");
        // `require` must not be mistaken for a definition
        assert!(!syms.iter().any(|s| s.contains("require")), "{syms:?}");
    }

    // `daslang` feature: the vendored grammar parses daScript's gen2 brace syntax
    // (the indentation dialect is not supported upstream - see vendor SOURCE.txt).
    #[cfg(feature = "daslang")]
    #[test]
    fn extract_daslang_brace_via_grammar() {
        let src = "require app_core\n\ndef add(a : int, b : int) : int {\n    return a + b\n}\n\nstruct Spark {\n    energy : float\n}\n\nenum Element {\n    Fire\n}\n";
        let syms = extract(&mut parser(), Lang::Daslang, src);
        assert!(syms.iter().any(|s| s.contains("def add(")), "{syms:?}");
        assert!(syms.iter().any(|s| s == "struct Spark"), "{syms:?}");
        assert!(syms.iter().any(|s| s == "enum Element"), "{syms:?}");
        // `require` must not be mistaken for a definition
        assert!(!syms.iter().any(|s| s.contains("require")), "{syms:?}");
    }

    #[test]
    fn extract_glsl_struct_and_functions() {
        let src = "#version 330 core\nstruct Light { vec3 pos; };\nvec4 shade(vec3 n) {\n    return vec4(n, 1.0);\n}\nvoid main() {\n    if (true) { discard; }\n}\n";
        let syms = extract(&mut parser(), Lang::Glsl, src);
        assert!(syms.iter().any(|s| s == "struct Light"), "{syms:?}");
        assert!(syms.iter().any(|s| s.contains("shade(")), "{syms:?}");
        assert!(syms.iter().any(|s| s.contains("main(")), "{syms:?}");
        // control-flow keywords are not functions
        assert!(!syms.iter().any(|s| s.contains("if (")), "{syms:?}");
    }

    #[test]
    fn cached_symbols_reuses_until_mtime_changes() {
        let dir = std::env::temp_dir().join(format!("cbtest_repomap_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.hpp");
        std::fs::write(&f, "struct First {};\n").unwrap();
        let mut p = parser();
        let m1 = std::fs::metadata(&f).unwrap();
        let t1 = cache::mtime_ns(&m1);
        let s1 = cached_symbols(&mut p, Lang::Cpp, &f, t1);
        assert!(s1.iter().any(|s| s == "struct First"));
        // Same mtime → cached clone, even though disk content "changed" logically.
        let again = cached_symbols(&mut p, Lang::Cpp, &f, t1);
        assert_eq!(s1, again);
        // New content + new mtime → re-extract.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&f, "struct Second {};\n").unwrap();
        let m2 = std::fs::metadata(&f).unwrap();
        let t2 = cache::mtime_ns(&m2);
        let s2 = cached_symbols(&mut p, Lang::Cpp, &f, t2);
        assert!(s2.iter().any(|s| s == "struct Second"), "{s2:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn squeeze_collapses_and_caps() {
        assert_eq!(squeeze("a   b\n\tc"), "a b c");
        let long = "x".repeat(200);
        let out = squeeze(&long);
        assert!(out.chars().count() <= 110);
        assert!(out.ends_with('…'));
    }

    fn fixture_tree(name: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("cbtest_repomap_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/alpha.cpp"),
            "namespace a { struct Alpha {}; int f() { return 0; } }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/beta.cpp"),
            "namespace b { struct Beta {}; void g() {} }\n",
        )
        .unwrap();
        root
    }

    #[test]
    fn files_mode_lists_counts_without_signatures() {
        let root = fixture_tree("files");
        let out = build(
            &root,
            &serde_json::json!({ "paths": ["src"], "mode": "files" }),
        )
        .unwrap();
        assert!(out.contains("[files mode]"), "{out}");
        assert!(out.contains("src/alpha.cpp  ("), "{out}");
        assert!(out.contains("symbols)"), "{out}");
        assert!(!out.contains("namespace a"), "{out}");
        assert!(!out.contains("struct Alpha"), "{out}");
        assert!(out.contains("hint:"), "{out}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn expand_dumps_symbols_for_named_files() {
        let root = fixture_tree("expand");
        let out = build(
            &root,
            &serde_json::json!({
                "paths": ["src"],
                "mode": "files",
                "expand": ["src/alpha.cpp"]
            }),
        )
        .unwrap();
        assert!(out.contains("[files mode]"), "{out}");
        assert!(out.contains("namespace a"), "{out}");
        assert!(out.contains("struct Alpha"), "{out}");
        // beta stays count-only
        assert!(out.contains("src/beta.cpp  ("), "{out}");
        assert!(!out.contains("namespace b"), "{out}");
        assert!(!out.contains("hint:"), "{out}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn auto_mode_uses_symbols_when_pattern_set() {
        let root = fixture_tree("auto");
        let out = build(
            &root,
            &serde_json::json!({ "paths": ["src"], "pattern": "alpha" }),
        )
        .unwrap();
        assert!(out.contains("[symbols mode]"), "{out}");
        assert!(out.contains("namespace a"), "{out}");
        assert!(!out.contains("src/beta.cpp"), "{out}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pagerank_ranks_central_and_git_dirty_files_ahead_of_alpha_and_suggests_on_wide_trunc() {
        let _g = crate::cache::test_env_lock();
        let cache =
            std::env::temp_dir().join(format!("cbtest_repomap_pr_cache_{}", std::process::id()));
        let root =
            std::env::temp_dir().join(format!("cbtest_repomap_pr_root_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache);
        let _ = std::fs::remove_dir_all(&root);
        std::env::set_var("XDG_CACHE_HOME", &cache);
        std::fs::create_dir_all(root.join("src")).unwrap();

        // Alphabetically last ("src/z_core.rs"), but called by multiple callers!
        std::fs::write(
            root.join("src/z_core.rs"),
            "pub fn core_hub() -> i32 { 42 }\n",
        )
        .unwrap();
        // Alphabetically earlier callers + leaves (9 files total >= WIDE_TREE_MIN_FILES).
        for idx in 0..7 {
            std::fs::write(
                root.join(format!("src/a_caller_{idx}.rs")),
                format!("pub fn caller_{idx}() -> i32 {{ core_hub() }}\n"),
            )
            .unwrap();
        }
        std::fs::write(
            root.join("src/m_recent_leaf.rs"),
            "pub fn isolated_leaf() -> i32 { 1 }\n",
        )
        .unwrap();

        // Initialize git repo and make m_recent_leaf.rs dirty so git-recency boosts it over older callers.
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&root)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
        };
        assert!(git(&["init"]).success());
        assert!(git(&["config", "user.email", "test@example.com"]).success());
        assert!(git(&["config", "user.name", "repomap test"]).success());
        assert!(git(&["add", "."]).success());
        assert!(git(&["commit", "-m", "init"]).success());
        std::fs::write(
            root.join("src/m_recent_leaf.rs"),
            "pub fn isolated_leaf() -> i32 { 2 }\n",
        )
        .unwrap();

        // 1. Default alpha mode: truncated wide tree (>= 8 files) suggests rank:"pagerank".
        let alpha_trunc = build(
            &root,
            &serde_json::json!({ "paths": ["src"], "mode": "files", "token_budget": 18 }),
        )
        .unwrap();
        assert!(alpha_trunc.contains("src/a_caller_0.rs"), "{alpha_trunc}");
        assert!(
            alpha_trunc.contains("rank:\"pagerank\""),
            "expected pagerank suggestion on truncated wide tree:\n{alpha_trunc}"
        );

        // 2. rank:"pagerank" floats z_core.rs to #1 (highest incoming call centrality)
        // and ranks dirty m_recent_leaf.rs ahead of clean leaf callers.
        let pr_out = build(
            &root,
            &serde_json::json!({ "paths": ["src"], "mode": "files", "rank": "pagerank" }),
        )
        .unwrap();
        assert!(pr_out.contains("[files mode, rank=pagerank]"), "{pr_out}");
        let file_lines: Vec<&str> = pr_out
            .lines()
            .filter(|l| l.starts_with("src/") && l.ends_with("symbols)"))
            .collect();
        assert_eq!(file_lines.len(), 9, "{file_lines:?}");
        assert!(
            file_lines[0].starts_with("src/z_core.rs"),
            "expected z_core.rs first under pagerank, got: {file_lines:?}"
        );
        let pos_dirty = file_lines
            .iter()
            .position(|l| l.starts_with("src/m_recent_leaf.rs"))
            .unwrap();
        let pos_clean_caller = file_lines
            .iter()
            .position(|l| l.starts_with("src/a_caller_0.rs"))
            .unwrap();
        assert!(
            pos_dirty < pos_clean_caller,
            "dirty file should outrank clean zero-in-degree caller: {file_lines:?}"
        );

        // 3. Invalid rank errors cleanly.
        assert!(build(&root, &serde_json::json!({ "paths": ["src"], "rank": "bogus" })).is_err());

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&cache);
    }
}
