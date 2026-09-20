//! Locked append-only Izakaya journal and rebuildable projection.
//!
//! Linked Git worktrees share one coordination group via `git rev-parse --git-common-dir`.
//! The journal is the source of truth. `projection.json` is a cache and is rebuilt when
//! its sequence does not match the last valid event.

use serde_json::{json, Value};
use std::collections::hash_map::DefaultHasher;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::model::{self, AgentState, Claim, SCHEMA_VERSION};
use crate::workspace;

const MAX_IDEMPOTENCY: usize = 256;
const MAX_MESSAGES: usize = 64;
const MAX_HANDOFFS: usize = 256;

#[derive(Clone, Debug)]
pub struct Agent {
    pub agent_id: String,
    pub lease_id: String,
    pub revision: u64,
    pub state: String,
    pub role: String,
    pub task: String,
    pub summary: String,
    pub mas_session: Option<String>,
    pub checked_in_at: u64,
    pub last_seen_at: u64,
    pub expires_at: u64,
    pub ttl_secs: u64,
    pub workspace_id: String,
    pub worktree: String,
    pub branch: String,
    pub base_oid: String,
    pub head_oid: String,
    pub dirty_paths: Vec<String>,
    pub claims: Vec<Claim>,
    pub blockers: Vec<String>,
    pub checkpoint: Option<String>,
    pub checkout_reason: Option<String>,
}

impl Agent {
    fn to_value(&self) -> Value {
        json!({
            "agent_id": self.agent_id,
            "lease_id": self.lease_id,
            "revision": self.revision,
            "state": self.state,
            "role": self.role,
            "task": self.task,
            "summary": self.summary,
            "mas_session": self.mas_session,
            "checked_in_at": self.checked_in_at,
            "last_seen_at": self.last_seen_at,
            "expires_at": self.expires_at,
            "ttl_secs": self.ttl_secs,
            "workspace_id": self.workspace_id,
            "worktree": self.worktree,
            "branch": self.branch,
            "base_oid": self.base_oid,
            "head_oid": self.head_oid,
            "dirty_paths": self.dirty_paths,
            "claims": self.claims.iter().map(Claim::to_value).collect::<Vec<_>>(),
            "blockers": self.blockers,
            "checkpoint": self.checkpoint,
            "checkout_reason": self.checkout_reason,
        })
    }

    fn from_value(v: &Value) -> Option<Self> {
        Some(Agent {
            agent_id: v.get("agent_id").and_then(Value::as_str)?.to_string(),
            lease_id: str_or(v, "lease_id"),
            revision: u(v, "revision"),
            state: str_or(v, "state"),
            role: str_or(v, "role"),
            task: str_or(v, "task"),
            summary: str_or(v, "summary"),
            mas_session: opt_str(v, "mas_session"),
            checked_in_at: u(v, "checked_in_at"),
            last_seen_at: u(v, "last_seen_at"),
            expires_at: u(v, "expires_at"),
            ttl_secs: u(v, "ttl_secs"),
            workspace_id: str_or(v, "workspace_id"),
            worktree: str_or(v, "worktree"),
            branch: str_or(v, "branch"),
            base_oid: str_or(v, "base_oid"),
            head_oid: str_or(v, "head_oid"),
            dirty_paths: strs(v, "dirty_paths"),
            claims: v
                .get("claims")
                .and_then(Value::as_array)
                .map(|arr| arr.iter().filter_map(Claim::from_value).collect())
                .unwrap_or_default(),
            blockers: strs(v, "blockers"),
            checkpoint: opt_str(v, "checkpoint"),
            checkout_reason: opt_str(v, "checkout_reason"),
        })
    }
}

#[derive(Clone, Debug)]
pub struct Handoff {
    pub id: String,
    pub from: String,
    pub to: String,
    pub status: String,
    pub summary: String,
    pub mas_session: Option<String>,
    pub mas_entry_id: Option<u64>,
    pub note_ref: Option<String>,
    pub run_ids: Vec<String>,
    pub artifacts: Vec<String>,
    pub checkpoint: Option<String>,
    pub anchors: Vec<String>,
    pub commands: Vec<String>,
    pub constraints: Vec<String>,
    pub created_seq: u64,
    pub created_at: u64,
    pub accepted_by: Option<String>,
}

