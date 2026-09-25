//! Triage composites: multi-symbol context, Tracy→code bundles, and test impact.
//!
//! These tools pack several existing extractors under a shared token budget so
//! agents can scope a change or hitch without N round-trips.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

use crate::{
    call_graph, config, diff_map, progress, stats, symbol_context, symbol_def, symbol_refs, test_map,
    trace_profile,
};

const DEFAULT_BATCH_BUDGET: usize = 4000;
const DEFAULT_PERF_BUDGET: usize = 4000;
const DEFAULT_IMPACT_BUDGET: usize = 2400;

/// Condensed contexts for many symbols under one shared token budget.
pub fn batch_context(root: &Path, args: &Value) -> Result<String, String> {
    let symbols = parse_symbols(args)?;
    if symbols.is_empty() {
        return Err("symbols is required (non-empty array of symbol names)".into());
    }
    let paths = config::paths_from_args(root, args)?;
    let budget = usize_arg(args, "token_budget", DEFAULT_BATCH_BUDGET);
    let (want_context, want_refs) = include_flags(args);
    let per_sym_cap = (budget / symbols.len().max(1)).clamp(200, 1200);

    // One adjacency walk answers callers/callees for every symbol.
    let adj = call_graph::adjacency(root, &paths);
    let mut baseline = adj.scanned_bytes / 4;

    let mut out = format!(
        "batch_context - {} symbol(s)  (roots {paths:?})  budget={budget}\n",
        symbols.len()
    );
    let mut used = out.len() / 4;
    let mut omitted = 0usize;

    for (idx, sym) in symbols.iter().enumerate() {
        if used >= budget {
            omitted = symbols.len() - idx;
            break;
        }
        let remaining = budget.saturating_sub(used);
        let section_budget = remaining.min(per_sym_cap).max(80);

        let mut section = format!("\n## {sym}\n");
        let (info, scanned) = symbol_def::locate(root, sym, &paths, want_context);
        baseline += scanned / 4;

        match &info {
            Some(info) => {
                section.push_str(&format!(
                    "  {}:{}-{}  ({})\n",
                    info.rel,
                    info.line,
                    info.end_line,
                    info.kind.label()
                ));
                section.push_str(&format!("  signature: {}\n", info.signature));
                if want_context && !info.body.is_empty() {
                    let body_chars = section_budget.saturating_mul(2);
                    section.push_str("  definition:\n");
                    section.push_str(&indent_clip(&info.body, body_chars));
                }
            }
            None => {
                if let Some((rel, line)) = adj.sites.get(call_graph::seg(sym)) {
                    section.push_str(&format!("  site (from call graph): {rel}:{line}\n"));
                } else {
                    section.push_str(&format!(
                        "  (no definition under {paths:?})\n  {}\n",
                        config::symbol_not_found_hint(&paths)
                    ));
                }
            }
        }

        if want_refs {
            let callers = adj.callers_of(sym);
            let callees: Vec<String> = adj
                .callees
                .get(call_graph::seg(sym))
                .map(|s| s.iter().cloned().collect())
                .unwrap_or_default();
            section.push_str(&format!("  calls ({}): ", callees.len()));
            if callees.is_empty() {
                section.push_str("(none)\n");
            } else {
                let shown = callees.len().min(12);
                section.push_str(&callees[..shown].join(", "));
                if callees.len() > shown {
                    section.push_str(&format!(" … (+{} more)", callees.len() - shown));
                }
                section.push('\n');
            }
            section.push_str(&format!("  called by ({}):\n", callers.len()));
            if callers.is_empty() {
                section.push_str("    (none)\n");
            } else {
                for (name, rel, line) in callers.iter().take(8) {
                    section.push_str(&format!("    {name}   {rel}:{line}\n"));
                }
                if callers.len() > 8 {
                    section.push_str(&format!("    … (+{} more)\n", callers.len() - 8));
                }
            }
        }

        let stok = section.len() / 4;
        if used + stok > budget && idx > 0 {
            omitted = symbols.len() - idx;
            break;
        }
        used += stok;
        out.push_str(&section);
    }

    if omitted > 0 {
        out.push_str(&format!(
            "\n… ({omitted} later symbol(s) omitted by token_budget; raise token_budget or narrow symbols)\n"
        ));
    }

    stats::record("batch_context", baseline, (out.len() / 4) as u64);
    Ok(out)
}

