//! Propose or persist a new `path_profiles` entry in `.wordkeep/config.json`.
//!
//! Agents hit `not_found` when searching outside the active profile. This tool
//! derives a profile name, paths, hints, and matching `commit_scopes` entry so
//! the next session can pass `profile:"…"`. Default mode is dry-run `propose`;
//! `apply` writes the JSON merge.

use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::path::Path;

use crate::config::{self, CONFIG_REL};
use crate::symbol_refs;

const MODE_PROPOSE: &str = "propose";
const MODE_APPLY: &str = "apply";
const MAX_HINTS: usize = 8;

/// Entry point for the `profile_upsert` MCP tool.
pub fn run(root: &Path, args: &Value) -> Result<String, String> {
    let mode = args
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or(MODE_PROPOSE)
        .trim();
    if mode != MODE_PROPOSE && mode != MODE_APPLY {
        return Err(format!(
            "unknown mode {mode:?}; expected \"propose\" or \"apply\""
        ));
    }

    let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
    let set_default = args
        .get("set_default")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut paths = explicit_paths(args)?;
    let symbol = args
        .get("symbol")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();

    if paths.is_empty() {
        if let Some(sym) = symbol {
            paths = discover_paths_for_symbol(root, sym)?;
        }
    }
    if paths.is_empty() {
        return Err(
            "paths is required (or provide symbol so profile_upsert can discover a tree)".into(),
        );
    }
    for p in &paths {
        let abs = root.join(p);
        if !abs.exists() {
            return Err(format!("path does not exist under --root: {p}"));
        }
    }

    let name = match args.get("name").and_then(Value::as_str).map(str::trim) {
        Some(n) if !n.is_empty() => sanitize_profile_name(n)?,
        _ => derive_name(&paths)?,
    };

    let existing = config::list_profile_names(root);
    let collision = existing.iter().any(|e| e == &name);
    if collision && mode == MODE_APPLY && !force {
        return Err(format!(
            "path profile {name:?} already exists; pass force:true to overwrite, or pick another name"
        ));
    }

    let mut hints = explicit_hints(args);
    hints.extend(seed_hints_from_paths(&paths));
    hints.extend(seed_hints_from_query(query));
    let hints = dedupe_hints(hints);

    let commit_paths: Vec<String> = paths
        .iter()
        .map(|p| {
            if p.ends_with('/') {
                p.clone()
            } else {
                format!("{p}/")
            }
        })
        .collect();

    let patch = json!({
        "path_profiles": { name.clone(): paths.clone() },
        "profile_hints": { name.clone(): hints.clone() },
        "commit_scopes": { name.clone(): commit_paths.clone() },
    });

    let mut out = String::new();
    out.push_str(&format!(
        "profile_upsert - mode={mode} name={name:?}\n\
         paths: {paths:?}\n\
         hints: {hints:?}\n\
         commit_scopes: {commit_paths:?}\n"
    ));
    if set_default {
        out.push_str("set_default: true (will update default_profile)\n");
    }
    if collision {
        out.push_str("(profile name already exists — apply needs force:true)\n");
    }
    if let Some(sym) = symbol {
        out.push_str(&format!("discovered via symbol: {sym:?}\n"));
    }

    if mode == MODE_PROPOSE {
        out.push_str("\n(dry-run; pass mode:\"apply\" to write .wordkeep/config.json)\n");
        out.push_str(&format!(
            "\ncandidate patch:\n{}\n",
            serde_json::to_string_pretty(&patch).unwrap_or_default()
        ));
        return Ok(out);
    }

    apply_patch(root, &name, &paths, &hints, &commit_paths, set_default)?;
    out.push_str(&format!(
        "\nwrote {CONFIG_REL}; use profile:{name:?} on symbol tools.\n"
    ));
    Ok(out)
}

fn explicit_paths(args: &Value) -> Result<Vec<String>, String> {
    let Some(arr) = args.get("paths").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let raw: Vec<String> = arr
        .iter()
        .filter_map(|x| x.as_str().map(String::from))
        .collect();
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    config::normalize_search_paths(&raw)
}

