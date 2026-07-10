//! dead_code - defined symbols with zero callers and zero non-def references,
//! the natural sibling of `undocumented`. Reuses `call_graph`'s adjacency for
//! fan-in and `symbol_refs::occurrence_counts` to confirm no call/ref usage.
//! Probable entry points (`main`, test files), public/exported symbols,
//! generated/vendor-only usage, and git-age stale tiers are tagged as caveats.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use tree_sitter::Parser;

use crate::call_graph::{self, seg};
use crate::diff_map::{git_last_commit_secs, repo_top};
use crate::lang::Lang;
use crate::symbol_def::{self, DefKind};
use crate::{cache, stats, symbol_refs, walk};

struct Item {
    rel: String,
    kind: DefKind,
    line: usize,
    signature: String,
    public_caveat: bool,
    entry_caveat: bool,
    gen_only: bool,
    age_days: Option<u64>,
    stale: bool,
}

struct AgeCache {
    disk: cache::DiskMap,
    now_secs: u64,
    git_ok: bool,
    top: Option<std::path::PathBuf>,
}

impl AgeCache {
    fn new(root: &Path) -> Self {
        let top = repo_top(root);
        AgeCache {
            disk: cache::DiskMap::load("git_age.json"),
            now_secs: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            git_ok: top.is_some(),
            top,
        }
    }

    fn save(&mut self) {
        self.disk.save();
    }

    fn age_days(&mut self, root: &Path, rel: &str, mtime_ns: u64) -> Option<u64> {
        if !self.git_ok {
            return None;
        }
        if let Some(v) = self.disk.get(rel, mtime_ns) {
            return v.get("age_days").and_then(Value::as_u64);
        }
        let top = self.top.as_ref()?;
        let abs = root.join(rel);
        let rel_top = abs
            .strip_prefix(top)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| rel.to_string());
        let commit = git_last_commit_secs(root, &rel_top)?;
        let age = self.now_secs.saturating_sub(commit) / 86_400;
        self.disk.put(rel, mtime_ns, json!({ "age_days": age }));
        Some(age)
    }
}

pub fn build(root: &Path, args: &Value) -> Result<String, String> {
    let paths = crate::config::paths_from_args(root, args)?;
    let generated_paths: Vec<String> = args
        .get("generated_paths")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let stale_days = args
        .get("stale_days")
        .and_then(Value::as_u64)
        .unwrap_or(90)
        .max(1);
    let max = args.get("max").and_then(Value::as_u64).unwrap_or(40).max(1) as usize;
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(1200) as usize;
    let include_private = args
        .get("include_private")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let adj = call_graph::adjacency(root, &paths);
    let (counts, _occ_bytes) = symbol_refs::occurrence_counts(root, &paths);
    let (gen_counts, gen_bytes) = if generated_paths.is_empty() {
        (BTreeMap::new(), 0u64)
    } else {
        symbol_refs::occurrence_counts(root, &generated_paths)
    };

    let prune = walk::prune_set(root);
    let mut parser = Parser::new();
    let mut age_cache = AgeCache::new(root);
    let mut file_ages: BTreeMap<(String, u64), Option<u64>> = BTreeMap::new();
    let mut items: Vec<Item> = Vec::new();
    for p in &paths {
        let base = root.join(p);
        for entry in walk::files(&base, &prune) {
            let path = entry.path();
            let Some(lang) = Lang::from_path(path) else {
                continue;
            };
            let Ok(meta) = entry.metadata() else { continue };
            let mt = cache::mtime_ns(&meta);
            let rel = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            let age_days = *file_ages
                .entry((rel.clone(), mt))
                .or_insert_with(|| age_cache.age_days(root, &rel, mt));
            for d in symbol_def::definitions(&mut parser, lang, path, mt) {
                if !matches!(d.kind, DefKind::Function | DefKind::Record) {
                    continue;
                }
                if !include_private && seg(&d.name).starts_with('_') {
                    continue;
                }
                let name_seg = seg(&d.name);
                if adj.callers.get(name_seg).map(|s| s.len()).unwrap_or(0) > 0 {
                    continue;
                }
                let (defs, calls, refs) = counts
                    .get(name_seg)
                    .copied()
                    .or_else(|| counts.get(&d.name).copied())
                    .unwrap_or((1, 0, 0));
                let external = calls + refs;
                if external > 0 {
                    continue;
                }
                let _ = defs;
                let gen_only = gen_counts
                    .get(name_seg)
                    .or_else(|| gen_counts.get(&d.name))
                    .map(|(_, c, r)| *c + *r > 0)
                    .unwrap_or(false);
                let public_caveat = d.exported || is_header_path(&rel);
                let entry_caveat = is_probable_entry(name_seg, &rel);
                let stale = age_days.map(|d| d >= stale_days).unwrap_or(false);
                items.push(Item {
                    rel: rel.clone(),
                    kind: d.kind,
                    line: d.line,
                    signature: d.signature,
                    public_caveat,
                    entry_caveat,
                    gen_only,
                    age_days,
                    stale,
                });
            }
        }
    }
    age_cache.save();

    items.sort_by(|a, b| {
        let ca = a.public_caveat || a.entry_caveat || a.gen_only;
        let cb = b.public_caveat || b.entry_caveat || b.gen_only;
        ca.cmp(&cb)
            .then_with(|| b.stale.cmp(&a.stale))
            .then_with(|| a.rel.cmp(&b.rel))
            .then_with(|| a.line.cmp(&b.line))
    });

    let out = render(&paths, stale_days, &items, max, budget);
    stats::record(
        "dead_code",
        (adj.scanned_bytes + gen_bytes) / 4,
        (out.len() / 4) as u64,
    );
    Ok(out)
}

