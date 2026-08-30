//! MAS-scoped revertible write journal (temporal composability PoC).
//!
//! When `knowledge_upsert` is called with `effect_session`, each write is recorded
//! with a before/after snapshot. On `mas_finalize` with `effects: "recover"`, inverses
//! are applied LIFO. On `effects: "commit"`, the journal is dropped.
//!
//! Journals live under `$XDG_CACHE_HOME/wordkeep/workspaces/<id>/mas/effects/<session>.json`.
//! A companion `.lock` file serializes concurrent MCP processes.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{mas, workspace};

const JOURNAL_VERSION: u64 = 1;

#[derive(Clone, Debug)]
struct JournalEntry {
    path: String,
    /// `None` = file did not exist before this write.
    before: Option<String>,
    /// Content immediately after the write (used for conflict detection).
    after: String,
    ts: u64,
}

#[derive(Clone, Debug)]
struct Journal {
    session: String,
    status: String,
    entries: Vec<JournalEntry>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn effects_dir(root: &Path) -> Result<PathBuf, String> {
    let dir = workspace::ensure_workspace_dir(root)?
        .join("mas")
        .join("effects");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    Ok(dir)
}

fn journal_path(root: &Path, session: &str) -> Result<PathBuf, String> {
    mas::validate_session_id(session)?;
    Ok(effects_dir(root)?.join(format!("{session}.json")))
}

fn lock_path(root: &Path, session: &str) -> Result<PathBuf, String> {
    Ok(effects_dir(root)?.join(format!("{session}.lock")))
}

struct SessionLock {
    _file: std::fs::File,
}

impl SessionLock {
    fn acquire(root: &Path, session: &str) -> Result<Self, String> {
        use fs2::FileExt;
        let path = lock_path(root, session)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| format!("open lock {}: {e}", path.display()))?;
        file.lock_exclusive()
            .map_err(|e| format!("lock {}: {e}", path.display()))?;
        Ok(SessionLock { _file: file })
    }
}

fn entry_to_value(e: &JournalEntry) -> Value {
    json!({
        "path": e.path,
        "before": e.before,
        "after": e.after,
        "ts": e.ts,
    })
}

fn entry_from_value(v: &Value) -> Option<JournalEntry> {
    Some(JournalEntry {
        path: v.get("path").and_then(Value::as_str)?.to_string(),
        before: v.get("before").and_then(|b| {
            if b.is_null() {
                None
            } else {
                b.as_str().map(String::from)
            }
        }),
        after: v.get("after").and_then(Value::as_str)?.to_string(),
        ts: v.get("ts").and_then(Value::as_u64).unwrap_or(0),
    })
}

fn journal_to_value(root: &Path, j: &Journal) -> Value {
    json!({
        "version": JOURNAL_VERSION,
        "workspace_id": workspace::workspace_id(root),
        "session": j.session,
        "status": j.status,
        "entries": j.entries.iter().map(entry_to_value).collect::<Vec<_>>(),
    })
}

fn journal_from_value(v: &Value) -> Option<Journal> {
    let entries: Vec<JournalEntry> = v
        .get("entries")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(entry_from_value).collect())
        .unwrap_or_default();
    Some(Journal {
        session: v.get("session").and_then(Value::as_str)?.to_string(),
        status: v
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("open")
            .to_string(),
        entries,
    })
}

fn load_journal(root: &Path, session: &str) -> Result<Option<Journal>, String> {
    let path = journal_path(root, session)?;
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let v: Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))?;
    journal_from_value(&v)
        .ok_or_else(|| format!("invalid journal file {}", path.display()))
        .map(Some)
}

fn save_journal(root: &Path, j: &Journal) -> Result<(), String> {
    let path = journal_path(root, &j.session)?;
    workspace::write_atomic_json(&path, &journal_to_value(root, j))
}

fn delete_journal(root: &Path, session: &str) -> Result<(), String> {
    let path = journal_path(root, session)?;
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("remove {}: {e}", path.display()))?;
    }
    Ok(())
}

fn read_file_or_missing(root: &Path, rel: &str) -> Result<Option<String>, String> {
    let full = root.join(rel);
    if !full.exists() {
        return Ok(None);
    }
    let s = std::fs::read_to_string(&full).map_err(|e| format!("read {}: {e}", full.display()))?;
    Ok(Some(s))
}

