//! Izakaya presence mutations and the read-only board.
//!
//! Check-in, update, and checkout are explicit. Lease expiry marks an agent stale
//! but does not check it out, transfer claims, or touch Git.

use serde_json::{json, Value};
use std::path::Path;

use super::model::{self, AgentState, Claim};
use super::store::{self, Agent, Event, Prepared, Projection};
use crate::{config, mas};

pub fn status(root: &Path, args: &Value) -> Result<Value, String> {
    model::reject_unsafe_payload(args)?;
    let snap = store::read(root)?;
    Ok(render_status(root, &snap.projection, &snap.events, args))
}

pub fn check_in(root: &Path, args: &Value) -> Result<Value, String> {
    model::reject_unsafe_payload(args)?;
    let agent_id = agent_id_of(args)?;
    let snap = store::read(root)?;
    if let Some(text) = cached(&snap.projection, args) {
        return Ok(json!({"idempotent": true, "text": text, "agent_id": agent_id}));
    }
    let now = store::now_secs();
    let force = flag(args, "force");
    let lease_in = opt(args, "lease_id");
    let (lease_id, revision, fresh) = match snap.projection.agents.get(&agent_id) {
        Some(agent) if agent.state != AgentState::CheckedOut.as_str() => {
            let expired = model::is_stale(parse_state(&agent.state)?, agent.expires_at, now);
            if lease_in.as_deref() == Some(agent.lease_id.as_str()) {
                (agent.lease_id.clone(), agent.revision + 1, false)
            } else if expired || force {
                (
                    new_lease(&agent_id, agent.revision + 1, now),
                    agent.revision + 1,
                    true,
                )
            } else {
                return Err(format!(
                    "agent {agent_id} is checked in; pass lease_id to resume, or force:true / wait for expiry"
                ));
            }
        }
        Some(agent) => (
            new_lease(&agent_id, agent.revision + 1, now),
            agent.revision + 1,
            true,
        ),
        None => (new_lease(&agent_id, 1, now), 1, true),
    };
    if let Some(exp) = args.get("expected_revision").and_then(Value::as_u64) {
        let have = snap
            .projection
            .agents
            .get(&agent_id)
            .map(|a| a.revision)
            .unwrap_or(0);
        if exp != have {
            return Err(format!("revision conflict: expected {exp} have {have}"));
        }
    }
    let ttl = args
        .get("ttl_secs")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| config::izakaya_ttl_secs(root))
        .max(1);
    let git = observe(root, args);
    let claims = parse_claims(root, args)?;
    let accept = opt(args, "handoff_id");
    if let Some(id) = &accept {
        let handoff = snap
            .projection
            .handoffs
            .iter()
            .find(|h| &h.id == id)
            .ok_or_else(|| format!("unknown handoff {id}"))?;
        if handoff.status != "offered" {
            return Err(format!("handoff {id} is {}", handoff.status));
        }
        if !handoff.to.is_empty() && handoff.to != agent_id {
            return Err(format!(
                "handoff {id} is offered to {}, not {agent_id}",
                handoff.to
            ));
        }
    }
    let mas_session = opt(args, "mas_session");
    let mas_note = mas_note(root, mas_session.as_deref());
    let state = AgentState::CheckedIn.as_str();
    let body = json!({
        "state": state,
        "role": bound_opt(args, "role"),
        "task": bound_opt(args, "task"),
        "summary": bound_opt(args, "summary"),
        "mas_session": mas_session,
        "mas_note": mas_note,
        "checked_in_at": if fresh { now } else { snap.projection.agents.get(&agent_id).map(|a| a.checked_in_at).unwrap_or(now) },
        "expires_at": now.saturating_add(ttl),
        "ttl_secs": ttl,
        "revision": revision,
        "workspace_id": crate::workspace::workspace_id(root),
        "worktree": git.worktree,
        "branch": git.branch,
        "base_oid": git.base_oid,
        "head_oid": git.head_oid,
        "dirty_paths": git.dirty_paths,
        "claims": claims.iter().map(Claim::to_value).collect::<Vec<_>>(),
        "blockers": string_list(args, "blockers"),
        "checkpoint": opt(args, "checkpoint"),
        "accept_handoff": accept,
    });
    store::commit(
        root,
        Prepared {
            kind: "check_in".into(),
            agent_id,
            lease_id,
            idempotency_key: opt(args, "idempotency_key"),
            body,
            result_fn: render_check_in,
        },
    )
}

