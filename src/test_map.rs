//! test_map - which test files exercise a symbol.
//!
//! Reuses `symbol_refs`' tree-sitter occurrence classifier (`refs_by_file`),
//! scoped to the test tree, so an agent can run the *narrowest* relevant suite
//! instead of the whole thing after touching a function or type. Test files are
//! ranked by hit count (definitions and calls weigh the most).

use serde_json::Value;
use std::path::Path;

use crate::stats;
use crate::symbol_refs;

pub fn build(root: &Path, args: &Value) -> Result<String, String> {
    let symbol = args
        .get("symbol")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if symbol.is_empty() {
        return Err("symbol is required".into());
    }
    let paths = crate::config::paths_from_args_or(root, args, &["tests"])?;
    let max = args.get("max").and_then(Value::as_u64).unwrap_or(40).max(1) as usize;
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(1200) as usize;
    let tag_filter: Vec<String> = args
        .get("tags")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .map(|t| {
                    t.trim()
                        .trim_start_matches('[')
                        .trim_end_matches(']')
                        .to_string()
                })
                .filter(|t| !t.is_empty())
                .collect()
        })
        .unwrap_or_default();

    let (per, scanned_bytes) = symbol_refs::refs_by_file(root, symbol, &paths);
    let baseline = scanned_bytes / 4;

    let mut out = format!("test_map - \"{symbol}\"  (roots {paths:?})");
    if !tag_filter.is_empty() {
        out.push_str(&format!("  tags [{tag_filter:?}]"));
    }
    out.push('\n');

    let mut filtered: Vec<symbol_refs::FileRefs> = per;
    if !tag_filter.is_empty() {
        filtered.retain(|fr| {
            let full = root.join(&fr.rel);
            file_has_all_tags(&full, &tag_filter)
        });
        if filtered.is_empty() {
            out.push_str(&format!(
                "\n(no test files with tags {tag_filter:?} reference \"{symbol}\")\n"
            ));
            out.push_str("Run with fewer tags or omit \"tags\" to broaden.\n");
            stats::record("test_map", baseline, (out.len() / 4) as u64);
            return Ok(out);
        }
    }

    if filtered.is_empty() {
        out.push_str("\n(no test file references this symbol - it may be untested)\n");
        stats::record("test_map", baseline, (out.len() / 4) as u64);
        return Ok(out);
    }

    out.push_str(&format!(
        "\n{} test file(s) reference it (most relevant first):\n",
        filtered.len()
    ));
    let mut used = out.len() / 4;
    let mut shown = 0usize;
    for fr in &filtered {
        if shown >= max {
            out.push_str(&format!(
                "  … (+{} more; raise \"max\")\n",
                filtered.len() - shown
            ));
            break;
        }
        let total = fr.defs + fr.calls + fr.refs;
        let tags = catch2_tags_in_file(&root.join(&fr.rel));
        let tag_note = if tags.is_empty() {
            String::new()
        } else {
            format!("  [{}]", tags.join("]["))
        };
        let row = format!(
            "  {:<44} {total} hit(s)  ({} def, {} call, {} ref){tag_note}\n",
            fr.rel, fr.defs, fr.calls, fr.refs
        );
        let lt = row.len() / 4;
        if used + lt > budget && shown > 0 {
            out.push_str(&format!(
                "  … (+{} more; raise token_budget)\n",
                filtered.len() - shown
            ));
            break;
        }
        used += lt;
        shown += 1;
        out.push_str(&row);
    }

    out.push_str("\nRun the top file(s) first rather than the whole suite.\n");
    if !tag_filter.is_empty() {
        out.push_str(&format!(
            "Test filter example: {}\n",
            crate::config::test_filter_hint(root, &tag_filter.join("]["))
        ));
    }
    stats::record("test_map", baseline, (out.len() / 4) as u64);
    Ok(out)
}

/// Extract Catch2 tags from `TEST_CASE(..., "[Tag][Other]")` lines in a test file.
fn catch2_tags_in_file(path: &Path) -> Vec<String> {
    let Ok(src) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut tags = std::collections::BTreeSet::new();
    for line in src.lines() {
        if !line.contains("TEST_CASE") {
            continue;
        }
        if let Some(i) = line.rfind('"') {
            let rest = &line[..i];
            if let Some(j) = rest.rfind('"') {
                let tag_str = &line[j + 1..i];
                if tag_str.starts_with('[') {
                    for part in tag_str.split(']') {
                        let t = part.trim_start_matches('[').trim();
                        if !t.is_empty() {
                            tags.insert(t.to_string());
                        }
                    }
                }
            }
        }
    }
    tags.into_iter().collect()
}

fn file_has_all_tags(path: &Path, required: &[String]) -> bool {
    let have = catch2_tags_in_file(path);
    required
        .iter()
        .all(|t| have.iter().any(|h| h.eq_ignore_ascii_case(t)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn ranks_test_files_referencing_a_symbol() {
        let dir = std::env::temp_dir().join(format!("cbtest_testmap_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write(
            &dir,
            "tests/test_widget.cpp",
            "void t() { widget_area(); widget_area(); }\n",
        );
        write(&dir, "tests/test_other.cpp", "void u() { unrelated(); }\n");
        let out = build(&dir, &serde_json::json!({ "symbol": "widget_area" })).unwrap();
        assert!(out.contains("test_widget.cpp"), "{out}");
        assert!(!out.contains("test_other.cpp"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reports_when_untested() {
        let dir = std::env::temp_dir().join(format!("cbtest_testmap2_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write(&dir, "tests/test_a.cpp", "void t() { something_else(); }\n");
        let out = build(&dir, &serde_json::json!({ "symbol": "never_referenced" })).unwrap();
        assert!(
            out.contains("untested") || out.contains("no test file"),
            "{out}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn filters_by_catch2_tags() {
        let dir = std::env::temp_dir().join(format!("cbtest_testmap3_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write(
            &dir,
            "tests/test_unit.cpp",
            "TEST_CASE(\"x\", \"[unit]\") { void t() { helper_fn(); } }\n",
        );
        write(
            &dir,
            "tests/test_other.cpp",
            "TEST_CASE(\"y\", \"[Net]\") { void t() { helper_fn(); } }\n",
        );
        let out = build(
            &dir,
            &serde_json::json!({ "symbol": "helper_fn", "tags": ["unit"] }),
        )
        .unwrap();
        assert!(out.contains("test_unit.cpp"), "{out}");
        assert!(!out.contains("test_other.cpp"), "{out}");
        assert!(out.contains("[unit]"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