fn write_or_delete(root: &Path, rel: &str, contents: Option<&str>) -> Result<(), String> {
    let full = root.join(rel);
    match contents {
        None => {
            if full.exists() {
                std::fs::remove_file(&full)
                    .map_err(|e| format!("remove {}: {e}", full.display()))?;
            }
        }
        Some(text) => {
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
            }
            let tmp = full.with_extension("wordkeep-tmp");
            std::fs::write(&tmp, text).map_err(|e| format!("write {}: {e}", tmp.display()))?;
            std::fs::rename(&tmp, &full).map_err(|e| format!("rename {}: {e}", full.display()))?;
        }
    }
    Ok(())
}

/// Whether an open journal with pending entries exists for this session.
pub fn has_pending(root: &Path, session: &str) -> Result<bool, String> {
    Ok(match load_journal(root, session)? {
        Some(j) => j.status == "open" && !j.entries.is_empty(),
        None => false,
    })
}

/// Count of pending journal entries (0 if none or committed/recovered).
pub fn pending_count(root: &Path, session: &str) -> Result<usize, String> {
    Ok(match load_journal(root, session)? {
        Some(j) if j.status == "open" => j.entries.len(),
        _ => 0,
    })
}

/// Record a write in the session journal (creates journal if needed).
pub fn record_write(
    root: &Path,
    session: &str,
    rel_path: &str,
    before: Option<String>,
    after: String,
) -> Result<(), String> {
    mas::require_open_session(root, session)?;
    let _lock = SessionLock::acquire(root, session)?;
    let mut j = load_journal(root, session)?.unwrap_or(Journal {
        session: session.to_string(),
        status: "open".into(),
        entries: Vec::new(),
    });
    if j.status != "open" {
        return Err(format!(
            "effect journal for session {session} is {} (cannot record)",
            j.status
        ));
    }
    j.entries.push(JournalEntry {
        path: rel_path.to_string(),
        before,
        after,
        ts: now_secs(),
    });
    save_journal(root, &j)
}

/// Apply inverses LIFO and mark journal recovered.
pub fn recover(root: &Path, session: &str) -> Result<String, String> {
    let _lock = SessionLock::acquire(root, session)?;
    let Some(mut j) = load_journal(root, session)? else {
        return Ok(format!(
            "effect_recover - session {session}: no journal (noop)"
        ));
    };
    if j.status == "recovered" {
        return Ok(format!(
            "effect_recover - session {session}: already recovered (noop)"
        ));
    }
    if j.status == "committed" {
        return Err(format!(
            "effect journal for session {session} was committed; cannot recover"
        ));
    }
    if j.entries.is_empty() {
        j.status = "recovered".into();
        save_journal(root, &j)?;
        return Ok(format!(
            "effect_recover - session {session}: empty journal marked recovered"
        ));
    }
    let n = j.entries.len();
    for e in j.entries.iter().rev() {
        let current = read_file_or_missing(root, &e.path)?;
        match current {
            Some(cur) if cur == e.after => {}
            Some(_) => {
                return Err(format!(
                    "conflict at {}: on-disk content changed since journaled write",
                    e.path
                ));
            }
            None if e.after.is_empty() => {}
            None => {
                return Err(format!(
                    "conflict at {}: file missing but journal expected post-write content",
                    e.path
                ));
            }
        }
        write_or_delete(root, &e.path, e.before.as_deref())?;
    }
    j.status = "recovered".into();
    save_journal(root, &j)?;
    Ok(format!(
        "effect_recover - session {session}: reverted {n} write(s) in LIFO order"
    ))
}

/// Drop the journal without reverting (writes become permanent).
pub fn commit(root: &Path, session: &str) -> Result<String, String> {
    let _lock = SessionLock::acquire(root, session)?;
    let Some(j) = load_journal(root, session)? else {
        return Ok(format!(
            "effect_commit - session {session}: no journal (noop)"
        ));
    };
    if j.status == "committed" {
        return Ok(format!(
            "effect_commit - session {session}: already committed (noop)"
        ));
    }
    if j.status == "recovered" {
        return Err(format!(
            "effect journal for session {session} was recovered; cannot commit"
        ));
    }
    delete_journal(root, session)?;
    Ok(format!(
        "effect_commit - session {session}: committed {} write(s)",
        j.entries.len()
    ))
}

/// Resolve pending effects per finalize decision.
pub fn resolve_effects(root: &Path, session: &str, decision: &str) -> Result<String, String> {
    match decision {
        "commit" => commit(root, session),
        "recover" => recover(root, session),
        other => Err(format!(
            "effects must be \"commit\" or \"recover\", got {other:?}"
        )),
    }
}

