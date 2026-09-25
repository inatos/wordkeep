//! module_map - group symbols by directory/module and report cross-module call
//! edges (coupling), outgoing and incoming. Reuses `call_graph`'s whole-tree
//! adjacency plus a path→module mapping; optional per-edge symbol samples,
//! min_edge filtering, and focus module prefix.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::call_graph::{self, seg};
use crate::continuation::{self, KIND_MODULE_SKIP};
use crate::{progress, stats};

const SAMPLE_CAP: usize = 4;

pub fn build(root: &Path, args: &Value) -> Result<String, String> {
    const FP_KEYS: &[&str] = &[
        "paths",
        "profile",
        "depth",
        "max",
        "samples",
        "min_edge",
        "focus",
    ];
    let paths = crate::config::paths_from_args(root, args)?;
    let depth = args
        .get("depth")
        .and_then(Value::as_u64)
        .map(|d| d.max(1) as usize);
    let max = args.get("max").and_then(Value::as_u64).unwrap_or(40).max(1) as usize;
    let sample_cap = args.get("samples").and_then(Value::as_u64).unwrap_or(3) as usize;
    let min_edge = args
        .get("min_edge")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1);
    let focus = args
        .get("focus")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(1200) as usize;
    let args_fp = continuation::args_fingerprint(args, FP_KEYS);
    let resume_at =
        continuation::resume_offset(args, "module_map", FP_KEYS, KIND_MODULE_SKIP)? as usize;

    progress::tick(0, None, "module_map: adjacency");
    let adj = call_graph::adjacency(root, &paths);

    let mut sym_count: BTreeMap<String, usize> = BTreeMap::new();
    let mut seg_mod: BTreeMap<String, String> = BTreeMap::new();
    for (name, (rel, _line)) in &adj.sites {
        let m = module_of(rel, depth);
        *sym_count.entry(m.clone()).or_insert(0) += 1;
        seg_mod.insert(seg(name).to_string(), m);
    }

    let mut cross: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut edge_samples: BTreeMap<(String, String), Vec<(String, String)>> = BTreeMap::new();
    for (caller, callees) in &adj.callees {
        let Some(from) = seg_mod.get(caller) else {
            continue;
        };
        for callee in callees {
            let Some(to) = seg_mod.get(callee) else {
                continue;
            };
            if from == to {
                continue;
            }
            let key = (from.clone(), to.clone());
            *cross.entry(key.clone()).or_insert(0) += 1;
            let samples = edge_samples.entry(key).or_default();
            if samples.len() < SAMPLE_CAP {
                samples.push((caller.clone(), callee.clone()));
            }
        }
    }

    let filtered = filter_cross(&cross, min_edge);
    let visible = visible_modules(&focus, &sym_count, &filtered);

    let out = render(&RenderCtx {
        paths: &paths,
        depth,
        sym_count: &sym_count,
        cross: &filtered,
        edge_samples: &edge_samples,
        visible: &visible,
        focus: &focus,
        sample_cap,
        max,
        budget,
        args_fp,
        resume_at,
    });
    progress::tick(visible.len() as u64, Some(visible.len() as u64), "module_map: done");
    stats::record("module_map", adj.scanned_bytes / 4, (out.len() / 4) as u64);
    Ok(out)
}

fn module_of(rel: &str, depth: Option<usize>) -> String {
    let parts: Vec<&str> = rel.split('/').collect();
    if parts.len() <= 1 {
        return ".".to_string();
    }
    let dir_parts = &parts[..parts.len() - 1];
    match depth {
        Some(d) => dir_parts
            .iter()
            .take(d)
            .copied()
            .collect::<Vec<_>>()
            .join("/"),
        None => dir_parts.join("/"),
    }
}

fn filter_cross(
    cross: &BTreeMap<(String, String), usize>,
    min_edge: u64,
) -> BTreeMap<(String, String), usize> {
    cross
        .iter()
        .filter(|(_, n)| **n >= min_edge as usize)
        .map(|(k, n)| (k.clone(), *n))
        .collect()
}

