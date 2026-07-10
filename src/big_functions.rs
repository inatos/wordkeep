//! big_functions - largest function bodies by line span, ranked as refactor
//! candidates. Reuses `symbol_def::function_spans` (start/end lines + signature)
//! across the tree; cheap, deterministic, token-budgeted.

use serde_json::Value;
use std::path::Path;

use tree_sitter::Parser;

use crate::lang::Lang;
use crate::{cache, stats, symbol_def, walk};

struct Item {
    rel: String,
    start: usize,
    lines: usize,
    signature: String,
}

pub fn build(root: &Path, args: &Value) -> Result<String, String> {
    let paths = crate::config::paths_from_args(root, args)?;
    let max = args.get("max").and_then(Value::as_u64).unwrap_or(30).max(1) as usize;
    let min_lines = args
        .get("min_lines")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1) as usize;
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(1200) as usize;

    let prune = walk::prune_set(root);
    let mut parser = Parser::new();
    let mut items: Vec<Item> = Vec::new();
    let mut scanned_bytes = 0u64;
    for p in &paths {
        let base = root.join(p);
        for entry in walk::files(&base, &prune) {
            let path = entry.path();
            let Some(lang) = Lang::from_path(path) else {
                continue;
            };
            let Ok(meta) = entry.metadata() else { continue };
            scanned_bytes += meta.len();
            let mt = cache::mtime_ns(&meta);
            let rel = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            for sp in symbol_def::function_spans(&mut parser, lang, path, mt) {
                let lines = sp.end.saturating_sub(sp.start).saturating_add(1);
                if lines < min_lines {
                    continue;
                }
                items.push(Item {
                    rel: rel.clone(),
                    start: sp.start,
                    lines,
                    signature: sp.signature,
                });
            }
        }
    }

    items.sort_by(|a, b| {
        b.lines
            .cmp(&a.lines)
            .then_with(|| a.rel.cmp(&b.rel))
            .then_with(|| a.start.cmp(&b.start))
    });

    let out = render(&paths, min_lines, &items, max, budget);
    stats::record("big_functions", scanned_bytes / 4, (out.len() / 4) as u64);
    Ok(out)
}

fn render(paths: &[String], min_lines: usize, items: &[Item], max: usize, budget: usize) -> String {
    let total = items.len();
    let mut out = format!(
        "big_functions - {total} function(s) with ≥{min_lines} line(s)  (roots {paths:?}, ranked by span)\n\n"
    );
    if total == 0 {
        out.push_str("(no functions in scope)\n");
        return out;
    }
    for (idx, it) in items.iter().enumerate() {
        if idx >= max {
            out.push_str(&format!("… (+{} more; raise \"max\")\n", total - idx));
            break;
        }
        let sig = squeeze(&it.signature, 90);
        let line = format!("  {:>4}L  {}:{}  {}\n", it.lines, it.rel, it.start, sig);
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
    fn line_span_includes_both_endpoints() {
        assert_eq!(10_usize.saturating_sub(3).saturating_add(1), 8);
    }

    #[test]
    fn squeeze_caps() {
        assert_eq!(squeeze("a  b", 10), "a b");
    }
}
