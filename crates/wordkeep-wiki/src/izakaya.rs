//! Read-only Izakaya board for the wiki dashboard.
//!
//! Reads the same coordination journal as `izakaya_status`. Does not lock, append,
//! or rebuild the projection — a lagging projection is reported, not repaired.

use crate::indexer::global_cache_dir;
use serde_json::{json, Value};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const ORPHAN_SECS: u64 = 86_400;
const MAX_EVENTS: usize = 24;
const MAX_FINDINGS: usize = 16;

pub(crate) fn board(root: &Path) -> Value {
    let now = now_secs();
    let id = coordination_id(root);
    let dir = global_cache_dir()
        .join("wordkeep")
        .join("coordination")
        .join(&id)
        .join("izakaya");
    let projection_path = dir.join("projection.json");
    let events_path = dir.join("events.ndjson");
    let Some(raw) = std::fs::read_to_string(&projection_path).ok() else {
        return json!({
            "available": true,
            "empty": true,
            "coordination_id": id,
            "seq": 0,
            "now": now,
            "message": "No Izakaya journal yet. Agents appear here after izakaya_check_in.",
            "counts": empty_counts(),
            "agents": [],
            "handoffs": [],
            "messages": [],
            "events": recent_events(&events_path),
            "findings": [],
            "active_policy": Value::Null,
            "malformed": 0,
            "projection_behind": false,
        });
    };
    let projection: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(error) => {
            return json!({
                "available": false,
                "coordination_id": id,
                "now": now,
                "message": format!("projection.json is not valid JSON: {error}"),
            });
        }
    };
    let seq = u64_field(&projection, "seq");
    let journal_seq = last_event_seq(&events_path).unwrap_or(seq);
    let agents = projection
        .get("agents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let agent_views: Vec<Value> = agents.iter().map(|agent| agent_view(agent, now)).collect();
    let handoffs = projection
        .get("handoffs")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|handoff| handoff_view(handoff, &agent_views, now))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let messages = projection
        .get("messages")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .rev()
                .take(16)
                .map(message_view)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut messages = messages;
    messages.reverse();

    json!({
        "available": true,
        "empty": agent_views.is_empty() && handoffs.is_empty(),
        "coordination_id": projection.get("coordination_id").and_then(Value::as_str).unwrap_or(&id),
        "seq": seq,
        "journal_seq": journal_seq,
        "updated_at": u64_field(&projection, "updated_at"),
        "now": now,
        "active_policy": projection.get("active_policy").cloned().unwrap_or(Value::Null),
        "malformed": u64_field(&projection, "malformed"),
        "projection_behind": journal_seq > seq,
        "counts": counts(&agent_views, &handoffs),
        "agents": agent_views,
        "handoffs": handoffs,
        "messages": messages,
        "events": recent_events(&events_path),
        "findings": findings(&agents),
    })
}

fn empty_counts() -> Value {
    json!({
        "live_code": 0,
        "checked_in": 0,
        "suspended": 0,
        "checked_out": 0,
        "stale": 0,
        "handoffs_open": 0,
    })
}

fn counts(agents: &[Value], handoffs: &[Value]) -> Value {
    let mut live_code = 0u64;
    let mut checked_in = 0u64;
    let mut suspended = 0u64;
    let mut checked_out = 0u64;
    let mut stale = 0u64;
    for agent in agents {
        if agent.get("stale").and_then(Value::as_bool) == Some(true) {
            stale += 1;
        }
        match agent.get("state").and_then(Value::as_str).unwrap_or("") {
            "live_code" => live_code += 1,
            "checked_in" => checked_in += 1,
            "suspended" => suspended += 1,
            "checked_out" => checked_out += 1,
            _ => {}
        }
    }
    let handoffs_open = handoffs
        .iter()
        .filter(|h| {
            matches!(
                h.get("derived_status").and_then(Value::as_str),
                Some("offered" | "orphaned")
            )
        })
        .count() as u64;
    json!({
        "live_code": live_code,
        "checked_in": checked_in,
        "suspended": suspended,
        "checked_out": checked_out,
        "stale": stale,
        "handoffs_open": handoffs_open,
    })
}

