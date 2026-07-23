//! index_stale - detect when wordkeep on-disk indexes may lag git changes.
//!
//! Compares recently changed source files (vs a git ref) against the mtime of
//! wordkeep's shared + workspace caches. Distinguishes self-refreshable mtime
//! misses from parser-binary rebuilds, and reports sources outside active
//! search coverage.

use serde_json::Value;
use std::path::Path;
use std::process::Command;

use crate::{cache, config, stats, workspace};

const SOURCE_EXT: [&str; 26] = [
    "cpp", "cc", "cxx", "h", "hpp", "hh", "c", "rs", "py", "pyi", "ts", "tsx", "mts", "cts", "cs",
    "glsl", "vert", "frag", "comp", "geom", "tesc", "tese", "vs", "fs", "svelte", "das",
];
const GLOBAL_CACHE_FILES: [&str; 2] = ["call_graph.json", "knowledge-chunks.json"];

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

    let search_paths =
        config::paths_from_args(root, args).unwrap_or_else(|_| config::default_paths(root));

    let head_sha = git_output(root, &["rev-parse", &gitref]).unwrap_or_default();
    let mut changed = git_changed_files(root, &gitref);
    changed.retain(|p| looks_like_source(p));

    let mut outside_coverage = Vec::new();
    for p in &changed {
        if !path_in_coverage(p, &search_paths) {
            outside_coverage.push(p.clone());
        }
    }

    let cache_dir = cache::dir();
    let ws_dir = workspace::workspace_dir(root);
    let cache_mtime = newest_cache_mtime(&cache_dir).max(newest_cache_mtime(&ws_dir));
    let newest_src = newest_file_mtime(root, &changed);

    let mut stale_reasons: Vec<String> = Vec::new();
    let mut self_refreshable = false;
    let mut needs_rebuild = false;

    if !cache_dir.is_dir() {
        stale_reasons.push("wordkeep cache directory missing (cold index)".into());
        needs_rebuild = true;
    } else if cache_mtime.is_none() {
        stale_reasons.push("no wordkeep disk caches built yet".into());
        self_refreshable = true;
    } else if let (Some(cm), Some(sm)) = (cache_mtime, newest_src) {
        if sm > cm {
            stale_reasons.push(format!(
                "source changed after cache (newest source {} ms after cache) — self-refreshable on next tool call",
                (sm - cm) / 1_000_000
            ));
            self_refreshable = true;
        }
    }

    if changed.len() > 32 {
        stale_reasons.push(format!(
            "large diff ({} source files vs {gitref}) — parser/binary rebuild recommended if callers look wrong",
            changed.len()
        ));
        needs_rebuild = true;
    }

    if !outside_coverage.is_empty() {
        stale_reasons.push(format!(
            "{} changed source file(s) outside active search coverage {:?}",
            outside_coverage.len(),
            search_paths
        ));
    }

    let mut out = format!("index_stale - git ref {gitref}");
    if !head_sha.is_empty() {
        out.push_str(&format!(" ({head_sha})"));
    }
    out.push_str(&format!("\nactive coverage: {search_paths:?}\n"));

    if changed.is_empty() {
        out.push_str("\nNo source changes vs ref (working tree clean for indexed extensions).\n");
    } else {
        out.push_str(&format!(
            "\n{} changed source file(s) vs {gitref}:\n",
            changed.len()
        ));
        for p in changed.iter().take(12) {
            let mark = if outside_coverage.iter().any(|o| o == p) {
                " [outside coverage]"
            } else {
                ""
            };
            out.push_str(&format!("  {p}{mark}\n"));
        }
        if changed.len() > 12 {
            out.push_str(&format!("  … (+{} more)\n", changed.len() - 12));
        }
    }

    if stale_reasons.is_empty() {
        out.push_str("\nStatus: indexes likely fresh.\n");
        out.push_str("Tip: rebuild wordkeep after adding/removing files or renaming symbols.\n");
    } else {
        let status = if needs_rebuild && !self_refreshable {
            "STALE - rebuild recommended"
        } else if needs_rebuild {
            "STALE - self-refreshable mtime miss; rebuild if symbol callers look wrong"
        } else if self_refreshable {
            "STALE - self-refreshable (next symbol/knowledge call will rebuild caches)"
        } else {
            "STALE - coverage / guidance"
        };
        out.push_str(&format!("\nStatus: {status}\n"));
        for r in &stale_reasons {
            out.push_str(&format!("  • {r}\n"));
        }
        if needs_rebuild {
            out.push_str(
                "\nRebuild: `cargo build --release` in the wordkeep repo, then reload the MCP client.\n",
            );
            out.push_str(
                "If symbol_refs/call_graph show 0 callers after a large diff, rebuild first.\n",
            );
        }
        if !outside_coverage.is_empty() {
            let profiles = config::list_profile_names(root);
            out.push_str(&format!(
                "Coverage tip: pass paths:[...] or profile one of {:?} to include those files.\n",
                profiles
            ));
        }
    }

    let _ = GLOBAL_CACHE_FILES;

    stats::record(
        "index_stale",
        changed.len() as u64 * 64,
        (out.len() / 4) as u64,
    );
    Ok(out)
}

