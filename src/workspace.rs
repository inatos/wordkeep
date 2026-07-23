//! Workspace-scoped persistence helpers.
//!
//! Wordkeep may be pointed at many repos via `--root`. Durable state that is
//! specific to a workspace (MAS sessions, run history, artifact caches) lives
//! under `$XDG_CACHE_HOME/wordkeep/workspaces/<root-hash>/` so sessions from
//! different trees never collide. Legacy global MAS files are migrated on first
//! access.

use serde_json::Value;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::cache;

/// Stable ID for a workspace root (hex of a hash of the canonical path).
pub fn workspace_id(root: &Path) -> String {
    let canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let key = canon.to_string_lossy().replace('\\', "/").to_lowercase();
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Cache directory for a workspace: `wordkeep/workspaces/<id>/`.
pub fn workspace_dir(root: &Path) -> PathBuf {
    cache::dir().join("workspaces").join(workspace_id(root))
}

/// Ensure the workspace cache directory exists.
pub fn ensure_workspace_dir(root: &Path) -> Result<PathBuf, String> {
    let dir = workspace_dir(root);
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    Ok(dir)
}

/// Unix seconds since epoch.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Atomically write pretty JSON to `path` (temp + rename).
pub fn write_atomic_json(path: &Path, doc: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let tmp = path.with_extension("tmp");
    let bytes = serde_json::to_vec_pretty(doc).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&tmp, &bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename {}: {e}", path.display()))
}

/// Load a JSON object from disk; missing file → `None`.
pub fn read_json(path: &Path) -> Result<Option<Value>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let v: Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))?;
    Ok(Some(v))
}

/// Atomically write plain text (temp + rename).
#[allow(dead_code)]
pub fn write_atomic_text(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, contents).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename {}: {e}", path.display()))
}

/// Path under the workspace cache.
pub fn workspace_file(root: &Path, name: &str) -> PathBuf {
    workspace_dir(root).join(name)
}

/// Legacy global MAS directory (pre-workspace namespacing).
pub fn legacy_mas_dir() -> PathBuf {
    cache::dir().join("mas")
}

/// Migrate a legacy global MAS session file into the workspace cache if needed.
/// Returns the destination path that should be used going forward.
pub fn migrate_mas_session(root: &Path, session: &str) -> Result<PathBuf, String> {
    let dest_dir = ensure_workspace_dir(root)?.join("mas");
    std::fs::create_dir_all(&dest_dir).map_err(|e| format!("mkdir {}: {e}", dest_dir.display()))?;
    let dest = dest_dir.join(format!("{session}.json"));
    if dest.exists() {
        return Ok(dest);
    }
    let legacy = legacy_mas_dir().join(format!("{session}.json"));
    if legacy.exists() {
        // Best-effort copy; leave the legacy file in place for other roots.
        if let Ok(bytes) = std::fs::read(&legacy) {
            let _ = std::fs::write(&dest, bytes);
        }
    }
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn workspace_id_is_stable_for_same_path() {
        let a = workspace_id(Path::new("/tmp/foo"));
        let b = workspace_id(Path::new("/tmp/foo"));
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn write_and_read_json_round_trip() {
        let dir = std::env::temp_dir().join(format!("wk_ws_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("doc.json");
        write_atomic_json(&path, &json!({"version": 1, "ok": true})).unwrap();
        let v = read_json(&path).unwrap().unwrap();
        assert_eq!(v["ok"], json!(true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrate_mas_session_copies_legacy() {
        let _guard = crate::cache::test_env_lock();
        let root = std::env::temp_dir().join(format!("wk_mig_root_{}", std::process::id()));
        let cache = std::env::temp_dir().join(format!("wk_mig_cache_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&cache);
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("XDG_CACHE_HOME", &cache);
        let legacy = legacy_mas_dir();
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(
            legacy.join("s1.json"),
            r#"{"version":1,"session":"s1","entries":[]}"#,
        )
        .unwrap();
        let dest = migrate_mas_session(&root, "s1").unwrap();
        assert!(dest.exists());
        assert!(dest.to_string_lossy().contains("workspaces"));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&cache);
    }
}
