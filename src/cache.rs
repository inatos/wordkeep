//! Small on-disk cache primitives shared across tools.
//!
//! Everything lives under a platform cache directory (`$XDG_CACHE_HOME/wordkeep/`
//! on Unix, `%LOCALAPPDATA%/wordkeep/` on Windows, else `~/.cache/wordkeep/` or
//! the OS temp dir). `DiskMap` is a JSON-backed, mtime-keyed
//! map from an absolute file path to an opaque payload `Value`: each consumer
//! decides how to (de)serialize its own payload. It is loaded once per process,
//! mutated in memory, and flushed atomically, so a cold spawn over a large tree
//! reuses the previous run's work instead of re-parsing from scratch.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

/// Shared lock for tests that mutate `XDG_CACHE_HOME`.
#[cfg(test)]
pub fn test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Root directory for all on-disk caches.
pub fn dir() -> PathBuf {
    wordkeep_knowledge::wordkeep_cache_dir()
}

/// Modified time as whole nanoseconds since the Unix epoch (`0` if unavailable).
pub fn mtime_ns(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// A JSON-backed, mtime-keyed path → payload cache.
pub struct DiskMap {
    path: PathBuf,
    map: HashMap<String, (u64, Value)>,
    dirty: bool,
}

impl DiskMap {
    /// Load the named cache file from the shared cache dir (best-effort).
    pub fn load(file_name: &str) -> DiskMap {
        Self::load_path(dir().join(file_name))
    }

    /// Load from an explicit path; any IO/format error yields an empty cache.
    pub fn load_path(path: PathBuf) -> DiskMap {
        let mut map = HashMap::new();
        if let Ok(bytes) = std::fs::read(&path) {
            if let Ok(Value::Object(obj)) = serde_json::from_slice::<Value>(&bytes) {
                if obj.get("version").and_then(Value::as_u64) == Some(1) {
                    if let Some(Value::Object(entries)) = obj.get("entries") {
                        for (k, v) in entries {
                            if let (Some(mt), Some(p)) =
                                (v.get("mtime_ns").and_then(Value::as_u64), v.get("payload"))
                            {
                                map.insert(k.clone(), (mt, p.clone()));
                            }
                        }
                    }
                }
            }
        }
        DiskMap {
            path,
            map,
            dirty: false,
        }
    }

    /// Cached payload for `key`, only when the stored mtime matches `mtime_ns`.
    pub fn get(&self, key: &str, mtime_ns: u64) -> Option<&Value> {
        match self.map.get(key) {
            Some((mt, v)) if *mt == mtime_ns => Some(v),
            _ => None,
        }
    }

    /// Insert/replace an entry and mark the cache dirty.
    pub fn put(&mut self, key: &str, mtime_ns: u64, payload: Value) {
        self.map.insert(key.to_string(), (mtime_ns, payload));
        self.dirty = true;
    }

    /// Remove an entry; returns whether a key was present.
    pub fn remove(&mut self, key: &str) -> bool {
        if self.map.remove(key).is_some() {
            self.dirty = true;
            true
        } else {
            false
        }
    }

    /// Flush atomically (temp file + rename) when something changed since load.
    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        let entries: serde_json::Map<String, Value> = self
            .map
            .iter()
            .map(|(k, (mt, v))| (k.clone(), json!({ "mtime_ns": mt, "payload": v })))
            .collect();
        let doc = json!({ "version": 1, "entries": Value::Object(entries) });
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = self.path.with_extension("tmp");
        if let Ok(bytes) = serde_json::to_vec(&doc) {
            if std::fs::write(&tmp, &bytes).is_ok() && std::fs::rename(&tmp, &self.path).is_ok() {
                self.dirty = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn diskmap_round_trips_and_honors_mtime() {
        let path = std::env::temp_dir()
            .join(format!("cbtest_diskmap_{}", std::process::id()))
            .join("m.json");
        let _ = std::fs::remove_file(&path);

        let mut m = DiskMap::load_path(path.clone());
        assert!(m.get("a", 100).is_none());
        m.put("a", 100, json!(["x", "y"]));
        m.save();

        let mut reloaded = DiskMap::load_path(path.clone());
        assert_eq!(reloaded.get("a", 100), Some(&json!(["x", "y"])));
        // Stale mtime is a miss, not a hit.
        assert!(reloaded.get("a", 101).is_none());
        reloaded.remove("a");
        assert!(reloaded.get("a", 100).is_none());

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn save_is_noop_when_clean() {
        let path = std::env::temp_dir()
            .join(format!("cbtest_diskmap_clean_{}", std::process::id()))
            .join("m.json");
        let _ = std::fs::remove_file(&path);
        let mut m = DiskMap::load_path(path.clone());
        m.save(); // nothing dirty → no file created
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
