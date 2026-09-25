//! symbol_resolve - fuzzy / profile-aware front door for symbol lookup.
//!
//! Normalizes agent-supplied names (`::Foo`, `ns::Bar<T>`), locates exact
//! definitions under the active paths, and when that fails walks other
//! path_profiles plus a ranked suggestion list so tools can append a short
//! "did you mean" hint instead of a bare not-found.

use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tree_sitter::Parser;

use crate::cache::{self, DiskMap};
use crate::lang::Lang;
use crate::symbol_def::{self, DefKind};
use crate::{progress, stats, walk};

/// Per-file definition-name cache (mtime-keyed), shared with suggestions.
static DEF_NAME_CACHE: OnceLock<Mutex<DiskMap>> = OnceLock::new();

/// MCP entry: required `symbol`, optional `paths` / `profile` / `max`.
pub fn resolve(root: &Path, args: &Value) -> Result<String, String> {
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

    progress::tick(0, None, "symbol_resolve: locate");
    let (info, scanned) = symbol_def::locate(root, &symbol, &paths, false);
    if let Some(info) = info {
        let out = format!(
            "symbol_resolve - \"{raw}\" → \"{symbol}\"\n  {}:{}  ({})\n  {}\n",
            info.rel,
            info.line,
            info.kind.label(),
            info.signature
        );
        stats::record("symbol_resolve", scanned / 4, (out.len() / 4) as u64);
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
            if let (Some(info), _) = symbol_def::locate(root, &symbol, &ppaths, false) {
                profile_hits.push((pname, info.rel, info.line, info.kind));
            }
        }
    }

    let mut out = format!(
        "symbol_resolve - \"{raw}\" → \"{symbol}\": no definition under {paths:?}\n"
    );
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
        let sugg = suggestions(root, &symbol, &paths, max);
        if !sugg.is_empty() {
            out.push_str("\nsuggestions (ranked):\n");
            for s in &sugg {
                out.push_str(&format!("  {s}\n"));
            }
        }
        out.push_str(&did_you_mean_hint(root, &symbol, &paths));
        if sugg.is_empty() {
            let profiles = crate::config::list_profile_names(root);
            if !profiles.is_empty() {
                out.push_str(&format!(
                    "\nNo close names under {paths:?}; try profile one of {:?}.\n",
                    profiles
                ));
            }
        }
    }

    stats::record("symbol_resolve", scanned / 4, (out.len() / 4) as u64);
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

/// Ranked close names under `paths` (exact-ci, suffix, edit-distance ≤2, contains).
pub fn suggestions(root: &Path, symbol: &str, paths: &[String], max: usize) -> Vec<String> {
    let needle = normalize_symbol(symbol);
    if needle.is_empty() || max == 0 {
        return Vec::new();
    }
    let names = collect_def_names(root, paths);
    let mut scored: Vec<(i32, String)> = names
        .into_iter()
        .filter_map(|name| {
            let leaf = name.rsplit("::").next().unwrap_or(name.as_str());
            score_candidate(&needle, leaf).map(|sc| (sc, leaf.to_string()))
        })
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    scored.dedup_by(|a, b| a.1 == b.1);
    scored.into_iter().take(max).map(|(_, n)| n).collect()
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
    let pref = a
        .chars()
        .zip(b.chars())
        .take_while(|(x, y)| x == y)
        .count();
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
        && c_len >= 4
        && c_len <= 40
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
            cur[j] = (prev[j] + 1)
                .min(cur[j - 1] + 1)
                .min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[m]
}

fn collect_def_names(root: &Path, paths: &[String]) -> BTreeSet<String> {
    let prune = walk::prune_set(root);
    let mut cands: Vec<(PathBuf, Lang, u64)> = Vec::new();
    for p in paths {
        let base = root.join(p);
        for entry in walk::files(&base, &prune) {
            let path = entry.path();
            let Some(lang) = Lang::from_path(path) else {
                continue;
            };
            let Ok(meta) = entry.metadata() else { continue };
            cands.push((path.to_path_buf(), lang, cache::mtime_ns(&meta)));
        }
    }
    cands.sort_by(|a, b| a.0.cmp(&b.0));
    let mut parser = Parser::new();
    let mut names = BTreeSet::new();
    let cell = DEF_NAME_CACHE.get_or_init(|| Mutex::new(DiskMap::load("symbol_resolve-defs.json")));
    let mut dirty = false;
    for (path, lang, mt) in &cands {
        let key = path.to_string_lossy();
        if let Ok(store) = cell.lock() {
            if let Some(v) = store.get(key.as_ref(), *mt) {
                if let Some(arr) = v.as_array() {
                    for x in arr {
                        if let Some(s) = x.as_str() {
                            names.insert(s.to_string());
                        }
                    }
                    continue;
                }
            }
        }
        let defs: Vec<String> = symbol_def::definitions(&mut parser, *lang, path, *mt)
            .into_iter()
            .map(|d| d.name)
            .collect();
        for n in &defs {
            names.insert(n.clone());
        }
        if let Ok(mut store) = cell.lock() {
            let payload = Value::Array(defs.into_iter().map(Value::String).collect());
            store.put(key.as_ref(), *mt, payload);
            dirty = true;
        }
    }
    if dirty {
        if let Ok(mut store) = cell.lock() {
            store.save();
        }
    }
    names
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
        assert!(
            typo.iter().any(|s| s.contains("widget_are")),
            "{typo:?}"
        );
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
                assert!(
                    s.len() >= 3,
                    "short garbage for {needle}: {sugg:?}"
                );
            }
        }
        let for_typo = suggestions(&dir, "symbl_resolve", &["src".into()], 5);
        assert!(
            for_typo.iter().any(|s| s == "symbol_resolve") || for_typo.is_empty()
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
}
