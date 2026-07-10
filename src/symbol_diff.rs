//! symbol_diff - how one symbol's definition changed vs a git ref. Shows the
//! actual +/- hunks within the symbol's tree-sitter span - a focused complement
//! to `diff_map`'s tree-wide list. For body-only edits, renders stacked old/new
//! body excerpts; for signature changes, renders old/new signature plus hunks.
//! Reuses `symbol_def::locate` and git helpers from `diff_map`.

use serde_json::Value;
use std::path::Path;

use crate::call_graph::seg;
use crate::diff_map::{git_diff, git_show, repo_top};
use crate::lang::Lang;
use crate::{stats, symbol_def};

struct Hunk {
    new_start: usize,
    new_count: usize,
    lines: Vec<String>,
}

pub fn build(root: &Path, args: &Value) -> Result<String, String> {
    let symbol = args
        .get("symbol")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if symbol.is_empty() {
        return Err(crate::config::symbol_required_err());
    }
    let gitref = args
        .get("ref")
        .and_then(Value::as_str)
        .unwrap_or("HEAD")
        .trim()
        .to_string();
    if gitref.is_empty() || gitref.starts_with('-') {
        return Err("ref must be a git revision, not an option".into());
    }
    let paths = crate::config::paths_from_args(root, args)?;
    let context = args.get("context").and_then(Value::as_u64).unwrap_or(3) as u32;
    let show_body = args.get("body").and_then(Value::as_bool).unwrap_or(false);
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(1200) as usize;

    let (info, scanned) = symbol_def::locate(root, symbol, &paths, true);
    let Some(info) = info else {
        let out = format!("symbol_diff - no definition of \"{symbol}\" in scope {paths:?}\n");
        stats::record("symbol_diff", 0, (out.len() / 4) as u64);
        return Ok(out);
    };

    let top = repo_top(root).unwrap_or_else(|| root.to_path_buf());
    let abs = root.join(&info.rel);
    let rel_top = abs
        .strip_prefix(&top)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| info.rel.clone());

    let diff = match git_diff(root, &gitref, &[rel_top.clone()], context) {
        Ok(d) => d,
        Err(msg) => {
            let out = format!("symbol_diff - {msg}");
            stats::record("symbol_diff", scanned / 4, (out.len() / 4) as u64);
            return Ok(out);
        }
    };

    if diff.trim().is_empty() {
        let out = format!(
            "symbol_diff - \"{}\" at {}:{}  (no changes vs {gitref} in this file)\n",
            info.name, info.rel, info.line
        );
        stats::record("symbol_diff", scanned / 4, (out.len() / 4) as u64);
        return Ok(out);
    }

    let baseline = scanned + diff.len() as u64;
    let lang = Lang::from_path(abs.as_path());
    let hunks = parse_hunks(&diff);
    let span = info.line..=info.end_line;
    let matched: Vec<&Hunk> = hunks
        .iter()
        .filter(|h| hunk_overlaps_span(h, &span))
        .collect();

    if let (Some(lang), Some(old_src)) = (lang, git_show(root, &gitref, &rel_top)) {
        if let Some((old_sig, old_body)) = old_body_for_symbol(&old_src, lang, &info.name) {
            if body_only(&old_sig, &info.signature) {
                let out = render_body_excerpt(
                    &gitref,
                    &info.name,
                    &info.rel,
                    info.line,
                    info.end_line,
                    &old_body,
                    &info.body,
                    budget,
                );
                stats::record("symbol_diff", baseline / 4, (out.len() / 4) as u64);
                return Ok(out);
            }
            let out = render_signature_change(
                &gitref,
                &info.name,
                &info.rel,
                info.line,
                info.end_line,
                &old_sig,
                &info.signature,
                &matched,
                show_body.then_some((&old_body, info.body.as_str())),
                budget,
            );
            stats::record("symbol_diff", baseline / 4, (out.len() / 4) as u64);
            return Ok(out);
        }
    }

    let out = render_hunks(
        &gitref,
        &info.name,
        &info.rel,
        info.line,
        info.end_line,
        &matched,
        budget,
    );
    stats::record("symbol_diff", baseline / 4, (out.len() / 4) as u64);
    Ok(out)
}

fn body_only(old_sig: &str, new_sig: &str) -> bool {
    normalize_sig(old_sig) == normalize_sig(new_sig)
}

fn normalize_sig(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn old_body_for_symbol(old_src: &str, lang: Lang, symbol: &str) -> Option<(String, String)> {
    let spans = symbol_def::function_spans_from_str(lang, old_src);
    let want = seg(symbol);
    let sp = spans
        .iter()
        .find(|s| seg(&s.name) == want || s.name == symbol)?;
    let lines: Vec<&str> = old_src.lines().collect();
    let start = sp.start.saturating_sub(1);
    let end = sp.end.min(lines.len());
    if start >= end {
        return None;
    }
    Some((sp.signature.clone(), lines[start..end].join("\n")))
}

fn hunk_overlaps_span(h: &Hunk, span: &std::ops::RangeInclusive<usize>) -> bool {
    if h.new_count == 0 {
        return span.contains(&h.new_start.max(1));
    }
    let end = h.new_start.saturating_add(h.new_count.saturating_sub(1));
    (h.new_start..=end).any(|l| span.contains(&l))
}

fn parse_hunks(diff: &str) -> Vec<Hunk> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut cur: Option<Hunk> = None;
    for line in diff.lines() {
        if line.starts_with("@@") {
            if let Some(h) = cur.take() {
                hunks.push(h);
            }
            if let Some((start, count)) = hunk_new_range(line) {
                cur = Some(Hunk {
                    new_start: start,
                    new_count: count,
                    lines: Vec::new(),
                });
            }
        } else if let Some(ref mut h) = cur {
            if line.starts_with('+') || line.starts_with('-') || line.starts_with(' ') {
                h.lines.push(line.to_string());
            }
        }
    }
    if let Some(h) = cur {
        hunks.push(h);
    }
    hunks
}

