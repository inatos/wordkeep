//! Read-only commit scope proposals from `git status --porcelain`.
//!
//! Groups dirty paths by configured `commit_scopes` prefixes and warns about
//! dirty submodules, secrets, generated dirs, binaries, and large files.
//! Never stages or commits.
//!
//! Skips configured `commit_ignore` path prefixes (default `.cache/`, `target/`,
//! `node_modules/`) and caches porcelain results for 2s so bursty agents do not
//! re-run `git status` on huge dirty trees.

use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::continuation::{self, KIND_SCOPE_SKIP};
use crate::{config, progress, stats};

const PORCELAIN_TTL: Duration = Duration::from_secs(2);
const DEFAULT_TOKEN_BUDGET: usize = 1600;
const DEFAULT_PER_SCOPE_CAP: usize = 20;
const UNTRACKED_BUCKET_CAP: usize = 12;

#[derive(Clone, Debug)]
struct Dirty {
    status: String,
    path: String,
    size: Option<u64>,
}

struct PorcelainCacheEntry {
    at: Instant,
    dirty: Vec<Dirty>,
}

/// Short-TTL porcelain cache keyed by absolute root path.
static PORCELAIN_CACHE: OnceLock<Mutex<HashMap<PathBuf, PorcelainCacheEntry>>> = OnceLock::new();

/// True when `path` matches any configured ignore prefix (normalized `/`).
pub(crate) fn path_matches_ignore(path: &str, prefixes: &[String]) -> bool {
    let p = path.replace('\\', "/");
    for pref in prefixes {
        let pref = pref.replace('\\', "/");
        let trimmed = pref.trim_end_matches('/');
        if trimmed.is_empty() {
            continue;
        }
        if p == trimmed || p.starts_with(&format!("{trimmed}/")) {
            return true;
        }
    }
    false
}

fn filter_ignored(dirty: Vec<Dirty>, ignore: &[String]) -> Vec<Dirty> {
    dirty
        .into_iter()
        .filter(|d| !path_matches_ignore(&d.path, ignore))
        .collect()
}