pub fn update(root: &Path, args: &Value) -> Result<Value, String> {
    model::reject_unsafe_payload(args)?;
    let agent_id = agent_id_of(args)?;
    let snap = store::read(root)?;
    if let Some(text) = cached(&snap.projection, args) {
        return Ok(json!({"idempotent": true, "text": text, "agent_id": agent_id}));
    }
    let agent = live_agent(&snap.projection, &agent_id)?;
    let lease = required(args, "lease_id")?;
    if agent.lease_id != lease {
        return Err(format!("lease mismatch for agent {agent_id}"));
    }
    expect_revision(args, agent)?;
    let now = store::now_secs();
    let from = parse_state(&agent.state)?;
    let to = args
        .get("state")
        .and_then(Value::as_str)
        .map(AgentState::parse)
        .transpose()?
        .unwrap_or(from);
    if to == AgentState::CheckedOut {
        return Err("use izakaya_check_out to leave; update cannot check out".into());
    }
    model::transition(from, to)?;
    let git = observe(root, args);
    let replace_dirty = args.get("dirty_paths").is_some() || flag_default_true(args, "observe_git");
    let dirty = if args.get("dirty_paths").is_some() {
        string_list(args, "dirty_paths")
    } else if flag_default_true(args, "observe_git") {
        git.dirty_paths.clone()
    } else {
        agent.dirty_paths.clone()
    };
    let checkpoint = opt(args, "checkpoint").or_else(|| agent.checkpoint.clone());
    if to == AgentState::Suspended
        && !dirty.is_empty()
        && checkpoint.as_deref().unwrap_or("").is_empty()
    {
        return Err("suspended dirty work requires a checkpoint reference (stash, patch id, or commit); refusing to pretend the work is saved".into());
    }
    let ttl = if to == AgentState::Suspended {
        config::izakaya_suspend_ttl_secs(root).max(1)
    } else {
        args.get("ttl_secs")
            .and_then(Value::as_u64)
            .unwrap_or(agent.ttl_secs.max(config::izakaya_ttl_secs(root)))
            .max(1)
    };
    let replace_claims = args.get("claims").is_some();
    let claims = if replace_claims {
        parse_claims(root, args)?
    } else {
        agent.claims.clone()
    };
    let note = args.get("note").cloned().map(|n| {
        json!({
            "to": n.get("to").and_then(Value::as_str).unwrap_or(""),
            "body": model::bound(n.get("body").and_then(Value::as_str).unwrap_or(""), model::MAX_SUMMARY),
        })
    });
    let body = json!({
        "state": to.as_str(),
        "revision": agent.revision + 1,
        "expires_at": now.saturating_add(ttl),
        "summary": args.get("summary").and_then(Value::as_str).map(|s| model::bound(s, model::MAX_SUMMARY)),
        "role": opt(args, "role"),
        "task": opt(args, "task"),
        "mas_session": if args.get("mas_session").is_some() { json!(opt(args, "mas_session")) } else { Value::Null },
        "replace_claims": replace_claims,
        "claims": claims.iter().map(Claim::to_value).collect::<Vec<_>>(),
        "replace_blockers": args.get("blockers").is_some(),
        "blockers": string_list(args, "blockers"),
        "replace_dirty": replace_dirty,
        "dirty_paths": dirty,
        "checkpoint": checkpoint,
        "branch": git.branch,
        "base_oid": git.base_oid,
        "head_oid": git.head_oid,
        "note": note,
        "ack_seqs": args.get("ack_seqs").cloned().unwrap_or(json!([])),
    });
    // Null summary should not wipe when omitted. apply_update treats any summary key as set.
    let mut body = body;
    if args.get("summary").is_none() {
        body.as_object_mut().unwrap().remove("summary");
    }
    if args.get("role").is_none() {
        body.as_object_mut().unwrap().remove("role");
    }
    if args.get("task").is_none() {
        body.as_object_mut().unwrap().remove("task");
    }
    if args.get("mas_session").is_none() {
        body.as_object_mut().unwrap().remove("mas_session");
    }
    if git.branch.is_empty() {
        body.as_object_mut().unwrap().remove("branch");
        body.as_object_mut().unwrap().remove("base_oid");
        body.as_object_mut().unwrap().remove("head_oid");
    }
    store::commit(
        root,
        Prepared {
            kind: "update".into(),
            agent_id,
            lease_id: lease,
            idempotency_key: opt(args, "idempotency_key"),
            body,
            result_fn: render_update,
        },
    )
}