fn hunk_new_range(line: &str) -> Option<(usize, usize)> {
    let plus = line.split('+').nth(1)?;
    let token = plus.split_whitespace().next()?;
    let mut it = token.split(',');
    let start = it.next()?.parse::<usize>().ok()?;
    let count = match it.next() {
        Some(s) => s.parse::<usize>().ok()?,
        None => 1,
    };
    Some((start, count))
}

fn render_body_excerpt(
    gitref: &str,
    name: &str,
    rel: &str,
    start: usize,
    end: usize,
    old_body: &str,
    new_body: &str,
    budget: usize,
) -> String {
    let mut out =
        format!("symbol_diff - \"{name}\" at {rel}:{start}-{end}  (vs {gitref}, body-only)\n\n");
    out.push_str(&format!("--- old ({gitref}) ---\n"));
    append_capped(&mut out, old_body, budget);
    out.push('\n');
    out.push_str("+++ new (working) ---\n");
    append_capped(&mut out, new_body, budget);
    out
}

fn render_signature_change(
    gitref: &str,
    name: &str,
    rel: &str,
    start: usize,
    end: usize,
    old_sig: &str,
    new_sig: &str,
    hunks: &[&Hunk],
    bodies: Option<(&str, &str)>,
    budget: usize,
) -> String {
    let mut out = format!(
        "symbol_diff - \"{name}\" at {rel}:{start}-{end}  (vs {gitref}, signature changed)\n\n"
    );
    out.push_str("signature:\n");
    out.push_str(&format!("- {old_sig}\n"));
    out.push_str(&format!("+ {new_sig}\n\n"));
    append_hunks(&mut out, hunks, budget);
    if let Some((old_body, new_body)) = bodies {
        out.push('\n');
        out.push_str(&format!("--- old body ({gitref}) ---\n"));
        append_capped(&mut out, old_body, budget);
        out.push('\n');
        out.push_str("+++ new body (working) ---\n");
        append_capped(&mut out, new_body, budget);
    }
    out
}

fn append_capped(out: &mut String, text: &str, budget: usize) {
    for line in text.lines() {
        if out.len() / 4 > budget {
            out.push_str("… (truncated by token_budget)\n");
            return;
        }
        out.push_str(line);
        out.push('\n');
    }
}

fn append_hunks(out: &mut String, hunks: &[&Hunk], budget: usize) {
    if hunks.is_empty() {
        out.push_str("(diff touches the file but no hunk overlaps this symbol's span)\n");
        return;
    }
    for h in hunks {
        out.push_str(&format!(
            "@@ lines {}-{} @@\n",
            h.new_start,
            h.new_start + h.new_count.saturating_sub(1)
        ));
        for ln in &h.lines {
            if out.len() / 4 > budget {
                out.push_str("… (truncated by token_budget)\n");
                return;
            }
            out.push_str(ln);
            out.push('\n');
        }
        out.push('\n');
    }
}

fn render_hunks(
    gitref: &str,
    name: &str,
    rel: &str,
    start: usize,
    end: usize,
    hunks: &[&Hunk],
    budget: usize,
) -> String {
    let mut out = format!("symbol_diff - \"{name}\" at {rel}:{start}-{end}  (vs {gitref})\n\n");
    append_hunks(&mut out, hunks, budget);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_only_compares_normalized_signatures() {
        assert!(body_only("int foo(int a)", "int  foo(int a)"));
        assert!(!body_only("int foo(int a)", "void foo(int a)"));
    }

    #[test]
    fn old_body_for_symbol_slices_span() {
        let src = "int a();\nint target() {\n  return 1;\n}\n";
        let (sig, body) = old_body_for_symbol(src, Lang::Cpp, "target").unwrap();
        assert!(sig.contains("target"));
        assert!(body.contains("return 1"));
    }

    #[test]
    fn render_signature_change_shows_sig_and_hunks() {
        let h = Hunk {
            new_start: 10,
            new_count: 2,
            lines: vec!["-old line".to_string(), "+new line".to_string()],
        };
        let out = render_signature_change(
            "HEAD",
            "foo",
            "src/a.cpp",
            10,
            15,
            "void foo(int x)",
            "void foo(int x, int y)",
            &[&h],
            Some(("old body", "new body")),
            1200,
        );
        assert!(out.contains("signature changed"));
        assert!(out.contains("- void foo(int x)"));
        assert!(out.contains("+ void foo(int x, int y)"));
        assert!(out.contains("-old line"));
        assert!(out.contains("old body"));
        assert!(out.contains("new body"));
    }

    #[test]
    fn hunk_new_range_parses() {
        assert_eq!(hunk_new_range("@@ -1,2 +3,4 @@"), Some((3, 4)));
    }

    #[test]
    fn overlap_detects_intersection() {
        let h = Hunk {
            new_start: 10,
            new_count: 5,
            lines: vec![],
        };
        assert!(hunk_overlaps_span(&h, &(10..=20)));
        assert!(!hunk_overlaps_span(&h, &(1..=5)));
    }

    #[test]
    fn parse_hunks_collects_lines() {
        let diff = "\
@@ -1,2 +10,3 @@
-old
+new
 context
";
        let hs = parse_hunks(diff);
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].lines.len(), 3);
    }
}
