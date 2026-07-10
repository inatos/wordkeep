//! index_stale - detect when wordkeep on-disk indexes may lag git changes.
//!
//! Compares recently changed source files (vs a git ref) against the mtime of
//! wordkeep's shared cache files. Recommends rebuilding the wordkeep binary and
//! reloading the MCP client when indexes look stale.

use serde_json::Value;
use std::path::Path;
use std::process::Command;

use crate::{cache, stats};

const SOURCE_EXT: [&str; 26] = [
    "cpp", "cc", "cxx", "h", "hpp", "hh", "c", "rs", "py", "pyi", "ts", "tsx", "mts", "cts", "cs",
    "glsl", "vert", "frag", "comp", "geom", "tesc", "tese", "vs", "fs", "svelte", "das",
];
const CACHE_FILES: [&str; 2] = ["call_graph.json", "knowledge-chunks.json"];

pub fn check(root: &Path, args: &Value) -> Result<String, String> {
    let gitref = args
        .get("ref")
        .and_then(Value::as_str)
        .unwrap_or("HEAD")
        .trim()
        .to_string();
    if gitref.is_empty() || gitref.starts_with('-') {
        return Err("ref must be a git revision".into());
    }

    let head_sha = git_output(root, &["rev-parse", &gitref]).unwrap_or_default();
    let mut changed = git_changed_files(root, &gitref);
    changed.retain(|p| looks_like_source(p));

    let cache_dir = cache::dir();
    let cache_mtime = newest_cache_mtime(&cache_dir);
    let newest_src = newest_file_mtime(root, &changed);

    let mut stale_reasons: Vec<String> = Vec::new();
    if !cache_dir.is_dir() {
        stale_reasons.push("wordkeep cache directory missing (cold index)".into());
    } else if cache_mtime.is_none() {
        stale_reasons.push("no wordkeep disk caches built yet".into());
    } else if let (Some(cm), Some(sm)) = (cache_mtime, newest_src) {
        if sm > cm {
            stale_reasons.push(format!(
                "source changed after cache (newest source {} ms after cache)",
                (sm - cm) / 1_000_000
            ));
        }
    }

    if changed.len() > 32 {
        stale_reasons.push(format!(
            "large diff ({} source files vs {gitref})",
            changed.len()
        ));
    }

    let mut out = format!("index_stale - git ref {gitref}");
    if !head_sha.is_empty() {
        out.push_str(&format!(" ({head_sha})"));
    }
    out.push('\n');

    if changed.is_empty() {
        out.push_str("\nNo source changes vs ref (working tree clean for indexed extensions).\n");
    } else {
        out.push_str(&format!(
            "\n{} changed source file(s) vs {gitref}:\n",
            changed.len()
        ));
        for p in changed.iter().take(12) {
            out.push_str(&format!("  {p}\n"));
        }
        if changed.len() > 12 {
            out.push_str(&format!("  … (+{} more)\n", changed.len() - 12));
        }
    }

    if stale_reasons.is_empty() {
        out.push_str("\nStatus: indexes likely fresh.\n");
        out.push_str("Tip: rebuild wordkeep after adding/removing files or renaming symbols.\n");
    } else {
        out.push_str("\nStatus: STALE - rebuild recommended\n");
        for r in &stale_reasons {
            out.push_str(&format!("  • {r}\n"));
        }
        out.push_str("\nRebuild: `cargo build --release` in the wordkeep repo, then reload the MCP client.\n");
        out.push_str(
            "If symbol_refs/call_graph show 0 callers after a large diff, rebuild first.\n",
        );
    }

    stats::record(
        "index_stale",
        changed.len() as u64 * 64,
        (out.len() / 4) as u64,
    );
    Ok(out)
}

fn git_output(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn git_changed_files(root: &Path, gitref: &str) -> Vec<String> {
    let mut files = Vec::new();
    for spec in [
        vec!["diff", "--name-only", gitref],
        vec!["diff", "--name-only", "--cached", gitref],
        vec!["ls-files", "--others", "--exclude-standard"],
    ] {
        if let Some(out) = git_output(root, &spec) {
            for line in out.lines() {
                let t = line.trim();
                if !t.is_empty() && !files.iter().any(|x| x == t) {
                    files.push(t.to_string());
                }
            }
        }
    }
    files
}

fn looks_like_source(rel: &str) -> bool {
    if rel.starts_with("src/") || rel.starts_with("tests/") || rel.starts_with("include/") {
        return true;
    }
    rel.rsplit('.')
        .next()
        .map(|ext| SOURCE_EXT.contains(&ext))
        .unwrap_or(false)
}

fn newest_cache_mtime(cache_dir: &Path) -> Option<u64> {
    let mut best = 0u64;
    let mut any = false;
    for name in CACHE_FILES {
        let p = cache_dir.join(name);
        if let Ok(m) = std::fs::metadata(&p) {
            any = true;
            best = best.max(cache::mtime_ns(&m));
        }
    }
    if any {
        Some(best)
    } else {
        None
    }
}

fn newest_file_mtime(root: &Path, rels: &[String]) -> Option<u64> {
    let mut best = 0u64;
    let mut any = false;
    for rel in rels {
        let ext = rel.rsplit('.').next().unwrap_or("");
        if !SOURCE_EXT.contains(&ext) {
            continue;
        }
        let p = root.join(rel);
        if let Ok(m) = std::fs::metadata(&p) {
            any = true;
            best = best.max(cache::mtime_ns(&m));
        }
    }
    if any {
        Some(best)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_source_filters() {
        assert!(looks_like_source("src/foo.cpp"));
        assert!(looks_like_source("tests/test_a.cpp"));
        assert!(!looks_like_source("docs/readme.md"));
    }
}