fn git_porcelain_uncached(root: &Path) -> Result<Vec<Dirty>, String> {
    let out = Command::new("git")
        .args(["status", "--porcelain", "-unormal"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("git status failed: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "git status exited {}; is this a git repo? {err}",
            out.status
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut dirty = Vec::new();
    for line in text.lines() {
        if line.len() < 3 {
            continue;
        }
        let status = line[..2].to_string();
        let mut path = line[3..].trim().to_string();
        // Handle renames: "R  old -> new"
        if let Some((_, neu)) = path.split_once(" -> ") {
            path = neu.trim().to_string();
        }
        // Unquoted paths only (porcelain quotes when needed).
        if path.starts_with('"') {
            path = path.trim_matches('"').replace("\\\"", "\"");
        }
        let size = std::fs::metadata(root.join(&path)).ok().map(|m| m.len());
        dirty.push(Dirty { status, path, size });
    }
    Ok(dirty)
}

fn git_porcelain(root: &Path, ignore: &[String]) -> Result<Vec<Dirty>, String> {
    let key = root.to_path_buf();
    let cell = PORCELAIN_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(store) = cell.lock() {
        if let Some(entry) = store.get(&key) {
            if entry.at.elapsed() < PORCELAIN_TTL {
                return Ok(filter_ignored(entry.dirty.clone(), ignore));
            }
        }
    }

    let raw = git_porcelain_uncached(root)?;

    if let Ok(mut store) = cell.lock() {
        store.insert(
            key,
            PorcelainCacheEntry {
                at: Instant::now(),
                dirty: raw.clone(),
            },
        );
    }

    Ok(filter_ignored(raw, ignore))
}

fn looks_secret(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.contains(".env")
        || p.ends_with(".pem")
        || p.ends_with(".key")
        || p.contains("id_rsa")
        || p.contains("credentials")
        || p.contains("secret")
}

fn looks_generated(path: &str) -> bool {
    let p = path.replace('\\', "/");
    p.contains("/target/")
        || p.starts_with("target/")
        || p.contains("/build/")
        || p.starts_with("build/")
        || p.contains("/node_modules/")
        || p.contains("/.cache/")
        || p.contains("/__pycache__/")
        || p.ends_with(".blend1")
        || p.contains("/obj/")
}

fn looks_binary(path: &str) -> bool {
    matches!(
        path.rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "webp"
            | "gif"
            | "blend"
            | "blend1"
            | "glb"
            | "gltf"
            | "bkkbp"
            | "dll"
            | "so"
            | "dylib"
            | "exe"
            | "wasm"
            | "bin"
            | "pdb"
            | "a"
            | "o"
            | "obj"
    )
}

fn is_submodule_dirty(status: &str) -> bool {
    // Porcelain submodule noise often shows mixed M/? markers.
    status.contains('M') && status.contains('?')
}

/// Propose reviewable path groups for the next commit(s).
pub fn propose(root: &Path, args: &Value) -> Result<String, String> {
    const FP_KEYS: &[&str] = &["large_file_bytes", "max_per_scope"];
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_TOKEN_BUDGET as u64)
        .max(1) as usize;
    let per_scope_cap = args
        .get("max_per_scope")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_PER_SCOPE_CAP as u64)
        .max(1) as usize;
    let large_threshold = args
        .get("large_file_bytes")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| config::large_file_bytes(root));
    let args_fp = continuation::args_fingerprint(args, FP_KEYS);
    let resume_at =
        continuation::resume_offset(args, "commit_scope", FP_KEYS, KIND_SCOPE_SKIP)? as usize;

    progress::tick(0, None, "commit_scope: porcelain");
    let ignore = config::commit_ignore(root);
    let dirty = match git_porcelain(root, &ignore) {
        Ok(d) => d,
        Err(e) => {
            return Ok(format!(
                "commit_scope - unavailable ({e})\n\
                 This tool is read-only and requires a git working tree."
            ));
        }
    };

    let scopes = config::commit_scopes(root);
    let mut grouped: Vec<(String, Vec<&Dirty>)> = Vec::new();
    let mut unscoped: Vec<&Dirty> = Vec::new();

    if scopes.is_empty() {
        unscoped = dirty.iter().collect();
    } else {
        for (name, prefixes) in &scopes {
            let mut members = Vec::new();
            for d in &dirty {
                if prefixes.iter().any(|p| {
                    d.path == *p || d.path.starts_with(&format!("{p}/")) || d.path.starts_with(p)
                }) {
                    members.push(d);
                }
            }
            if !members.is_empty() {
                grouped.push((name.clone(), members));
            }
        }
        for d in &dirty {
            let matched = scopes.iter().any(|(_, prefixes)| {
                prefixes.iter().any(|p| {
                    d.path == *p || d.path.starts_with(&format!("{p}/")) || d.path.starts_with(p)
                })
            });
            if !matched {
                unscoped.push(d);
            }
        }
    }

    let mut warnings = Vec::new();
    for d in &dirty {
        if d.status.contains('S') || d.path.contains("/.git") {
            warnings.push(format!("possible submodule noise: {} {}", d.status, d.path));
        }
        if looks_secret(&d.path) {
            warnings.push(format!("possible secret: {}", d.path));
        }
        if looks_generated(&d.path) {
            warnings.push(format!("generated/cache path: {}", d.path));
        }
        if looks_binary(&d.path) {
            warnings.push(format!("binary asset: {}", d.path));
        }
        if is_submodule_dirty(&d.status) {
            warnings.push(format!("possible submodule noise: {} {}", d.status, d.path));
        }
        if let Some(sz) = d.size {
            if sz >= large_threshold {
                warnings.push(format!(
                    "large file ({} bytes >= {large_threshold}): {}",
                    sz, d.path
                ));
            }
        }
    }
    warnings.sort();
    warnings.dedup();

    let mut header = format!(
        "commit_scope - {} dirty path(s) (read-only; never stages/commits)",
        dirty.len()
    );
    if resume_at > 0 {
        header.push_str(&format!(" [continuation from scope #{resume_at}]"));
    }
    if dirty.is_empty() {
        let mut out = format!("{header}\n");
        if !ignore.is_empty() && resume_at == 0 {
            out.push_str(&format!("ignored prefixes: {ignore:?}\n"));
        }
        out.push_str("\nWorking tree clean.\n");
        stats::record("commit_scope", 16, (out.len() / 4) as u64);
        return Ok(out);
    }

    let mut sections: Vec<String> = Vec::new();
    for (name, members) in &grouped {
        let mut sec = format!("\n## scope `{name}` ({} files)\n", members.len());
        append_paths_capped(&mut sec, members, per_scope_cap);
        sec.push_str("  suggested: review then `git add` these paths as one commit\n");
        sections.push(sec);
    }
    if !unscoped.is_empty() {
        let mut sec = format!("\n## unscoped ({} files)\n", unscoped.len());
        append_paths_capped(&mut sec, &unscoped, per_scope_cap);
        sections.push(sec);
    }
    if !warnings.is_empty() {
        let mut sec = String::from("\n## warnings\n");
        for w in warnings.iter().take(20) {
            sec.push_str(&format!("  • {w}\n"));
        }
        if warnings.len() > 20 {
            sec.push_str(&format!("  … (+{} more)\n", warnings.len() - 20));
        }
        sections.push(sec);
    }

    let mut meta_lines = String::new();
    if !ignore.is_empty() && resume_at == 0 {
        meta_lines.push_str(&format!("ignored prefixes: {ignore:?}\n"));
    }

    let mut body = String::new();
    let mut used = (header.len() + 1 + meta_lines.len()) / 4;
    let mut truncated_at: Option<usize> = None;

    for (idx, sec) in sections.iter().enumerate() {
        if idx < resume_at {
            continue;
        }
        let sec_tokens = sec.len() / 4;
        if idx > resume_at && used + sec_tokens > budget {
            truncated_at = Some(idx);
            break;
        }
        used += sec_tokens;
        body.push_str(sec);
    }

    if let Some(idx) = truncated_at {
        let omitted = sections.len().saturating_sub(idx);
        header.push_str(&format!(
            " (truncated by token_budget; +{omitted} section(s) omitted)"
        ));
    }
    let mut out = format!("{header}\n{meta_lines}{body}");
    if let Some(idx) = truncated_at {
        let omitted = sections.len().saturating_sub(idx);
        continuation::append_footer(
            &mut out,
            "commit_scope",
            args_fp,
            idx as u64,
            KIND_SCOPE_SKIP,
            &format!("+{omitted} section(s) omitted"),
        );
    }
    stats::record(
        "commit_scope",
        dirty.len() as u64 * 16,
        (out.len() / 4) as u64,
    );
    Ok(out)
}

