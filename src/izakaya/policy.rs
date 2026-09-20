//! Advisory policy promotion. Promotion never mutates presence, Git, or claims.
//!
//! The coding agent is the policy-development agent: it proposes a declarative spec,
//! calls replay, and may ask a human to promote. `advise` only reads.

use serde_json::{json, Value};
use std::path::Path;

use super::model::{self, AgentState};
use super::profiles;
use super::replay;
use super::store::{self, Event, Prepared, Projection};

pub fn list(root: &Path) -> Result<Value, String> {
    let snap = store::read(root)?;
    let mut lines = vec!["izakaya policy list".to_string(), "profiles:".to_string()];
    for id in profiles::list_ids(root) {
        lines.push(format!("  - {id}"));
    }
    lines.push("promoted:".into());
    if snap.projection.policies.is_empty() {
        lines.push("  (none)".into());
    }
    for policy in &snap.projection.policies {
        lines.push(format!(
            "  - {} status={} support={:.3} profile={}",
            policy.id, policy.status, policy.support, policy.profile
        ));
    }
    lines.push(format!(
        "active: {}",
        snap.projection
            .active_policy
            .clone()
            .unwrap_or_else(|| "(none)".into())
    ));
    Ok(json!({"text": lines.join("\n") + "\n", "read_only": true}))
}

pub fn evaluate(root: &Path, spec: &Value, profile: &str) -> Result<Value, String> {
    replay::evaluate(root, spec, profile)
}

pub fn promote(root: &Path, spec: &Value, profile: &str) -> Result<Value, String> {
    profiles::reject_executable(spec)?;
    let parsed = replay::parse_spec(spec)?;
    let report = replay::evaluate(root, spec, profile)?;
    if report.get("promotion_ok").and_then(Value::as_bool) != Some(true) {
        return Err(format!(
            "refusing to promote {}: holdout gates, support >= {:.2}, zero out-of-support, and a non-empty holdout are required\n{}",
            parsed.id,
            model::MIN_SUPPORT,
            report.get("text").and_then(Value::as_str).unwrap_or("")
        ));
    }
    let support = report
        .get("holdout_support")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let report_hash = report
        .get("report_hash")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    store::commit(
        root,
        Prepared {
            kind: "policy_promote".into(),
            agent_id: "policy".into(),
            lease_id: String::new(),
            idempotency_key: None,
            body: json!({
                "id": parsed.id,
                "support": support,
                "report_hash": report_hash,
                "profile": profile,
                "spec": spec,
            }),
            result_fn: render_promote,
        },
    )
}

pub fn retire(root: &Path, id: &str) -> Result<Value, String> {
    let id = model::validate_slug("policy.id", id)?;
    let snap = store::read(root)?;
    if !snap.projection.policies.iter().any(|p| p.id == id) {
        return Err(format!("unknown policy {id}"));
    }
    store::commit(
        root,
        Prepared {
            kind: "policy_retire".into(),
            agent_id: "policy".into(),
            lease_id: String::new(),
            idempotency_key: None,
            body: json!({"id": id}),
            result_fn: render_retire,
        },
    )
}

pub fn advise(root: &Path, args: &Value) -> Result<Value, String> {
    model::reject_unsafe_payload(args)?;
    let before = store::read(root)?;
    let seq_before = before.projection.seq;
    let events_before = before.events.len();
    let now = store::now_secs();
    let Some(active) = before.projection.active_policy.clone() else {
        return Ok(json!({
            "text": "izakaya_advise - no promoted advisory policy; recommendations are inert\n",
            "recommendations": [],
            "side_effects": false,
        }));
    };
    let Some(policy) = before
        .projection
        .policies
        .iter()
        .rev()
        .find(|p| p.id == active && p.status == "active")
    else {
        return Ok(json!({
            "text": "izakaya_advise - active policy record missing; recommendations are inert\n",
            "recommendations": [],
            "side_effects": false,
        }));
    };
    let spec =
        replay::parse_spec(&policy.spec).unwrap_or(replay::parse_spec(&replay::incumbent_spec())?);
    let prefers: Vec<_> = spec.prefer.iter().map(|(f, _)| f.clone()).collect();
    let views: Vec<_> = before
        .projection
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
    let findings = model::advisory_findings(&views, now);
    let mut recs = Vec::new();
    for finding in &findings {
        let wanted = match finding.kind.as_str() {
            "path_overlap" | "symbol_overlap" | "drift" => {
                prefers.iter().any(|f| f == "non_overlapping")
            }
            "stale" => true,
            _ => false,
        };
        if wanted {
            recs.push(format!(
                "advisory: {} (confidence {:.2})",
                finding.detail, policy.support
            ));
        }
    }
    for handoff in &before.projection.handoffs {
        let derived = store::derived_handoff_status(
            handoff,
            &before.projection.agents,
            now,
            crate::config::izakaya_orphan_secs(root),
        );
        if derived == "offered" && (prefers.iter().any(|f| f == "handoff") || prefers.is_empty()) {
            recs.push(format!(
                "advisory: handoff {} from {} to {} is unclaimed — accept only by checking in",
                handoff.id, handoff.from, handoff.to
            ));
        }
    }
    if recs.is_empty() {
        recs.push("advisory: no change recommended".into());
    }
    recs.push(
        "authority: read-only; this call does not check in, suspend, check out, or run git".into(),
    );
    let after = store::read(root)?;
    if after.projection.seq != seq_before || after.events.len() != events_before {
        return Err("izakaya_advise mutated state; this is a bug".into());
    }
    let text = format!(
        "izakaya_advise - policy {}\n{}\n",
        policy.id,
        recs.join("\n")
    );
    Ok(json!({
        "text": text,
        "policy_id": policy.id,
        "support": policy.support,
        "recommendations": recs,
        "side_effects": false,
        "seq": after.projection.seq,
    }))
}