fn agent_view(agent: &Value, now: u64) -> Value {
    let state = agent
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("checked_in");
    let expires_at = u64_field(agent, "expires_at");
    let stale = state != "checked_out" && expires_at > 0 && expires_at <= now;
    let claims = agent
        .get("claims")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|claim| {
                    let path = claim.get("path").and_then(Value::as_str).unwrap_or("");
                    if path.is_empty()
                        && claim
                            .get("symbols")
                            .and_then(Value::as_array)
                            .map(|s| s.is_empty())
                            .unwrap_or(true)
                    {
                        return None;
                    }
                    Some(json!({
                        "path": path,
                        "symbols": claim.get("symbols").cloned().unwrap_or(json!([])),
                        "intent": claim.get("intent").and_then(Value::as_str).unwrap_or(""),
                    }))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let dirty = agent
        .get("dirty_paths")
        .and_then(Value::as_array)
        .map(|items| items.len())
        .unwrap_or(0);
    let head = agent
        .get("head_oid")
        .and_then(Value::as_str)
        .unwrap_or("");
    let head_short: String = head.chars().take(8).collect();
    json!({
        "agent_id": agent.get("agent_id").and_then(Value::as_str).unwrap_or(""),
        "state": state,
        "stale": stale,
        "role": agent.get("role").and_then(Value::as_str).unwrap_or(""),
        "task": agent.get("task").and_then(Value::as_str).unwrap_or(""),
        "summary": truncate(agent.get("summary").and_then(Value::as_str).unwrap_or(""), 240),
        "branch": agent.get("branch").and_then(Value::as_str).unwrap_or(""),
        "head": head_short,
        "worktree": worktree_label(agent.get("worktree").and_then(Value::as_str).unwrap_or("")),
        "dirty_count": dirty,
        "claims": claims,
        "blockers": agent.get("blockers").cloned().unwrap_or(json!([])),
        "checkpoint": agent.get("checkpoint").cloned().unwrap_or(Value::Null),
        "checkout_reason": agent.get("checkout_reason").cloned().unwrap_or(Value::Null),
        "mas_session": agent.get("mas_session").cloned().unwrap_or(Value::Null),
        "revision": u64_field(agent, "revision"),
        "checked_in_at": u64_field(agent, "checked_in_at"),
        "last_seen_at": u64_field(agent, "last_seen_at"),
        "expires_at": expires_at,
    })
}

fn handoff_view(handoff: &Value, agents: &[Value], now: u64) -> Value {
    let status = handoff
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("offered");
    let from = handoff.get("from").and_then(Value::as_str).unwrap_or("");
    let to = handoff.get("to").and_then(Value::as_str).unwrap_or("");
    let created_at = u64_field(handoff, "created_at");
    let from_gone = agents.iter().any(|agent| {
        agent.get("agent_id").and_then(Value::as_str) == Some(from)
            && agent.get("state").and_then(Value::as_str) == Some("checked_out")
    }) || !agents
        .iter()
        .any(|agent| agent.get("agent_id").and_then(Value::as_str) == Some(from));
    let derived = if status == "accepted" {
        "accepted"
    } else if status == "offered"
        && from_gone
        && created_at > 0
        && now.saturating_sub(created_at) >= ORPHAN_SECS
        && (to.is_empty()
            || agents.iter().any(|agent| {
                agent.get("agent_id").and_then(Value::as_str) == Some(to)
                    && agent.get("state").and_then(Value::as_str) == Some("checked_out")
            })
            || !agents
                .iter()
                .any(|agent| agent.get("agent_id").and_then(Value::as_str) == Some(to)))
    {
        "orphaned"
    } else {
        status
    };
    json!({
        "id": handoff.get("id").and_then(Value::as_str).unwrap_or(""),
        "from": from,
        "to": to,
        "status": status,
        "derived_status": derived,
        "summary": truncate(handoff.get("summary").and_then(Value::as_str).unwrap_or(""), 240),
        "mas_session": handoff.get("mas_session").cloned().unwrap_or(Value::Null),
        "checkpoint": handoff.get("checkpoint").cloned().unwrap_or(Value::Null),
        "created_at": created_at,
        "accepted_by": handoff.get("accepted_by").cloned().unwrap_or(Value::Null),
    })
}

fn message_view(message: &Value) -> Value {
    json!({
        "seq": u64_field(message, "seq"),
        "from": message.get("from").and_then(Value::as_str).unwrap_or(""),
        "to": message.get("to").and_then(Value::as_str).unwrap_or(""),
        "body": truncate(message.get("body").and_then(Value::as_str).unwrap_or(""), 240),
        "acked": message.get("acked").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn findings(agents: &[Value]) -> Vec<Value> {
    let active: Vec<&Value> = agents
        .iter()
        .filter(|agent| agent.get("state").and_then(Value::as_str) != Some("checked_out"))
        .collect();
    let mut out = Vec::new();
    for i in 0..active.len() {
        for j in (i + 1)..active.len() {
            if out.len() >= MAX_FINDINGS {
                return out;
            }
            let a = active[i];
            let b = active[j];
            let a_id = a.get("agent_id").and_then(Value::as_str).unwrap_or("?");
            let b_id = b.get("agent_id").and_then(Value::as_str).unwrap_or("?");
            for path_a in claim_paths(a) {
                for path_b in claim_paths(b) {
                    if paths_overlap(&path_a, &path_b) {
                        out.push(json!({
                            "kind": "path_overlap",
                            "detail": format!("{a_id} and {b_id} both claim {path_a}"),
                        }));
                    }
                }
            }
            for sym_a in claim_symbols(a) {
                for sym_b in claim_symbols(b) {
                    if sym_a == sym_b && !sym_a.is_empty() {
                        out.push(json!({
                            "kind": "symbol_overlap",
                            "detail": format!("{a_id} and {b_id} both claim {sym_a}"),
                        }));
                    }
                }
            }
        }
    }
    for agent in &active {
        if out.len() >= MAX_FINDINGS {
            break;
        }
        let id = agent.get("agent_id").and_then(Value::as_str).unwrap_or("?");
        let claims = claim_paths(agent);
        let dirty = agent
            .get("dirty_paths")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let uncovered = dirty
            .iter()
            .filter_map(Value::as_str)
            .filter(|path| !claims.iter().any(|claim| paths_overlap(claim, path)))
            .count();
        if uncovered > 0 {
            out.push(json!({
                "kind": "drift",
                "detail": format!("{id} has {uncovered} dirty path(s) outside declared claims"),
            }));
        }
    }
    out.truncate(MAX_FINDINGS);
    out
}

fn claim_paths(agent: &Value) -> Vec<String> {
    agent
        .get("claims")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|claim| claim.get("path").and_then(Value::as_str))
                .filter(|path| !path.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn claim_symbols(agent: &Value) -> Vec<String> {
    agent
        .get("claims")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .flat_map(|claim| {
                    claim
                        .get("symbols")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn recent_events(path: &Path) -> Vec<Value> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in raw.lines().rev() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        out.push(json!({
            "seq": u64_field(&value, "seq"),
            "kind": value.get("kind").and_then(Value::as_str).unwrap_or(""),
            "agent_id": value.get("agent_id").and_then(Value::as_str).unwrap_or(""),
            "ts": u64_field(&value, "ts"),
            "result": truncate(value.get("result").and_then(Value::as_str).unwrap_or(""), 180),
        }));
        if out.len() == MAX_EVENTS {
            break;
        }
    }
    out.reverse();
    out
}

fn last_event_seq(path: &Path) -> Option<u64> {
    let raw = std::fs::read_to_string(path).ok()?;
    raw.lines().rev().find_map(|line| {
        let value: Value = serde_json::from_str(line).ok()?;
        Some(u64_field(&value, "seq")).filter(|seq| *seq > 0)
    })
}

fn paths_overlap(a: &str, b: &str) -> bool {
    let a = a.trim().trim_matches('/');
    let b = b.trim().trim_matches('/');
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    long.starts_with(short) && long.as_bytes().get(short.len()) == Some(&b'/')
}

fn coordination_id(root: &Path) -> String {
    if let Some(common) = git_common_dir(root) {
        let key = common.to_string_lossy().replace('\\', "/").to_lowercase();
        hash_id(&key)
    } else {
        let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let normalized = canonical
            .to_string_lossy()
            .replace('\\', "/")
            .to_lowercase();
        hash_id(&normalized)
    }
}

fn git_common_dir(root: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        return None;
    }
    let path = PathBuf::from(&text);
    let abs = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    Some(abs.canonicalize().unwrap_or(abs))
}

fn hash_id(key: &str) -> String {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn u64_field(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn worktree_label(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::paths_overlap;

    #[test]
    fn path_prefix_overlap_matches_izakaya_claims() {
        assert!(paths_overlap("src/a", "src/a/b.rs"));
        assert!(!paths_overlap("src/a", "src/ab"));
        assert!(!paths_overlap("", "src"));
        assert!(paths_overlap("src/a/", "/src/a"));
    }
}
