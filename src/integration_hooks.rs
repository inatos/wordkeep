//! integration_hooks - curated cross-subsystem call sites + optional call_path.
//!
//! Reads `.wordkeep/integration-hooks.md` for documented hooks (faster than
//! discovering cross-file wiring by hand). Optional `from`/`to` runs `call_path`
//! to verify or explore chains not yet indexed.

use serde_json::Value;
use std::path::Path;

use crate::{call_path, stats};

const HOOKS_FILE: &str = ".wordkeep/integration-hooks.md";

pub fn find(root: &Path, args: &Value) -> Result<String, String> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let from = args
        .get("from")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let to = args
        .get("to")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(1200) as usize;

    let hooks_path = root.join(HOOKS_FILE);
    let src = if hooks_path.is_file() {
        std::fs::read_to_string(&hooks_path)
            .map_err(|e| format!("read {}: {e}", hooks_path.display()))?
    } else {
        String::new()
    };
    let baseline = src.len() as u64;

    let mut out = String::from("integration_hooks");
    if !query.is_empty() {
        out.push_str(&format!(" - query {query:?}"));
    }
    if let (Some(a), Some(b)) = (from, to) {
        out.push_str(&format!(" - path {a} → {b}"));
    }
    out.push('\n');

    if src.is_empty() {
        out.push_str(&format!(
            "\n(no hooks file at {HOOKS_FILE} - add curated entries for cross-subsystem wiring)\n"
        ));
    } else {
        let sections = split_sections(&src);
        let q_tokens: Vec<String> = query
            .split_whitespace()
            .map(|t| t.to_lowercase())
            .filter(|t| !t.is_empty())
            .collect();
        let mut matched = 0usize;
        for sec in &sections {
            if !q_tokens.is_empty() {
                let lower = sec.to_lowercase();
                if !q_tokens.iter().all(|t| lower.contains(t)) {
                    continue;
                }
            }
            if from.is_some() || to.is_some() {
                let lower = sec.to_lowercase();
                if let Some(f) = from {
                    if !lower.contains(&f.to_lowercase()) {
                        continue;
                    }
                }
                if let Some(t) = to {
                    if !lower.contains(&t.to_lowercase()) {
                        continue;
                    }
                }
            }
            if out.len() / 4 > budget && matched > 0 {
                out.push_str("\n… (truncated by token_budget)\n");
                break;
            }
            out.push('\n');
            out.push_str(sec.trim());
            out.push_str("\n---\n");
            matched += 1;
        }
        if matched == 0 {
            out.push_str("\n(no matching hooks - try broader query or add an entry to integration-hooks.md)\n");
        } else {
            out.push_str(&format!("\n{matched} hook(s) matched.\n"));
        }
    }

    if let (Some(a), Some(b)) = (from, to) {
        out.push_str("\n## call_path verification\n\n");
        let paths = crate::config::paths_from_args(root, args)?;
        let cp_args = serde_json::json!({
            "from": a,
            "to": b,
            "paths": paths,
            "max_depth": args.get("max_depth").unwrap_or(&Value::from(8)),
            "token_budget": args.get("token_budget").unwrap_or(&Value::from(600)),
        });
        match call_path::build(root, &cp_args) {
            Ok(cp) => out.push_str(&cp),
            Err(e) => out.push_str(&format!("call_path error: {e}\n")),
        }
    }

    stats::record("integration_hooks", baseline / 4, (out.len() / 4) as u64);
    Ok(out)
}

fn split_sections(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in src.lines() {
        if line.starts_with("## ") && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        if line.starts_with("## ") || !cur.is_empty() || !line.trim().is_empty() {
            cur.push_str(line);
            cur.push('\n');
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn finds_hook_by_query() {
        let dir = std::env::temp_dir().join(format!("cbtest_hooks_{}", std::process::id()));
        let hooks = dir.join(".wordkeep");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::write(
            hooks.join("integration-hooks.md"),
            "# Hooks\n\n## A → B\n- **from:** Foo\n- **tags:** render\n",
        )
        .unwrap();
        let out = find(&dir, &json!({ "query": "render" })).unwrap();
        assert!(out.contains("A → B"), "{out}");
        let multi = find(&dir, &json!({ "query": "a b tags" })).unwrap();
        assert!(multi.contains("A → B"), "token AND should match: {multi}");
        let miss = find(&dir, &json!({ "query": "render water" })).unwrap();
        assert!(miss.contains("no matching hooks"), "{miss}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
