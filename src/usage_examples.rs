//! usage_examples - a symbol's highest-signal call sites *with surrounding
//! source lines* (not just locations), so an agent sees how an API is actually
//! used in practice. Reuses the `symbol_refs` call classifier to find the sites,
//! then reads a few context lines around each. The first `max` sites in
//! path/line order are shown; output is token-budgeted.

use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

use crate::{stats, symbol_refs};

pub fn build(root: &Path, args: &Value) -> Result<String, String> {
    let symbol = args
        .get("symbol")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if symbol.is_empty() {
        return Err("symbol is required".into());
    }
    let paths = crate::config::paths_from_args(root, args)?;
    let max = args.get("max").and_then(Value::as_u64).unwrap_or(5).max(1) as usize;
    let context = args
        .get("context")
        .and_then(Value::as_u64)
        .unwrap_or(2)
        .min(8) as usize;
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(1200) as usize;

    let (sites, scanned) = symbol_refs::call_sites(root, symbol, &paths);

    let mut out = format!(
        "usage_examples - \"{symbol}\": {} call site(s)  (roots {paths:?})\n",
        sites.len()
    );
    if sites.len() > max {
        out.push_str(&format!("showing the first {max} in path order\n"));
    }
    out.push('\n');

    if sites.is_empty() {
        out.push_str("(no call sites found - try a different symbol or widen \"paths\")\n");
        stats::record("usage_examples", scanned / 4, (out.len() / 4) as u64);
        return Ok(out);
    }

    // Read each file's lines once, even when several sites share a file.
    let mut file_lines: HashMap<String, Vec<String>> = HashMap::new();
    for site in sites.iter().take(max) {
        let lines = file_lines.entry(site.rel.clone()).or_insert_with(|| {
            std::fs::read_to_string(root.join(&site.rel))
                .map(|s| s.lines().map(String::from).collect())
                .unwrap_or_default()
        });
        out.push_str(&format!("{}:{}\n", site.rel, site.line));
        let target = site.line; // 1-based
        let lo = target.saturating_sub(context).max(1);
        let hi = (target + context).min(lines.len());
        for ln in lo..=hi {
            if let Some(text) = lines.get(ln - 1) {
                let marker = if ln == target { "→" } else { " " };
                out.push_str(&format!("  {marker} {ln:>5} | {text}\n"));
            }
        }
        out.push('\n');
        if out.len() / 4 > budget {
            out.push_str("… (truncated by token_budget)\n");
            break;
        }
    }

    stats::record("usage_examples", scanned / 4, (out.len() / 4) as u64);
    Ok(out)
}