fn render_promote(proj: &Projection, event: &Event) -> Value {
    let id = event.body.get("id").and_then(Value::as_str).unwrap_or("");
    json!({
        "text": format!(
            "izakaya policy promote - {id} active\nsupport: {}\nThis does not assign work or change agent leases.\n",
            event.body.get("support").and_then(Value::as_f64).unwrap_or(0.0)
        ),
        "policy_id": id,
        "active_policy": proj.active_policy,
    })
}

fn render_retire(proj: &Projection, event: &Event) -> Value {
    let id = event.body.get("id").and_then(Value::as_str).unwrap_or("");
    json!({
        "text": format!("izakaya policy retire - {id}\nactive: {:?}\n", proj.active_policy),
        "policy_id": id,
        "active_policy": proj.active_policy,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::izakaya::presence;
    use serde_json::json;

    fn episode(root: &std::path::Path, agent: &str, episode: &str, decision: &str) {
        let inn = presence::check_in(
            root,
            &json!({"agent_id": agent, "observe_git": false, "task": episode}),
        )
        .unwrap();
        let lease = inn["lease_id"].as_str().unwrap();
        presence::record_decision(
            root,
            &json!({
                "agent_id": agent,
                "lease_id": lease,
                "episode_id": episode,
                "decision_id": decision,
                "task": episode,
                "legal_actions": [{"id": "step", "kind": "continue", "overlapping": false}],
                "selected": ["step"],
                "fingerprints": {"model": "test", "evaluator": "gates"}
            }),
        )
        .unwrap();
        presence::record_outcome(
            root,
            &json!({
                "agent_id": agent,
                "lease_id": lease,
                "decision_id": decision,
                "action": "step",
                "metrics": {"correctness": 1, "integration": 1, "cost": 2}
            }),
        )
        .unwrap();
        presence::check_out(
            root,
            &json!({"agent_id": agent, "lease_id": lease, "reason": "completed"}),
        )
        .unwrap();
    }

    #[test]
    fn promote_requires_holdout_and_advise_is_side_effect_free() {
        let _g = crate::cache::test_env_lock();
        let cache = std::env::temp_dir().join(format!("wk_iza_pol_cache_{}", std::process::id()));
        let root = std::env::temp_dir().join(format!("wk_iza_pol_root_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("XDG_CACHE_HOME", &cache);
        std::env::set_var("WORDKEEP_IZAKAYA_NOW", "9000");

        episode(&root, "alpha", "task-a", "da");
        let err = promote(&root, &replay::incumbent_spec(), "coordination").unwrap_err();
        assert!(err.contains("holdout"), "{err}");

        episode(&root, "beta", "task-b", "db");
        let promoted = promote(&root, &replay::incumbent_spec(), "coordination").unwrap();
        assert!(promoted["text"]
            .as_str()
            .unwrap()
            .contains("does not assign"));
        let seq = store::read(&root).unwrap().projection.seq;
        let advice = advise(&root, &json!({})).unwrap();
        assert_eq!(advice["side_effects"], false);
        assert_eq!(store::read(&root).unwrap().projection.seq, seq);
        assert!(advice["text"].as_str().unwrap().contains("read-only"));
        retire(&root, "incumbent").unwrap();
        assert!(store::read(&root)
            .unwrap()
            .projection
            .active_policy
            .is_none());

        std::env::remove_var("XDG_CACHE_HOME");
        std::env::remove_var("WORDKEEP_IZAKAYA_NOW");
        let _ = std::fs::remove_dir_all(&cache);
        let _ = std::fs::remove_dir_all(&root);
    }
}
