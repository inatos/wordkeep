//! Run history metadata (never executes commands).
//!
//! Shell gates can report results via MCP `run_record` or the non-interactive
//! `wordkeep run-record` CLI. History lives under the workspace cache.

use serde_json::{json, Value};
use std::path::Path;

use crate::{coeffects, stats, workspace};

const STORE_VERSION: u64 = 1;
const FILE: &str = "runs.json";
const MAX_RUNS: usize = 200;

#[derive(Clone, Debug)]
pub struct Run {
    pub id: String,
    pub command: String,
    pub status: String,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<u64>,
    pub summary: String,
    pub log_path: Option<String>,
    pub key_failures: Vec<String>,
    pub artifacts: Vec<String>,
    pub tags: Vec<String>,
    pub defect_ids: Vec<String>,
    pub created: u64,
    pub updated: u64,
}

fn string_array(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    x.as_str()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn optional_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn required_str(args: &Value, key: &str) -> Result<String, String> {
    optional_str(args, key).ok_or_else(|| format!("{key} is required"))
}

fn validate_status(s: &str) -> Result<(), String> {
    match s {
        "running" | "passed" | "failed" | "cancelled" => Ok(()),
        other => Err(format!(
            "unknown status {other:?}; use running, passed, failed, or cancelled"
        )),
    }
}

fn run_to_value(r: &Run) -> Value {
    json!({
        "id": r.id,
        "command": r.command,
        "status": r.status,
        "exit_code": r.exit_code,
        "duration_ms": r.duration_ms,
        "summary": r.summary,
        "log_path": r.log_path,
        "key_failures": r.key_failures,
        "artifacts": r.artifacts,
        "tags": r.tags,
        "defect_ids": r.defect_ids,
        "created": r.created,
        "updated": r.updated,
    })
}

fn run_from_value(v: &Value) -> Option<Run> {
    Some(Run {
        id: v.get("id").and_then(Value::as_str)?.to_string(),
        command: v
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        status: v
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("running")
            .to_string(),
        exit_code: v.get("exit_code").and_then(Value::as_i64),
        duration_ms: v.get("duration_ms").and_then(Value::as_u64),
        summary: v
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        log_path: optional_str(v, "log_path"),
        key_failures: string_array(v, "key_failures"),
        artifacts: string_array(v, "artifacts"),
        tags: string_array(v, "tags"),
        defect_ids: string_array(v, "defect_ids"),
        created: v.get("created").and_then(Value::as_u64).unwrap_or(0),
        updated: v.get("updated").and_then(Value::as_u64).unwrap_or(0),
    })
}

fn store_path(root: &Path) -> std::path::PathBuf {
    workspace::workspace_file(root, FILE)
}

fn load_all(root: &Path) -> Result<Vec<Run>, String> {
    let path = store_path(root);
    let Some(v) = workspace::read_json(&path)? else {
        return Ok(Vec::new());
    };
    let arr = v
        .get("runs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(arr.iter().filter_map(run_from_value).collect())
}

fn save_all(root: &Path, runs: &[Run]) -> Result<(), String> {
    let mut trimmed = runs.to_vec();
    if trimmed.len() > MAX_RUNS {
        trimmed.sort_by_key(|r| r.updated);
        let drop_n = trimmed.len() - MAX_RUNS;
        trimmed.drain(0..drop_n);
    }
    let doc = json!({
        "version": STORE_VERSION,
        "workspace_id": workspace::workspace_id(root),
        "updated": workspace::now_secs(),
        "runs": trimmed.iter().map(run_to_value).collect::<Vec<_>>(),
    });
    workspace::ensure_workspace_dir(root)?;
    workspace::write_atomic_json(&store_path(root), &doc)
}

fn missing_flags(root: &Path, r: &Run) -> Vec<String> {
    let mut flags = Vec::new();
    if let Some(log) = &r.log_path {
        let p = if Path::new(log).is_absolute() {
            Path::new(log).to_path_buf()
        } else {
            root.join(log)
        };
        if !p.exists() {
            flags.push(format!("missing log: {log}"));
        }
    }
    for a in &r.artifacts {
        let p = if Path::new(a).is_absolute() {
            Path::new(a).to_path_buf()
        } else {
            root.join(a)
        };
        if !p.exists() {
            flags.push(format!("missing artifact: {a}"));
        }
    }
    flags
}

/// Create or update a run record (metadata only).
pub fn record(root: &Path, args: &Value) -> Result<String, String> {
    let command = required_str(args, "command")?;
    let status = optional_str(args, "status").unwrap_or_else(|| "running".into());
    validate_status(&status)?;
    let summary = optional_str(args, "summary").unwrap_or_default();
    let exit_code = args.get("exit_code").and_then(Value::as_i64);
    let duration_ms = args.get("duration_ms").and_then(Value::as_u64);
    let log_path = optional_str(args, "log_path");
    let key_failures = string_array(args, "key_failures");
    let artifacts = string_array(args, "artifacts");
    let tags = string_array(args, "tags");
    let defect_ids = string_array(args, "defect_ids");
    let now = workspace::now_secs();

    let mut runs = load_all(root)?;
    let id = if let Some(id) = optional_str(args, "id") {
        id
    } else {
        format!("run-{}", now)
    };

    let action = if let Some(existing) = runs.iter_mut().find(|r| r.id == id) {
        existing.command = command.clone();
        existing.status = status.clone();
        if exit_code.is_some() {
            existing.exit_code = exit_code;
        }
        if duration_ms.is_some() {
            existing.duration_ms = duration_ms;
        }
        if !summary.is_empty() {
            existing.summary = summary.clone();
        }
        if log_path.is_some() {
            existing.log_path = log_path.clone();
        }
        if !key_failures.is_empty() {
            existing.key_failures = key_failures.clone();
        }
        if !artifacts.is_empty() {
            existing.artifacts = artifacts.clone();
        }
        if !tags.is_empty() {
            existing.tags = tags.clone();
        }
        if !defect_ids.is_empty() {
            existing.defect_ids = defect_ids.clone();
        }
        existing.updated = now;
        "updated"
    } else {
        runs.push(Run {
            id: id.clone(),
            command: command.clone(),
            status: status.clone(),
            exit_code,
            duration_ms,
            summary: summary.clone(),
            log_path,
            key_failures,
            artifacts,
            tags,
            defect_ids,
            created: now,
            updated: now,
        });
        "created"
    };
    save_all(root, &runs)?;
    coeffects::notify(root, "run_record", coeffects::NotifyCtx::EMPTY);
    let out = format!("run_record - {action} {id} status={status} cmd={command:?}");
    stats::record("run_record", 64, (out.len() / 4) as u64);
    Ok(out)
}

/// Filter recent run history.
pub fn history(root: &Path, args: &Value) -> Result<String, String> {
    let status_filter = optional_str(args, "status");
    let tag = optional_str(args, "tag");
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .max(1) as usize;

    let mut runs = load_all(root)?;
    runs.retain(|r| {
        if let Some(s) = &status_filter {
            if !r.status.eq_ignore_ascii_case(s) {
                return false;
            }
        }
        if let Some(t) = &tag {
            if !r.tags.iter().any(|x| x.eq_ignore_ascii_case(t)) {
                return false;
            }
        }
        true
    });
    runs.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| b.id.cmp(&a.id)));
    runs.truncate(limit);

    let mut out = format!("run_history - {} run(s)\n", runs.len());
    for r in &runs {
        out.push_str(&format!(
            "\n[{}] {} — {}\n  command: {}\n",
            r.status, r.id, r.updated, r.command
        ));
        if let Some(code) = r.exit_code {
            out.push_str(&format!("  exit_code: {code}\n"));
        }
        if let Some(ms) = r.duration_ms {
            out.push_str(&format!("  duration_ms: {ms}\n"));
        }
        if !r.summary.is_empty() {
            out.push_str(&format!("  summary: {}\n", r.summary));
        }
        if let Some(log) = &r.log_path {
            out.push_str(&format!("  log: {log}\n"));
        }
        if !r.key_failures.is_empty() {
            out.push_str("  key_failures:\n");
            for f in r.key_failures.iter().take(5) {
                out.push_str(&format!("    - {f}\n"));
            }
        }
        if !r.artifacts.is_empty() {
            out.push_str(&format!("  artifacts: {}\n", r.artifacts.join(", ")));
        }
        if !r.tags.is_empty() {
            out.push_str(&format!("  tags: {}\n", r.tags.join(", ")));
        }
        if !r.defect_ids.is_empty() {
            out.push_str(&format!("  defects: {}\n", r.defect_ids.join(", ")));
        }
        let flags = missing_flags(root, r);
        if !flags.is_empty() {
            out.push_str(&format!("  warnings: {}\n", flags.join("; ")));
        }
    }
    if runs.is_empty() {
        out.push_str("\n(no matching runs)\n");
    }
    let store_tokens = std::fs::metadata(store_path(root))
        .map(|m| m.len() / 4)
        .unwrap_or(0);
    stats::record("run_history", store_tokens.max(64), (out.len() / 4) as u64);
    Ok(out)
}