fn visible_modules(
    focus: &str,
    sym_count: &BTreeMap<String, usize>,
    cross: &BTreeMap<(String, String), usize>,
) -> BTreeSet<String> {
    if focus.is_empty() {
        return sym_count.keys().cloned().collect();
    }
    let mut vis = BTreeSet::new();
    for m in sym_count.keys() {
        if m.starts_with(focus) {
            vis.insert(m.clone());
        }
    }
    for (from, to) in cross.keys() {
        if from.starts_with(focus) || to.starts_with(focus) {
            vis.insert(from.clone());
            vis.insert(to.clone());
        }
    }
    vis
}

fn group_outgoing(
    cross: &BTreeMap<(String, String), usize>,
) -> BTreeMap<String, Vec<(String, usize)>> {
    let mut by_from: BTreeMap<String, Vec<(String, usize)>> = BTreeMap::new();
    for ((from, to), n) in cross {
        by_from
            .entry(from.clone())
            .or_default()
            .push((to.clone(), *n));
    }
    for v in by_from.values_mut() {
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    }
    by_from
}

fn group_incoming(
    cross: &BTreeMap<(String, String), usize>,
) -> BTreeMap<String, Vec<(String, usize)>> {
    let mut by_to: BTreeMap<String, Vec<(String, usize)>> = BTreeMap::new();
    for ((from, to), n) in cross {
        by_to
            .entry(to.clone())
            .or_default()
            .push((from.clone(), *n));
    }
    for v in by_to.values_mut() {
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    }
    by_to
}

fn edge_sum(edges: Option<&Vec<(String, usize)>>) -> usize {
    edges.map(|v| v.iter().map(|(_, n)| n).sum()).unwrap_or(0)
}

fn format_samples(samples: &[(String, String)], cap: usize) -> String {
    samples
        .iter()
        .take(cap)
        .map(|(c, e)| format!("{c} → {e}"))
        .collect::<Vec<_>>()
        .join(", ")
}

struct RenderCtx<'a> {
    paths: &'a [String],
    depth: Option<usize>,
    sym_count: &'a BTreeMap<String, usize>,
    cross: &'a BTreeMap<(String, String), usize>,
    edge_samples: &'a BTreeMap<(String, String), Vec<(String, String)>>,
    visible: &'a BTreeSet<String>,
    focus: &'a str,
    sample_cap: usize,
    max: usize,
    budget: usize,
    args_fp: u64,
    resume_at: usize,
}

