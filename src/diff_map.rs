//! diff_map - the blast radius of a change.
//!
//! Given a git ref (or the working tree), list the functions whose definitions
//! the diff touched and, via one hop of `call_graph`, their immediate callers -
//! so an agent reviewing a change knows what it might break without reading
//! every hunk. The diff is parsed for changed new-side line ranges; each changed
//! file is parsed once (through the incremental cache) to map those lines back
//! to the enclosing function spans. Each changed function is then compared
//! against its blob at the ref, flagging the ABI-breaking case where the
//! *signature* (params/return) moved rather than just the body.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use tree_sitter::Parser;

use crate::lang::Lang;
use crate::{cache, call_graph, stats, symbol_def};

/// How a changed function differs at the diffed revision.
#[derive(Clone, Copy, PartialEq)]
enum Change {
    /// Only the body changed - the signature is identical at the ref.
    Body,
    /// The signature (params/return) moved - the ABI-breaking case.
    Signature,
    /// The function did not exist at the ref (newly added).
    New,
    /// The ref's blob could not be read, so signature status is unknown.
    Unknown,
}

impl Change {
    fn tag(self) -> &'static str {
        match self {
            Change::Signature => "  [signature changed]",
            Change::New => "  [new]",
            Change::Body | Change::Unknown => "",
        }
    }
}

/// A function definition the diff touched.
struct ChangedSym {
    name: String,
    line: usize,
    change: Change,
}

