//! Izakaya identity, lifecycle, and advisory-claim rules.
//!
//! Persisted states are `checked_in`, `live_code`, `suspended`, and `checked_out`.
//! `stale` is derived from lease expiry and is never stored or auto-promoted to
//! checkout. Claims warn; they never block edits.

use serde_json::Value;

pub const SCHEMA_VERSION: u64 = 1;
pub const MAX_SUMMARY: usize = 2_000;
pub const MAX_CLAIMS: usize = 64;
pub const MAX_SYMBOLS: usize = 32;
pub const MAX_PATHS: usize = 64;
pub const MAX_LIST: usize = 16;
pub const MAX_ITEM: usize = 240;
pub const MIN_SUPPORT: f64 = 0.7;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState {
    CheckedIn,
    LiveCode,
    Suspended,
    CheckedOut,
}

impl AgentState {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentState::CheckedIn => "checked_in",
            AgentState::LiveCode => "live_code",
            AgentState::Suspended => "suspended",
            AgentState::CheckedOut => "checked_out",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim() {
            "checked_in" => Ok(AgentState::CheckedIn),
            "live_code" => Ok(AgentState::LiveCode),
            "suspended" => Ok(AgentState::Suspended),
            "checked_out" => Ok(AgentState::CheckedOut),
            other => Err(format!(
                "unknown agent state {other:?}; expected checked_in|live_code|suspended|checked_out"
            )),
        }
    }
}

/// Live agents may heartbeat or move among the non-terminal states.
/// Checkout is terminal for an incarnation; a later check-in mints a new lease.
pub fn transition(from: AgentState, to: AgentState) -> Result<(), String> {
    if from == AgentState::CheckedOut && to != AgentState::CheckedIn {
        return Err("checked-out agent must check in before other transitions".into());
    }
    match to {
        AgentState::CheckedIn
        | AgentState::LiveCode
        | AgentState::Suspended
        | AgentState::CheckedOut => Ok(()),
    }
}

pub fn is_stale(state: AgentState, expires_at: u64, now: u64) -> bool {
    state != AgentState::CheckedOut && expires_at > 0 && now >= expires_at
}

pub fn validate_slug(kind: &str, raw: &str) -> Result<String, String> {
    let s = raw.trim();
    if s.is_empty() {
        return Err(format!("{kind} is required"));
    }
    if s.len() > 80 || s.contains('/') || s.contains('\\') || s.contains("..") {
        return Err(format!("{kind} must be a slug (no /, \\, or ..)"));
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    {
        return Err(format!("{kind} must match [A-Za-z0-9._-]"));
    }
    Ok(s.to_string())
}

pub fn bound(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.len() <= max {
        t.to_string()
    } else {
        let mut end = max;
        while !t.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        t[..end].to_string()
    }
}

pub fn bound_strings(
    items: impl IntoIterator<Item = impl AsRef<str>>,
    max_n: usize,
    max_len: usize,
) -> Vec<String> {
    items
        .into_iter()
        .filter_map(|s| {
            let t = bound(s.as_ref(), max_len);
            if t.is_empty() {
                None
            } else {
                Some(t)
            }
        })
        .take(max_n)
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    pub path: String,
    pub symbols: Vec<String>,
    pub intent: String,
}

impl Claim {
    pub fn to_value(&self) -> Value {
        serde_json::json!({
            "path": self.path,
            "symbols": self.symbols,
            "intent": self.intent,
        })
    }

    pub fn from_value(v: &Value) -> Option<Self> {
        let path = v.get("path").and_then(Value::as_str)?.trim().to_string();
        if path.is_empty() {
            return None;
        }
        let symbols = v
            .get("symbols")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(|s| bound(s, MAX_ITEM))
                    .filter(|s| !s.is_empty())
                    .take(MAX_SYMBOLS)
                    .collect()
            })
            .unwrap_or_default();
        let intent = v.get("intent").and_then(Value::as_str).unwrap_or("edit");
        let intent = if intent == "read" { "read" } else { "edit" };
        Some(Claim {
            path: bound(&path, MAX_ITEM),
            symbols,
            intent: intent.to_string(),
        })
    }
}

/// Prefix overlap on path boundaries. `src/a` overlaps `src/a/b.rs`, not `src/ab`.
pub fn paths_overlap(a: &str, b: &str) -> bool {
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

pub fn path_covers(claim: &str, dirty: &str) -> bool {
    paths_overlap(claim, dirty) || claim.trim_matches('/') == dirty.trim_matches('/')
}

#[derive(Clone, Debug)]
pub struct AgentView<'a> {
    pub agent_id: &'a str,
    pub state: AgentState,
    pub expires_at: u64,
    pub base_oid: &'a str,
    pub claims: &'a [Claim],
    pub dirty_paths: &'a [String],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub kind: String,
    pub agents: Vec<String>,
    pub detail: String,
    pub stale: bool,
}