fn explicit_hints(args: &Value) -> Vec<String> {
    args.get("hints")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn sanitize_profile_name(name: &str) -> Result<String, String> {
    let n = name.trim().to_ascii_lowercase();
    if n.is_empty() {
        return Err("name must not be empty".into());
    }
    if !n
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!("name must be alphanumeric/[_-]: {name:?}"));
    }
    Ok(n)
}

/// Deepest meaningful path segment: `web/bifrost` → `bifrost`.
pub(crate) fn derive_name(paths: &[String]) -> Result<String, String> {
    let Some(first) = paths.first() else {
        return Err("paths is required to derive a profile name".into());
    };
    let seg = first
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(first)
        .trim();
    sanitize_profile_name(seg)
}

fn seed_hints_from_paths(paths: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for p in paths {
        let trimmed = p.trim_end_matches('/');
        if !trimmed.is_empty() {
            out.push(trimmed.to_string());
        }
        if let Some(base) = trimmed.rsplit('/').next() {
            if !base.is_empty() {
                out.push(base.to_string());
            }
        }
    }
    out
}

fn seed_hints_from_query(query: &str) -> Vec<String> {
    query
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
        .map(|t| t.trim().to_ascii_lowercase())
        .filter(|t| t.len() >= 3)
        .collect()
}

fn dedupe_hints(hints: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for h in hints {
        let key = h.to_ascii_lowercase();
        if key.is_empty() || !seen.insert(key) {
            continue;
        }
        out.push(h);
        if out.len() >= MAX_HINTS {
            break;
        }
    }
    out
}

/// Candidate roots under `web/`, `tools/`, and `assets/scripts` that are not
/// already covered by an existing profile; keep those where `symbol` hits.
fn discover_paths_for_symbol(root: &Path, symbol: &str) -> Result<Vec<String>, String> {
    let covered = covered_path_prefixes(root);
    let mut candidates: Vec<String> = Vec::new();
    for base in ["web", "tools"] {
        let dir = root.join(base);
        if !dir.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for ent in entries.flatten() {
            if !ent.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let name = ent.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let rel = format!("{base}/{name}");
            if is_covered(&rel, &covered) {
                continue;
            }
            candidates.push(rel);
        }
    }
    let scripts = "assets/scripts".to_string();
    if root.join(&scripts).is_dir() && !is_covered(&scripts, &covered) {
        candidates.push(scripts);
    }
    // Prefer smaller / more specific trees first (alphabetical is fine + deterministic).
    candidates.sort();

    let mut hits = Vec::new();
    for cand in candidates {
        let (files, _) = symbol_refs::refs_by_file(root, symbol, std::slice::from_ref(&cand));
        if !files.is_empty() {
            hits.push(cand);
        }
    }
    if hits.is_empty() {
        return Err(format!(
            "symbol {symbol:?}: no hits under unprofiled web/*, tools/*, or assets/scripts"
        ));
    }
    Ok(hits)
}

fn covered_path_prefixes(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(cfg) = config::load_config(root) {
        if let Some(obj) = cfg.get("path_profiles").and_then(Value::as_object) {
            for arr in obj.values().filter_map(Value::as_array) {
                for p in arr.iter().filter_map(Value::as_str) {
                    out.push(p.trim_end_matches('/').to_string());
                }
            }
        }
        if let Some(arr) = cfg.get("default_paths").and_then(Value::as_array) {
            for p in arr.iter().filter_map(Value::as_str) {
                out.push(p.trim_end_matches('/').to_string());
            }
        }
    }
    out
}

fn is_covered(rel: &str, covered: &[String]) -> bool {
    let r = rel.trim_end_matches('/');
    covered
        .iter()
        .any(|c| r == c || r.starts_with(&format!("{c}/")) || c.starts_with(&format!("{r}/")))
}

