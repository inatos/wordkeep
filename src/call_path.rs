//! call_path - the shortest call chain between two functions.
//!
//! A depth-bounded breadth-first search over the one-hop edges `call_graph`
//! already distils, so an agent can trace how a leaf change propagates up to an
//! entry point (or down to a primitive) without walking `call_graph` by hand.
//! Names match on their trailing `::` segment, like the rest of the call tools.
//! BFS yields a *shortest* chain; a visited set bounds every node once, so it is
//! cycle-safe. Token-budgeted.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use crate::call_graph::{self, seg};
use crate::stats;

pub fn build(root: &Path, args: &Value) -> Result<String, String> {
    let from = args
        .get("from")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let to = args.get("to").and_then(Value::as_str).unwrap_or("").trim();
    if from.is_empty() || to.is_empty() {
        return Err("both 'from' and 'to' are required".into());
    }
    let paths = crate::config::paths_from_args(root, args)?;
    let max_depth = args
        .get("max_depth")
        .and_then(Value::as_u64)
        .unwrap_or(8)
        .clamp(1, 64) as usize;
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(1200) as usize;

    // One whole-tree adjacency build (warm-cached) backs both the search and the
    // "why no path" diagnostics.
    let adj = call_graph::adjacency(root, &paths);
    let from_k = seg(from).to_string();
    let to_k = seg(to).to_string();

    let path = bfs(&adj.callees, &from_k, &to_k, max_depth);
    let out = render(from, to, &paths, &adj, &path, max_depth, budget);
    stats::record("call_path", adj.scanned_bytes / 4, (out.len() / 4) as u64);
    Ok(out)
}

/// Breadth-first *shortest* path over the callee adjacency, from `from` to `to`,
/// bounded to `max_depth` hops. Cycle-safe via the `depth` visited map: each
/// node is enqueued at most once.
fn bfs(
    callees: &BTreeMap<String, BTreeSet<String>>,
    from: &str,
    to: &str,
    max_depth: usize,
) -> Option<Vec<String>> {
    if from == to {
        return Some(vec![from.to_string()]);
    }
    let mut prev: BTreeMap<String, String> = BTreeMap::new();
    let mut depth: BTreeMap<String, usize> = BTreeMap::new();
    let mut q: VecDeque<String> = VecDeque::new();
    depth.insert(from.to_string(), 0);
    q.push_back(from.to_string());
    while let Some(cur) = q.pop_front() {
        let d = depth[&cur];
        if d >= max_depth {
            continue;
        }
        if let Some(next) = callees.get(&cur) {
            for n in next {
                if depth.contains_key(n) {
                    continue;
                }
                depth.insert(n.clone(), d + 1);
                prev.insert(n.clone(), cur.clone());
                if n == to {
                    // Reconstruct from `to` back to `from` via the prev map.
                    let mut chain = vec![to.to_string()];
                    let mut c = to.to_string();
                    while let Some(p) = prev.get(&c) {
                        chain.push(p.clone());
                        c = p.clone();
                    }
                    chain.reverse();
                    return Some(chain);
                }
                q.push_back(n.clone());
            }
        }
    }
    None
}

fn render(
    from: &str,
    to: &str,
    paths: &[String],
    adj: &call_graph::Adjacency,
    path: &Option<Vec<String>>,
    max_depth: usize,
    budget: usize,
) -> String {
    let mut out = format!("call_path - \"{from}\" → \"{to}\"  (roots {paths:?})\n\n");
    match path {
        Some(chain) => {
            let hops = chain.len().saturating_sub(1);
            out.push_str(&format!("found a call chain ({hops} hop(s)):\n"));
            for (i, node) in chain.iter().enumerate() {
                let site = adj
                    .sites
                    .get(node)
                    .map(|(rel, line)| format!("{rel}:{line}"))
                    .unwrap_or_else(|| "(site unknown)".to_string());
                // Stair-step the chain so the direction reads top-down.
                let line = if i == 0 {
                    format!("  {node}   {site}\n")
                } else {
                    format!("{}→ {node}   {site}\n", "  ".repeat(i + 1))
                };
                if out.len() / 4 + line.len() / 4 > budget && i > 0 {
                    out.push_str("  … (truncated by token_budget)\n");
                    break;
                }
                out.push_str(&line);
            }
        }
        None => {
            out.push_str(&format!(
                "no call chain from \"{from}\" to \"{to}\" within depth {max_depth}.\n"
            ));
            let from_k = seg(from);
            let to_k = seg(to);
            let out_n = adj.callees.get(from_k).map(|s| s.len()).unwrap_or(0);
            let in_n = adj.callers.get(to_k).map(|s| s.len()).unwrap_or(0);
            out.push_str(&format!("  - \"{from}\": {out_n} direct callee(s) known\n"));
            match adj.sites.get(to_k) {
                Some((rel, line)) => out.push_str(&format!(
                    "  - \"{to}\": defined at {rel}:{line}, {in_n} caller(s)\n"
                )),
                None => out.push_str(&format!(
                    "  - \"{to}\": not seen as a defined function in scope\n"
                )),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(m: &mut BTreeMap<String, BTreeSet<String>>, a: &str, b: &str) {
        m.entry(a.to_string()).or_default().insert(b.to_string());
    }

    #[test]
    fn bfs_finds_shortest_chain() {
        let mut c = BTreeMap::new();
        edge(&mut c, "a", "b");
        edge(&mut c, "b", "c");
        edge(&mut c, "a", "c"); // a one-hop shortcut should win over a→b→c
        assert_eq!(
            bfs(&c, "a", "c", 8).unwrap(),
            vec!["a".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn bfs_respects_depth_and_cycles() {
        let mut c = BTreeMap::new();
        edge(&mut c, "a", "b");
        edge(&mut c, "b", "a"); // cycle must not loop forever
        edge(&mut c, "b", "c");
        assert!(bfs(&c, "a", "c", 1).is_none(), "c is two hops away");
        assert_eq!(bfs(&c, "a", "c", 2).unwrap().len(), 3);
    }

    #[test]
    fn bfs_same_node_is_trivial() {
        let c = BTreeMap::new();
        assert_eq!(bfs(&c, "x", "x", 8).unwrap(), vec!["x".to_string()]);
    }

    #[test]
    fn bfs_unreachable_is_none() {
        let mut c = BTreeMap::new();
        edge(&mut c, "a", "b");
        assert!(bfs(&c, "a", "z", 8).is_none());
    }
}