/// Relative paths with open journal entries (for coeffect notify before recover).
pub fn pending_paths(root: &Path, session: &str) -> Result<Vec<String>, String> {
    let Some(j) = load_journal(root, session)? else {
        return Ok(Vec::new());
    };
    if j.status != "open" || j.entries.is_empty() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for e in &j.entries {
        if !paths.iter().any(|p| p == &e.path) {
            paths.push(e.path.clone());
        }
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_root(n: u32) -> PathBuf {
        let root = std::env::temp_dir().join(format!("wk_effect_{}_{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".wordkeep/notes")).unwrap();
        root
    }

    fn with_env<F: FnOnce(&Path)>(f: F) {
        let _guard = crate::cache::test_env_lock();
        let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = temp_root(n);
        let cache =
            std::env::temp_dir().join(format!("wk_effect_cache_{}_{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&cache);
        std::env::set_var("XDG_CACHE_HOME", &cache);
        mas::get_or_create_session_for_test(&root, "fx-test").unwrap();
        f(&root);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn record_and_recover_create() {
        with_env(|root| {
            let rel = ".wordkeep/notes/a.md";
            record_write(root, "fx-test", rel, None, "# A\n\nhello\n".to_string()).unwrap();
            write_or_delete(root, rel, Some("# A\n\nhello\n")).unwrap();
            assert!(has_pending(root, "fx-test").unwrap());
            let msg = recover(root, "fx-test").unwrap();
            assert!(msg.contains("reverted 1"));
            assert!(!root.join(rel).exists());
        });
    }

    #[test]
    fn lifo_two_writes_same_file() {
        with_env(|root| {
            let rel = ".wordkeep/notes/b.md";
            write_or_delete(root, rel, Some("# B\n\nv1\n")).unwrap();
            record_write(
                root,
                "fx-test",
                rel,
                Some("# B\n\nv1\n".into()),
                "# B\n\nv2\n".to_string(),
            )
            .unwrap();
            write_or_delete(root, rel, Some("# B\n\nv2\n")).unwrap();
            record_write(
                root,
                "fx-test",
                rel,
                Some("# B\n\nv2\n".into()),
                "# B\n\nv3\n".to_string(),
            )
            .unwrap();
            write_or_delete(root, rel, Some("# B\n\nv3\n")).unwrap();
            recover(root, "fx-test").unwrap();
            let content = std::fs::read_to_string(root.join(rel)).unwrap();
            assert_eq!(content, "# B\n\nv1\n");
        });
    }

    #[test]
    fn commit_drops_journal() {
        with_env(|root| {
            record_write(
                root,
                "fx-test",
                ".wordkeep/notes/c.md",
                None,
                "# C\n\nx\n".to_string(),
            )
            .unwrap();
            write_or_delete(root, ".wordkeep/notes/c.md", Some("# C\n\nx\n")).unwrap();
            commit(root, "fx-test").unwrap();
            assert!(!has_pending(root, "fx-test").unwrap());
            assert!(root.join(".wordkeep/notes/c.md").exists());
        });
    }

    #[test]
    fn conflict_refuses_recover() {
        with_env(|root| {
            let rel = ".wordkeep/notes/d.md";
            record_write(root, "fx-test", rel, None, "# D\n\norig\n".to_string()).unwrap();
            write_or_delete(root, rel, Some("# D\n\norig\n")).unwrap();
            write_or_delete(root, rel, Some("# D\n\nexternal edit\n")).unwrap();
            let err = recover(root, "fx-test").unwrap_err();
            assert!(err.contains("conflict"));
        });
    }

    #[test]
    fn recover_idempotent_after_recovered() {
        with_env(|root| {
            let rel = ".wordkeep/notes/e.md";
            record_write(root, "fx-test", rel, None, "# E\n\nz\n".to_string()).unwrap();
            write_or_delete(root, rel, Some("# E\n\nz\n")).unwrap();
            recover(root, "fx-test").unwrap();
            let msg = recover(root, "fx-test").unwrap();
            assert!(msg.contains("already recovered"));
        });
    }

    #[test]
    fn journal_survives_simulated_restart() {
        with_env(|root| {
            let rel = ".wordkeep/notes/restart.md";
            record_write(root, "fx-test", rel, None, "# R\n\npersist\n".to_string()).unwrap();
            write_or_delete(root, rel, Some("# R\n\npersist\n")).unwrap();
            assert!(has_pending(root, "fx-test").unwrap());
            assert_eq!(pending_count(root, "fx-test").unwrap(), 1);
            recover(root, "fx-test").unwrap();
            assert!(!root.join(rel).exists());
        });
    }
}