pub fn check_out(root: &Path, args: &Value) -> Result<Value, String> {
    model::reject_unsafe_payload(args)?;
    let agent_id = agent_id_of(args)?;
    let snap = store::read(root)?;
    if let Some(text) = cached(&snap.projection, args) {
        return Ok(json!({"idempotent": true, "text": text, "agent_id": agent_id}));
    }
    let Some(agent) = snap.projection.agents.get(&agent_id) else {
        return Err(format!("agent {agent_id} is not checked in"));
    };
    if agent.state == AgentState::CheckedOut.as_str() {
        return Ok(json!({
            "idempotent": true,
            "agent_id": agent_id,
            "state": "checked_out",
            "text": format!("izakaya_check_out - agent {agent_id} already checked out"),
        }));
    }
    let lease = required(args, "lease_id")?;
    if agent.lease_id != lease {
        return Err(format!("lease mismatch for agent {agent_id}"));
    }
    expect_revision(args, agent)?;
    let handoff = args.get("handoff").filter(|v| !v.is_null()).cloned();
    let reason = if let Some(raw) = args.get("reason").and_then(Value::as_str) {
        model::checkout_reason(raw)?
    } else if handoff.is_some() {
        "handed_off"
    } else {
        "completed"
    };
    if reason == "handed_off" && handoff.is_none() {
        return Err(
            "handed_off checkout requires a handoff capsule (summary and/or mas_session)".into(),
        );
    }
    let handoff = handoff.map(|h| bound_handoff(&h, &format!("h{}", snap.projection.seq + 1)));
    if let Some(h) = &handoff {
        if h["summary"].as_str().unwrap_or("").is_empty()
            && h["mas_session"].is_null()
            && h["note_ref"].is_null()
        {
            return Err("handoff capsule needs summary, mas_session, or note_ref".into());
        }
    }
    let mas_session = handoff
        .as_ref()
        .and_then(|h| h.get("mas_session"))
        .and_then(Value::as_str);
    let mas_note = mas_note(root, mas_session);
    let mut body = json!({
        "reason": reason,
        "revision": agent.revision + 1,
        "mas_note": mas_note,
        "handoff": handoff,
    });
    if body["handoff"].is_null() {
        body.as_object_mut().unwrap().remove("handoff");
    }
    store::commit(
        root,
        Prepared {
            kind: "check_out".into(),
            agent_id,
            lease_id: lease,
            idempotency_key: opt(args, "idempotency_key"),
            body,
            result_fn: render_check_out,
        },
    )
}

pub fn record_decision(root: &Path, args: &Value) -> Result<Value, String> {
    model::reject_unsafe_payload(args)?;
    let agent_id = agent_id_of(args)?;
    let snap = store::read(root)?;
    if let Some(text) = cached(&snap.projection, args) {
        return Ok(json!({"idempotent": true, "text": text}));
    }
    let agent = live_agent(&snap.projection, &agent_id)?;
    let lease = required(args, "lease_id")?;
    if agent.lease_id != lease {
        return Err(format!("lease mismatch for agent {agent_id}"));
    }
    let episode = model::validate_slug(
        "episode_id",
        args.get("episode_id")
            .and_then(Value::as_str)
            .unwrap_or(&agent_id),
    )?;
    let decision_id = args
        .get("decision_id")
        .and_then(Value::as_str)
        .map(|s| model::validate_slug("decision_id", s))
        .transpose()?
        .unwrap_or_else(|| format!("d{}", snap.projection.seq + 1));
    let legal = parse_actions(args, "legal_actions")?;
    if legal.is_empty() {
        return Err("legal_actions must list the frontier visible at this decision".into());
    }
    let selected = string_list(args, "selected");
    if selected.is_empty() {
        return Err("selected must name the batch actually started (empty batches are a stop, omit the call)".into());
    }
    for id in &selected {
        if !legal.iter().any(|a| a["id"].as_str() == Some(id.as_str())) {
            return Err(format!("selected action {id} is not in legal_actions"));
        }
    }
    let body = json!({
        "episode_id": episode,
        "decision_id": decision_id,
        "legal_actions": legal,
        "selected": selected,
        "profile_id": args.get("profile").and_then(Value::as_str).unwrap_or("coordination"),
        "policy_id": args.get("policy_id").and_then(Value::as_str).unwrap_or("manual"),
        "fingerprints": fingerprints(args),
        "budget": args.get("budget").and_then(Value::as_u64).unwrap_or(1),
        "workers": args.get("workers").and_then(Value::as_u64).unwrap_or(1),
        "task": bound_opt(args, "task"),
        "observation_hash": bound_opt(args, "observation_hash"),
    });
    store::commit(
        root,
        Prepared {
            kind: "decision".into(),
            agent_id,
            lease_id: lease,
            idempotency_key: opt(args, "idempotency_key"),
            body,
            result_fn: render_decision,
        },
    )
}