fn is_untracked(status: &str) -> bool {
    status.contains('?')
}

fn untracked_bucket(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if path.ends_with('/') {
        return trimmed.to_string();
    }
    match trimmed.rfind('/') {
        Some(i) if i > 0 => trimmed[..i].to_string(),
        _ => ".".to_string(),
    }
}

/// List modified and staged paths up to `DEFAULT_PER_SCOPE_CAP`, collapsing
/// overflow and untracked files to directory counts so huge dirty trees stay bounded.
#[cfg(test)]
fn append_paths(out: &mut String, members: &[&Dirty]) {
    append_paths_capped(out, members, DEFAULT_PER_SCOPE_CAP);
}

fn append_paths_capped(out: &mut String, members: &[&Dirty], max_tracked: usize) {
    let mut tracked_shown = 0usize;
    let mut tracked_overflow: BTreeMap<String, usize> = BTreeMap::new();
    let mut tracked_extra = 0usize;
    let mut untracked: BTreeMap<String, usize> = BTreeMap::new();

    for d in members {
        if is_untracked(&d.status) {
            *untracked.entry(untracked_bucket(&d.path)).or_default() += 1;
        } else if tracked_shown < max_tracked {
            out.push_str(&format!("  {} {}\n", d.status, d.path));
            tracked_shown += 1;
        } else {
            tracked_extra += 1;
            *tracked_overflow
                .entry(untracked_bucket(&d.path))
                .or_default() += 1;
        }
    }

    if tracked_extra > 0 {
        let dirs: Vec<String> = tracked_overflow
            .iter()
            .take(6)
            .map(|(dir, n)| {
                let label = if dir == "." { "repo root" } else { dir.as_str() };
                format!("{label}/ ({n})")
            })
            .collect();
        let more_dirs = if tracked_overflow.len() > 6 {
            format!(", +{} more dir(s)", tracked_overflow.len() - 6)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "  … +{tracked_extra} more tracked across {} dir(s): {}{more_dirs}\n",
            tracked_overflow.len(),
            dirs.join(", ")
        ));
    }

    let total_untracked_dirs = untracked.len();
    let mut extra_untracked_files = 0usize;
    for (idx, (dir, n)) in untracked.into_iter().enumerate() {
        if idx < UNTRACKED_BUCKET_CAP {
            let label = if dir == "." { "repo root" } else { dir.as_str() };
            out.push_str(&format!("  ?? {label}/ ({n} untracked)\n"));
        } else {
            extra_untracked_files += n;
        }
    }
    if total_untracked_dirs > UNTRACKED_BUCKET_CAP {
        out.push_str(&format!(
            "  … (+{} more untracked dir(s), {extra_untracked_files} file(s))\n",
            total_untracked_dirs - UNTRACKED_BUCKET_CAP
        ));
    }
}

