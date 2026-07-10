//! doc_comment - a symbol's intent and contract without its body.
//!
//! Given a symbol, return just its leading documentation (`///`, `/** */`,
//! `"""…"""`, JSDoc) plus its signature, so an agent learns what a function
//! promises and how to call it without reading the implementation. The comment
//! nodes adjacent to a declaration come straight from `tree-sitter`; the
//! definition is found by the shared `symbol_def` locator.

use serde_json::Value;
use std::path::Path;

use crate::stats;
use crate::symbol_def::{self, DefInfo};

pub fn build(root: &Path, args: &Value) -> Result<String, String> {
    let symbol = args
        .get("symbol")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if symbol.is_empty() {
        return Err(crate::config::symbol_required_err());
    }
    let paths = crate::config::paths_from_args(root, args)?;

    let (info, scanned) = symbol_def::locate(root, symbol, &paths, false);
    let out = match info {
        Some(d) => render(symbol, &d),
        None => format!("doc_comment - \"{symbol}\": no definition found under {paths:?}"),
    };
    stats::record("doc_comment", scanned / 4, (out.len() / 4) as u64);
    Ok(out)
}

fn render(symbol: &str, d: &DefInfo) -> String {
    let mut out = format!(
        "doc_comment - \"{symbol}\"  {}:{}  ({})\n\n",
        d.rel,
        d.line,
        d.kind.label()
    );
    if d.doc.is_empty() {
        out.push_str("(no leading doc comment - showing signature only)\n");
    } else {
        for line in &d.doc {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str("\nsignature:\n");
    out.push_str("  ");
    out.push_str(&d.signature);
    out.push('\n');
    out
}
