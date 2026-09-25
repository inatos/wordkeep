//! Workspace defaults from `.wordkeep/config.json` at the repo root.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::walk;

pub const CONFIG_REL: &str = ".wordkeep/config.json";

/// Reject absolute paths and `..` traversal. Returns a normalized forward-slash path.
pub fn validate_rel_path(rel: &str) -> Result<String, String> {
    let norm = rel.trim().replace('\\', "/");
    if norm.is_empty() {
        return Err("path must not be empty".into());
    }
    if Path::new(&norm).is_absolute() || norm.starts_with('/') {
        return Err(format!("path must be relative to --root: {norm}"));
    }
    // Reject drive-letter prefixes (e.g. C: or C:/) on all platforms.
    if norm.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && norm.as_bytes().get(1) == Some(&b':')
    {
        return Err(format!("path must be relative to --root: {norm}"));
    }
    if norm.split('/').any(|c| c == "..") {
        return Err("path must not contain ..".into());
    }
    Ok(norm)
}

/// Validate every entry in a tool `paths` array.
pub fn normalize_search_paths(paths: &[String]) -> Result<Vec<String>, String> {
    paths.iter().map(|p| validate_rel_path(p)).collect()
}

/// Load `.wordkeep/config.json` as a JSON value (or `None` if missing/invalid).
pub fn load_config(root: &Path) -> Option<Value> {
    let path = root.join(CONFIG_REL);
    let raw = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn string_vec(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Named path profile: `path_profiles.<name>` → list of relative roots.
pub fn profile_paths(root: &Path, name: &str) -> Option<Vec<String>> {
    let cfg = load_config(root)?;
    let profiles = cfg.get("path_profiles")?.as_object()?;
    let arr = profiles.get(name)?.as_array()?;
    let paths: Vec<String> = arr
        .iter()
        .filter_map(|x| x.as_str().map(String::from))
        .collect();
    if paths.is_empty() {
        None
    } else {
        Some(paths)
    }
}

/// `default_profile` from config, if set.
pub fn default_profile(root: &Path) -> Option<String> {
    load_config(root).and_then(|v| {
        v.get("default_profile")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
    })
}

/// Infer a profile from `profile_hints` keywords against a query string.
/// First matching hint wins (deterministic order of object keys as stored).
pub fn infer_profile(root: &Path, query: &str) -> Option<String> {
    let cfg = load_config(root)?;
    let hints = cfg.get("profile_hints")?.as_object()?;
    let q = query.to_ascii_lowercase();
    // Collect and sort keys for determinism (serde_json Map is ordered insertion).
    let mut keys: Vec<&String> = hints.keys().collect();
    keys.sort();
    for key in keys {
        let Some(arr) = hints.get(key).and_then(Value::as_array) else {
            continue;
        };
        for kw in arr.iter().filter_map(Value::as_str) {
            if !kw.is_empty() && q.contains(&kw.to_ascii_lowercase()) {
                return Some(key.clone());
            }
        }
    }
    None
}

/// Resolve search paths:
/// explicit `paths` → explicit `profile` → keyword-inferred profile (via `hint`)
/// → `default_profile` paths → `default_paths` → `["src"]`.
pub fn paths_from_args(root: &Path, args: &Value) -> Result<Vec<String>, String> {
    paths_from_args_with_hint(root, args, None)
}

/// Like [`paths_from_args`], but optionally uses `hint` for keyword profile inference.
pub fn paths_from_args_with_hint(
    root: &Path,
    args: &Value,
    hint: Option<&str>,
) -> Result<Vec<String>, String> {
    if let Some(arr) = args.get("paths").and_then(Value::as_array) {
        let raw: Vec<String> = arr
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect();
        if !raw.is_empty() {
            return normalize_search_paths(&raw);
        }
    }
    if let Some(name) = args
        .get("profile")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let Some(p) = profile_paths(root, name) else {
            return Err(format!("unknown path profile {name:?}"));
        };
        if p.is_empty() {
            return Err(format!("path profile {name:?} is empty"));
        }
        return normalize_search_paths(&p);
    }
    // Keyword inference when a hint is provided (e.g. symbol name / query).
    if let Some(h) = hint.filter(|s| !s.is_empty()) {
        if let Some(name) = infer_profile(root, h) {
            if let Some(p) = profile_paths(root, &name) {
                return normalize_search_paths(&p);
            }
        }
    }
    if let Some(name) = default_profile(root) {
        if let Some(p) = profile_paths(root, &name) {
            return normalize_search_paths(&p);
        }
    }
    Ok(default_paths(root))
}

/// Resolve `roots` for knowledge_search / upsert indexing.
pub fn doc_roots_from_args(root: &Path, args: &Value) -> Result<Vec<String>, String> {
    if let Some(arr) = args.get("roots").and_then(Value::as_array) {
        let raw: Vec<String> = arr
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect();
        if !raw.is_empty() {
            return normalize_search_paths(&raw);
        }
    }
    if let Some(cfg) = load_config(root) {
        let defaults = string_vec(&cfg, "default_doc_roots");
        if !defaults.is_empty() {
            return normalize_search_paths(&defaults);
        }
    }
    Ok(vec![
        ".cursor/rules".into(),
        "docs".into(),
        ".github".into(),
        "README.md".into(),
    ])
}

/// Configured artifact roots (relative); empty if unset.
pub fn artifact_roots(root: &Path) -> Vec<String> {
    load_config(root)
        .map(|cfg| string_vec(&cfg, "artifact_roots"))
        .unwrap_or_default()
}

/// Extra knowledge write roots beyond the built-in allowlist.
pub fn knowledge_write_roots(root: &Path) -> Vec<String> {
    load_config(root)
        .map(|cfg| string_vec(&cfg, "knowledge_write_roots"))
        .unwrap_or_default()
}

/// Named commit scopes: `commit_scopes.<name>` → path prefixes.
pub fn commit_scopes(root: &Path) -> Vec<(String, Vec<String>)> {
    let Some(cfg) = load_config(root) else {
        return Vec::new();
    };
    let Some(obj) = cfg.get("commit_scopes").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out: Vec<(String, Vec<String>)> = obj
        .iter()
        .map(|(k, v)| {
            let paths = v
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            (k.clone(), paths)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Large-file warning threshold in bytes for commit_scope (default 5 MiB).
pub fn large_file_bytes(root: &Path) -> u64 {
    load_config(root)
        .and_then(|cfg| cfg.get("large_file_bytes").and_then(Value::as_u64))
        .unwrap_or(5 * 1024 * 1024)
}

/// Path prefixes `commit_scope` drops from porcelain (default `.cache/`, `target/`,
/// `node_modules/` when the key is absent). An explicit empty array disables filtering.
pub fn commit_ignore(root: &Path) -> Vec<String> {
    match load_config(root) {
        Some(cfg) if cfg.get("commit_ignore").is_some() => string_vec(&cfg, "commit_ignore"),
        _ => vec![
            ".cache/".into(),
            "target/".into(),
            "node_modules/".into(),
        ],
    }
}

/// Whether mas_finalize should auto-promote when `promote` is omitted.
pub fn mas_auto_promote(root: &Path) -> bool {
    load_config(root)
        .and_then(|cfg| {
            cfg.get("mas")
                .and_then(|m| m.get("auto_promote"))
                .and_then(Value::as_bool)
        })
        .unwrap_or(false)
}

/// Token cap for `kind: "handoff"` MAS entries (default 1600).
pub fn mas_handoff_tokens(root: &Path) -> usize {
    load_config(root)
        .and_then(|cfg| {
            cfg.get("mas")
                .and_then(|m| m.get("handoff_tokens"))
                .and_then(Value::as_u64)
        })
        .map(|n| n as usize)
        .unwrap_or(1600)
}

fn izakaya_u64(root: &Path, key: &str, default: u64) -> u64 {
    load_config(root)
        .and_then(|cfg| {
            cfg.get("izakaya")
                .and_then(|m| m.get(key))
                .and_then(Value::as_u64)
        })
        .unwrap_or(default)
}

/// Active-agent lease seconds (default 300). `WORDKEEP_IZAKAYA_TTL_SECS` overrides for tests.
pub fn izakaya_ttl_secs(root: &Path) -> u64 {
    std::env::var("WORDKEEP_IZAKAYA_TTL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| izakaya_u64(root, "ttl_secs", 300))
}

/// Suspended-agent lease seconds (default 86400).
pub fn izakaya_suspend_ttl_secs(root: &Path) -> u64 {
    std::env::var("WORDKEEP_IZAKAYA_SUSPEND_TTL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| izakaya_u64(root, "suspend_ttl_secs", 86_400))
}

/// Age after which an unaccepted handoff from a checked-out agent is orphaned.
pub fn izakaya_orphan_secs(root: &Path) -> u64 {
    std::env::var("WORDKEEP_IZAKAYA_ORPHAN_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| izakaya_u64(root, "orphan_secs", 86_400))
}

/// Repo-relative directory of declarative discovery profiles.
pub fn izakaya_profiles_dir(root: &Path) -> String {
    load_config(root)
        .and_then(|cfg| {
            cfg.get("izakaya")
                .and_then(|m| m.get("profiles_dir"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| ".wordkeep/izakaya/profiles".to_string())
}

/// Like [`paths_from_args`], but uses `fallback` when `paths` is omitted or empty.
pub fn paths_from_args_or(
    _root: &Path,
    args: &Value,
    fallback: &[&str],
) -> Result<Vec<String>, String> {
    if let Some(arr) = args.get("paths").and_then(Value::as_array) {
        let raw: Vec<String> = arr
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect();
        if !raw.is_empty() {
            return normalize_search_paths(&raw);
        }
    }
    Ok(fallback.iter().map(|s| s.to_string()).collect())
}

/// Subdirs to search when a tool's `paths` argument is omitted.
/// Reads `default_paths` from `<root>/.wordkeep/config.json`; falls back to `["src"]`.
pub fn default_paths(root: &Path) -> Vec<String> {
    let Some(v) = load_config(root) else {
        return fallback();
    };
    let Some(arr) = v.get("default_paths").and_then(Value::as_array) else {
        return fallback();
    };
    let paths: Vec<String> = arr
        .iter()
        .filter_map(|x| x.as_str().map(String::from))
        .collect();
    if paths.is_empty() {
        fallback()
    } else {
        paths
    }
}

fn fallback() -> Vec<String> {
    vec!["src".to_string()]
}

fn read_config_string(root: &Path, key: &str) -> Option<String> {
    load_config(root)?
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Optional test-runner prefix from `.wordkeep/config.json` `test_command`.
pub fn test_filter_hint(root: &Path, tag: &str) -> String {
    let filter = if tag.starts_with('[') {
        tag.to_string()
    } else {
        format!("[{tag}]")
    };
    match read_config_string(root, "test_command") {
        Some(cmd) => format!("{cmd} \"{filter}\""),
        None => format!("your test runner, filter \"{filter}\""),
    }
}

/// Error text when a symbol-name tool is called without `symbol`.
pub fn symbol_required_err() -> String {
    "symbol is required (function/type/method NAME, not a file path). \
     For file:line navigation use outline; for nearby code use symbol_refs with kind:ref."
        .into()
}

/// Resolve a user-supplied file path against the workspace root and search paths.
/// Accepts repo-relative paths (`pkg/lib/Foo.cs`) or a unique basename
/// (`ModelLoader.cs`) searched under configured roots / profiles.
/// Convenience wrapper around [`resolve_file_with_args`] with empty args.
/// Kept for callers/tests that do not need profile/paths overrides.
#[allow(dead_code)]
pub fn resolve_file(root: &Path, file: &str) -> Result<PathBuf, String> {
    resolve_file_with_args(root, file, &serde_json::json!({}))
}

/// Like [`resolve_file`], honoring optional `profile` / `paths` in `args`.
pub fn resolve_file_with_args(root: &Path, file: &str, args: &Value) -> Result<PathBuf, String> {
    let rel = validate_rel_path(file)?;

    let direct = root.join(&rel);
    if direct.is_file() {
        return Ok(direct);
    }

    let search = paths_from_args(root, args).unwrap_or_else(|_| default_paths(root));
    for base in &search {
        let p = root.join(base).join(&rel);
        if p.is_file() {
            return Ok(p);
        }
    }

    if !rel.contains('/') {
        let prune = walk::prune_set(root);
        let mut matches = basename_matches(root, &rel, &prune, search.clone());
        if matches.is_empty() {
            matches = basename_matches(root, &rel, &prune, vec!["".to_string()]);
        }
        return match matches.len() {
            0 => {
                let profiles = list_profile_names(root);
                let hint = if profiles.is_empty() {
                    String::new()
                } else {
                    format!("; try profile: {:?} or broaden paths", profiles.join(", "))
                };
                Err(format!(
                    "cannot read {rel} (not under root or search paths{hint})"
                ))
            }
            1 => Ok(matches.remove(0)),
            n => Err(format!(
                "ambiguous file {rel} ({n} matches); pass a repo-relative path"
            )),
        };
    }

    Err(format!("cannot read {rel}"))
}

/// Names of configured path profiles (sorted).
pub fn list_profile_names(root: &Path) -> Vec<String> {
    let Some(cfg) = load_config(root) else {
        return Vec::new();
    };
    let Some(obj) = cfg.get("path_profiles").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut names: Vec<String> = obj.keys().cloned().collect();
    names.sort();
    names
}

/// Collect paths whose basename equals `name`, searching each `bases` entry under `root`.
/// Pass `bases: [""]` to walk the whole repo (still pruned). Results are deduped.
pub fn basename_matches(
    root: &Path,
    name: &str,
    prune: &HashSet<String>,
    bases: Vec<String>,
) -> Vec<PathBuf> {
    let mut matches = Vec::new();
    for base in bases {
        let start = if base.is_empty() {
            root.to_path_buf()
        } else {
            root.join(&base)
        };
        if !start.is_dir() {
            continue;
        }
        for entry in walk::files(&start, prune) {
            if entry.file_name().to_string_lossy() == name {
                let p = entry.into_path();
                if !matches.contains(&p) {
                    matches.push(p);
                }
            }
        }
    }
    matches
}

/// Repo-relative path string for display (forward slashes).
pub fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Appended to symbol_context not-found lines.
pub fn symbol_not_found_hint(paths: &[String]) -> String {
    format!(
        "Hint: broaden with paths:[...] or profile, or profile_upsert to add a missing tree \
         (currently searching {paths:?}); names match on the trailing :: segment."
    )
}

/// Hint when a zero-result query might be outside the active profile coverage.
#[allow(dead_code)]
pub fn coverage_hint(root: &Path, paths: &[String]) -> String {
    let profiles = list_profile_names(root);
    if profiles.is_empty() {
        return format!(
            "Hint: zero results under {paths:?}; broaden paths, call profile_upsert, \
             or check index_stale after a large diff."
        );
    }
    format!(
        "Hint: zero results under {paths:?}; try profile one of {:?} or broaden paths; \
         use profile_upsert to persist a new tree; check index_stale after a large diff.",
        profiles
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wk_cfg_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".wordkeep")).unwrap();
        dir
    }

    #[test]
    fn missing_config_falls_back_to_src() {
        let dir = std::env::temp_dir().join(format!("wk_cfg_missing_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(default_paths(&dir), vec!["src".to_string()]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_default_paths_from_config() {
        let dir = tmp("ok");
        fs::write(
            dir.join(".wordkeep/config.json"),
            r#"{"default_paths":["src","pkg/lib"]}"#,
        )
        .unwrap();
        assert_eq!(
            default_paths(&dir),
            vec!["src".to_string(), "pkg/lib".to_string()]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn profile_precedence_explicit_paths_win() {
        let dir = tmp("prof");
        fs::write(
            dir.join(".wordkeep/config.json"),
            r#"{
              "default_paths":["src"],
              "default_profile":"engine",
              "path_profiles":{"engine":["src"],"kkbp":["tools/kkbp"]},
              "profile_hints":{"kkbp":["kkbp","ghilli"]}
            }"#,
        )
        .unwrap();
        let args = serde_json::json!({"paths":["docs"], "profile":"kkbp"});
        assert_eq!(
            paths_from_args(&dir, &args).unwrap(),
            vec!["docs".to_string()]
        );
        let args = serde_json::json!({"profile":"kkbp"});
        assert_eq!(
            paths_from_args(&dir, &args).unwrap(),
            vec!["tools/kkbp".to_string()]
        );
        let args = serde_json::json!({});
        assert_eq!(
            paths_from_args_with_hint(&dir, &args, Some("fix ghilli cloak")).unwrap(),
            vec!["tools/kkbp".to_string()]
        );
        let args = serde_json::json!({});
        assert_eq!(
            paths_from_args(&dir, &args).unwrap(),
            vec!["src".to_string()]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_file_finds_by_basename_under_default_paths() {
        let dir = tmp("resolve");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join(".wordkeep/config.json"),
            r#"{"default_paths":["src"]}"#,
        )
        .unwrap();
        fs::write(dir.join("src/Foo.rs"), "fn main() {}\n").unwrap();
        let p = resolve_file(&dir, "Foo.rs").unwrap();
        assert_eq!(p, dir.join("src/Foo.rs"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_file_falls_back_to_whole_repo_for_basename() {
        let dir = tmp("resolve_repo");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("pkg/lib")).unwrap();
        fs::write(
            dir.join(".wordkeep/config.json"),
            r#"{"default_paths":["src"]}"#,
        )
        .unwrap();
        fs::write(dir.join("pkg/lib/stats.rs"), "fn main() {}\n").unwrap();
        let p = resolve_file(&dir, "stats.rs").unwrap();
        assert_eq!(p, dir.join("pkg/lib/stats.rs"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_rel_path_rejects_traversal_and_absolute() {
        assert!(validate_rel_path("../etc/passwd").is_err());
        assert!(validate_rel_path("/etc/passwd").is_err());
        assert!(validate_rel_path("C:/Windows/System32").is_err());
        assert_eq!(validate_rel_path("src/foo.rs").unwrap(), "src/foo.rs");
    }

    #[test]
    fn paths_from_args_rejects_escape() {
        let dir = tmp("escape");
        let args = serde_json::json!({ "paths": ["../outside"] });
        assert!(paths_from_args(&dir, &args).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_filter_hint_uses_config_or_fallback() {
        let dir = tmp("test_cmd");
        // empty config still exists from tmp(); remove it for fallback case
        let _ = fs::remove_file(dir.join(".wordkeep/config.json"));
        assert_eq!(
            test_filter_hint(&dir, "unit"),
            "your test runner, filter \"[unit]\""
        );
        fs::write(
            dir.join(".wordkeep/config.json"),
            r#"{"test_command":"ctest -R"}"#,
        )
        .unwrap();
        assert_eq!(test_filter_hint(&dir, "unit"), "ctest -R \"[unit]\"");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_ignore_defaults_and_override() {
        let dir = tmp("cignore");
        let _ = fs::remove_file(dir.join(".wordkeep/config.json"));
        let defaults = commit_ignore(&dir);
        assert!(defaults.iter().any(|p| p.starts_with(".cache")));
        assert!(defaults.iter().any(|p| p.starts_with("target")));
        assert!(defaults.iter().any(|p| p.starts_with("node_modules")));

        fs::write(
            dir.join(".wordkeep/config.json"),
            r#"{"commit_ignore":["pacman-overlay/"]}"#,
        )
        .unwrap();
        let custom = commit_ignore(&dir);
        assert_eq!(custom, vec!["pacman-overlay/".to_string()]);

        fs::write(dir.join(".wordkeep/config.json"), r#"{"commit_ignore":[]}"#).unwrap();
        assert!(commit_ignore(&dir).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