fn render(ctx: &RenderCtx<'_>) -> String {
    let paths = ctx.paths;
    let depth = ctx.depth;
    let sym_count = ctx.sym_count;
    let cross = ctx.cross;
    let edge_samples = ctx.edge_samples;
    let visible = ctx.visible;
    let focus = ctx.focus;
    let sample_cap = ctx.sample_cap;
    let max = ctx.max;
    let budget = ctx.budget;
    let args_fp = ctx.args_fp;
    let resume_at = ctx.resume_at;
    let depth_note = depth
        .map(|d| format!("depth {d}"))
        .unwrap_or_else(|| "full dir".to_string());
    let focus_note = if focus.is_empty() {
        String::new()
    } else {
        format!(", focus \"{focus}\"")
    };
    let mod_count = visible.len();
    let mut out = format!(
        "module_map - {mod_count} module(s), {} cross-module edge(s)  (roots {paths:?}, {depth_note}{focus_note})",
        cross.len()
    );
    if resume_at > 0 {
        out.push_str(&format!(" [continuation from module #{resume_at}]"));
    }
    out.push_str("\n\n");

    if visible.is_empty() {
        out.push_str("(no modules match focus)\n");
        return out;
    }

    let by_from = group_outgoing(cross);
    let by_to = group_incoming(cross);

    let mut modules: Vec<(&String, &usize)> = sym_count
        .iter()
        .filter(|(m, _)| visible.contains(*m))
        .collect();
    modules.sort_by(|a, b| {
        let total_a = edge_sum(by_from.get(a.0)) + edge_sum(by_to.get(a.0));
        let total_b = edge_sum(by_from.get(b.0)) + edge_sum(by_to.get(b.0));
        total_b.cmp(&total_a).then_with(|| a.0.cmp(b.0))
    });

    let mut truncated_at: Option<usize> = None;
    let mut shown = 0usize;
    for (idx, (mod_name, count)) in modules.into_iter().enumerate() {
        if idx < resume_at {
            continue;
        }
        if shown >= max {
            out.push_str(&format!(
                "… (+{} more modules; raise \"max\")\n",
                mod_count - idx
            ));
            break;
        }
        let out_n = edge_sum(by_from.get(mod_name));
        let in_n = edge_sum(by_to.get(mod_name));
        let block = format!("{mod_name}/  ({count} symbols, {out_n} out / {in_n} in)\n");
        if out.len() / 4 + block.len() / 4 > budget && shown > 0 {
            truncated_at = Some(idx);
            break;
        }
        out.push_str(&block);

        let mut edge_truncated = false;
        if let Some(edges) = by_from.get(mod_name) {
            if edges.is_empty() {
                out.push_str("  (no outgoing cross-module calls)\n");
            } else {
                for (to, n) in edges.iter().take(8) {
                    let line = format!("  → {to}/  ({n} call edge(s))\n");
                    if out.len() / 4 + line.len() / 4 > budget {
                        edge_truncated = true;
                        break;
                    }
                    out.push_str(&line);
                    if sample_cap > 0 {
                        if let Some(pairs) = edge_samples.get(&(mod_name.clone(), to.clone())) {
                            let eg = format_samples(pairs, sample_cap);
                            if !eg.is_empty() {
                                let sample_line = format!("      e.g. {eg}\n");
                                if out.len() / 4 + sample_line.len() / 4 > budget {
                                    edge_truncated = true;
                                    break;
                                }
                                out.push_str(&sample_line);
                            }
                        }
                    }
                }
                if !edge_truncated && edges.len() > 8 {
                    out.push_str(&format!(
                        "  … (+{} more outgoing targets)\n",
                        edges.len() - 8
                    ));
                }
            }
        } else {
            out.push_str("  (no outgoing cross-module calls)\n");
        }

        if !edge_truncated {
            if let Some(edges) = by_to.get(mod_name) {
                for (from, n) in edges.iter().take(8) {
                    let line = format!("  ← {from}/  ({n} call edge(s) in)\n");
                    if out.len() / 4 + line.len() / 4 > budget {
                        edge_truncated = true;
                        break;
                    }
                    out.push_str(&line);
                }
                if !edge_truncated && edges.len() > 8 {
                    out.push_str(&format!(
                        "  … (+{} more incoming sources)\n",
                        edges.len() - 8
                    ));
                }
            }
        }

        out.push('\n');
        shown += 1;
        if edge_truncated {
            truncated_at = Some(idx + 1);
            break;
        }
    }
    if let Some(idx) = truncated_at {
        continuation::append_footer(
            &mut out,
            "module_map",
            args_fp,
            idx as u64,
            KIND_MODULE_SKIP,
            &format!("+{} more modules", mod_count.saturating_sub(idx)),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_of_respects_depth() {
        assert_eq!(module_of("src/audio/engine.cpp", None), "src/audio");
        assert_eq!(module_of("src/audio/engine.cpp", Some(1)), "src");
        assert_eq!(module_of("src/audio/engine.cpp", Some(2)), "src/audio");
    }

    #[test]
    fn filter_cross_drops_below_min() {
        let mut cross = BTreeMap::new();
        cross.insert(("src/a".to_string(), "src/b".to_string()), 1);
        cross.insert(("src/a".to_string(), "src/c".to_string()), 5);
        let f = filter_cross(&cross, 3);
        assert_eq!(f.len(), 1);
        assert_eq!(f.get(&("src/a".to_string(), "src/c".to_string())), Some(&5));
    }

    #[test]
    fn visible_modules_includes_coupled_neighbors() {
        let mut sym = BTreeMap::new();
        sym.insert("src/a".to_string(), 2);
        sym.insert("src/b".to_string(), 3);
        sym.insert("src/c".to_string(), 1);
        let mut cross = BTreeMap::new();
        cross.insert(("src/a".to_string(), "src/b".to_string()), 2);
        let vis = visible_modules("src/a", &sym, &cross);
        assert!(vis.contains("src/a"));
        assert!(vis.contains("src/b"));
        assert!(!vis.contains("src/c"));
    }

    #[test]
    fn format_samples_renders_pairs() {
        let pairs = vec![("run".to_string(), "helper".to_string())];
        assert_eq!(format_samples(&pairs, 3), "run → helper");
    }
}