pub fn record_outcome(root: &Path, args: &Value) -> Result<Value, String> {
    model::reject_unsafe_payload(args)?;
    let agent_id = agent_id_of(args)?;
    let snap = store::read(root)?;
    if let Some(text) = cached(&snap.projection, args) {
        return Ok(json!({"idempotent": true, "text": text}));
    }
    let agent = live_agent(&snap.projection, &agent_id)?;
    let lease = required(args, "lease_id")?;
    if agent.lease_id != lease {
        return Err(format!("lease mismatch for agent {agent_id}"));
    }
    let decision_id = model::validate_slug("decision_id", &required(args, "decision_id")?)?;
    let action = model::validate_slug("action", &required(args, "action")?)?;
    let known = snap.events.iter().any(|e| {
        e.kind == "decision"
            && e.body.get("decision_id").and_then(Value::as_str) == Some(decision_id.as_str())
    });
    if !known {
        return Err(format!("unknown decision_id {decision_id}"));
    }
    let mut metrics = btree_map_from(args.get("metrics"));
    let run_ids = string_list(args, "run_ids");
    let profile_id = args
        .get("profile")
        .and_then(Value::as_str)
        .unwrap_or("coordination");
    if let Ok(profile) = super::profiles::load(root, profile_id) {
        for id in &run_ids {
            if let Some(run) = crate::runs::get(root, id) {
                if run.status == "passed" {
                    metrics.entry("correctness".into()).or_insert(1.0);
                    metrics.entry("integration".into()).or_insert(1.0);
                } else if run.status == "failed" {
                    metrics.insert("correctness".into(), 0.0);
                }
                if let Some(ms) = run.duration_ms {
                    metrics.entry("cost".into()).or_insert(ms as f64);
                    metrics.entry("critical_path".into()).or_insert(ms as f64);
                }
                super::profiles::apply_run_adapters(&profile, &run, &mut metrics);
            }
        }
    }
    let body = json!({
        "decision_id": decision_id,
        "action": action,
        "metrics": metrics,
        "censored": flag(args, "censored"),
        "run_ids": run_ids,
        "artifacts": string_list(args, "artifacts"),
        "profile_id": profile_id,
        "fingerprints": fingerprints(args),
    });
    store::commit(
        root,
        Prepared {
            kind: "outcome".into(),
            agent_id,
            lease_id: lease,
            idempotency_key: opt(args, "idempotency_key"),
            body,
            result_fn: render_outcome,
        },
    )
}

fn btree_map_from(v: Option<&Value>) -> std::collections::BTreeMap<String, f64> {
    let mut out = std::collections::BTreeMap::new();
    let Some(obj) = v.and_then(Value::as_object) else {
        return out;
    };
    for (k, val) in obj {
        if let Some(n) = val.as_f64() {
            out.insert(k.clone(), n);
        } else if let Some(n) = val.as_i64() {
            out.insert(k.clone(), n as f64);
        }
    }
    out
}

fn render_check_in(proj: &Projection, event: &Event) -> Value {
    let agent = &proj.agents[&event.agent_id];
    let findings = finding_lines(proj, event.ts);
    let mut text = format!(
        "izakaya_check_in - agent {}\nstate: {}\nlease_id: {}\nrevision: {}\nseq: {}\nexpires_at: {}\n",
        agent.agent_id, agent.state, agent.lease_id, agent.revision, event.seq, agent.expires_at
    );
    if let Some(note) = event.body.get("mas_note").and_then(Value::as_str) {
        if !note.is_empty() {
            text.push_str(note);
            text.push('\n');
        }
    }
    text.push_str(&findings);
    json!({
        "text": text,
        "agent_id": agent.agent_id,
        "state": agent.state,
        "lease_id": agent.lease_id,
        "revision": agent.revision,
        "seq": event.seq,
        "expires_at": agent.expires_at,
        "advisory": true,
    })
}

fn render_update(proj: &Projection, event: &Event) -> Value {
    let agent = &proj.agents[&event.agent_id];
    let text = format!(
        "izakaya_update - agent {}\nstate: {}\nlease_id: {}\nrevision: {}\nexpires_at: {}\n{}",
        agent.agent_id,
        agent.state,
        agent.lease_id,
        agent.revision,
        agent.expires_at,
        finding_lines(proj, event.ts)
    );
    json!({
        "text": text,
        "agent_id": agent.agent_id,
        "state": agent.state,
        "revision": agent.revision,
        "expires_at": agent.expires_at,
    })
}

