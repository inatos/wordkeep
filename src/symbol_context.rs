//! symbol_context - one payload with everything needed to edit a symbol safely.
//!
//! Composes the existing extractors: the symbol's definition body (via
//! `symbol_def`), one hop of `call_graph` (what it calls and who calls it), and,
//! when the symbol is a record, its `type_layout`. The result is the "show me
//! this and its blast radius" view, still token-budgeted so it stays cheap.

use serde_json::Value;
use std::path::Path;

use crate::continuation::{self, KIND_CALLER_SKIP};
use crate::store;
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
    let max = args.get("max").and_then(Value::as_u64).unwrap_or(8).max(1) as usize;
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(1600) as usize;
    let if_none_match = args.get("if_none_match").and_then(Value::as_str);
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
        out.push_str(&crate::symbol_resolve::did_you_mean_hint(
            root, symbol, &paths,
        ));
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

    let etag = compute_etag(
        root,
        symbol,
        &paths,
        max,
        budget,
        resume_at,
        &info,
        &hop,
        layout.as_deref(),
    );
    let baseline = (scanned_def + hop.scanned_bytes) / 4;

    if store::etag_matches(if_none_match, &etag) {
        let short = store::unchanged_etag_line(&etag);
        progress::tick(1, Some(1), "symbol_context: unchanged");
        stats::record_ext(
            "symbol_context",
            stats::RecordMeta {
                baseline,
                returned: (short.len() / 4) as u64,
                bytes_read: hop.bytes_read,
                cache_hits: hop.cache_hits.saturating_add(1),
                cache_misses: hop.cache_misses,
                reason: Some("etag_hit".to_string()),
            },
        );
        return Ok(short);
    }

    let out = render(
        symbol,
        &info,
        &hop,
        layout.as_deref(),
        &etag,
        max,
        budget,
        args_fp,
        resume_at,
    );
    progress::tick(1, Some(1), "symbol_context: done");
    stats::record_ext(
        "symbol_context",
        stats::RecordMeta {
            baseline,
            returned: (out.len() / 4) as u64,
            bytes_read: hop.bytes_read,
            cache_hits: hop.cache_hits,
            cache_misses: hop.cache_misses,
            reason: None,
        },
    );
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn compute_etag(
    root: &Path,
    symbol: &str,
    paths: &[String],
    max: usize,
    budget: usize,
    resume_at: usize,
    info: &DefInfo,
    hop: &call_graph::OneHop,
    layout: Option<&str>,
) -> String {
    let file_bytes = std::fs::read(root.join(&info.rel))
        .unwrap_or_else(|_| info.body.as_bytes().to_vec());
    let mut graph_sig = String::new();
    for c in &hop.callees {
        if let Some((src, conf)) = hop.callee_provenance.get(c) {
            graph_sig.push_str(&format!("ce:{c}:{src}:{conf:.2}\n"));
        } else {
            graph_sig.push_str(&format!("ce:{c}\n"));
        }
    }
    for c in &hop.caller_sites {
        graph_sig.push_str(&format!(
            "cr:{}:{}:{}:{}:{:.2}:{}\n",
            c.name, c.rel, c.line, c.source, c.confidence, c.is_test as u8
        ));
    }
    graph_sig.push_str(&format!(
        "hub:{}:{}:{}:{}\n",
        hop.hub.fan_in, hop.hub.prod_fan_in, hop.hub.k_core, hop.hub.fan_out
    ));
    let paths_joined = paths.join(",");
    let meta = format!("{symbol}\0{paths_joined}\0{max}\0{budget}\0{resume_at}\0{}", info.rel);
    store::etag_hex(&[
        b"symbol_context\0v1",
        meta.as_bytes(),
        &file_bytes,
        graph_sig.as_bytes(),
        layout.unwrap_or("").as_bytes(),
    ])
}