impl Handoff {
    pub fn to_value(&self) -> Value {
        json!({
            "id": self.id,
            "from": self.from,
            "to": self.to,
            "status": self.status,
            "summary": self.summary,
            "mas_session": self.mas_session,
            "mas_entry_id": self.mas_entry_id,
            "note_ref": self.note_ref,
            "run_ids": self.run_ids,
            "artifacts": self.artifacts,
            "checkpoint": self.checkpoint,
            "anchors": self.anchors,
            "commands": self.commands,
            "constraints": self.constraints,
            "created_seq": self.created_seq,
            "created_at": self.created_at,
            "accepted_by": self.accepted_by,
        })
    }

    fn from_value(v: &Value) -> Option<Self> {
        Some(Handoff {
            id: v.get("id").and_then(Value::as_str)?.to_string(),
            from: str_or(v, "from"),
            to: str_or(v, "to"),
            status: {
                let s = str_or(v, "status");
                if s.is_empty() {
                    "offered".into()
                } else {
                    s
                }
            },
            summary: str_or(v, "summary"),
            mas_session: opt_str(v, "mas_session"),
            mas_entry_id: v.get("mas_entry_id").and_then(Value::as_u64),
            note_ref: opt_str(v, "note_ref"),
            run_ids: strs(v, "run_ids"),
            artifacts: strs(v, "artifacts"),
            checkpoint: opt_str(v, "checkpoint"),
            anchors: strs(v, "anchors"),
            commands: strs(v, "commands"),
            constraints: strs(v, "constraints"),
            created_seq: u(v, "created_seq"),
            created_at: u(v, "created_at"),
            accepted_by: opt_str(v, "accepted_by"),
        })
    }
}

#[derive(Clone, Debug)]
pub struct Note {
    pub seq: u64,
    pub from: String,
    pub to: String,
    pub body: String,
    pub acked: bool,
}

#[derive(Clone, Debug)]
pub struct PolicyRec {
    pub id: String,
    pub status: String,
    pub support: f64,
    pub report_hash: String,
    pub profile: String,
    pub spec: Value,
}

#[derive(Clone, Debug)]
struct IdemHit {
    seq: u64,
    result: String,
}

#[derive(Clone, Debug)]
pub struct Projection {
    pub version: u64,
    pub coordination_id: String,
    pub seq: u64,
    pub updated_at: u64,
    pub agents: BTreeMap<String, Agent>,
    pub handoffs: Vec<Handoff>,
    pub messages: Vec<Note>,
    pub policies: Vec<PolicyRec>,
    pub active_policy: Option<String>,
    pub malformed: u64,
    idempotency: BTreeMap<String, IdemHit>,
}

impl Projection {
    pub fn empty(coordination_id: String) -> Self {
        Projection {
            version: SCHEMA_VERSION,
            coordination_id,
            seq: 0,
            updated_at: 0,
            agents: BTreeMap::new(),
            handoffs: Vec::new(),
            messages: Vec::new(),
            policies: Vec::new(),
            active_policy: None,
            malformed: 0,
            idempotency: BTreeMap::new(),
        }
    }

    pub fn to_value(&self) -> Value {
        json!({
            "version": self.version,
            "coordination_id": self.coordination_id,
            "seq": self.seq,
            "updated_at": self.updated_at,
            "agents": self.agents.values().map(Agent::to_value).collect::<Vec<_>>(),
            "handoffs": self.handoffs.iter().map(Handoff::to_value).collect::<Vec<_>>(),
            "messages": self.messages.iter().map(|m| json!({
                "seq": m.seq, "from": m.from, "to": m.to, "body": m.body, "acked": m.acked
            })).collect::<Vec<_>>(),
            "policies": self.policies.iter().map(|p| json!({
                "id": p.id,
                "status": p.status,
                "support": p.support,
                "report_hash": p.report_hash,
                "profile": p.profile,
                "spec": p.spec,
            })).collect::<Vec<_>>(),
            "active_policy": self.active_policy,
            "malformed": self.malformed,
            "idempotency": self.idempotency.iter().map(|(k, h)| json!({
                "key": k, "seq": h.seq, "result": h.result
            })).collect::<Vec<_>>(),
        })
    }