fn render_check_out(proj: &Projection, event: &Event) -> Value {
    let reason = event
        .body
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("completed");
    let mut text = format!(
        "izakaya_check_out - agent {}\nstate: checked_out\nreason: {reason}\nclaims: released\n",
        event.agent_id
    );
    if let Some(note) = event.body.get("mas_note").and_then(Value::as_str) {
        if !note.is_empty() {
            text.push_str(note);
            text.push('\n');
        }
    }
    if let Some(h) = proj
        .handoffs
        .iter()
        .rev()
        .find(|h| h.from == event.agent_id)
    {
        text.push_str(&format!("handoff_id: {} to {}\n", h.id, h.to));
    }
    text.push_str("Izakaya did not finalize any MAS session.\n");
    let handoff_id = proj
        .handoffs
        .iter()
        .rev()
        .find(|h| h.from == event.agent_id)
        .map(|h| h.id.clone());
    json!({
        "text": text,
        "agent_id": event.agent_id,
        "state": "checked_out",
        "reason": reason,
        "handoff_id": handoff_id,
    })
}

fn render_decision(_proj: &Projection, event: &Event) -> Value {
    let id = event
        .body
        .get("decision_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    json!({
        "text": format!("izakaya_record_decision - agent {} decision {id} seq {}\n", event.agent_id, event.seq),
        "decision_id": id,
        "seq": event.seq,
    })
}