#[allow(clippy::too_many_arguments)]
fn render(
    symbol: &str,
    info: &DefInfo,
    hop: &call_graph::OneHop,
    layout: Option<&str>,
    etag: &str,
    max: usize,
    budget: usize,
    args_fp: u64,
    resume_at: usize,
) -> String {
    let mut out = format!(
        "symbol_context - \"{symbol}\"  {}:{}-{}  ({})  etag={etag}",
        info.rel,
        info.line,
        info.end_line,
        info.kind.label()
    );
    if let Some(badge) = hop.hub.badge() {
        out.push_str(&format!("  {badge}"));
    }
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

        let uniform_prov = uniform_callee_provenance(&hop.callees, &hop.callee_provenance);
        if let Some(ref prov) = uniform_prov {
            out.push_str(&format!("\ncalls ({}) [{prov}]:", hop.callees.len()));
        } else {
            out.push_str(&format!("\ncalls ({}):", hop.callees.len()));
        }
        if hop.callees.is_empty() {
            out.push_str(" (none)\n");
        } else {
            out.push('\n');
            let shown = hop.callees.len().min(max);
            out.push_str("  ");
            if uniform_prov.is_some() {
                out.push_str(&hop.callees[..shown].join(", "));
            } else {
                let items: Vec<String> = hop.callees[..shown]
                    .iter()
                    .map(|c| {
                        if let Some((src, conf)) = hop.callee_provenance.get(c) {
                            format!("{c} [{}]", call_graph::format_provenance(src, *conf))
                        } else {
                            c.clone()
                        }
                    })
                    .collect();
                out.push_str(&items.join(", "));
            }
            if hop.callees.len() > shown {
                out.push_str(&format!(" … (+{} more)", hop.callees.len() - shown));
            }
            out.push('\n');
        }
    }

    let total_callers = hop.caller_sites.len();
    let prod_callers = hop.caller_sites.iter().filter(|c| !c.is_test).count();
    let test_callers = total_callers.saturating_sub(prod_callers);
    let count_label = call_graph::format_caller_count(total_callers, prod_callers, test_callers);
    out.push_str(&format!("\ncalled by ({count_label}):\n"));
    if hop.caller_sites.is_empty() {
        out.push_str("  (none)\n");
    } else {
        let mut truncated_at: Option<usize> = None;
        let mut shown = 0usize;
        for (i, c) in hop.caller_sites.iter().enumerate() {
            if i < resume_at {
                continue;
            }
            if shown >= max {
                truncated_at = Some(i);
                break;
            }
            let tag = call_graph::format_caller_tag(c);
            let row = format!("  {}   {}:{}  {tag}\n", c.name, c.rel, c.line);
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
                &format!("+{} more callers", total_callers.saturating_sub(idx)),
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

fn uniform_callee_provenance(
    callees: &[String],
    prov: &std::collections::BTreeMap<String, (String, f64)>,
) -> Option<String> {
    if callees.is_empty() {
        return None;
    }
    let first = prov.get(&callees[0])?;
    let label = call_graph::format_provenance(&first.0, first.1);
    for c in &callees[1..] {
        let p = prov.get(c)?;
        if call_graph::format_provenance(&p.0, p.1) != label {
            return None;
        }
    }
    Some(label)
}

/// Indent each line by two spaces and clip the whole block to `cap` characters,
/// appending a truncation note if it overflows.
fn indent_clip(body: &str, cap: usize) -> String {
    let clipped: String = if body.chars().count() > cap {
        let t: String = body.chars().take(cap).collect();
        format!("{t}\n… (definition clipped to token_budget)")
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn extract_etag(text: &str) -> String {
        text.lines()
            .next()
            .and_then(|l| l.split("etag=").nth(1))
            .and_then(|rest| rest.split_whitespace().next())
            .expect("etag= missing from header")
            .to_string()
    }

    #[test]
    fn caller_cap_pages_with_continuation() {
        let info = DefInfo {
            rel: "src/a.rs".into(),
            line: 1,
            end_line: 2,
            kind: DefKind::Function,
            name: "foo".into(),
            signature: "fn foo()".into(),
            doc: Vec::new(),
            body: "fn foo() {}".into(),
        };
        let caller_sites: Vec<call_graph::CallerSite> = (0..12)
            .map(|i| call_graph::CallerSite {
                name: format!("c{i}"),
                rel: "src/b.rs".into(),
                line: i + 1,
                text: String::new(),
                source: store::EDGE_SOURCE_TREESITTER.into(),
                confidence: store::CONFIDENCE_TREESITTER,
                is_test: false,
            })
            .collect();
        let hop = call_graph::OneHop {
            callees: Vec::new(),
            callee_provenance: BTreeMap::new(),
            callers: caller_sites
                .iter()
                .map(|c| (c.name.clone(), c.rel.clone(), c.line))
                .collect(),
            caller_sites,
            hub: call_graph::classify_hub(12, 12, 0, 0, 2),
            scanned_bytes: 0,
            bytes_read: 0,
            cache_hits: 0,
            cache_misses: 0,
        };
        let out = render("foo", &info, &hop, None, "0123456789abcdef", 8, 1600, 1, 0);
        assert!(out.contains("c0"), "{out}");
        assert!(out.contains("c7"), "{out}");
        assert!(!out.contains("  c8   "), "{out}");
        assert!(out.contains("continuation:"), "{out}");
        assert!(out.contains("fn foo()"), "{out}");
        assert!(out.contains("etag=0123456789abcdef"), "{out}");
        assert!(out.contains("[core-hub fan-in=12 k-core=2]"), "{out}");
    }

    struct TempXdg {
        _lock: std::sync::MutexGuard<'static, ()>,
        xdg: std::path::PathBuf,
        prev: Option<std::ffi::OsString>,
    }

    impl TempXdg {
        fn new(tag: &str) -> Self {
            let lock = crate::cache::test_env_lock();
            let xdg =
                std::env::temp_dir().join(format!("wk_symctx_xdg_{tag}_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&xdg);
            std::fs::create_dir_all(&xdg).unwrap();
            let prev = std::env::var_os("XDG_CACHE_HOME");
            std::env::set_var("XDG_CACHE_HOME", &xdg);
            Self {
                _lock: lock,
                xdg,
                prev,
            }
        }
    }

    impl Drop for TempXdg {
        fn drop(&mut self) {
            if let Some(ref p) = self.prev {
                std::env::set_var("XDG_CACHE_HOME", p);
            }
            let _ = std::fs::remove_dir_all(&self.xdg);
        }
    }

    #[test]
    fn symbol_context_etag_hit_miss_and_invalidation() {
        let _env = TempXdg::new("etag");
        let root = std::env::temp_dir().join(format!("wk_symctx_etag_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/mod.rs"),
            "fn helper() -> i32 { 1 }\nfn run() -> i32 { helper() }\n",
        )
        .unwrap();

        let first = build(
            &root,
            &serde_json::json!({ "symbol": "helper", "paths": ["src"] }),
        )
        .unwrap();
        let etag1 = extract_etag(&first);
        assert_eq!(etag1.len(), 16);
        assert!(first.contains("fn helper()"), "{first}");

        // Matching if_none_match -> 1-line unchanged response.
        let hit = build(
            &root,
            &serde_json::json!({
                "symbol": "helper",
                "paths": ["src"],
                "if_none_match": etag1
            }),
        )
        .unwrap();
        assert_eq!(hit, format!("unchanged (etag={etag1})\n"));
        assert_eq!(hit.lines().count(), 1);

        // Non-matching if_none_match -> full payload.
        let miss = build(
            &root,
            &serde_json::json!({
                "symbol": "helper",
                "paths": ["src"],
                "if_none_match": "0000000000000000"
            }),
        )
        .unwrap();
        assert!(miss.contains("fn helper()"), "{miss}");
        assert_eq!(extract_etag(&miss), etag1);

        // Editing source invalidates ETag so old if_none_match misses and returns updated body.
        std::fs::write(
            root.join("src/mod.rs"),
            "fn helper() -> i32 { 2 }\nfn run() -> i32 { helper() }\n",
        )
        .unwrap();
        let after = build(
            &root,
            &serde_json::json!({
                "symbol": "helper",
                "paths": ["src"],
                "if_none_match": etag1
            }),
        )
        .unwrap();
        assert!(after.contains("{ 2 }"), "{after}");
        let etag2 = extract_etag(&after);
        assert_ne!(etag1, etag2);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn symbol_context_prioritizes_prod_callers_shows_hub_and_scip_provenance() {
        let _env = TempXdg::new("rank");
        let root = std::env::temp_dir().join(format!("wk_symctx_rank_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "fn core_fn() -> i32 { 7 }\n\
             fn prod_alpha() -> i32 { core_fn() }\n\
             fn prod_beta() -> i32 { core_fn() }\n\
             fn prod_gamma() -> i32 { core_fn() }\n\
             fn prod_delta() -> i32 { core_fn() }\n\
             fn prod_eps() -> i32 { core_fn() }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("tests/test_lib.rs"),
            "fn aaa_test_case() -> i32 { core_fn() }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("index.scip"),
            r#"{
                "metadata": { "tool_info": { "name": "rust-analyzer" } },
                "edges": [
                    { "file": "src/lib.rs", "src_name": "prod_alpha", "dst_name": "core_fn", "line": 2, "kind": "call" }
                ]
            }"#,
        )
        .unwrap();

        let out = build(
            &root,
            &serde_json::json!({
                "symbol": "core_fn",
                "paths": ["src", "tests"],
                "max": 8
            }),
        )
        .unwrap();
        assert!(out.contains("[hub fan-in=6 k-core=1]"), "{out}");
        assert!(out.contains("called by (6 total: 5 prod, 1 test):"), "{out}");
        assert!(out.contains("prod_alpha   src/lib.rs:2  [scip 1.0]"), "{out}");
        let p_prod = out.find("prod_eps").unwrap();
        let p_test = out.find("aaa_test_case").unwrap();
        assert!(p_prod < p_test, "prod must precede test:\n{out}");
        assert!(out.contains("[test, treesitter 0.5]"), "{out}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn symbol_context_shows_binary_scip_provenance_on_callees_and_callers() {
        let _env = TempXdg::new("scip_e2e");
        let root = std::env::temp_dir().join(format!("wk_symctx_scip_e2e_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/engine.rs"),
            "fn leaf_a() -> i32 { 1 }\n\
             fn leaf_b() -> i32 { 2 }\n\
             fn middle() -> i32 { leaf_a() + leaf_b() }\n\
             fn top() -> i32 { middle() }\n",
        )
        .unwrap();
        std::fs::write(root.join("Cargo.lock"), "# lock v1\n").unwrap();
        // SCIP overlay verifies middle -> leaf_a and top -> middle, leaving middle -> leaf_b as treesitter 0.5.
        std::fs::write(
            root.join("index.scip"),
            r#"{
                "metadata": { "tool_info": { "name": "rust-analyzer" } },
                "edges": [
                    { "file": "src/engine.rs", "src_name": "middle", "dst_name": "leaf_a", "line": 3, "kind": "call" },
                    { "file": "src/engine.rs", "src_name": "top", "dst_name": "middle", "line": 4, "kind": "call" }
                ]
            }"#,
        )
        .unwrap();

        let out = build(
            &root,
            &serde_json::json!({ "symbol": "middle", "paths": ["src"] }),
        )
        .unwrap();
        // Mixed callee provenance renders inline tags; caller shows [scip 1.0].
        assert!(out.contains("leaf_a [scip 1.0]"), "{out}");
        assert!(out.contains("leaf_b [treesitter 0.5]"), "{out}");
        assert!(out.contains("top   src/engine.rs:4  [scip 1.0]"), "{out}");

        // And index_health reports the active SCIP overlay + manifest SHA + edge counts.
        let health = crate::index_stale::health(&root, &serde_json::json!({"ref": "HEAD"})).unwrap();
        assert!(health.contains("## SCIP / Semantic overlay"), "{health}");
        assert!(health.contains("active"), "{health}");
        assert!(health.contains("Cargo.lock [sha="), "{health}");
        assert!(health.contains("scip=2 (1.0)"), "{health}");

        let _ = std::fs::remove_dir_all(&root);
    }
}