/// Tracy hitch profile + condensed context/tests for top hotspot symbols.
pub fn perf_triage(root: &Path, args: &Value) -> Result<String, String> {
    let budget = usize_arg(args, "token_budget", DEFAULT_PERF_BUDGET);
    let profile_budget = (budget * 2) / 5;
    let mut profile_args = args.clone();
    if let Some(obj) = profile_args.as_object_mut() {
        obj.insert("token_budget".into(), json!(profile_budget));
    }

    progress::tick(0, Some(3), "perf_triage: profile");
    let mut out = String::from("perf_triage - Tracy hitch → code/tests bundle\n\n");
    let profile = match trace_profile::build(root, &profile_args) {
        Ok(t) => t,
        Err(e) => format!("trace_profile error: {e}\n"),
    };
    out.push_str(&clip_to_tokens(&profile, profile_budget));

    let mut hotspots: Vec<String> = args
        .get("symbol")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| vec![s.to_string()])
        .unwrap_or_else(|| extract_hotspot_names(&profile, 3));

    if hotspots.is_empty() {
        // Fall back: optional symbols array
        if let Ok(syms) = parse_symbols(args) {
            hotspots = syms.into_iter().take(3).collect();
        }
    }

    let paths = config::paths_from_args(root, args).unwrap_or_else(|_| config::default_paths(root));
    let per = budget
        .saturating_sub(out.len() / 4)
        .checked_div(hotspots.len().max(1) * 2)
        .unwrap_or(200)
        .clamp(120, 800);

    if !hotspots.is_empty() {
        out.push_str("\n\n## Hotspot symbol context + tests\n");
        out.push_str(&format!("hotspots: {}\n", hotspots.join(", ")));
    }

    for sym in &hotspots {
        if out.len() / 4 >= budget {
            out.push_str("\n… (further hotspots omitted by token_budget)\n");
            break;
        }
        out.push_str(&format!("\n### {sym}\n"));
        let ctx_args = json!({
            "symbol": sym,
            "paths": paths,
            "token_budget": per,
            "max": 8,
        });
        match symbol_context::build(root, &ctx_args) {
            Ok(c) => out.push_str(&clip_to_tokens(&c, per)),
            Err(e) => out.push_str(&format!("symbol_context error: {e}\n")),
        }
        if out.len() / 4 >= budget {
            break;
        }
        let test_args = json!({
            "symbol": sym,
            "paths": ["tests"],
            "token_budget": per / 2,
            "max": 6,
        });
        match test_map::build(root, &test_args) {
            Ok(t) => {
                out.push('\n');
                out.push_str(&clip_to_tokens(&t, per / 2));
            }
            Err(e) => out.push_str(&format!("\ntest_map error: {e}\n")),
        }
    }

    if runtime_latest_exists(root) {
        out.push_str(
            "\n\n## Runtime snapshot\n\
             `.wordkeep/runtime/latest.json` (or workspace runtime) present — \
             consider `memory_diff` / `locality_hotspots` after capture before/after.\n",
        );
    }

    if out.len() / 4 > budget {
        out = clip_to_tokens(&out, budget);
    }

    stats::record(
        "perf_triage",
        (profile_budget + hotspots.len() * per) as u64,
        (out.len() / 4) as u64,
    );
    Ok(out)
}