pub fn advisory_findings(agents: &[AgentView<'_>], now: u64) -> Vec<Finding> {
    let live: Vec<_> = agents
        .iter()
        .filter(|a| a.state != AgentState::CheckedOut)
        .collect();
    let mut out = Vec::new();
    for a in &live {
        if is_stale(a.state, a.expires_at, now) {
            out.push(Finding {
                kind: "stale".into(),
                agents: vec![a.agent_id.to_string()],
                detail: format!("{} lease expired; work was not checked out", a.agent_id),
                stale: true,
            });
        }
        for dirty in a.dirty_paths {
            let covered = a.claims.iter().any(|c| path_covers(&c.path, dirty));
            if !covered {
                out.push(Finding {
                    kind: "drift".into(),
                    agents: vec![a.agent_id.to_string()],
                    detail: format!(
                        "{} dirty path {dirty} is outside declared claims",
                        a.agent_id
                    ),
                    stale: is_stale(a.state, a.expires_at, now),
                });
            }
        }
    }
    for i in 0..live.len() {
        for j in (i + 1)..live.len() {
            let a = live[i];
            let b = live[j];
            let stale =
                is_stale(a.state, a.expires_at, now) || is_stale(b.state, b.expires_at, now);
            for ca in a.claims {
                for cb in b.claims {
                    if paths_overlap(&ca.path, &cb.path) {
                        out.push(Finding {
                            kind: "path_overlap".into(),
                            agents: vec![a.agent_id.to_string(), b.agent_id.to_string()],
                            detail: format!(
                                "{}:{} overlaps {}:{}",
                                a.agent_id, ca.path, b.agent_id, cb.path
                            ),
                            stale,
                        });
                    }
                    for sa in &ca.symbols {
                        if cb.symbols.iter().any(|sb| sb == sa) {
                            out.push(Finding {
                                kind: "symbol_overlap".into(),
                                agents: vec![a.agent_id.to_string(), b.agent_id.to_string()],
                                detail: format!(
                                    "{} and {} both claim symbol {sa}",
                                    a.agent_id, b.agent_id
                                ),
                                stale,
                            });
                        }
                    }
                }
            }
            if !a.base_oid.is_empty() && !b.base_oid.is_empty() && a.base_oid != b.base_oid {
                out.push(Finding {
                    kind: "base_divergence".into(),
                    agents: vec![a.agent_id.to_string(), b.agent_id.to_string()],
                    detail: format!(
                        "{} base {} != {} base {}",
                        a.agent_id, a.base_oid, b.agent_id, b.base_oid
                    ),
                    stale,
                });
            }
        }
    }
    out.sort_by(|x, y| x.kind.cmp(&y.kind).then(x.detail.cmp(&y.detail)));
    out.dedup();
    out
}

/// Reject accidental secret material and source bodies. Top-level only for source keys.
pub fn reject_unsafe_payload(v: &Value) -> Result<(), String> {
    if let Some(obj) = v.as_object() {
        for key in ["patch", "diff", "source", "file_contents", "contents"] {
            if obj.contains_key(key) {
                return Err(format!(
                    "refusing to store {key}; Izakaya keeps references, not source bodies"
                ));
            }
        }
    }
    fn walk(v: &Value) -> Result<(), String> {
        match v {
            Value::String(s) if s.contains("-----BEGIN ") => {
                Err("refusing to store secret-looking material".into())
            }
            Value::Array(items) => {
                for item in items {
                    walk(item)?;
                }
                Ok(())
            }
            Value::Object(map) => {
                for item in map.values() {
                    walk(item)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    walk(v)
}

pub fn checkout_reason(raw: &str) -> Result<&'static str, String> {
    match raw.trim() {
        "completed" => Ok("completed"),
        "handed_off" => Ok("handed_off"),
        "abandoned" => Ok("abandoned"),
        other => Err(format!(
            "unknown checkout reason {other:?}; expected completed|handed_off|abandoned"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_is_derived_and_checkout_is_not_stale() {
        assert!(is_stale(AgentState::LiveCode, 10, 10));
        assert!(!is_stale(AgentState::LiveCode, 10, 9));
        assert!(!is_stale(AgentState::CheckedOut, 1, 100));
    }

    #[test]
    fn path_overlap_respects_boundaries() {
        assert!(paths_overlap("src/a", "src/a/b.rs"));
        assert!(!paths_overlap("src/a", "src/ab"));
        assert!(!paths_overlap("", "src"));
    }

    #[test]
    fn findings_are_advisory_and_flag_drift() {
        let claims = vec![Claim {
            path: "src/ok.rs".into(),
            symbols: vec!["foo".into()],
            intent: "edit".into(),
        }];
        let dirty = vec!["src/other.rs".into()];
        let agents = vec![AgentView {
            agent_id: "a",
            state: AgentState::LiveCode,
            expires_at: 50,
            base_oid: "abc",
            claims: &claims,
            dirty_paths: &dirty,
        }];
        let found = advisory_findings(&agents, 10);
        assert!(found.iter().any(|f| f.kind == "drift"));
        assert!(found.iter().all(|f| f.kind != "blocked"));
    }

    #[test]
    fn slug_rejects_traversal() {
        assert!(validate_slug("agent_id", "../x").is_err());
        assert_eq!(validate_slug("agent_id", "solver-1").unwrap(), "solver-1");
    }
}