pub fn build(root: &Path, args: &Value) -> Result<String, String> {
    let gitref = args
        .get("ref")
        .and_then(Value::as_str)
        .unwrap_or("HEAD")
        .trim()
        .to_string();
    // Reject anything that could be read as a git option (e.g. `--output=…`):
    // the ref is passed as a bare argument before `--`.
    if gitref.is_empty() || gitref.starts_with('-') {
        return Err("ref must be a git revision, not an option".into());
    }
    let paths = crate::config::paths_from_args(root, args)?;
    let max = args.get("max").and_then(Value::as_u64).unwrap_or(40).max(1) as usize;
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(1200) as usize;

    let diff = match git_diff(root, &gitref, &paths, 0) {
        Ok(d) => d,
        // Git missing / not a repo / bad ref: a graceful message, not an error,
        // so the tool never surfaces as a protocol failure.
        Err(msg) => {
            let out = format!("diff_map - {msg}");
            stats::record("diff_map", 0, (out.len() / 4) as u64);
            return Ok(out);
        }
    };

    // Diff paths are repo-top-relative; resolve files against the repo top, but
    // display them relative to our root when possible.
    let top = repo_top(root).unwrap_or_else(|| root.to_path_buf());
    let changed = parse_diff(&diff);

    let mut parser = Parser::new();
    let mut per_file: Vec<(String, Vec<ChangedSym>)> = Vec::new();
    let mut baseline_bytes = 0u64;
    for (rel_top, lines) in &changed {
        let abs = top.join(rel_top);
        let Some(lang) = Lang::from_path(&abs) else {
            continue;
        };
        let Ok(meta) = std::fs::metadata(&abs) else {
            continue;
        };
        baseline_bytes += meta.len();
        let mt = cache::mtime_ns(&meta);
        let spans = symbol_def::function_spans(&mut parser, lang, &abs, mt);
        // Old-side signatures (keyed by trailing segment) so we can tell an
        // ABI-breaking signature change from a body-only edit. If the ref's blob
        // can't be read (e.g. the file is new), signature status stays Unknown.
        let old_src = git_show(root, &gitref, rel_top);
        if let Some(s) = &old_src {
            baseline_bytes += s.len() as u64;
        }
        let old_sigs: Option<BTreeMap<String, BTreeSet<String>>> = old_src.map(|s| {
            let mut m: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
            for sp in symbol_def::function_spans_from_str(lang, &s) {
                m.entry(seg(&sp.name).to_string())
                    .or_default()
                    .insert(sp.signature);
            }
            m
        });
        let rel_disp = abs
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| rel_top.clone());
        let mut hits: Vec<ChangedSym> = Vec::new();
        for s in &spans {
            if lines.range(s.start..=s.end).next().is_some() {
                let change = match &old_sigs {
                    None => Change::Unknown,
                    Some(map) => match map.get(seg(&s.name)) {
                        None => Change::New,
                        Some(set) if set.contains(&s.signature) => Change::Body,
                        Some(_) => Change::Signature,
                    },
                };
                hits.push(ChangedSym {
                    name: s.name.clone(),
                    line: s.start,
                    change,
                });
            }
        }
        if !hits.is_empty() {
            per_file.push((rel_disp, hits));
        }
    }

    let out = render(
        &gitref,
        &paths,
        root,
        &per_file,
        max,
        budget,
        baseline_bytes,
    );
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn render(
    gitref: &str,
    paths: &[String],
    root: &Path,
    per_file: &[(String, Vec<ChangedSym>)],
    max: usize,
    budget: usize,
    baseline_bytes: u64,
) -> String {
    let total: usize = per_file.iter().map(|(_, v)| v.len()).sum();
    let mut out = format!("diff_map - ref {gitref}  (roots {paths:?})\n\n");

    if total == 0 {
        out.push_str(&format!(
            "(no changed functions vs {gitref} under {paths:?})\n"
        ));
        stats::record("diff_map", baseline_bytes / 4, (out.len() / 4) as u64);
        return out;
    }

    // Resolve callers once per distinct symbol name, capped at `max`, so a wide
    // diff doesn't walk the tree hundreds of times.
    let mut analyzed = 0usize;
    let mut caller_scan = 0u64;
    let mut caller_cache: std::collections::HashMap<String, Vec<(String, String, usize)>> =
        std::collections::HashMap::new();

    for (rel, syms) in per_file {
        out.push_str(&format!("{rel}:\n"));
        for s in syms {
            out.push_str(&format!(
                "  {}  (line {}){}\n",
                s.name,
                s.line,
                s.change.tag()
            ));
            let callers = if let Some(c) = caller_cache.get(&s.name) {
                Some(c.clone())
            } else if analyzed < max {
                let hop = call_graph::one_hop(root, &s.name, paths);
                if caller_scan == 0 {
                    caller_scan = hop.scanned_bytes;
                }
                analyzed += 1;
                caller_cache.insert(s.name.clone(), hop.callers.clone());
                Some(hop.callers)
            } else {
                None
            };
            match callers {
                None => out.push_str("    called by: (not analyzed - over max)\n"),
                Some(c) if c.is_empty() => out.push_str("    called by: (none in scope)\n"),
                Some(c) => {
                    let shown: Vec<String> = c
                        .iter()
                        .take(8)
                        .map(|(name, crel, cline)| format!("{name} ({crel}:{cline})"))
                        .collect();
                    let extra = if c.len() > 8 {
                        format!(" … (+{} more)", c.len() - 8)
                    } else {
                        String::new()
                    };
                    out.push_str(&format!("    called by: {}{}\n", shown.join(", "), extra));
                }
            }
            if out.len() / 4 > budget {
                out.push_str("  … (truncated by token_budget)\n");
                let footer = format!(
                    "\n{total} changed function(s) across {} file(s).\n",
                    per_file.len()
                );
                out.push_str(&footer);
                stats::record(
                    "diff_map",
                    (baseline_bytes + caller_scan) / 4,
                    (out.len() / 4) as u64,
                );
                return out;
            }
        }
    }

    out.push_str(&format!(
        "\n{total} changed function(s) across {} file(s).\n",
        per_file.len()
    ));
    stats::record(
        "diff_map",
        (baseline_bytes + caller_scan) / 4,
        (out.len() / 4) as u64,
    );
    out
}

