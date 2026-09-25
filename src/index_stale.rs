//! index_stale - detect when wordkeep on-disk indexes may lag git changes.
//!
//! Compares recently changed source files (vs a git ref) against the mtime of
//! wordkeep's shared + workspace caches. Distinguishes self-refreshable mtime
//! misses from parser-binary rebuilds, and reports sources outside active
//! search coverage.

use serde_json::Value;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::{cache, config, progress, stats, workspace};

const SOURCE_EXT: [&str; 26] = [
    "cpp", "cc", "cxx", "h", "hpp", "hh", "c", "rs", "py", "pyi", "ts", "tsx", "mts", "cts", "cs",
    "glsl", "vert", "frag", "comp", "geom", "tesc", "tese", "vs", "fs", "svelte", "das",
];
const GLOBAL_CACHE_FILES: [&str; 2] = ["call_graph.json", "knowledge-chunks.json"];
const HEALTH_TTL: Duration = Duration::from_secs(5);
const HEALTH_SAMPLE_CAP: usize = 80;

struct HealthCache {
    key: String,
    at: Instant,
    body: String,
    baseline: u64,
}

static HEALTH_CACHE: Mutex<Option<HealthCache>> = Mutex::new(None);

pub fn check(root: &Path, args: &Value) -> Result<String, String> {
    if args
        .get("mode")
        .and_then(Value::as_str)
        .is_some_and(|m| m.eq_ignore_ascii_case("health"))
    {
        return health(root, args);
    }
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

/// Profile coverage + proactive stale report (`index_health` tool).
pub fn health(root: &Path, args: &Value) -> Result<String, String> {
    let gitref = args
        .get("ref")
        .and_then(Value::as_str)
        .unwrap_or("HEAD")
        .trim()
        .to_string();
    if gitref.is_empty() || gitref.starts_with('-') {
        return Err("ref must be a git revision".into());
    }

    progress::tick(0, None, "index_health: profiles");
    let cache_key = format!("{}|{gitref}", root.display());
    if let Ok(guard) = HEALTH_CACHE.lock() {
        if let Some(hit) = guard.as_ref() {
            if hit.key == cache_key && hit.at.elapsed() < HEALTH_TTL {
                let out = hit.body.clone();
                stats::record(
                    "index_health",
                    hit.baseline.max(64),
                    (out.len() / 4) as u64,
                );
                return Ok(out);
            }
        }
    }

    let (out, baseline) = health_build(root, &gitref)?;
    if let Ok(mut guard) = HEALTH_CACHE.lock() {
        *guard = Some(HealthCache {
            key: cache_key,
            at: Instant::now(),
            body: out.clone(),
            baseline,
        });
    }
    stats::record("index_health", baseline.max(64), (out.len() / 4) as u64);
    Ok(out)
}

fn health_build(root: &Path, gitref: &str) -> Result<(String, u64), String> {
    let profiles = config::list_profile_names(root);
    let mut all_profile_paths: Vec<String> = Vec::new();
    let mut out = String::from("index_health - path profile coverage + stale check\n\n");
    out.push_str("## Path profiles\n");
    if profiles.is_empty() {
        out.push_str("(none configured — using default_paths / src)\n");
    } else {
        for name in &profiles {
            let paths = config::profile_paths(root, name).unwrap_or_default();
            let mut status = Vec::new();
            for p in &paths {
                let abs = root.join(p);
                let mark = if abs.exists() { "ok" } else { "MISSING" };
                status.push(format!("{p} [{mark}]"));
                if !all_profile_paths.iter().any(|x| x == p) {
                    all_profile_paths.push(p.clone());
                }
            }
            out.push_str(&format!("  {name}: {}\n", status.join(", ")));
        }
    }

    let default = config::default_paths(root);
    for p in &default {
        if !all_profile_paths.iter().any(|x| x == p) {
            all_profile_paths.push(p.clone());
        }
    }
    out.push_str(&format!("\nunion coverage roots: {all_profile_paths:?}\n"));

    out.push_str("\n## Coverage gaps (sample under web/, tools/)\n");
    let (gaps, sample_bytes) = sample_coverage_gaps(root, &all_profile_paths);
    if gaps.is_empty() {
        out.push_str("(no sampled source files outside profile union)\n");
    } else {
        for g in gaps.iter().take(16) {
            let suggested = config::infer_profile(root, g)
                .unwrap_or_else(|| suggest_profile_name(g));
            out.push_str(&format!("  {g}  → suggest profile \"{suggested}\"\n"));
        }
        if gaps.len() > 16 {
            out.push_str(&format!("  … (+{} more sampled)\n", gaps.len() - 16));
        }
        out.push_str(
            "Tip: `profile_upsert` with paths:[...] to persist a missing tree.\n",
        );
    }

    // Tracked changes only — `ls-files --others` is too slow on huge dirty trees.
    let mut changed = Vec::new();
    if let Some(diff) = git_output(root, &["diff", "--name-only", gitref]) {
        for line in diff.lines() {
            let p = line.trim();
            if !p.is_empty() {
                changed.push(p.to_string());
            }
        }
    }
    changed.retain(|p| looks_like_source(p));
    let outside: Vec<&String> = changed
        .iter()
        .filter(|p| !path_in_coverage(p, &all_profile_paths))
        .collect();
    out.push_str(&format!(
        "\n## Git changes outside coverage (vs {gitref})\n"
    ));
    if outside.is_empty() {
        out.push_str("(none — tracked only; untracked omitted for speed)\n");
    } else {
        for p in outside.iter().take(12) {
            let suggested = config::infer_profile(root, p)
                .unwrap_or_else(|| suggest_profile_name(p));
            out.push_str(&format!("  {p}  → try profile \"{suggested}\"\n"));
        }
        if outside.len() > 12 {
            out.push_str(&format!("  … (+{} more)\n", outside.len() - 12));
        }
    }

    out.push_str("\n## Index freshness\n");
    let stale_args = serde_json::json!({ "ref": gitref });
    match check_freshness_only(root, &stale_args) {
        Ok(s) => out.push_str(&s),
        Err(e) => out.push_str(&format!("stale check error: {e}\n")),
    }

    let cfg = std::fs::metadata(root.join(".wordkeep/config.json"))
        .map(|m| m.len())
        .unwrap_or(0);
    let changed_bytes: u64 = changed.iter().map(|p| p.len() as u64).sum();
    let baseline = ((cfg + changed_bytes + sample_bytes) / 4)
        .max((out.len() / 4) as u64)
        .max(64);
    Ok((out, baseline))
}

/// Prefer `git ls-files` (fast) under web/ + tools/; fall back to a capped walk.
fn sample_coverage_gaps(root: &Path, coverage: &[String]) -> (Vec<String>, u64) {
    let mut gaps: Vec<String> = Vec::new();
    let mut bytes = 0u64;
    if let Some(listed) = git_ls_files(root, &["web", "tools"]) {
        for rel in listed {
            if !looks_like_source(&rel) {
                continue;
            }
            bytes += rel.len() as u64;
            if path_in_coverage(&rel, coverage) {
                continue;
            }
            gaps.push(rel);
            if gaps.len() >= 24 {
                break;
            }
        }
        return (gaps, bytes);
    }

    let sample_roots = ["web", "tools"];
    let prune = crate::walk::prune_set(root);
    for sample in &sample_roots {
        let base = root.join(sample);
        if !base.is_dir() {
            continue;
        }
        for entry in crate::walk::files(&base, &prune).take(HEALTH_SAMPLE_CAP) {
            let path = entry.path();
            let Some(rel) = path.strip_prefix(root).ok() else {
                continue;
            };
            let rel_s = rel.to_string_lossy().replace('\\', "/");
            if !looks_like_source(&rel_s) {
                continue;
            }
            bytes += rel_s.len() as u64;
            if !path_in_coverage(&rel_s, coverage) {
                gaps.push(rel_s);
                if gaps.len() >= 24 {
                    break;
                }
            }
        }
        if gaps.len() >= 24 {
            break;
        }
    }
    (gaps, bytes)
}

fn git_ls_files(root: &Path, paths: &[&str]) -> Option<Vec<String>> {
    let mut cmd = Command::new("git");
    cmd.args(["-C"])
        .arg(root)
        .args(["ls-files", "--"])
        .args(paths);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Some(
        text.lines()
            .map(|l| l.trim().replace('\\', "/"))
            .filter(|l| !l.is_empty())
            .collect(),
    )
}

fn suggest_profile_name(path: &str) -> String {
    let p = path.trim_matches('/').replace('\\', "/");
    let parts: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
    if parts.first().copied() == Some("tests") {
        return "tests".into();
    }
    if parts.len() >= 2 {
        let leaf = parts[1];
        // Avoid using a source basename (e.g. test_foo.cpp) as a profile name.
        if leaf.contains('.') {
            return parts[0].to_string();
        }
        return leaf.to_string();
    }
    if let Some(first) = parts.first() {
        (*first).to_string()
    } else {
        "custom".into()
    }
}

/// Condensed stale status for embedding in `health` (avoids recursive mode=health).
fn check_freshness_only(root: &Path, args: &Value) -> Result<String, String> {
    let mut args = args.clone();
    if let Some(obj) = args.as_object_mut() {
        obj.remove("mode");
    }
    // Call the original check path without health redirect by inlining key bits.
    let gitref = args
        .get("ref")
        .and_then(Value::as_str)
        .unwrap_or("HEAD")
        .trim()
        .to_string();
    let search_paths =
        config::paths_from_args(root, &args).unwrap_or_else(|_| config::default_paths(root));
    let mut changed = git_changed_files_ex(root, &gitref, false);
    changed.retain(|p| looks_like_source(p));
    let cache_dir = cache::dir();
    let ws_dir = workspace::workspace_dir(root);
    let cache_mtime = newest_cache_mtime(&cache_dir).max(newest_cache_mtime(&ws_dir));
    let newest_src = newest_file_mtime(root, &changed);
    if !cache_dir.is_dir() {
        return Ok("Status: STALE - cache directory missing (cold index)\n".into());
    }
    if let (Some(cm), Some(sm)) = (cache_mtime, newest_src) {
        if sm > cm {
            return Ok(format!(
                "Status: STALE - self-refreshable (source newer than cache by {} ms); \
                 next symbol/knowledge call rebuilds caches under {search_paths:?}\n",
                (sm - cm) / 1_000_000
            ));
        }
    }
    if changed.is_empty() {
        Ok("Status: indexes likely fresh (no source changes vs ref).\n".into())
    } else {
        Ok(format!(
            "Status: indexes likely fresh ({} changed source file(s); cache mtime ok).\n",
            changed.len()
        ))
    }
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
    git_changed_files_ex(root, gitref, true)
}

/// When `include_untracked` is false, skip `ls-files --others` (slow on huge dirty trees).
fn git_changed_files_ex(root: &Path, gitref: &str, include_untracked: bool) -> Vec<String> {
    let mut files = Vec::new();
    if let Some(diff) = git_output(root, &["diff", "--name-only", gitref]) {
        for line in diff.lines() {
            let p = line.trim();
            if !p.is_empty() {
                files.push(p.to_string());
            }
        }
    }
    if include_untracked {
        if let Some(untracked) = git_output(root, &["ls-files", "--others", "--exclude-standard"]) {
            for line in untracked.lines() {
                let p = line.trim();
                if !p.is_empty() && !files.iter().any(|f| f == p) {
                    files.push(p.to_string());
                }
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

    #[test]
    fn suggest_profile_from_path() {
        assert_eq!(suggest_profile_name("web/bifrost/foo.ts"), "bifrost");
        assert_eq!(suggest_profile_name("tools/wordkeep/src/a.rs"), "wordkeep");
        assert_eq!(suggest_profile_name("tests/test_asset_factory.cpp"), "tests");
        assert_eq!(suggest_profile_name("assets/scripts/foo.das"), "scripts");
    }

    #[test]
    fn health_lists_profiles_or_defaults() {
        let dir = std::env::temp_dir().join(format!("wk_index_health_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("tools/sample")).unwrap();
        std::fs::write(dir.join("tools/sample/a.rs"), "fn x() {}\n").unwrap();
        std::fs::create_dir_all(dir.join(".wordkeep")).unwrap();
        std::fs::write(
            dir.join(".wordkeep/config.json"),
            r#"{"path_profiles":{"engine":["src"]},"default_paths":["src"]}"#,
        )
        .unwrap();
        let out = health(&dir, &serde_json::json!({"ref": "HEAD"})).unwrap();
        assert!(out.contains("index_health"), "{out}");
        assert!(out.contains("engine"), "{out}");
        assert!(out.contains("Coverage gaps") || out.contains("tools/"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