/// Rank test files by how many git-changed symbols they reference.
pub fn test_impact(root: &Path, args: &Value) -> Result<String, String> {
    let gitref = args
        .get("ref")
        .and_then(Value::as_str)
        .unwrap_or("HEAD")
        .trim();
    if gitref.is_empty() || gitref.starts_with('-') {
        return Err("ref must be a git revision".into());
    }
    let paths = config::paths_from_args(root, args)?;
    let budget = usize_arg(args, "token_budget", DEFAULT_IMPACT_BUDGET);
    let max = args.get("max").and_then(Value::as_u64).unwrap_or(40).max(1) as usize;

    let changed = diff_map::changed_functions(root, gitref, &paths)?;
    let mut out = format!(
        "test_impact - ref {gitref}  (roots {paths:?})\n\
         {} changed function(s)\n\n",
        changed.len()
    );

    if changed.is_empty() {
        out.push_str("(no changed functions vs ref under search paths)\n");
        stats::record("test_impact", 64, (out.len() / 4) as u64);
        return Ok(out);
    }

    out.push_str("## Changed symbols\n");
    let mut shown = 0usize;
    for (rel, name, line) in &changed {
        if shown >= max || out.len() / 4 > budget / 3 {
            out.push_str(&format!(
                "  … (+{} more)\n",
                changed.len().saturating_sub(shown)
            ));
            break;
        }
        out.push_str(&format!("  {name}  {rel}:{line}\n"));
        shown += 1;
    }

    let test_paths = config::paths_from_args_or(root, &json!({}), &["tests"])?;
    let mut file_hits: BTreeMap<String, usize> = BTreeMap::new();
    let mut baseline = 0u64;
    let mut unique_syms: Vec<String> = Vec::new();
    for (_, name, _) in &changed {
        let seg = call_graph::seg(name).to_string();
        if unique_syms.iter().any(|s| s == &seg) {
            continue;
        }
        unique_syms.push(seg.clone());
    }

    for sym in &unique_syms {
        if out.len() / 4 > budget {
            break;
        }
        let (per, scanned) = symbol_refs::refs_by_file(root, sym, &test_paths);
        baseline += scanned / 4;
        for fr in per {
            *file_hits.entry(fr.rel).or_default() += 1;
        }
    }

    let mut ranked: Vec<(String, usize)> = file_hits.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    out.push_str("\n## Ranked test files (by changed-symbol hits)\n");
    if ranked.is_empty() {
        out.push_str("(no test file references to changed symbols)\n");
    } else {
        for (i, (rel, hits)) in ranked.iter().enumerate() {
            if i >= max || out.len() / 4 > budget {
                out.push_str(&format!("  … (+{} more)\n", ranked.len() - i));
                break;
            }
            out.push_str(&format!("  {hits} hit(s)  {rel}\n"));
        }
    }

    // Suggest a Catch2-style filter from the top test basename when possible.
    let hint_tag = ranked
        .first()
        .and_then(|(rel, _)| {
            std::path::Path::new(rel)
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.trim_start_matches("test_").to_string())
        })
        .unwrap_or_else(|| "unit".into());
    out.push_str(&format!(
        "\nSuggested ctest filter: {}\n",
        config::test_filter_hint(root, &hint_tag)
    ));
    out.push_str("Tip: run the top-ranked test file(s) first rather than the whole suite.\n");

    if out.len() / 4 > budget {
        out = clip_to_tokens(&out, budget);
    }

    stats::record("test_impact", baseline.max(64), (out.len() / 4) as u64);
    Ok(out)
}

fn parse_symbols(args: &Value) -> Result<Vec<String>, String> {
    let Some(arr) = args.get("symbols").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    Ok(arr
        .iter()
        .filter_map(|v| {
            v.as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
        })
        .collect())
}

fn include_flags(args: &Value) -> (bool, bool) {
    let Some(arr) = args.get("include").and_then(Value::as_array) else {
        return (true, true);
    };
    if arr.is_empty() {
        return (true, true);
    }
    let mut ctx = false;
    let mut refs = false;
    for v in arr {
        match v.as_str().unwrap_or("").to_ascii_lowercase().as_str() {
            "context" | "body" | "def" | "definition" => ctx = true,
            "refs" | "calls" | "graph" | "one_hop" | "call_graph" => refs = true,
            "all" => {
                ctx = true;
                refs = true;
            }
            _ => {}
        }
    }
    if !ctx && !refs {
        (true, true)
    } else {
        (ctx, refs)
    }
}

fn usize_arg(args: &Value, key: &str, default: usize) -> usize {
    args.get(key)
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .unwrap_or(default)
        .max(64)
}

fn clip_to_tokens(text: &str, budget: usize) -> String {
    let cap = budget.saturating_mul(4);
    if text.len() <= cap {
        return text.to_string();
    }
    let mut out: String = text.chars().take(cap.saturating_sub(40)).collect();
    out.push_str("\n… (truncated by token_budget)\n");
    out
}