fn is_header_path(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    lower.ends_with(".h")
        || lower.ends_with(".hpp")
        || lower.ends_with(".hh")
        || lower.ends_with(".hxx")
        || lower.ends_with(".cuh")
}

fn is_probable_entry(name: &str, rel: &str) -> bool {
    if name == "main" {
        return true;
    }
    if rel.starts_with("tests/") || rel.contains("/tests/") {
        return true;
    }
    let base = rel.rsplit('/').next().unwrap_or(rel);
    if base.starts_with("test_") || base.ends_with("_test.cpp") || base.ends_with("_test.rs") {
        return true;
    }
    false
}

fn caveat_tags(
    public: bool,
    entry: bool,
    gen_only: bool,
    stale: bool,
    age_days: Option<u64>,
) -> String {
    let mut tags = String::new();
    if public {
        tags.push_str("  [public]");
    }
    if entry {
        tags.push_str("  [entry?]");
    }
    if gen_only {
        tags.push_str("  [gen-only]");
    }
    if stale {
        if let Some(d) = age_days {
            tags.push_str(&format!("  [stale {d}d]"));
        } else {
            tags.push_str("  [stale]");
        }
    }
    tags
}

fn render(paths: &[String], stale_days: u64, items: &[Item], max: usize, budget: usize) -> String {
    let total = items.len();
    let mut out = format!(
        "dead_code - {total} symbol(s) with zero callers and zero references  \
         (roots {paths:?}; [public] = exported/header API, [entry?] = probable entry, \
         [gen-only] = used only under generated_paths, [stale Nd] = file last touched \
         ≥{stale_days}d ago - verify before removing)\n\n"
    );
    if total == 0 {
        out.push_str("(no unused symbols in scope)\n");
        return out;
    }
    for (idx, it) in items.iter().enumerate() {
        if idx >= max {
            out.push_str(&format!("… (+{} more; raise \"max\")\n", total - idx));
            break;
        }
        let tags = caveat_tags(
            it.public_caveat,
            it.entry_caveat,
            it.gen_only,
            it.stale,
            it.age_days,
        );
        let sig = squeeze(&it.signature, 90);
        let line = format!(
            "  {}:{}  ({})  {}{}\n",
            it.rel,
            it.line,
            it.kind.label(),
            sig,
            tags
        );
        if out.len() / 4 + line.len() / 4 > budget && idx > 0 {
            out.push_str("… (truncated by token_budget)\n");
            break;
        }
        out.push_str(&line);
    }
    out
}

fn squeeze(s: &str, cap: usize) -> String {
    let one = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= cap {
        one
    } else {
        let t: String = one.chars().take(cap.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_point_detection() {
        assert!(is_probable_entry("main", "src/app.cpp"));
        assert!(is_probable_entry("foo", "tests/unit/foo.cpp"));
        assert!(is_probable_entry("bar", "src/test_bar.cpp"));
        assert!(!is_probable_entry("helper", "src/util/helper.cpp"));
    }

    #[test]
    fn is_header_path_detects_common_extensions() {
        assert!(is_header_path("src/foo.h"));
        assert!(is_header_path("src/bar.hpp"));
        assert!(is_header_path("src/baz.cuh"));
        assert!(!is_header_path("src/foo.cpp"));
    }

    #[test]
    fn caveat_tags_combine() {
        assert_eq!(caveat_tags(true, false, false, false, None), "  [public]");
        assert_eq!(caveat_tags(false, false, true, false, None), "  [gen-only]");
        assert_eq!(
            caveat_tags(false, false, false, true, Some(120)),
            "  [stale 120d]"
        );
    }

    #[test]
    fn confidence_sort_stale_clean_first() {
        let mut items = [
            Item {
                rel: "a.cpp".into(),
                kind: DefKind::Function,
                line: 1,
                signature: String::new(),
                public_caveat: false,
                entry_caveat: false,
                gen_only: false,
                age_days: Some(10),
                stale: false,
            },
            Item {
                rel: "b.cpp".into(),
                kind: DefKind::Function,
                line: 1,
                signature: String::new(),
                public_caveat: false,
                entry_caveat: false,
                gen_only: false,
                age_days: Some(100),
                stale: true,
            },
        ];
        items.sort_by(|a, b| {
            let ca = a.public_caveat || a.entry_caveat || a.gen_only;
            let cb = b.public_caveat || b.entry_caveat || b.gen_only;
            ca.cmp(&cb)
                .then_with(|| b.stale.cmp(&a.stale))
                .then_with(|| a.rel.cmp(&b.rel))
        });
        assert_eq!(items[0].rel, "b.cpp");
    }
}