fn path_in_coverage(path: &str, coverage: &[String]) -> bool {
    let p = path.replace('\\', "/");
    coverage.iter().any(|c| {
        let c = c.trim_matches('/').replace('\\', "/");
        if c.is_empty() {
            return true;
        }
        p == c || p.starts_with(&format!("{c}/"))
    })
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
    if let Some(diff) = git_output(root, &["diff", "--name-only", gitref]) {
        for line in diff.lines() {
            let p = line.trim();
            if !p.is_empty() {
                files.push(p.to_string());
            }
        }
    }
    if let Some(untracked) = git_output(root, &["ls-files", "--others", "--exclude-standard"]) {
        for line in untracked.lines() {
            let p = line.trim();
            if !p.is_empty() && !files.iter().any(|f| f == p) {
                files.push(p.to_string());
            }
        }
    }
    files
}

fn looks_like_source(path: &str) -> bool {
    path.rsplit('.')
        .next()
        .map(|e| SOURCE_EXT.iter().any(|x| x.eq_ignore_ascii_case(e)))
        .unwrap_or(false)
}

fn newest_cache_mtime(dir: &Path) -> Option<u64> {
    if !dir.is_dir() {
        return None;
    }
    let mut best = None;
    let Ok(rd) = std::fs::read_dir(dir) else {
        return None;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            let mt = cache::mtime_ns(&meta);
            best = Some(best.map_or(mt, |b: u64| b.max(mt)));
        }
    }
    // Also scan one level of workspaces/
    let ws = dir.join("workspaces");
    if ws.is_dir() {
        if let Ok(rd) = std::fs::read_dir(&ws) {
            for entry in rd.flatten() {
                if let Ok(rd2) = std::fs::read_dir(entry.path()) {
                    for f in rd2.flatten() {
                        if f.path().extension().and_then(|e| e.to_str()) == Some("json") {
                            if let Ok(meta) = f.metadata() {
                                let mt = cache::mtime_ns(&meta);
                                best = Some(best.map_or(mt, |b: u64| b.max(mt)));
                            }
                        }
                    }
                }
            }
        }
    }
    best
}

fn newest_file_mtime(root: &Path, files: &[String]) -> Option<u64> {
    let mut best = None;
    for f in files {
        let p = root.join(f);
        if let Ok(meta) = std::fs::metadata(&p) {
            let mt = cache::mtime_ns(&meta);
            best = Some(best.map_or(mt, |b: u64| b.max(mt)));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_detects_outside() {
        assert!(path_in_coverage("src/a.rs", &["src".into()]));
        assert!(!path_in_coverage("tools/kkbp/a.rs", &["src".into()]));
        assert!(path_in_coverage("tools/kkbp/a.rs", &["tools/kkbp".into()]));
    }
}
