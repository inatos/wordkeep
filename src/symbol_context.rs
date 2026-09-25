//! symbol_context - one payload with everything needed to edit a symbol safely.
//!
//! Composes the existing extractors: the symbol's definition body (via
//! `symbol_def`), one hop of `call_graph` (what it calls and who calls it), and,
//! when the symbol is a record, its `type_layout`. The result is the "show me
//! this and its blast radius" view, still token-budgeted so it stays cheap.

use serde_json::Value;
use std::path::Path;

use crate::continuation::{self, KIND_CALLER_SKIP};
use crate::symbol_def::{self, DefInfo, DefKind};
use crate::{call_graph, progress, stats, type_layout};

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
    let max = args.get("max").and_then(Value::as_u64).unwrap_or(40).max(1) as usize;
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(1600) as usize;
    const FP_KEYS: &[&str] = &["symbol", "paths", "profile", "max"];
    let args_fp = continuation::args_fingerprint(args, FP_KEYS);
    let resume_at =
        continuation::resume_offset(args, "symbol_context", FP_KEYS, KIND_CALLER_SKIP)? as usize;

    progress::tick(0, None, "symbol_context: locate");
    let (info, scanned_def) = symbol_def::locate(root, symbol, &paths, true);
    let Some(info) = info else {
        let mut out = format!(
            "symbol_context - \"{symbol}\": no definition found under {paths:?}\n{}",
            crate::config::symbol_not_found_hint(&paths)
        );
        out.push_str(&crate::symbol_resolve::did_you_mean_hint(root, symbol, &paths));
        stats::record("symbol_context", scanned_def / 4, (out.len() / 4) as u64);
        return Ok(out);
    };

    let hop = call_graph::one_hop(root, symbol, &paths);
    // A record's field layout is the other half of "can I touch this safely".
    let layout = if info.kind == DefKind::Record {
        type_layout::layout_block(root, &info.name, &paths, budget / 3).0
    } else {
        None
    };

    let out = render(
        symbol,
        &info,
        &hop,
        layout.as_deref(),
        max,
        budget,
        args_fp,
        resume_at,
    );
    let baseline = (scanned_def + hop.scanned_bytes) / 4;
    progress::tick(1, Some(1), "symbol_context: done");
    stats::record("symbol_context", baseline, (out.len() / 4) as u64);
    Ok(out)
}

fn render(
    symbol: &str,
    info: &DefInfo,
    hop: &call_graph::OneHop,
    layout: Option<&str>,
    max: usize,
    budget: usize,
    args_fp: u64,
    resume_at: usize,
) -> String {
    let mut out = format!(
        "symbol_context - \"{symbol}\"  {}:{}-{}  ({})",
        info.rel,
        info.line,
        info.end_line,
        info.kind.label()
    );
    if resume_at > 0 {
        out.push_str(&format!(" [continuation from caller #{resume_at}]"));
    }
    out.push_str("\n\n");

    // On continuation pages, skip the heavy definition body (already seen).
    if resume_at == 0 {
        out.push_str("signature:\n  ");
        out.push_str(&info.signature);
        out.push('\n');

        if !info.doc.is_empty() {
            out.push_str("\ndoc:\n");
            for line in &info.doc {
                out.push_str("  ");
                out.push_str(line);
                out.push('\n');
            }
        }

        // The body is the largest section; cap it to most of the budget and let the
        // graph/layout sections have the rest.
        out.push_str("\ndefinition:\n");
        let body_cap_chars = budget.saturating_mul(4).saturating_mul(3) / 5;
        out.push_str(&indent_clip(&info.body, body_cap_chars));

        out.push_str(&format!("\ncalls ({}):", hop.callees.len()));
        if hop.callees.is_empty() {
            out.push_str(" (none)\n");
        } else {
            out.push('\n');
            let shown = hop.callees.len().min(max);
            out.push_str("  ");
            out.push_str(&hop.callees[..shown].join(", "));
            if hop.callees.len() > shown {
                out.push_str(&format!(" … (+{} more)", hop.callees.len() - shown));
            }
            out.push('\n');
        }
    }

    out.push_str(&format!("\ncalled by ({}):\n", hop.callers.len()));
    if hop.callers.is_empty() {
        out.push_str("  (none)\n");
    } else {
        let mut truncated_at: Option<usize> = None;
        let mut shown = 0usize;
        for (i, (name, rel, line)) in hop.callers.iter().enumerate() {
            if i < resume_at {
                continue;
            }
            if shown >= max {
                out.push_str(&format!("  … (+{} more)\n", hop.callers.len() - i));
                break;
            }
            let row = format!("  {name}   {rel}:{line}\n");
            if out.len() / 4 + row.len() / 4 > budget && shown > 0 {
                truncated_at = Some(i);
                break;
            }
            out.push_str(&row);
            shown += 1;
        }
        if let Some(idx) = truncated_at {
            continuation::append_footer(
                &mut out,
                "symbol_context",
                args_fp,
                idx as u64,
                KIND_CALLER_SKIP,
                &format!("+{} more callers", hop.callers.len().saturating_sub(idx)),
            );
        }
    }

    if resume_at == 0 {
        if let Some(block) = layout {
            out.push_str("\nlayout:\n");
            for line in block.lines() {
                out.push_str("  ");
                out.push_str(line);
                out.push('\n');
            }
        }
    }

    out
}

/// Indent each line by two spaces and clip the whole block to `cap` characters,
/// appending a truncation note if it overflows.
fn indent_clip(body: &str, cap: usize) -> String {
    let clipped: String = if body.chars().count() > cap {
        let t: String = body.chars().take(cap).collect();
        format!("{t}\n… (truncated; raise token_budget)")
    } else {
        body.to_string()
    };
    let mut out = String::new();
    for line in clipped.lines() {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
    out
}
