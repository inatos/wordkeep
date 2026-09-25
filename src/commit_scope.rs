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
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::{config, progress, stats};

const PORCELAIN_TTL: Duration = Duration::from_secs(2);

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
        .args(["status", "--porcelain", "-uall"])
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
    let large_threshold = args
        .get("large_file_bytes")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| config::large_file_bytes(root));

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

    let mut out = format!(
        "commit_scope - {} dirty path(s) (read-only; never stages/commits)\n",
        dirty.len()
    );
    if !ignore.is_empty() {
        out.push_str(&format!("ignored prefixes: {ignore:?}\n"));
    }
    if dirty.is_empty() {
        out.push_str("\nWorking tree clean.\n");
        stats::record("commit_scope", 16, (out.len() / 4) as u64);
        return Ok(out);
    }

    for (name, members) in &grouped {
        out.push_str(&format!("\n## scope `{name}` ({} files)\n", members.len()));
        for d in members.iter().take(40) {
            out.push_str(&format!("  {} {}\n", d.status, d.path));
        }
        if members.len() > 40 {
            out.push_str(&format!("  … (+{} more)\n", members.len() - 40));
        }
        out.push_str("  suggested: review then `git add` these paths as one commit\n");
    }
    if !unscoped.is_empty() {
        out.push_str(&format!("\n## unscoped ({} files)\n", unscoped.len()));
        for d in unscoped.iter().take(40) {
            out.push_str(&format!("  {} {}\n", d.status, d.path));
        }
        if unscoped.len() > 40 {
            out.push_str(&format!("  … (+{} more)\n", unscoped.len() - 40));
        }
    }
    if !warnings.is_empty() {
        out.push_str("\n## warnings\n");
        for w in warnings.iter().take(30) {
            out.push_str(&format!("  • {w}\n"));
        }
        if warnings.len() > 30 {
            out.push_str(&format!("  … (+{} more)\n", warnings.len() - 30));
        }
    }
    stats::record(
        "commit_scope",
        dirty.len() as u64 * 16,
        (out.len() / 4) as u64,
    );
    Ok(out)
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
        let ignore = vec![
            ".cache/".into(),
            "target/".into(),
            "node_modules/".into(),
        ];
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
}