fn apply_patch(
    root: &Path,
    name: &str,
    paths: &[String],
    hints: &[String],
    commit_paths: &[String],
    set_default: bool,
) -> Result<(), String> {
    let cfg_path = root.join(CONFIG_REL);
    let mut cfg: Value = if cfg_path.exists() {
        let raw =
            std::fs::read_to_string(&cfg_path).map_err(|e| format!("read {CONFIG_REL}: {e}"))?;
        serde_json::from_str(&raw).map_err(|e| format!("parse {CONFIG_REL}: {e}"))?
    } else {
        json!({})
    };
    if !cfg.is_object() {
        return Err(format!("{CONFIG_REL} root must be a JSON object"));
    }
    let obj = cfg.as_object_mut().unwrap();

    ensure_object(obj, "path_profiles").insert(
        name.to_string(),
        Value::Array(paths.iter().cloned().map(Value::String).collect()),
    );
    ensure_object(obj, "profile_hints").insert(
        name.to_string(),
        Value::Array(hints.iter().cloned().map(Value::String).collect()),
    );
    ensure_object(obj, "commit_scopes").insert(
        name.to_string(),
        Value::Array(commit_paths.iter().cloned().map(Value::String).collect()),
    );
    if set_default {
        obj.insert("default_profile".into(), Value::String(name.to_string()));
    }

    if let Some(parent) = cfg_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir .wordkeep: {e}"))?;
    }
    let pretty = serde_json::to_string_pretty(&cfg).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&cfg_path, format!("{pretty}\n"))
        .map_err(|e| format!("write {CONFIG_REL}: {e}"))?;
    Ok(())
}

fn ensure_object<'a>(obj: &'a mut Map<String, Value>, key: &str) -> &'a mut Map<String, Value> {
    if !obj.get(key).map(Value::is_object).unwrap_or(false) {
        obj.insert(key.to_string(), Value::Object(Map::new()));
    }
    obj.get_mut(key).unwrap().as_object_mut().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wk_prof_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".wordkeep")).unwrap();
        dir
    }

    #[test]
    fn derive_name_from_deepest_segment() {
        assert_eq!(derive_name(&["web/bifrost".into()]).unwrap(), "bifrost");
        assert_eq!(derive_name(&["tools/kkbp/".into()]).unwrap(), "kkbp");
    }

    #[test]
    fn propose_does_not_write() {
        let dir = tmp("propose");
        fs::create_dir_all(dir.join("web/bifrost")).unwrap();
        fs::write(
            dir.join(".wordkeep/config.json"),
            r#"{"path_profiles":{"engine":["src"]}}"#,
        )
        .unwrap();
        let before = fs::read_to_string(dir.join(".wordkeep/config.json")).unwrap();
        let out = run(&dir, &json!({"mode":"propose","paths":["web/bifrost"]})).unwrap();
        assert!(out.contains("dry-run"), "{out}");
        assert!(out.contains("bifrost"), "{out}");
        assert_eq!(
            fs::read_to_string(dir.join(".wordkeep/config.json")).unwrap(),
            before
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_merges_and_collision_requires_force() {
        let dir = tmp("apply");
        fs::create_dir_all(dir.join("web/bifrost")).unwrap();
        fs::write(
            dir.join(".wordkeep/config.json"),
            r#"{"path_profiles":{"engine":["src"]},"keep":true}"#,
        )
        .unwrap();
        let out = run(
            &dir,
            &json!({"mode":"apply","paths":["web/bifrost"],"name":"bifrost"}),
        )
        .unwrap();
        assert!(out.contains("wrote"), "{out}");
        let cfg: Value =
            serde_json::from_str(&fs::read_to_string(dir.join(".wordkeep/config.json")).unwrap())
                .unwrap();
        assert_eq!(cfg["keep"], true);
        assert_eq!(cfg["path_profiles"]["bifrost"][0], "web/bifrost");
        assert!(!cfg["profile_hints"]["bifrost"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(cfg["commit_scopes"]["bifrost"][0], "web/bifrost/");

        let err = run(
            &dir,
            &json!({"mode":"apply","paths":["web/bifrost"],"name":"bifrost"}),
        )
        .unwrap_err();
        assert!(err.contains("already exists"), "{err}");

        let _ = run(
            &dir,
            &json!({"mode":"apply","paths":["web/bifrost"],"name":"bifrost","force":true,"hints":["wallspin"]}),
        )
        .unwrap();
        let cfg2: Value =
            serde_json::from_str(&fs::read_to_string(dir.join(".wordkeep/config.json")).unwrap())
                .unwrap();
        let hints = cfg2["profile_hints"]["bifrost"].as_array().unwrap();
        assert!(
            hints.iter().any(|h| h.as_str() == Some("wallspin")),
            "{hints:?}"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