    pub fn from_value(v: &Value) -> Option<Self> {
        let version = v.get("version").and_then(Value::as_u64).unwrap_or(0);
        if version == 0 || version > SCHEMA_VERSION {
            return None;
        }
        let mut agents = BTreeMap::new();
        for item in v
            .get("agents")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(agent) = Agent::from_value(item) {
                agents.insert(agent.agent_id.clone(), agent);
            }
        }
        let handoffs = v
            .get("handoffs")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(Handoff::from_value).collect())
            .unwrap_or_default();
        let messages = v
            .get("messages")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| {
                        Some(Note {
                            seq: u(m, "seq"),
                            from: str_or(m, "from"),
                            to: str_or(m, "to"),
                            body: str_or(m, "body"),
                            acked: m.get("acked").and_then(Value::as_bool).unwrap_or(false),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let policies = v
            .get("policies")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|p| {
                        Some(PolicyRec {
                            id: p.get("id").and_then(Value::as_str)?.to_string(),
                            status: str_or(p, "status"),
                            support: p.get("support").and_then(Value::as_f64).unwrap_or(0.0),
                            report_hash: str_or(p, "report_hash"),
                            profile: str_or(p, "profile"),
                            spec: p.get("spec").cloned().unwrap_or(Value::Null),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut idempotency = BTreeMap::new();
        if let Some(arr) = v.get("idempotency").and_then(Value::as_array) {
            for item in arr {
                if let Some(key) = item.get("key").and_then(Value::as_str) {
                    idempotency.insert(
                        key.to_string(),
                        IdemHit {
                            seq: u(item, "seq"),
                            result: str_or(item, "result"),
                        },
                    );
                }
            }
        }
        Some(Projection {
            version,
            coordination_id: str_or(v, "coordination_id"),
            seq: u(v, "seq"),
            updated_at: u(v, "updated_at"),
            agents,
            handoffs,
            messages,
            policies,
            active_policy: opt_str(v, "active_policy"),
            malformed: u(v, "malformed"),
            idempotency,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Event {
    pub v: u64,
    pub seq: u64,
    pub id: String,
    pub idempotency_key: Option<String>,
    pub ts: u64,
    pub kind: String,
    pub agent_id: String,
    pub lease_id: String,
    pub result: String,
    pub body: Value,
}

impl Event {
    pub fn to_value(&self) -> Value {
        json!({
            "v": self.v,
            "seq": self.seq,
            "id": self.id,
            "idempotency_key": self.idempotency_key,
            "ts": self.ts,
            "kind": self.kind,
            "agent_id": self.agent_id,
            "lease_id": self.lease_id,
            "result": self.result,
            "body": self.body,
        })
    }

    pub fn from_value(v: &Value) -> Result<Self, String> {
        let version = v.get("v").and_then(Value::as_u64).unwrap_or(SCHEMA_VERSION);
        if version > SCHEMA_VERSION {
            return Err(format!("unsupported event version {version}"));
        }
        let kind = v
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("malformed")
            .to_string();
        Ok(Event {
            v: version,
            seq: u(v, "seq"),
            id: str_or(v, "id"),
            idempotency_key: opt_str(v, "idempotency_key"),
            ts: u(v, "ts"),
            kind,
            agent_id: str_or(v, "agent_id"),
            lease_id: str_or(v, "lease_id"),
            result: str_or(v, "result"),
            body: v.get("body").cloned().unwrap_or(Value::Null),
        })
    }
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub projection: Projection,
    pub events: Vec<Event>,
}

pub struct GitSnapshot {
    pub branch: String,
    pub head_oid: String,
    pub base_oid: String,
    pub dirty_paths: Vec<String>,
    pub worktree: String,
}

pub fn now_secs() -> u64 {
    if let Ok(v) = std::env::var("WORDKEEP_IZAKAYA_NOW") {
        if let Ok(n) = v.parse() {
            return n;
        }
    }
    workspace::now_secs()
}

pub fn hash_id(key: &str) -> String {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

pub fn git_common_dir(root: &Path) -> Option<PathBuf> {
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

pub fn coordination_id(root: &Path) -> String {
    if let Some(common) = git_common_dir(root) {
        let key = common.to_string_lossy().replace('\\', "/").to_lowercase();
        hash_id(&key)
    } else {
        workspace::workspace_id(root)
    }
}

pub fn git_snapshot(root: &Path) -> GitSnapshot {
    let worktree = root
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/");
    GitSnapshot {
        branch: git_line(root, &["rev-parse", "--abbrev-ref", "HEAD"]),
        head_oid: git_line(root, &["rev-parse", "HEAD"]),
        base_oid: {
            let upstream = git_line(root, &["merge-base", "HEAD", "@{upstream}"]);
            if upstream.is_empty() {
                git_line(root, &["rev-parse", "HEAD^"])
            } else {
                upstream
            }
        },
        dirty_paths: git_dirty(root),
        worktree,
    }
}

fn git_line(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(root).args(args).output();
    match out {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => String::new(),
    }
}

fn git_dirty(root: &Path) -> Vec<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain", "-z"])
        .output();
    let Ok(out) = out else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    out.stdout
        .split(|b| *b == 0)
        .filter_map(|entry| {
            if entry.len() < 4 {
                return None;
            }
            let text = String::from_utf8_lossy(&entry[3..]).trim().to_string();
            let path = text.split(" -> ").last().unwrap_or("").trim();
            if path.is_empty() {
                None
            } else {
                Some(model::bound(path, model::MAX_ITEM))
            }
        })
        .take(model::MAX_PATHS)
        .collect()
}

fn group_dir(root: &Path) -> PathBuf {
    crate::cache::dir()
        .join("coordination")
        .join(coordination_id(root))
        .join("izakaya")
}

struct GroupLock {
    _file: std::fs::File,
}

impl GroupLock {
    fn acquire(dir: &Path) -> Result<Self, String> {
        use fs2::FileExt;
        std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
        let path = dir.join("izakaya.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| format!("open lock {}: {e}", path.display()))?;
        file.lock_exclusive()
            .map_err(|e| format!("lock {}: {e}", path.display()))?;
        Ok(GroupLock { _file: file })
    }
}

fn events_path(dir: &Path) -> PathBuf {
    dir.join("events.ndjson")
}

fn projection_path(dir: &Path) -> PathBuf {
    dir.join("projection.json")
}

pub fn load_events(dir: &Path) -> Result<Vec<Event>, String> {
    let path = events_path(dir);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let text = String::from_utf8_lossy(&raw);
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let mut events = Vec::new();
    let mut drop_tail = false;
    for (i, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(v) => match Event::from_value(&v) {
                Ok(event) => events.push(event),
                Err(_) if i + 1 == lines.len() => drop_tail = true,
                Err(_) => events.push(malformed_event(i as u64 + 1)),
            },
            Err(_) if i + 1 == lines.len() => drop_tail = true,
            Err(_) => events.push(malformed_event(i as u64 + 1)),
        }
    }
    if drop_tail {
        let kept: Vec<&str> = lines[..lines.len().saturating_sub(1)]
            .iter()
            .copied()
            .filter(|l| !l.is_empty())
            .collect();
        let mut out = kept.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        std::fs::write(&path, out).map_err(|e| format!("repair {}: {e}", path.display()))?;
    }
    Ok(events)
}

fn malformed_event(n: u64) -> Event {
    Event {
        v: SCHEMA_VERSION,
        seq: 0,
        id: format!("malformed-{n}"),
        idempotency_key: None,
        ts: 0,
        kind: "malformed".into(),
        agent_id: String::new(),
        lease_id: String::new(),
        result: String::new(),
        body: Value::Null,
    }
}

fn fold(events: &[Event], coordination_id: &str) -> Projection {
    let mut proj = Projection::empty(coordination_id.to_string());
    for event in events {
        apply(&mut proj, event);
    }
    proj
}

pub fn apply(proj: &mut Projection, event: &Event) {
    if event.kind == "malformed" {
        proj.malformed += 1;
        return;
    }
    if event.seq > 0 {
        proj.seq = event.seq;
    }
    proj.updated_at = event.ts;
    match event.kind.as_str() {
        "check_in" => apply_check_in(proj, event),
        "update" => apply_update(proj, event),
        "check_out" => apply_check_out(proj, event),
        "policy_promote" => apply_promote(proj, event),
        "policy_retire" => apply_retire(proj, event),
        "decision" | "outcome" => {}
        _ => {
            proj.malformed += 1;
        }
    }
    if let Some(key) = &event.idempotency_key {
        proj.idempotency.insert(
            key.clone(),
            IdemHit {
                seq: event.seq,
                result: event.result.clone(),
            },
        );
        while proj.idempotency.len() > MAX_IDEMPOTENCY {
            if let Some(old) = proj
                .idempotency
                .iter()
                .min_by_key(|(_, h)| h.seq)
                .map(|(k, _)| k.clone())
            {
                proj.idempotency.remove(&old);
            } else {
                break;
            }
        }
    }
}

fn apply_check_in(proj: &mut Projection, event: &Event) {
    let body = &event.body;
    let agent = Agent {
        agent_id: event.agent_id.clone(),
        lease_id: event.lease_id.clone(),
        revision: u(body, "revision").max(1),
        state: str_or(body, "state"),
        role: str_or(body, "role"),
        task: str_or(body, "task"),
        summary: str_or(body, "summary"),
        mas_session: opt_str(body, "mas_session"),
        checked_in_at: u(body, "checked_in_at"),
        last_seen_at: event.ts,
        expires_at: u(body, "expires_at"),
        ttl_secs: u(body, "ttl_secs"),
        workspace_id: str_or(body, "workspace_id"),
        worktree: str_or(body, "worktree"),
        branch: str_or(body, "branch"),
        base_oid: str_or(body, "base_oid"),
        head_oid: str_or(body, "head_oid"),
        dirty_paths: strs(body, "dirty_paths"),
        claims: claims_of(body),
        blockers: strs(body, "blockers"),
        checkpoint: opt_str(body, "checkpoint"),
        checkout_reason: None,
    };
    if let Some(id) = opt_str(body, "accept_handoff") {
        if let Some(h) = proj
            .handoffs
            .iter_mut()
            .find(|h| h.id == id && h.status == "offered")
        {
            h.status = "accepted".into();
            h.accepted_by = Some(event.agent_id.clone());
        }
    }
    proj.agents.insert(agent.agent_id.clone(), agent);
}

fn apply_update(proj: &mut Projection, event: &Event) {
    let Some(agent) = proj.agents.get_mut(&event.agent_id) else {
        return;
    };
    let body = &event.body;
    agent.lease_id = event.lease_id.clone();
    agent.revision = u(body, "revision");
    agent.last_seen_at = event.ts;
    agent.expires_at = u(body, "expires_at");
    if let Some(state) = opt_str(body, "state") {
        agent.state = state;
    }
    if body.get("summary").is_some() {
        agent.summary = str_or(body, "summary");
    }
    if body.get("role").is_some() {
        agent.role = str_or(body, "role");
    }
    if body.get("task").is_some() {
        agent.task = str_or(body, "task");
    }
    if body.get("mas_session").is_some() {
        agent.mas_session = opt_str(body, "mas_session");
    }
    if body
        .get("replace_claims")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        agent.claims = claims_of(body);
    }
    if body
        .get("replace_blockers")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        agent.blockers = strs(body, "blockers");
    }
    if body
        .get("replace_dirty")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        agent.dirty_paths = strs(body, "dirty_paths");
    }
    if body.get("checkpoint").is_some() {
        agent.checkpoint = opt_str(body, "checkpoint");
    }
    if let Some(branch) = opt_str(body, "branch") {
        agent.branch = branch;
    }
    if let Some(base) = opt_str(body, "base_oid") {
        agent.base_oid = base;
    }
    if let Some(head) = opt_str(body, "head_oid") {
        agent.head_oid = head;
    }
    if let Some(note) = body.get("note") {
        if proj.messages.len() >= MAX_MESSAGES {
            proj.messages.remove(0);
        }
        proj.messages.push(Note {
            seq: event.seq,
            from: event.agent_id.clone(),
            to: str_or(note, "to"),
            body: str_or(note, "body"),
            acked: false,
        });
    }
    if let Some(acks) = body.get("ack_seqs").and_then(Value::as_array) {
        for ack in acks {
            if let Some(seq) = ack.as_u64() {
                for msg in &mut proj.messages {
                    if msg.seq == seq && (msg.to == event.agent_id || msg.to.is_empty()) {
                        msg.acked = true;
                    }
                }
            }
        }
    }
}

fn apply_check_out(proj: &mut Projection, event: &Event) {
    let Some(agent) = proj.agents.get_mut(&event.agent_id) else {
        return;
    };
    agent.state = AgentState::CheckedOut.as_str().to_string();
    agent.revision = u(&event.body, "revision");
    agent.last_seen_at = event.ts;
    agent.claims.clear();
    agent.checkout_reason = opt_str(&event.body, "reason");
    if let Some(h) = event.body.get("handoff") {
        if proj.handoffs.len() >= MAX_HANDOFFS {
            proj.handoffs.remove(0);
        }
        proj.handoffs.push(Handoff {
            id: str_or(h, "id"),
            from: event.agent_id.clone(),
            to: str_or(h, "to"),
            status: "offered".into(),
            summary: str_or(h, "summary"),
            mas_session: opt_str(h, "mas_session"),
            mas_entry_id: h.get("mas_entry_id").and_then(Value::as_u64),
            note_ref: opt_str(h, "note_ref"),
            run_ids: strs(h, "run_ids"),
            artifacts: strs(h, "artifacts"),
            checkpoint: opt_str(h, "checkpoint"),
            anchors: strs(h, "anchors"),
            commands: strs(h, "commands"),
            constraints: strs(h, "constraints"),
            created_seq: event.seq,
            created_at: event.ts,
            accepted_by: None,
        });
    }
}

fn apply_promote(proj: &mut Projection, event: &Event) {
    let id = str_or(&event.body, "id");
    if id.is_empty() {
        return;
    }
    for policy in &mut proj.policies {
        if policy.status == "active" {
            policy.status = "retired".into();
        }
    }
    proj.policies.push(PolicyRec {
        id: id.clone(),
        status: "active".into(),
        support: event
            .body
            .get("support")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        report_hash: str_or(&event.body, "report_hash"),
        profile: str_or(&event.body, "profile"),
        spec: event.body.get("spec").cloned().unwrap_or(Value::Null),
    });
    proj.active_policy = Some(id);
}

fn apply_retire(proj: &mut Projection, event: &Event) {
    let id = str_or(&event.body, "id");
    for policy in &mut proj.policies {
        if policy.id == id {
            policy.status = "retired".into();
        }
    }
    if proj.active_policy.as_deref() == Some(id.as_str()) {
        proj.active_policy = None;
    }
}

pub fn read(root: &Path) -> Result<Snapshot, String> {
    let id = coordination_id(root);
    let dir = group_dir(root);
    if !dir.exists() {
        return Ok(Snapshot {
            projection: Projection::empty(id),
            events: Vec::new(),
        });
    }
    let _lock = GroupLock::acquire(&dir)?;
    load_locked(&dir, &id)
}

fn load_locked(dir: &Path, coordination_id: &str) -> Result<Snapshot, String> {
    let events = load_events(dir)?;
    let rebuilt = fold(&events, coordination_id);
    let path = projection_path(dir);
    let projection = match workspace::read_json(&path)? {
        Some(v) => match Projection::from_value(&v) {
            Some(p) if p.seq == rebuilt.seq && p.coordination_id == coordination_id => p,
            _ => {
                let _ = workspace::write_atomic_json(&path, &rebuilt.to_value());
                rebuilt
            }
        },
        None => {
            if !events.is_empty() {
                let _ = workspace::write_atomic_json(&path, &rebuilt.to_value());
            }
            rebuilt
        }
    };
    Ok(Snapshot { projection, events })
}

pub struct Prepared {
    pub kind: String,
    pub agent_id: String,
    pub lease_id: String,
    pub idempotency_key: Option<String>,
    pub body: Value,
    pub result_fn: fn(&Projection, &Event) -> Value,
}

pub fn commit(root: &Path, prepared: Prepared) -> Result<Value, String> {
    let id = coordination_id(root);
    let dir = group_dir(root);
    let _lock = GroupLock::acquire(&dir)?;
    let snap = load_locked(&dir, &id)?;
    if let Some(key) = &prepared.idempotency_key {
        if let Some(hit) = snap.projection.idempotency.get(key) {
            return Ok(json!({
                "idempotent": true,
                "seq": hit.seq,
                "text": hit.result,
            }));
        }
    }
    let seq = snap.projection.seq + 1;
    let ts = now_secs();
    let mut event = Event {
        v: SCHEMA_VERSION,
        seq,
        id: format!("e{seq}"),
        idempotency_key: prepared.idempotency_key,
        ts,
        kind: prepared.kind,
        agent_id: prepared.agent_id,
        lease_id: prepared.lease_id,
        result: String::new(),
        body: prepared.body,
    };
    let mut next = snap.projection.clone();
    apply(&mut next, &event);
    let mut rendered = (prepared.result_fn)(&next, &event);
    event.result = rendered
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if let Some(key) = &event.idempotency_key {
        if let Some(hit) = next.idempotency.get_mut(key) {
            hit.result = event.result.clone();
        }
    }
    append_event(&dir, &event)?;
    workspace::write_atomic_json(&projection_path(&dir), &next.to_value())?;
    rendered["seq"] = json!(seq);
    rendered["lease_id"] = json!(event.lease_id);
    rendered["coordination_id"] = json!(id);
    rendered["idempotent"] = json!(false);
    if rendered.get("text").is_none() {
        rendered["text"] = json!(event.result);
    }
    Ok(rendered)
}

fn append_event(dir: &Path, event: &Event) -> Result<(), String> {
    let path = events_path(dir);
    let line =
        serde_json::to_string(&event.to_value()).map_err(|e| format!("serialize event: {e}"))?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("append {}: {e}", path.display()))?;
    writeln!(file, "{line}").map_err(|e| format!("write {}: {e}", path.display()))?;
    file.sync_all()
        .map_err(|e| format!("sync {}: {e}", path.display()))?;
    Ok(())
}

pub fn idempotent_result(proj: &Projection, key: &str) -> Option<String> {
    proj.idempotency.get(key).map(|h| h.result.clone())
}

fn claims_of(v: &Value) -> Vec<Claim> {
    v.get("claims")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Claim::from_value).collect())
        .unwrap_or_default()
}

fn str_or(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn opt_str(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| {
        if x.is_null() {
            None
        } else {
            x.as_str().map(|s| s.to_string()).filter(|s| !s.is_empty())
        }
    })
}

fn u(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn strs(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub fn derived_handoff_status(
    h: &Handoff,
    agents: &BTreeMap<String, Agent>,
    now: u64,
    orphan_secs: u64,
) -> String {
    if h.status == "accepted" {
        return "accepted".into();
    }
    let from_out = agents
        .get(&h.from)
        .map(|a| a.state == AgentState::CheckedOut.as_str())
        .unwrap_or(true);
    let to_state = agents.get(&h.to);
    let recipient_gone = !h.to.is_empty()
        && to_state
            .map(|a| a.state == AgentState::CheckedOut.as_str())
            .unwrap_or(false);
    let aged = now.saturating_sub(h.created_at) >= orphan_secs;
    if from_out && aged && (h.to.is_empty() || recipient_gone || to_state.is_none()) {
        "orphaned".into()
    } else {
        "offered".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isolated(name: &str) -> (std::sync::MutexGuard<'static, ()>, PathBuf, PathBuf) {
        let guard = crate::cache::test_env_lock();
        let stamp = format!("{name}_{}", std::process::id());
        let cache = std::env::temp_dir().join(format!("wk_iza_cache_{stamp}"));
        let root = std::env::temp_dir().join(format!("wk_iza_root_{stamp}"));
        let _ = std::fs::remove_dir_all(&cache);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("XDG_CACHE_HOME", &cache);
        (guard, root, cache)
    }

    #[test]
    fn partial_tail_is_dropped_and_earlier_events_remain() {
        let (_g, root, cache) = isolated("tail");
        let dir = group_dir(&root);
        std::fs::create_dir_all(&dir).unwrap();
        let event = Event {
            v: 1,
            seq: 1,
            id: "e1".into(),
            idempotency_key: None,
            ts: 1,
            kind: "check_in".into(),
            agent_id: "a".into(),
            lease_id: "lease".into(),
            result: "ok".into(),
            body: json!({"state":"checked_in","revision":1,"checked_in_at":1,"expires_at":99,"ttl_secs":10}),
        };
        append_event(&dir, &event).unwrap();
        let path = events_path(&dir);
        let mut raw = std::fs::read_to_string(&path).unwrap();
        raw.push_str("{\"kind\":");
        std::fs::write(&path, raw).unwrap();
        let events = load_events(&dir).unwrap();
        assert_eq!(events.len(), 1);
        assert!(!std::fs::read_to_string(&path)
            .unwrap()
            .contains("{\"kind\":"));
        let snap = read(&root).unwrap();
        assert!(snap.projection.agents.contains_key("a"));
        std::env::remove_var("XDG_CACHE_HOME");
        let _ = std::fs::remove_dir_all(&cache);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_projection_rebuilds_from_journal() {
        let (_g, root, cache) = isolated("rebuild");
        let dir = group_dir(&root);
        std::fs::create_dir_all(&dir).unwrap();
        let event = Event {
            v: 1,
            seq: 3,
            id: "e3".into(),
            idempotency_key: Some("k".into()),
            ts: 5,
            kind: "check_in".into(),
            agent_id: "solver".into(),
            lease_id: "L".into(),
            result: "back".into(),
            body: json!({"state":"live_code","revision":2,"task":"izakaya","expires_at":50}),
        };
        append_event(&dir, &event).unwrap();
        let snap = read(&root).unwrap();
        assert_eq!(snap.projection.seq, 3);
        assert_eq!(snap.projection.agents["solver"].task, "izakaya");
        assert!(projection_path(&dir).exists());
        std::env::remove_var("XDG_CACHE_HOME");
        let _ = std::fs::remove_dir_all(&cache);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn schema_defaults_missing_optional_fields() {
        let v =
            json!({"version": 1, "seq": 1, "agents": [{"agent_id": "a", "state": "checked_in"}]});
        let p = Projection::from_value(&v).unwrap();
        assert!(p.messages.is_empty());
        assert_eq!(p.agents["a"].claims.len(), 0);
        assert!(Projection::from_value(&json!({"version": 99})).is_none());
    }

    #[test]
    fn linked_worktrees_share_coordination_id() {
        let out = Command::new("git").arg("--version").output();
        if !out.map(|o| o.status.success()).unwrap_or(false) {
            return;
        }
        let (_g, root, cache) = isolated("wt");
        let run = |dir: &Path, args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            assert!(ok, "git {args:?} in {}", dir.display());
        };
        run(&root, &["init", "-b", "main"]);
        run(&root, &["config", "user.email", "izakaya@example.com"]);
        run(&root, &["config", "user.name", "izakaya"]);
        std::fs::write(root.join("README"), b"hi").unwrap();
        run(&root, &["add", "README"]);
        run(&root, &["commit", "-m", "init"]);
        let wt = root
            .parent()
            .unwrap()
            .join(format!("wk_iza_wt_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&wt);
        run(&root, &["worktree", "add", wt.to_str().unwrap(), "HEAD"]);
        assert_eq!(coordination_id(&root), coordination_id(&wt));
        assert_ne!(workspace::workspace_id(&root), workspace::workspace_id(&wt));
        let _ = std::fs::remove_dir_all(&wt);
        std::env::remove_var("XDG_CACHE_HOME");
        let _ = std::fs::remove_dir_all(&cache);
        let _ = std::fs::remove_dir_all(&root);
    }
}