/// `git -C <root> diff --unified=N <ref> -- <paths…>`, captured as text. The ref
/// is a bare argument (validated non-option) and paths follow `--`, so neither
/// can inject git options.
pub(crate) fn git_diff(
    root: &Path,
    gitref: &str,
    paths: &[String],
    unified: u32,
) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .arg("--no-pager")
        .arg("diff")
        .arg(format!("--unified={unified}"))
        .arg("--no-color")
        .arg(gitref);
    if !paths.is_empty() {
        cmd.arg("--");
        for p in paths {
            cmd.arg(p);
        }
    }
    let output = cmd
        .output()
        .map_err(|e| format!("git not available: {e}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let first = err.lines().next().unwrap_or("git diff failed");
        return Err(format!(
            "{first} (is {} a git repo and {gitref} a valid ref?)",
            root.display()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `git -C <root> show <ref>:<repo_rel_path>` - the file's content at the
/// revision, or None when the path didn't exist there / git is unavailable. The
/// spec is a single bare argument, so it can't inject git options.
pub(crate) fn git_show(root: &Path, gitref: &str, rel_top: &str) -> Option<String> {
    let spec = format!("{gitref}:{rel_top}");
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("--no-pager")
        .arg("show")
        .arg(&spec)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Last `::`-separated segment of a (possibly qualified) name, matching how
/// `call_graph`/`symbol_def` key names so old and new signatures line up.
fn seg(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

/// Repo top via `git rev-parse --show-toplevel`, so diff paths resolve to files.
pub(crate) fn repo_top(root: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let t = s.trim();
    (!t.is_empty()).then(|| PathBuf::from(t))
}

/// Unix seconds of the last commit touching `rel_top`, or None when git/file unavailable.
pub(crate) fn git_last_commit_secs(root: &Path, rel_top: &str) -> Option<u64> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("--no-pager")
        .arg("log")
        .arg("-1")
        .arg("--format=%ct")
        .arg("--")
        .arg(rel_top)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    s.trim().parse::<u64>().ok()
}

/// Parse a unified diff into `(repo_relative_path, changed_new_side_lines)`,
/// keeping only files that have at least one changed line.
fn parse_diff(diff: &str) -> Vec<(String, BTreeSet<usize>)> {
    let mut files: Vec<(String, BTreeSet<usize>)> = Vec::new();
    let mut cur: Option<(String, BTreeSet<usize>)> = None;
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            if let Some((name, set)) = cur.take() {
                if !set.is_empty() {
                    files.push((name, set));
                }
            }
            let path = rest.strip_prefix("b/").unwrap_or(rest);
            cur = (path != "/dev/null").then(|| (path.to_string(), BTreeSet::new()));
        } else if line.starts_with("@@") {
            if let Some((_, set)) = cur.as_mut() {
                if let Some((start, count)) = hunk_new_range(line) {
                    if count == 0 {
                        set.insert(start.max(1));
                    } else {
                        for l in start..start + count {
                            set.insert(l);
                        }
                    }
                }
            }
        }
    }
    if let Some((name, set)) = cur.take() {
        if !set.is_empty() {
            files.push((name, set));
        }
    }
    files
}

/// The new-side `(start, count)` from a hunk header `@@ -a,b +c,d @@`.
fn hunk_new_range(line: &str) -> Option<(usize, usize)> {
    let plus = line.split('+').nth(1)?; // "c,d @@ …"
    let token = plus.split_whitespace().next()?; // "c,d" or "c"
    let mut it = token.split(',');
    let start = it.next()?.parse::<usize>().ok()?;
    let count = match it.next() {
        Some(s) => s.parse::<usize>().ok()?,
        None => 1,
    };
    Some((start, count))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunk_new_range_parses_both_forms() {
        assert_eq!(hunk_new_range("@@ -1,2 +3,4 @@"), Some((3, 4)));
        assert_eq!(hunk_new_range("@@ -1 +5 @@"), Some((5, 1)));
        assert_eq!(hunk_new_range("@@ -1,0 +2,0 @@ int foo(a+b)"), Some((2, 0)));
    }

    #[test]
    fn parse_diff_extracts_changed_lines() {
        let diff = "\
diff --git a/src/foo.cpp b/src/foo.cpp
index 111..222 100644
--- a/src/foo.cpp
+++ b/src/foo.cpp
@@ -10,0 +11,2 @@ void foo()
+    int x = 1;
+    int y = 2;
@@ -20,1 +22,1 @@ void bar()
-    old();
+    fresh();
";
        let files = parse_diff(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, "src/foo.cpp");
        assert!(files[0].1.contains(&11));
        assert!(files[0].1.contains(&12));
        assert!(files[0].1.contains(&22));
    }

    #[test]
    fn parse_diff_skips_deleted_file_target() {
        let diff = "\
--- a/src/gone.cpp
+++ /dev/null
@@ -1,3 +0,0 @@
-a
-b
-c
";
        assert!(parse_diff(diff).is_empty());
    }
}
