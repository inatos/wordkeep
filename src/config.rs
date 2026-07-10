//! Workspace defaults from `.wordkeep/config.json` at the repo root.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::walk;

const CONFIG_REL: &str = ".wordkeep/config.json";

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

/// Resolve `paths` from tool args or `.wordkeep/config.json` `default_paths`.
pub fn paths_from_args(root: &Path, args: &Value) -> Result<Vec<String>, String> {
    if let Some(arr) = args.get("paths").and_then(Value::as_array) {
        let raw: Vec<String> = arr
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect();
        if !raw.is_empty() {
            return normalize_search_paths(&raw);
        }
    }
    Ok(default_paths(root))
}

/// Resolve `roots` for knowledge_search / upsert indexing.
pub fn doc_roots_from_args(args: &Value) -> Result<Vec<String>, String> {
    if let Some(arr) = args.get("roots").and_then(Value::as_array) {
        let raw: Vec<String> = arr
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect();
        if !raw.is_empty() {
            return normalize_search_paths(&raw);
        }
    }
    Ok(vec![
        ".cursor/rules".into(),
        "docs".into(),
        ".github".into(),
        "README.md".into(),
    ])
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
    let path = root.join(CONFIG_REL);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return fallback();
    };
    let Ok(v) = serde_json::from_str::<Value>(&raw) else {
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
    let path = root.join(CONFIG_REL);
    let raw = std::fs::read_to_string(&path).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    v.get(key)
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

/// Resolve a user-supplied file path against the workspace root and `default_paths`.
/// Accepts repo-relative paths (`pkg/lib/Foo.cs`) or a unique basename
/// (`ModelLoader.cs`) searched under configured roots.
pub fn resolve_file(root: &Path, file: &str) -> Result<PathBuf, String> {
    let rel = validate_rel_path(file)?;

    let direct = root.join(&rel);
    if direct.is_file() {
        return Ok(direct);
    }

    for base in default_paths(root) {
        let p = root.join(&base).join(&rel);
        if p.is_file() {
            return Ok(p);
        }
    }

    if !rel.contains('/') {
        let prune = walk::prune_set(root);
        let mut matches = basename_matches(root, &rel, &prune, default_paths(root));
        if matches.is_empty() {
            matches = basename_matches(root, &rel, &prune, vec!["".to_string()]);
        }
        return match matches.len() {
            0 => Err(format!(
                "cannot read {rel} (not under root or default_paths)"
            )),
            1 => Ok(matches.remove(0)),
            n => Err(format!(
                "ambiguous file {rel} ({n} matches); pass a repo-relative path"
            )),
        };
    }

    Err(format!("cannot read {rel}"))
}

/// Collect paths whose basename equals `name`, searching each `bases` entry under `root`.
/// Pass `bases: [""]` to walk the whole repo (still pruned). Results are deduped.
fn basename_matches(
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
        "Hint: broaden with paths:[...] or set default_paths in .wordkeep/config.json \
         (currently searching {paths:?}); names match on the trailing :: segment."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
        let dir = std::env::temp_dir().join(format!("wk_cfg_ok_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".wordkeep")).unwrap();
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
    fn resolve_file_finds_by_basename_under_default_paths() {
        let dir = std::env::temp_dir().join(format!("wk_resolve_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".wordkeep")).unwrap();
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
        let dir = std::env::temp_dir().join(format!("wk_resolve_repo_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".wordkeep")).unwrap();
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
        let dir = std::env::temp_dir().join(format!("wk_paths_escape_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let args = serde_json::json!({ "paths": ["../outside"] });
        assert!(paths_from_args(&dir, &args).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_filter_hint_uses_config_or_fallback() {
        let dir = std::env::temp_dir().join(format!("wk_test_cmd_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(
            test_filter_hint(&dir, "unit"),
            "your test runner, filter \"[unit]\""
        );
        fs::create_dir_all(dir.join(".wordkeep")).unwrap();
        fs::write(
            dir.join(".wordkeep/config.json"),
            r#"{"test_command":"ctest -R"}"#,
        )
        .unwrap();
        assert_eq!(test_filter_hint(&dir, "unit"), "ctest -R \"[unit]\"");
        let _ = fs::remove_dir_all(&dir);
    }
}