fn render_outcome(_proj: &Projection, event: &Event) -> Value {
    let id = event
        .body
        .get("decision_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let action = event
        .body
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("");
    json!({
        "text": format!("izakaya_record_outcome - decision {id} action {action}\n"),
        "decision_id": id,
        "action": action,
    })
}

fn render_status(root: &Path, proj: &Projection, events: &[Event], args: &Value) -> Value {
    let now = store::now_secs();
    let orphan_secs = config::izakaya_orphan_secs(root);
    let want_agent = args
        .get("agent")
        .and_then(Value::as_str)
        .or_else(|| args.get("agent_id").and_then(Value::as_str));
    let want_state = args.get("state").and_then(Value::as_str);
    let want_path = args.get("path").and_then(Value::as_str);
    let since = args.get("since_seq").and_then(Value::as_u64).unwrap_or(0);
    let include_out = flag(args, "include_checked_out") || want_state == Some("checked_out");
    let mut lines = vec![format!(
        "izakaya_status - coordination {} seq {} (read-only)",
        proj.coordination_id, proj.seq
    )];
    if proj.malformed > 0 {
        lines.push(format!("malformed_events: {}", proj.malformed));
    }
    let mut listed = Vec::new();
    for agent in proj.agents.values() {
        let state = AgentState::parse(&agent.state).unwrap_or(AgentState::CheckedIn);
        let stale = model::is_stale(state, agent.expires_at, now);
        if let Some(id) = want_agent {
            if agent.agent_id != id {
                continue;
            }
        }
        if let Some(filter) = want_state {
            let matches = if filter == "stale" {
                stale
            } else {
                agent.state == filter
            };
            if !matches {
                continue;
            }
        } else if agent.state == "checked_out" && !include_out {
            continue;
        } else if stale && want_agent.is_none() {
            continue;
        }
        if let Some(path) = want_path {
            let hit = agent
                .claims
                .iter()
                .any(|c| model::paths_overlap(&c.path, path))
                || agent
                    .dirty_paths
                    .iter()
                    .any(|p| model::paths_overlap(p, path));
            if !hit {
                continue;
            }
        }
        listed.push(agent_line(agent, stale));
    }
    if listed.is_empty() {
        lines.push("agents: (none active)".into());
    } else {
        lines.push("agents:".into());
        lines.extend(listed);
    }
    let stale: Vec<_> = proj
        .agents
        .values()
        .filter(|a| {
            AgentState::parse(&a.state)
                .map(|s| model::is_stale(s, a.expires_at, now))
                .unwrap_or(false)
        })
        .map(|a| a.agent_id.clone())
        .collect();
    lines.push(format!(
        "stale: {}",
        if stale.is_empty() {
            "(none)".into()
        } else {
            stale.join(", ")
        }
    ));
    lines.push(finding_lines(proj, now).trim_end().to_string());
    if proj.handoffs.is_empty() {
        lines.push("handoffs: (none)".into());
    } else {
        lines.push("handoffs:".into());
        for h in &proj.handoffs {
            let derived = store::derived_handoff_status(h, &proj.agents, now, orphan_secs);
            lines.push(format!(
                "  - {} {} -> {} derived={derived} {}",
                h.id, h.from, h.to, h.summary
            ));
        }
    }
    let inbox: Vec<_> = proj
        .messages
        .iter()
        .filter(|m| {
            want_agent
                .map(|id| m.to == id || m.from == id || m.to.is_empty())
                .unwrap_or(true)
        })
        .filter(|m| !m.acked)
        .map(|m| format!("  - #{} {} -> {}: {}", m.seq, m.from, m.to, m.body))
        .collect();
    if !inbox.is_empty() {
        lines.push("messages:".into());
        lines.extend(inbox);
    }
    let incremental: Vec<_> = events
        .iter()
        .filter(|e| e.seq > since && e.kind != "malformed")
        .map(|e| format!("  - seq {} {} {}", e.seq, e.kind, e.agent_id))
        .collect();
    if since > 0 {
        lines.push("since_seq:".into());
        if incremental.is_empty() {
            lines.push("  (none)".into());
        } else {
            lines.extend(incremental);
        }
    }
    let active_policy = proj
        .active_policy
        .clone()
        .unwrap_or_else(|| "(none)".into());
    lines.push(format!("advisory_policy: {active_policy}"));
    let text = lines.join("\n") + "\n";
    json!({
        "text": text,
        "coordination_id": proj.coordination_id,
        "seq": proj.seq,
        "stale": stale,
        "read_only": true,
    })
}

fn agent_line(agent: &Agent, stale: bool) -> String {
    let claims: Vec<_> = agent.claims.iter().map(|c| c.path.clone()).collect();
    format!(
        "  - {} {} rev {} expires {} task {:?} claims [{}]{}{}",
        agent.agent_id,
        agent.state,
        agent.revision,
        agent.expires_at,
        agent.task,
        claims.join(", "),
        if stale { " STALE" } else { "" },
        agent
            .mas_session
            .as_ref()
            .map(|s| format!(" mas={s}"))
            .unwrap_or_default()
    )
}

fn finding_lines(proj: &Projection, now: u64) -> String {
    let views: Vec<_> = proj
        .agents
        .values()
        .filter_map(|a| {
            Some(model::AgentView {
                agent_id: &a.agent_id,
                state: AgentState::parse(&a.state).ok()?,
                expires_at: a.expires_at,
                base_oid: &a.base_oid,
                claims: &a.claims,
                dirty_paths: &a.dirty_paths,
            })
        })
        .collect();
    let found = model::advisory_findings(&views, now);
    if found.is_empty() {
        return "advisory: (no overlaps)\n".into();
    }
    let mut text = String::from("advisory:\n");
    for f in found {
        text.push_str(&format!(
            "  - {} {}{}\n",
            f.kind,
            f.detail,
            if f.stale { " [stale]" } else { "" }
        ));
    }
    text
}

fn bound_handoff(v: &Value, id: &str) -> Value {
    json!({
        "id": id,
        "to": v.get("to").and_then(Value::as_str).unwrap_or(""),
        "summary": model::bound(v.get("summary").and_then(Value::as_str).unwrap_or(""), model::MAX_SUMMARY),
        "mas_session": v.get("mas_session").and_then(Value::as_str).filter(|s| !s.is_empty()),
        "mas_entry_id": v.get("mas_entry_id").and_then(Value::as_u64),
        "note_ref": v.get("note_ref").and_then(Value::as_str),
        "run_ids": strs(v, "run_ids"),
        "artifacts": strs(v, "artifacts"),
        "checkpoint": v.get("checkpoint").and_then(Value::as_str),
        "anchors": strs(v, "anchors"),
        "commands": strs(v, "commands"),
        "constraints": strs(v, "constraints"),
    })
}

fn parse_actions(args: &Value, key: &str) -> Result<Vec<Value>, String> {
    let Some(arr) = args.get(key).and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for item in arr.iter().take(model::MAX_CLAIMS) {
        if let Some(id) = item.as_str() {
            out.push(json!({"id": model::validate_slug("action", id)?, "parent": "root", "overlapping": false, "kind": "root", "paths": [], "causal_parents": []}));
        } else {
            let id = model::validate_slug(
                "action",
                item.get("id").and_then(Value::as_str).unwrap_or(""),
            )?;
            out.push(json!({
                "id": id,
                "parent": item.get("parent").and_then(Value::as_str).unwrap_or("root"),
                "overlapping": item.get("overlapping").and_then(Value::as_bool).unwrap_or(false),
                "kind": item.get("kind").and_then(Value::as_str).unwrap_or("continue"),
                "paths": strs(item, "paths"),
                "causal_parents": strs(item, "causal_parents"),
            }));
        }
    }
    Ok(out)
}

fn parse_claims(_root: &Path, args: &Value) -> Result<Vec<Claim>, String> {
    let Some(arr) = args.get("claims").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for item in arr.iter().take(model::MAX_CLAIMS) {
        let mut claim = if let Some(path) = item.as_str() {
            Claim {
                path: path.to_string(),
                symbols: Vec::new(),
                intent: "edit".into(),
            }
        } else {
            Claim::from_value(item).ok_or_else(|| "claim needs a path".to_string())?
        };
        claim.path = crate::config::validate_rel_path(&claim.path)?;
        out.push(claim);
    }
    Ok(out)
}

fn fingerprints(args: &Value) -> Value {
    let Some(obj) = args.get("fingerprints").and_then(Value::as_object) else {
        return json!({});
    };
    let mut out = serde_json::Map::new();
    for (k, v) in obj.iter().take(16) {
        if let Some(s) = v.as_str() {
            out.insert(model::bound(k, 40), json!(model::bound(s, model::MAX_ITEM)));
        }
    }
    Value::Object(out)
}

fn mas_note(root: &Path, session: Option<&str>) -> String {
    let Some(session) = session else {
        return String::new();
    };
    match mas::session_snapshot(root, session) {
        Ok(Some(snap)) => format!(
            "mas_session {session} status={} round {}/{} (Izakaya does not finalize it)",
            snap.status, snap.round, snap.max_rounds
        ),
        _ => format!("mas_session {session} (reference only; Izakaya does not finalize it)"),
    }
}

fn live_agent<'a>(proj: &'a Projection, agent_id: &str) -> Result<&'a Agent, String> {
    let agent = proj
        .agents
        .get(agent_id)
        .ok_or_else(|| format!("agent {agent_id} is not checked in"))?;
    if agent.state == AgentState::CheckedOut.as_str() {
        return Err(format!("agent {agent_id} is checked out"));
    }
    Ok(agent)
}

