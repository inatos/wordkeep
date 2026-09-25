//! integration_hooks - curated cross-subsystem call sites + optional call_path.
//!
//! Reads `.wordkeep/integration-hooks.md` for documented hooks (faster than
//! discovering cross-file wiring by hand). Optional `from`/`to` runs `call_path`
//! to verify or explore chains not yet indexed.
//!
//! Query matching: AND (all tokens) first; if empty, OR/scored fallback (rank by
//! how many query tokens hit). Empty results include a vocabulary hint of `## `
//! section headings.

use serde_json::Value;
use std::path::Path;

use crate::{call_path, stats};

const HOOKS_FILE: &str = ".wordkeep/integration-hooks.md";
const VOCAB_CAP: usize = 24;
const OR_TOP: usize = 12;

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

        // Soften empty query terms: Ok with vocabulary hint (do not Err).
        if query.is_empty() && from.is_none() && to.is_none() {
            out.push_str("\n(no query terms - listing vocabulary; pass query / from / to to filter)\n");
            append_vocab_hint(&mut out, &sections);
        } else {
            let (matched_secs, mode) = select_sections(&sections, &q_tokens, from, to);
            let mut matched = 0usize;
            for sec in &matched_secs {
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
                out.push_str(
                    "\n(no matching hooks - try broader query or add an entry to integration-hooks.md)\n",
                );
                append_vocab_hint(&mut out, &sections);
            } else {
                let mode_note = match mode {
                    MatchMode::And => "",
                    MatchMode::Or => " (OR fallback)",
                    MatchMode::Unfiltered => "",
                };
                out.push_str(&format!("\n{matched} hook(s) matched{mode_note}.\n"));
            }
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum MatchMode {
    And,
    Or,
    Unfiltered,
}

fn passes_from_to(sec: &str, from: Option<&str>, to: Option<&str>) -> bool {
    if from.is_none() && to.is_none() {
        return true;
    }
    let lower = sec.to_lowercase();
    if let Some(f) = from {
        if !lower.contains(&f.to_lowercase()) {
            return false;
        }
    }
    if let Some(t) = to {
        if !lower.contains(&t.to_lowercase()) {
            return false;
        }
    }
    true
}

fn select_sections<'a>(
    sections: &'a [String],
    q_tokens: &[String],
    from: Option<&str>,
    to: Option<&str>,
) -> (Vec<&'a String>, MatchMode) {
    if q_tokens.is_empty() {
        let all: Vec<&String> = sections
            .iter()
            .filter(|s| passes_from_to(s, from, to))
            .collect();
        return (all, MatchMode::Unfiltered);
    }

    // 1) AND — every token must appear.
    let and_hits: Vec<&String> = sections
        .iter()
        .filter(|sec| {
            if !passes_from_to(sec, from, to) {
                return false;
            }
            let lower = sec.to_lowercase();
            q_tokens.iter().all(|t| lower.contains(t))
        })
        .collect();
    if !and_hits.is_empty() {
        return (and_hits, MatchMode::And);
    }

    // 2) OR / scored — rank by how many query tokens hit (threshold ≥ 1).
    let mut scored: Vec<(usize, &String)> = Vec::new();
    for sec in sections {
        if !passes_from_to(sec, from, to) {
            continue;
        }
        let lower = sec.to_lowercase();
        let hits = q_tokens.iter().filter(|t| lower.contains(t.as_str())).count();
        if hits >= 1 {
            scored.push((hits, sec));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    let or_hits: Vec<&String> = scored.into_iter().take(OR_TOP).map(|(_, s)| s).collect();
    (or_hits, MatchMode::Or)
}

fn section_headings(sections: &[String]) -> Vec<String> {
    sections
        .iter()
        .filter_map(|sec| {
            sec.lines()
                .find(|l| l.starts_with("## "))
                .map(|l| l[3..].trim().to_string())
                .filter(|h| !h.is_empty())
        })
        .collect()
}

fn append_vocab_hint(out: &mut String, sections: &[String]) {
    let headings = section_headings(sections);
    if headings.is_empty() {
        return;
    }
    out.push_str("\nhint: valid ## headings (query vocabulary):\n");
    for h in headings.iter().take(VOCAB_CAP) {
        out.push_str(&format!("  - {h}\n"));
    }
    if headings.len() > VOCAB_CAP {
        out.push_str(&format!("  … ({} more)\n", headings.len() - VOCAB_CAP));
    }
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
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn or_fallback_when_and_misses() {
        let dir = std::env::temp_dir().join(format!("cbtest_hooks_or_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let hooks = dir.join(".wordkeep");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::write(
            hooks.join("integration-hooks.md"),
            "# Hooks\n\n## render -> physics\n- **tags:** render\n\n## audio mix\n- **tags:** audio\n",
        )
        .unwrap();
        // AND would miss (no section has both "render" and "water"); OR hits render.
        let out = find(&dir, &json!({ "query": "render water" })).unwrap();
        assert!(out.contains("render -> physics"), "{out}");
        assert!(out.contains("OR fallback"), "{out}");
        assert!(!out.contains("audio mix"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_match_includes_vocab_hint() {
        let dir = std::env::temp_dir().join(format!("cbtest_hooks_vocab_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let hooks = dir.join(".wordkeep");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::write(
            hooks.join("integration-hooks.md"),
            "# Hooks\n\n## render -> physics\n- **tags:** render\n\n## audio mix\n- **tags:** audio\n",
        )
        .unwrap();
        let out = find(&dir, &json!({ "query": "zzzz-no-such-token" })).unwrap();
        assert!(out.contains("no matching hooks"), "{out}");
        assert!(out.contains("valid ## headings"), "{out}");
        assert!(out.contains("render -> physics"), "{out}");
        assert!(out.contains("audio mix"), "{out}");
        // Empty query terms → Ok with vocabulary (no Err).
        let empty = find(&dir, &json!({})).unwrap();
        assert!(empty.contains("no query terms"), "{empty}");
        assert!(empty.contains("valid ## headings"), "{empty}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
