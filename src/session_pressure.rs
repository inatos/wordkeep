//! Session context-pressure heuristic.
//!
//! Reports a documented proxy (`low|medium|high|critical`) from workspace-scoped
//! tool events since the last durable handoff, error/truncation rate, MAS open
//! questions / round budget, unresolved defects, and failed runs.
//!
//! Host turn count and true context-window usage are **unavailable** — this is
//! an in-process heuristic only.

use serde_json::Value;
use std::path::Path;

use crate::{defects, mas, runs, stats, workspace};

const HANDOFF_MARKER: &str = "last_handoff.json";

/// Record that a durable handoff was produced (resets the pressure window).
pub fn mark_handoff(root: &Path) -> Result<(), String> {
    let doc = serde_json::json!({
        "ts": workspace::now_secs(),
        "workspace_id": workspace::workspace_id(root),
    });
    workspace::ensure_workspace_dir(root)?;
    workspace::write_atomic_json(&workspace::workspace_file(root, HANDOFF_MARKER), &doc)
}

fn last_handoff_ts(root: &Path) -> Option<u64> {
    let v = workspace::read_json(&workspace::workspace_file(root, HANDOFF_MARKER))
        .ok()
        .flatten()?;
    v.get("ts").and_then(Value::as_u64)
}

fn level_from_score(score: u32) -> &'static str {
    match score {
        0..=2 => "low",
        3..=5 => "medium",
        6..=8 => "high",
        _ => "critical",
    }
}

/// Report session pressure for the current workspace.
pub fn report(root: &Path, args: &Value) -> Result<String, String> {
    let session = args
        .get("session")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let since = last_handoff_ts(root).unwrap_or(0);
    let events = stats::events_since(root, since);
    let total = events.len() as u64;
    let errors = events.iter().filter(|e| e.outcome == "error").count() as u64;
    let truncs = events.iter().filter(|e| e.outcome == "truncated").count() as u64;
    let err_rate = errors
        .checked_mul(100)
        .and_then(|n| n.checked_div(total))
        .unwrap_or(0);
    let trunc_rate = truncs
        .checked_mul(100)
        .and_then(|n| n.checked_div(total))
        .unwrap_or(0);

    let unresolved = defects::unresolved(root);
    let eyeball = unresolved
        .iter()
        .filter(|d| d.status == "eyeball_fail")
        .count();
    let failed_runs = runs::recent_failed(root, 10);

    let mut score = 0u32;
    let mut factors = Vec::new();

    if total >= 80 {
        score += 3;
        factors.push(format!("many tool events since handoff ({total})"));
    } else if total >= 40 {
        score += 2;
        factors.push(format!("elevated tool events since handoff ({total})"));
    } else if total >= 20 {
        score += 1;
        factors.push(format!("moderate tool events since handoff ({total})"));
    }

    if err_rate >= 20 {
        score += 2;
        factors.push(format!("high error rate ({err_rate}%)"));
    } else if err_rate >= 10 {
        score += 1;
        factors.push(format!("elevated error rate ({err_rate}%)"));
    }
    if trunc_rate >= 20 {
        score += 1;
        factors.push(format!("high truncation rate ({trunc_rate}%)"));
    }

    if eyeball > 0 {
        score += 3;
        factors.push(format!("{eyeball} eyeball_fail defect(s)"));
    } else if !unresolved.is_empty() {
        score += 1;
        factors.push(format!("{} unresolved defect(s)", unresolved.len()));
    }

    if failed_runs.len() >= 3 {
        score += 2;
        factors.push(format!("{} recent failed runs", failed_runs.len()));
    } else if !failed_runs.is_empty() {
        score += 1;
        factors.push(format!("{} recent failed run(s)", failed_runs.len()));
    }

    if let Some(s) = session {
        if let Ok(Some(snap)) = mas::session_snapshot(root, s) {
            if snap.open_questions > 5 {
                score += 2;
                factors.push(format!(
                    "MAS session {s}: {} open questions",
                    snap.open_questions
                ));
            } else if snap.open_questions > 0 {
                score += 1;
                factors.push(format!(
                    "MAS session {s}: {} open question(s)",
                    snap.open_questions
                ));
            }
            if snap.rounds_remaining == 0 && snap.status != "final" {
                score += 2;
                factors.push(format!("MAS session {s}: round budget exhausted"));
            } else if snap.rounds_remaining == 1 {
                score += 1;
                factors.push(format!("MAS session {s}: 1 round remaining"));
            }
        }
    }

    let level = level_from_score(score);
    let mut out = format!(
        "session_pressure - level={level} score={score}\n\
         NOTE: host turn count / context-window usage is unavailable; \
         this is a workspace heuristic only.\n\
         events_since_handoff: {total} (errors={errors}/{err_rate}%, trunc={truncs}/{trunc_rate}%)\n\
         last_handoff_ts: {}\n\
         unresolved_defects: {} (eyeball_fail={eyeball})\n\
         recent_failed_runs: {}\n",
        if since == 0 {
            "never".into()
        } else {
            since.to_string()
        },
        unresolved.len(),
        failed_runs.len(),
    );
    if factors.is_empty() {
        out.push_str("factors: (none — pressure low)\n");
    } else {
        out.push_str("factors:\n");
        for f in &factors {
            out.push_str(&format!("  • {f}\n"));
        }
    }
    match level {
        "high" | "critical" => out.push_str(
            "advice: call session_handoff (or mas_finalize with promote) before continuing.\n",
        ),
        "medium" => out
            .push_str("advice: consider session_handoff soon if more exploratory work remains.\n"),
        _ => out.push_str("advice: continue; pressure is within normal bounds.\n"),
    }
    stats::record("session_pressure", 128, (out.len() / 4) as u64);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    #[test]
    fn level_thresholds() {
        assert_eq!(level_from_score(0), "low");
        assert_eq!(level_from_score(4), "medium");
        assert_eq!(level_from_score(7), "high");
        assert_eq!(level_from_score(12), "critical");
    }

    #[test]
    fn events_since_reads_beyond_ring_for_pressure() {
        let _guard = crate::cache::test_env_lock();
        let pid = std::process::id();
        let cache_home = std::env::temp_dir().join(format!("wk_pressure_cache_{pid}"));
        let root = std::env::temp_dir().join(format!("wk_pressure_root_{pid}"));
        let _ = std::fs::remove_dir_all(&cache_home);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("XDG_CACHE_HOME", &cache_home);

        let dir = workspace::ensure_workspace_dir(&root).unwrap();
        let wid = workspace::workspace_id(&root);
        let mut f = std::fs::File::create(dir.join("events.jsonl")).unwrap();
        for i in 1..=220u64 {
            writeln!(
                f,
                "{}",
                json!({
                    "ts": i,
                    "tool": "outline",
                    "baseline": 1,
                    "returned": 1,
                    "elapsed_us": 100,
                    "outcome": if i % 10 == 0 { "error" } else { "ok" },
                    "workspace_id": wid,
                    "bytes_read": 0,
                    "cache_hits": 0,
                    "cache_misses": 0,
                    "wordkeep_version": "0.2.0",
                })
            )
            .unwrap();
        }
        drop(f);

        let events = stats::events_since(&root, 0);
        assert!(
            events.len() > 200,
            "session_pressure needs jsonl history beyond the 200 ring; got {}",
            events.len()
        );

        let _ = std::fs::remove_dir_all(&cache_home);
        let _ = std::fs::remove_dir_all(&root);
    }
}