fn expect_revision(args: &Value, agent: &Agent) -> Result<(), String> {
    if let Some(exp) = args.get("expected_revision").and_then(Value::as_u64) {
        if exp != agent.revision {
            return Err(format!(
                "revision conflict: expected {exp} have {}",
                agent.revision
            ));
        }
    }
    Ok(())
}

fn parse_state(raw: &str) -> Result<AgentState, String> {
    AgentState::parse(raw)
}

fn new_lease(agent: &str, revision: u64, now: u64) -> String {
    format!("{agent}-{revision}-{now}")
}

fn agent_id_of(args: &Value) -> Result<String, String> {
    let raw = args
        .get("agent_id")
        .and_then(Value::as_str)
        .or_else(|| args.get("agent").and_then(Value::as_str))
        .ok_or("agent_id is required")?;
    model::validate_slug("agent_id", raw)
}

fn required(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{key} is required"))
}

fn opt(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(|s| model::bound(s, model::MAX_SUMMARY))
        .filter(|s| !s.is_empty())
}

fn bound_opt(args: &Value, key: &str) -> String {
    opt(args, key).unwrap_or_default()
}

fn flag(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn flag_default_true(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(true)
}

fn string_list(args: &Value, key: &str) -> Vec<String> {
    let raw: Vec<String> = args
        .get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    model::bound_strings(raw, model::MAX_LIST, model::MAX_ITEM)
}

fn strs(v: &Value, key: &str) -> Vec<String> {
    string_list(v, key)
}

fn cached(proj: &Projection, args: &Value) -> Option<String> {
    opt(args, "idempotency_key").and_then(|k| store::idempotent_result(proj, &k))
}

fn observe(root: &Path, args: &Value) -> store::GitSnapshot {
    if flag_default_true(args, "observe_git") {
        store::git_snapshot(root)
    } else {
        store::GitSnapshot {
            branch: String::new(),
            head_oid: String::new(),
            base_oid: String::new(),
            dirty_paths: Vec::new(),
            worktree: root.display().to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn isolated(name: &str) -> (std::sync::MutexGuard<'static, ()>, std::path::PathBuf) {
        let guard = crate::cache::test_env_lock();
        let cache =
            std::env::temp_dir().join(format!("wk_iza_p_cache_{name}_{}", std::process::id()));
        let root =
            std::env::temp_dir().join(format!("wk_iza_p_root_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("XDG_CACHE_HOME", &cache);
        std::env::set_var("WORDKEEP_IZAKAYA_NOW", "1000");
        (guard, root)
    }

    fn cleanup(root: &Path) {
        std::env::remove_var("XDG_CACHE_HOME");
        std::env::remove_var("WORDKEEP_IZAKAYA_NOW");
        std::env::remove_var("WORDKEEP_IZAKAYA_ORPHAN_SECS");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lifecycle_lease_overlap_and_handoff() {
        let (_g, root) = isolated("life");
        let empty = status(&root, &json!({})).unwrap();
        assert!(empty["text"].as_str().unwrap().contains("none active"));
        assert!(store::read(&root).unwrap().events.is_empty());

        let inn = check_in(
            &root,
            &json!({
                "agent_id": "alpha",
                "task": "presence",
                "claims": [{"path": "src/a.rs", "symbols": ["Foo"]}],
                "idempotency_key": "in-1",
                "observe_git": false
            }),
        )
        .unwrap();
        let again = check_in(
            &root,
            &json!({"agent_id": "alpha", "idempotency_key": "in-1", "observe_git": false}),
        )
        .unwrap();
        assert_eq!(again["idempotent"], true);
        let lease = inn["lease_id"].as_str().unwrap().to_string();
        assert!(check_in(&root, &json!({"agent_id": "alpha", "observe_git": false})).is_err());

        check_in(
            &root,
            &json!({
                "agent_id": "beta",
                "claims": [{"path": "src/a.rs", "symbols": ["Foo"]}],
                "observe_git": false
            }),
        )
        .unwrap();
        let board = status(&root, &json!({})).unwrap();
        let text = board["text"].as_str().unwrap();
        assert!(text.contains("path_overlap"), "{text}");
        assert!(text.contains("symbol_overlap"), "{text}");

        let err = update(
            &root,
            &json!({"agent_id": "alpha", "lease_id": "nope", "observe_git": false}),
        );
        assert!(err.unwrap_err().contains("lease mismatch"));
        update(
            &root,
            &json!({
                "agent_id": "alpha",
                "lease_id": lease,
                "state": "live_code",
                "observe_git": false,
                "note": {"to": "beta", "body": "taking src/a.rs first"}
            }),
        )
        .unwrap();
        let suspend = update(
            &root,
            &json!({
                "agent_id": "alpha",
                "lease_id": lease,
                "state": "suspended",
                "dirty_paths": ["src/a.rs"],
                "observe_git": false
            }),
        );
        assert!(suspend.unwrap_err().contains("checkpoint"));
        update(
            &root,
            &json!({
                "agent_id": "alpha",
                "lease_id": lease,
                "state": "suspended",
                "dirty_paths": ["src/a.rs"],
                "checkpoint": "stash@{0}",
                "observe_git": false
            }),
        )
        .unwrap();

        let out = check_out(
            &root,
            &json!({
                "agent_id": "alpha",
                "lease_id": lease,
                "reason": "handed_off",
                "handoff": {
                    "to": "beta",
                    "summary": "finish overlap",
                    "mas_session": "not-a-session",
                    "anchors": ["src/a.rs:1"]
                }
            }),
        )
        .unwrap();
        assert!(out["text"].as_str().unwrap().contains("does not finalize"));
        let hid = out["handoff_id"].as_str().unwrap().to_string();
        let beta = check_in(
            &root,
            &json!({"agent_id": "gamma", "handoff_id": hid, "observe_git": false}),
        );
        assert!(beta.unwrap_err().contains("offered to"));
        let beta_lease = store::read(&root).unwrap().projection.agents["beta"]
            .lease_id
            .clone();
        check_in(&root, &json!({"agent_id": "beta", "handoff_id": hid, "lease_id": beta_lease, "observe_git": false})).unwrap();
        let accepted = store::read(&root).unwrap();
        assert_eq!(accepted.projection.handoffs[0].status, "accepted");
        cleanup(&root);
    }

    #[test]
    fn expiry_does_not_check_out() {
        let (_g, root) = isolated("stale");
        std::env::set_var("WORDKEEP_IZAKAYA_NOW", "1000");
        let inn = check_in(
            &root,
            &json!({"agent_id": "alpha", "ttl_secs": 10, "observe_git": false}),
        )
        .unwrap();
        std::env::set_var("WORDKEEP_IZAKAYA_NOW", "2000");
        let board = status(&root, &json!({"state": "stale"})).unwrap();
        assert!(board["text"].as_str().unwrap().contains("alpha"), "{board}");
        let still = store::read(&root).unwrap();
        assert_eq!(still.projection.agents["alpha"].state, "checked_in");
        let lease = inn["lease_id"].as_str().unwrap();
        update(&root, &json!({"agent_id": "alpha", "lease_id": lease, "state": "live_code", "observe_git": false})).unwrap();
        assert_eq!(
            store::read(&root).unwrap().projection.agents["alpha"].state,
            "live_code"
        );
        cleanup(&root);
    }

    #[test]
    fn concurrent_check_ins_do_not_drop_agents() {
        let (_g, root) = isolated("conc");
        let a = root.clone();
        let b = root.clone();
        let t1 = std::thread::spawn(move || {
            check_in(
                &a,
                &json!({"agent_id": "one", "task": "a", "observe_git": false}),
            )
            .unwrap()
        });
        let t2 = std::thread::spawn(move || {
            check_in(
                &b,
                &json!({"agent_id": "two", "task": "b", "observe_git": false}),
            )
            .unwrap()
        });
        t1.join().unwrap();
        t2.join().unwrap();
        let snap = store::read(&root).unwrap();
        assert!(snap.projection.agents.contains_key("one"));
        assert!(snap.projection.agents.contains_key("two"));
        cleanup(&root);
    }
}