/// Recent failed runs for session_pressure / handoff.
pub fn recent_failed(root: &Path, limit: usize) -> Vec<Run> {
    let mut runs = load_all(root).unwrap_or_default();
    runs.retain(|r| r.status == "failed");
    runs.sort_by_key(|r| std::cmp::Reverse(r.updated));
    runs.truncate(limit);
    runs
}

/// Recent runs of any status (newest first).
pub fn recent(root: &Path, limit: usize) -> Vec<Run> {
    let mut runs = load_all(root).unwrap_or_default();
    runs.sort_by_key(|r| std::cmp::Reverse(r.updated));
    runs.truncate(limit);
    runs
}

/// Look up one recorded run by id. Metadata only; never executes the command.
/// Retained for offline Izakaya outcome adapters (tests / future CLI).
#[allow(dead_code)]
pub fn get(root: &Path, id: &str) -> Option<Run> {
    load_all(root)
        .unwrap_or_default()
        .into_iter()
        .find(|r| r.id == id)
}

/// CLI entry: `wordkeep run-record --command ... --status ...` (metadata only).
pub fn cli_record(root: &Path, argv: &[String]) -> Result<(), i32> {
    let mut args = json!({});
    let mut i = 0usize;
    while i < argv.len() {
        let a = &argv[i];
        let key = match a.as_str() {
            "--id" => "id",
            "--command" | "-c" => "command",
            "--status" | "-s" => "status",
            "--exit-code" => "exit_code",
            "--duration-ms" => "duration_ms",
            "--summary" => "summary",
            "--log" => "log_path",
            "--tag" => "__tag",
            "--artifact" => "__artifact",
            "--defect" => "__defect",
            "--failure" => "__failure",
            "--help" | "-h" => {
                eprintln!(
                    "wordkeep run-record --command <cmd> [--status passed|failed|running|cancelled] \\\n\
                       [--id <id>] [--exit-code N] [--duration-ms N] [--summary <text>] \\\n\
                       [--log <path>] [--tag T] [--artifact P] [--defect ID] [--failure LINE]"
                );
                return Ok(());
            }
            other if other.starts_with('-') => {
                eprintln!("unknown flag {other}");
                return Err(2);
            }
            _ => {
                i += 1;
                continue;
            }
        };
        i += 1;
        let Some(val) = argv.get(i) else {
            eprintln!("missing value for {a}");
            return Err(2);
        };
        match key {
            "exit_code" => {
                let n: i64 = val.parse().map_err(|_| {
                    eprintln!("--exit-code must be an integer");
                    2
                })?;
                args["exit_code"] = json!(n);
            }
            "duration_ms" => {
                let n: u64 = val.parse().map_err(|_| {
                    eprintln!("--duration-ms must be an integer");
                    2
                })?;
                args["duration_ms"] = json!(n);
            }
            "__tag" => {
                let arr = args
                    .as_object_mut()
                    .unwrap()
                    .entry("tags")
                    .or_insert_with(|| json!([]));
                arr.as_array_mut().unwrap().push(json!(val));
            }
            "__artifact" => {
                let arr = args
                    .as_object_mut()
                    .unwrap()
                    .entry("artifacts")
                    .or_insert_with(|| json!([]));
                arr.as_array_mut().unwrap().push(json!(val));
            }
            "__defect" => {
                let arr = args
                    .as_object_mut()
                    .unwrap()
                    .entry("defect_ids")
                    .or_insert_with(|| json!([]));
                arr.as_array_mut().unwrap().push(json!(val));
            }
            "__failure" => {
                let arr = args
                    .as_object_mut()
                    .unwrap()
                    .entry("key_failures")
                    .or_insert_with(|| json!([]));
                arr.as_array_mut().unwrap().push(json!(val));
            }
            other => {
                args[other] = json!(val);
            }
        }
        i += 1;
    }
    match record(root, &args) {
        Ok(msg) => {
            println!("{msg}");
            Ok(())
        }
        Err(e) => {
            eprintln!("run-record error: {e}");
            Err(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn record_and_update() {
        let _guard = crate::cache::test_env_lock();
        let root = std::env::temp_dir().join(format!("wk_run_{}", std::process::id()));
        let cache = std::env::temp_dir().join(format!("wk_run_cache_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&cache);
        fs::create_dir_all(&root).unwrap();
        std::env::set_var("XDG_CACHE_HOME", &cache);
        record(
            &root,
            &json!({"id":"r1","command":"ctest","status":"running"}),
        )
        .unwrap();
        record(
            &root,
            &json!({"id":"r1","command":"ctest","status":"failed","exit_code":1,"summary":"boom"}),
        )
        .unwrap();
        let out = history(&root, &json!({"status":"failed"})).unwrap();
        assert!(out.contains("r1"));
        assert!(out.contains("boom"));
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&cache);
    }
}