/// Parse porcelain lines for unit tests.
#[cfg(test)]
pub fn parse_porcelain_line(line: &str) -> Option<(String, String)> {
    if line.len() < 3 {
        return None;
    }
    let status = line[..2].to_string();
    let mut path = line[3..].trim().to_string();
    if let Some((_, neu)) = path.split_once(" -> ") {
        path = neu.trim().to_string();
    }
    Some((status, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untracked_files_collapse_to_directory_counts() {
        let members = vec![
            Dirty {
                status: " M".into(),
                path: "src/a.rs".into(),
                size: None,
            },
            Dirty {
                status: "??".into(),
                path: "screenshots/a.png".into(),
                size: None,
            },
            Dirty {
                status: "??".into(),
                path: "screenshots/b.png".into(),
                size: None,
            },
        ];
        let refs: Vec<&Dirty> = members.iter().collect();
        let mut out = String::new();
        append_paths(&mut out, &refs);
        assert!(out.contains(" M src/a.rs"), "{out}");
        assert!(out.contains("screenshots/ (2 untracked)"), "{out}");
        assert!(!out.contains("a.png"), "{out}");
    }

    #[test]
    fn parses_porcelain() {
        let (s, p) = parse_porcelain_line(" M src/foo.rs").unwrap();
        assert_eq!(s, " M");
        assert_eq!(p, "src/foo.rs");
        let (_, p) = parse_porcelain_line("R  old.rs -> new.rs").unwrap();
        assert_eq!(p, "new.rs");
    }

    #[test]
    fn secret_and_binary_heuristics() {
        assert!(looks_secret(".env.local"));
        assert!(looks_binary("shot.png"));
        assert!(looks_generated("target/release/foo"));
    }

    #[test]
    fn ignore_filters_cache_and_target_prefixes() {
        let ignore = vec![".cache/".into(), "target/".into(), "node_modules/".into()];
        assert!(path_matches_ignore(".cache/foo.bin", &ignore));
        assert!(path_matches_ignore("target/debug/bar", &ignore));
        assert!(path_matches_ignore("node_modules/pkg/index.js", &ignore));
        assert!(path_matches_ignore(".cache", &ignore));
        assert!(!path_matches_ignore("src/main.rs", &ignore));
        assert!(!path_matches_ignore("docs/.cache_notes.md", &ignore));

        let dirty = vec![
            Dirty {
                status: "??".into(),
                path: ".cache/x".into(),
                size: None,
            },
            Dirty {
                status: " M".into(),
                path: "src/a.rs".into(),
                size: Some(10),
            },
            Dirty {
                status: "??".into(),
                path: "target/release/wordkeep".into(),
                size: None,
            },
        ];
        let kept = filter_ignored(dirty, &ignore);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].path, "src/a.rs");
    }

    #[test]
    fn ignore_respects_trailing_slash_variants() {
        let ignore = vec!["pacman-overlay".into()];
        assert!(path_matches_ignore("pacman-overlay/pkg", &ignore));
        assert!(!path_matches_ignore("src/pacman-overlay.rs", &ignore));
    }

    #[test]
    fn per_scope_tracked_collapse_and_continuation_paging() {
        let members = vec![
            Dirty { status: " M".into(), path: "src/a.rs".into(), size: None },
            Dirty { status: " M".into(), path: "src/b.rs".into(), size: None },
            Dirty { status: " M".into(), path: "src/sub/c.rs".into(), size: None },
            Dirty { status: " M".into(), path: "src/sub/d.rs".into(), size: None },
        ];
        let refs: Vec<&Dirty> = members.iter().collect();
        let mut collapsed = String::new();
        append_paths_capped(&mut collapsed, &refs, 2);
        assert!(collapsed.contains(" M src/a.rs"), "{collapsed}");
        assert!(collapsed.contains(" M src/b.rs"), "{collapsed}");
        assert!(collapsed.contains("+2 more tracked"), "{collapsed}");
        assert!(collapsed.contains("src/sub/ (2)"), "{collapsed}");

        // End-to-end continuation paging over a real git working tree with multiple scopes.
        let dir = std::env::temp_dir().join(format!("wk_cs_page_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("alpha")).unwrap();
        std::fs::create_dir_all(dir.join("beta")).unwrap();
        std::fs::create_dir_all(dir.join(".wordkeep")).unwrap();
        std::fs::write(
            dir.join(".wordkeep/config.json"),
            r#"{"commit_scopes":{"alpha":["alpha"],"beta":["beta"]}}"#,
        )
        .unwrap();
        assert!(Command::new("git").arg("init").current_dir(&dir).status().unwrap().success());
        assert!(Command::new("git").args(["config", "user.email", "t@t"]).current_dir(&dir).status().unwrap().success());
        assert!(Command::new("git").args(["config", "user.name", "t"]).current_dir(&dir).status().unwrap().success());
        std::fs::write(dir.join("alpha/one.rs"), "fn a() {}\n").unwrap();
        std::fs::write(dir.join("beta/two.rs"), "fn b() {}\n").unwrap();
        assert!(Command::new("git").args(["add", "."]).current_dir(&dir).status().unwrap().success());
        assert!(Command::new("git").args(["commit", "-m", "init"]).current_dir(&dir).status().unwrap().success());
        std::fs::write(dir.join("alpha/one.rs"), "fn a() { 1; }\n").unwrap();
        std::fs::write(dir.join("beta/two.rs"), "fn b() { 2; }\n").unwrap();

        let p1 = propose(&dir, &serde_json::json!({ "token_budget": 20 })).unwrap();
        assert!(p1.contains("scope `alpha`"), "{p1}");
        assert!(!p1.contains("scope `beta`"), "{p1}");
        assert!(p1.contains("truncated by token_budget"), "{p1}");
        let tok = p1
            .lines()
            .find_map(|l| l.strip_prefix("continuation: ").map(str::trim))
            .expect("continuation token");

        let p2 = propose(
            &dir,
            &serde_json::json!({ "token_budget": 20, "continuation": tok }),
        )
        .unwrap();
        assert!(p2.contains("[continuation from scope #1]"), "{p2}");
        assert!(p2.contains("scope `beta`"), "{p2}");
        assert!(!p2.contains("scope `alpha`"), "{p2}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
