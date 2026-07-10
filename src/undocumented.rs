//! undocumented - exported symbols that lack a leading doc comment, ranked by
//! fan-in (how many distinct functions call them), so an agent knows which
//! public APIs most need documentation. Reuses the `doc_comment` locator (via
//! `symbol_def::definitions`, which reports doc-presence) and `call_graph`'s
//! whole-tree adjacency for caller counts. Leading-underscore names (Python
//! privates, `_impl` helpers) are skipped unless asked for. Token-budgeted.

use serde_json::Value;
use std::path::Path;

use tree_sitter::Parser;

use crate::call_graph::{self, seg};
use crate::lang::Lang;
use crate::symbol_def::{self, DefKind};
use crate::{cache, stats, walk};

struct Item {
    rel: String,
    kind: DefKind,
    line: usize,
    signature: String,
    fan_in: usize,
}

pub fn build(root: &Path, args: &Value) -> Result<String, String> {
    let paths = crate::config::paths_from_args(root, args)?;
    let max = args.get("max").and_then(Value::as_u64).unwrap_or(40).max(1) as usize;
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(1200) as usize;
    let include_private = args
        .get("include_private")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // Fan-in comes from the whole-tree call adjacency, built once (warm-cached).
    // Its scanned-bytes also serve as the savings baseline: this is the source an
    // agent would otherwise read to find undocumented APIs.
    let adj = call_graph::adjacency(root, &paths);

    let prune = walk::prune_set(root);
    let mut parser = Parser::new();
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
            for d in symbol_def::definitions(&mut parser, lang, path, mt) {
                if d.has_doc {
                    continue;
                }
                // Public-API surface: functions and records (types). Other kinds
                // (type aliases, traits) are noisier and left out.
                if !matches!(d.kind, DefKind::Function | DefKind::Record) {
                    continue;
                }
                if !include_private && seg(&d.name).starts_with('_') {
                    continue;
                }
                let fan_in = adj.callers.get(seg(&d.name)).map(|s| s.len()).unwrap_or(0);
                items.push(Item {
                    rel: rel.clone(),
                    kind: d.kind,
                    line: d.line,
                    signature: d.signature,
                    fan_in,
                });
            }
        }
    }

    // Most-called undocumented APIs first (they hurt most), then a stable
    // path/line order so output is deterministic.
    items.sort_by(|a, b| {
        b.fan_in
            .cmp(&a.fan_in)
            .then_with(|| a.rel.cmp(&b.rel))
            .then_with(|| a.line.cmp(&b.line))
    });

    let out = render(&paths, &items, max, budget);
    stats::record(
        "undocumented",
        adj.scanned_bytes / 4,
        (out.len() / 4) as u64,
    );
    Ok(out)
}

fn render(paths: &[String], items: &[Item], max: usize, budget: usize) -> String {
    let total = items.len();
    let mut out = format!(
        "undocumented - {total} symbol(s) lacking a leading doc comment  \
         (roots {paths:?}, ranked by caller count)\n\n"
    );
    if total == 0 {
        out.push_str("(every function/record in scope has a leading doc comment)\n");
        return out;
    }
    for (idx, it) in items.iter().enumerate() {
        if idx >= max {
            out.push_str(&format!("… (+{} more; raise \"max\")\n", total - idx));
            break;
        }
        let sig = squeeze(&it.signature, 100);
        let line = format!(
            "  {:>3}×  {}:{}  ({})  {}\n",
            it.fan_in,
            it.rel,
            it.line,
            it.kind.label(),
            sig
        );
        if out.len() / 4 + line.len() / 4 > budget && idx > 0 {
            out.push_str("… (truncated by token_budget)\n");
            break;
        }
        out.push_str(&line);
    }
    out
}

/// Collapse interior whitespace to single spaces and cap the length, so a
/// multi-line signature renders on one tidy row.
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
    fn squeeze_collapses_and_caps() {
        assert_eq!(squeeze("a   b\n c", 100), "a b c");
        assert_eq!(squeeze(&"x".repeat(200), 10).chars().count(), 10);
    }
}