fn indent_clip(body: &str, cap_chars: usize) -> String {
    let clipped: String = if body.chars().count() > cap_chars {
        let t: String = body.chars().take(cap_chars).collect();
        format!("{t}\n… (truncated)")
    } else {
        body.to_string()
    };
    let mut out = String::new();
    for line in clipped.lines() {
        out.push_str("    ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Pull hotspot zone names from a `trace_profile` / `trace_summary` text block.
fn extract_hotspot_names(profile: &str, max: usize) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_table = false;
    for line in profile.lines() {
        let trimmed = line.trim_end();
        if trimmed.contains("sorted by") || trimmed.starts_with("top ") {
            in_table = true;
            continue;
        }
        if trimmed.starts_with("## ") && in_table {
            // Next major section (diff_map / index_stale).
            if !trimmed.contains("Zone") {
                break;
            }
        }
        if !in_table {
            continue;
        }
        if trimmed.is_empty()
            || trimmed.contains("total ms")
            || trimmed.starts_with('…')
            || trimmed.starts_with("Tip:")
        {
            continue;
        }
        // Table rows are left-padded zone names (~30 cols) then numbers.
        let zone = if trimmed.len() >= 30 {
            trimmed[..30].trim()
        } else {
            trimmed.split_whitespace().next().unwrap_or("")
        };
        let cleaned = clean_zone_name(zone);
        if cleaned.is_empty() {
            continue;
        }
        if names.iter().any(|n| n == &cleaned) {
            continue;
        }
        names.push(cleaned);
        if names.len() >= max {
            break;
        }
    }
    names
}

fn clean_zone_name(z: &str) -> String {
    let z = z.trim();
    if z.is_empty() {
        return String::new();
    }
    // Prefer trailing `::` segment; drop path-like prefixes.
    let seg = z
        .rsplit("::")
        .next()
        .unwrap_or(z)
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(z)
        .trim();
    let s: String = seg
        .chars()
        .skip_while(|c| !c.is_ascii_alphanumeric() && *c != '_')
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if s.len() < 2 {
        String::new()
    } else {
        s
    }
}

fn runtime_latest_exists(root: &Path) -> bool {
    root.join(".wordkeep/runtime/latest.json").is_file()
        || crate::workspace::workspace_file(root, "runtime/latest.json").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn clean_zone_strips_tracy_noise() {
        assert_eq!(clean_zone_name("PhysicsStep                   "), "PhysicsStep");
        assert_eq!(clean_zone_name("ns::sys_water_sim"), "sys_water_sim");
        assert!(clean_zone_name("  ").is_empty());
    }

    #[test]
    fn extract_hotspots_from_table() {
        let text = "\
trace_profile - Tracy hitch workflow

## 1. Zone ranking

trace_summary - debug/a.csv
12 zones, sorted by max; share = % of summed zone time
top 3:

zone                           total ms   share    calls    mean us  site
PhysicsStep                         1.000  50.00%        2      100.0  a.cpp:1
sys_water_sim                       0.500  25.00%        1       50.0  b.cpp:2
render_pass                         0.250  12.50%        1       25.0  c.cpp:3

## 2. Git blast radius (diff_map)
";
        let names = extract_hotspot_names(text, 3);
        assert_eq!(
            names,
            vec![
                "PhysicsStep".to_string(),
                "sys_water_sim".to_string(),
                "render_pass".to_string()
            ]
        );
    }

    #[test]
    fn batch_context_shared_budget() {
        let dir = std::env::temp_dir().join(format!("wk_triage_batch_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write(
            &dir,
            "src/a.cpp",
            "void alpha() {}\nvoid beta() { alpha(); }\nvoid gamma() { beta(); }\n",
        );
        let out = batch_context(
            &dir,
            &json!({
                "symbols": ["alpha", "beta", "gamma"],
                "paths": ["src"],
                "token_budget": 2000,
                "include": ["refs"]
            }),
        )
        .unwrap();
        assert!(out.contains("## alpha"), "{out}");
        assert!(out.contains("## beta"), "{out}");
        assert!(out.contains("called by"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn include_flags_default_both() {
        assert_eq!(include_flags(&json!({})), (true, true));
        assert_eq!(include_flags(&json!({"include": ["refs"]})), (false, true));
        assert_eq!(
            include_flags(&json!({"include": ["context"]})),
            (true, false)
        );
    }
}
