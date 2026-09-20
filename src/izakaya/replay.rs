//! Historical supported replay. Not a counterfactual simulator.
//!
//! A candidate may stop early, take a recorded subset, or batch independent recorded
//! actions. Asking for an unrecorded continuation returns `out_of_support` and no
//! fabricated reward. Active and suspended episodes stay censored.

use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::model::{self, AgentState};
use super::profiles;
use super::store::{self, Event, Projection};

#[derive(Clone, Debug)]
struct Action {
    id: String,
    overlapping: bool,
    kind: String,
    paths: Vec<String>,
    causal_parents: Vec<String>,
}

#[derive(Clone, Debug)]
struct Decision {
    seq: u64,
    id: String,
    agent_id: String,
    episode_id: String,
    task: String,
    legal: Vec<Action>,
    selected: Vec<String>,
    fingerprints: BTreeMap<String, String>,
    ts: u64,
}

#[derive(Clone, Debug)]
struct Outcome {
    metrics: BTreeMap<String, f64>,
    censored: bool,
}

#[derive(Clone, Debug)]
struct World {
    episode_id: String,
    task: String,
    ts: u64,
    agent_ids: BTreeSet<String>,
    decisions: Vec<Decision>,
    censored: bool,
}

#[derive(Clone, Debug)]
pub struct Spec {
    pub id: String,
    pub selection: String,
    pub max_batch: usize,
    pub max_rounds: usize,
    pub prefer: Vec<(String, f64)>,
    pub min_score: f64,
    pub beta: f64,
}

pub fn incumbent_spec() -> Value {
    json!({
        "id": "incumbent",
        "version": 1,
        "selection": "recorded",
        "max_batch": 32,
        "max_rounds": 10000,
        "prefer": []
    })
}

pub fn parse_spec(v: &Value) -> Result<Spec, String> {
    profiles::reject_executable(v)?;
    let id = model::validate_slug(
        "policy.id",
        v.get("id").and_then(Value::as_str).unwrap_or("policy"),
    )?;
    let version = v.get("version").and_then(Value::as_u64).unwrap_or(1);
    if version != 1 {
        return Err(format!("unsupported policy version {version}"));
    }
    let selection = v
        .get("selection")
        .and_then(Value::as_str)
        .unwrap_or("score");
    if selection != "recorded" && selection != "score" {
        return Err("policy.selection must be recorded or score".into());
    }
    let max_batch = v.get("max_batch").and_then(Value::as_u64).unwrap_or(1) as usize;
    let max_rounds = v.get("max_rounds").and_then(Value::as_u64).unwrap_or(32) as usize;
    if !(1..=32).contains(&max_batch) || !(1..=10_000).contains(&max_rounds) {
        return Err("policy max_batch must be 1..=32 and max_rounds 1..=10000".into());
    }
    let mut prefer = Vec::new();
    if let Some(arr) = v.get("prefer").and_then(Value::as_array) {
        for item in arr {
            let feature = item.get("feature").and_then(Value::as_str).unwrap_or("");
            if !matches!(feature, "non_overlapping" | "continue" | "root" | "handoff") {
                return Err(format!("unknown policy feature {feature}"));
            }
            let weight = item.get("weight").and_then(Value::as_f64).unwrap_or(1.0);
            prefer.push((feature.to_string(), weight));
        }
    }
    let beta = v.get("beta").and_then(Value::as_f64).unwrap_or(1.0);
    if beta <= 0.0 {
        return Err("policy.beta must be > 0".into());
    }
    Ok(Spec {
        id,
        selection: selection.to_string(),
        max_batch,
        max_rounds,
        prefer,
        min_score: v.get("min_score").and_then(Value::as_f64).unwrap_or(0.0),
        beta,
    })
}

pub fn evaluate(root: &Path, spec_value: &Value, profile_id: &str) -> Result<Value, String> {
    let spec = parse_spec(spec_value)?;
    let profile = profiles::load(root, profile_id)?;
    let snap = store::read(root)?;
    let worlds = compile_worlds(&snap.events, &snap.projection);
    let outcomes = compile_outcomes(&snap.events);
    let incumbent = parse_spec(&incumbent_spec())?;
    let (train, holdout) = split_holdout(&worlds);
    let task_holdout = task_holdout(&worlds);
    let full_cand = replay_set(&worlds, &spec, &outcomes);
    let full_inc = replay_set(&worlds, &incumbent, &outcomes);
    let hold_cand = replay_set(&holdout, &spec, &outcomes);
    let hold_inc = replay_set(&holdout, &incumbent, &outcomes);
    let task_cand = replay_set(&task_holdout, &spec, &outcomes);
    let task_inc = replay_set(&task_holdout, &incumbent, &outcomes);
    let hold_gates = gates_ok(&hold_cand, &hold_inc, &profile.gates);
    let task_gates = task_holdout.is_empty() || gates_ok(&task_cand, &task_inc, &profile.gates);
    let support = hold_cand.support;
    let promotion_ok = !holdout.is_empty()
        && support + f64::EPSILON >= model::MIN_SUPPORT
        && hold_gates
        && task_gates
        && hold_cand.out_of_support == 0;
    let trace_key = json!(full_cand.traces);
    let report_hash = store::hash_id(&trace_key.to_string());
    let mut fingerprints = BTreeMap::new();
    for world in &worlds {
        for decision in &world.decisions {
            for (k, v) in &decision.fingerprints {
                fingerprints.insert(k.clone(), v.clone());
            }
        }
    }
    let text = format!(
        "izakaya_replay - policy {} profile {}\nmode: historical_supported_replay\nbeta_sweep: false\nsupport: {:.3}\nholdout_worlds: {}\npromotion_ok: {promotion_ok}\nout_of_support: {}\ncensored_episodes: {}\nreport_hash: {report_hash}\nincumbent_trace: {}\ncandidate_trace: {}\n",
        spec.id,
        profile.id,
        full_cand.support,
        holdout.len(),
        full_cand.out_of_support,
        full_cand.censored,
        full_inc.traces.join(" > "),
        full_cand.traces.join(" > ")
    );
    Ok(json!({
        "text": text,
        "policy_id": spec.id,
        "profile": profile.id,
        "mode": "historical_supported_replay",
        "beta": spec.beta,
        "beta_sweep": false,
        "support": full_cand.support,
        "holdout_support": hold_cand.support,
        "out_of_support": full_cand.out_of_support,
        "censored_episodes": full_cand.censored_ids,
        "promotion_ok": promotion_ok,
        "report_hash": report_hash,
        "train_worlds": train.len(),
        "holdout_worlds": holdout.len(),
        "task_holdout_worlds": task_holdout.len(),
        "gates_ok": hold_gates && task_gates,
        "pareto": {
            "winner": Value::Null,
            "note": "no single winner; compare axes",
            "candidate": full_cand.pareto,
            "incumbent": full_inc.pareto
        },
        "fingerprints": fingerprints,
        "agents_seen": worlds.iter().map(|w| w.agent_ids.len()).sum::<usize>(),
        "objectives": profile.objectives.iter().map(|o| json!({
            "metric": o.metric,
            "direction": o.direction,
            "weight": o.weight
        })).collect::<Vec<_>>(),
        "wordkeep": env!("CARGO_PKG_VERSION"),
        "traces": full_cand.traces,
        "revealed_actions": full_cand.revealed_actions,
    }))
}

struct Agg {
    support: f64,
    out_of_support: u64,
    censored: u64,
    censored_ids: Vec<String>,
    traces: Vec<String>,
    revealed_actions: Vec<String>,
    gate_mins: BTreeMap<String, f64>,
    pareto: Value,
}

fn replay_set(
    worlds: &[World],
    spec: &Spec,
    outcomes: &BTreeMap<(String, String), Outcome>,
) -> Agg {
    let mut in_support = 0u64;
    let mut out_of_support = 0u64;
    let mut censored = 0u64;
    let mut censored_ids = Vec::new();
    let mut traces = Vec::new();
    let mut revealed_actions = Vec::new();
    let mut gate_mins: BTreeMap<String, f64> = BTreeMap::new();
    let mut cost = 0.0f64;
    let mut conflicts = 0.0f64;
    let mut critical = 0.0f64;
    let mut quality = 0.0f64;
    for world in worlds {
        if world.censored {
            censored += 1;
            censored_ids.push(world.episode_id.clone());
            continue;
        }
        let played = replay_world(world, spec, outcomes);
        in_support += played.in_support;
        out_of_support += played.out_of_support;
        traces.extend(played.trace);
        for (action, metrics) in played.revealed {
            revealed_actions.push(action);
            for (k, v) in metrics {
                gate_mins
                    .entry(k.clone())
                    .and_modify(|cur| *cur = cur.min(v))
                    .or_insert(v);
                if k == "cost" {
                    cost += v;
                }
                if k == "conflicts" {
                    conflicts += v;
                }
                if k == "critical_path" {
                    critical = critical.max(v);
                }
                if k == "score" {
                    quality = quality.max(v);
                }
            }
        }
    }
    let total = in_support + out_of_support;
    let support = if total == 0 {
        1.0
    } else {
        in_support as f64 / total as f64
    };
    Agg {
        support,
        out_of_support,
        censored,
        censored_ids,
        traces,
        revealed_actions,
        gate_mins,
        pareto: json!({
            "quality": quality,
            "cost": cost,
            "conflicts": conflicts,
            "critical_path": critical
        }),
    }
}

struct Played {
    in_support: u64,
    out_of_support: u64,
    trace: Vec<String>,
    revealed: Vec<(String, BTreeMap<String, f64>)>,
}

fn replay_world(
    world: &World,
    spec: &Spec,
    outcomes: &BTreeMap<(String, String), Outcome>,
) -> Played {
    let mut played = Played {
        in_support: 0,
        out_of_support: 0,
        trace: Vec::new(),
        revealed: Vec::new(),
    };
    let mut rounds = 0usize;
    for decision in &world.decisions {
        if rounds >= spec.max_rounds {
            break;
        }
        let batch = if spec.selection == "recorded" {
            decision
                .legal
                .iter()
                .filter(|a| decision.selected.iter().any(|s| s == &a.id))
                .cloned()
                .collect::<Vec<_>>()
        } else {
            choose_batch(decision, spec)
        };
        if batch.is_empty() {
            break;
        }
        rounds += 1;
        for action in batch {
            let supported = decision.selected.iter().any(|s| s == &action.id);
            if supported {
                played.in_support += 1;
                played.trace.push(format!("{}:{}", decision.id, action.id));
                if let Some(outcome) = outcomes.get(&(decision.id.clone(), action.id.clone())) {
                    if !outcome.censored {
                        played
                            .revealed
                            .push((action.id.clone(), outcome.metrics.clone()));
                    }
                }
            } else {
                played.out_of_support += 1;
                played
                    .trace
                    .push(format!("{}:{}:out_of_support", decision.id, action.id));
            }
        }
    }
    played
}

fn choose_batch(decision: &Decision, spec: &Spec) -> Vec<Action> {
    let mut ranked = decision.legal.clone();
    ranked.sort_by(|a, b| {
        score(b, spec)
            .partial_cmp(&score(a, spec))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.id.cmp(&b.id))
    });
    let mut batch = Vec::new();
    for action in ranked {
        if score(&action, spec) < spec.min_score {
            continue;
        }
        if batch.len() >= spec.max_batch {
            break;
        }
        if batch.iter().all(|other| independent(other, &action)) {
            batch.push(action);
        }
    }
    batch
}

fn score(action: &Action, spec: &Spec) -> f64 {
    let mut total = 0.0;
    for (feature, weight) in &spec.prefer {
        let hit = match feature.as_str() {
            "non_overlapping" => !action.overlapping,
            "continue" => action.kind == "continue",
            "root" => action.kind == "root",
            "handoff" => action.kind == "handoff",
            _ => false,
        };
        if hit {
            total += weight;
        }
    }
    total * spec.beta
}

fn independent(a: &Action, b: &Action) -> bool {
    if a.causal_parents.iter().any(|p| p == &b.id) || b.causal_parents.iter().any(|p| p == &a.id) {
        return false;
    }
    !a.paths
        .iter()
        .any(|p| b.paths.iter().any(|q| model::paths_overlap(p, q)))
}

fn gates_ok(candidate: &Agg, incumbent: &Agg, gates: &[String]) -> bool {
    for gate in gates {
        match (candidate.gate_mins.get(gate), incumbent.gate_mins.get(gate)) {
            (Some(c), Some(i)) if *c + 1e-9 < *i => return false,
            (None, Some(_)) => return false,
            _ => {}
        }
    }
    true
}

fn split_holdout(worlds: &[World]) -> (Vec<World>, Vec<World>) {
    let mut worlds = worlds.to_vec();
    worlds.sort_by(|a, b| a.ts.cmp(&b.ts).then(a.episode_id.cmp(&b.episode_id)));
    let n = worlds.len();
    if n < 2 {
        return (worlds, Vec::new());
    }
    let hold_n = ((n as f64) * 0.2).ceil() as usize;
    let hold_n = hold_n.clamp(1, n - 1);
    let split = n - hold_n;
    let holdout = worlds.split_off(split);
    (worlds, holdout)
}

fn task_holdout(worlds: &[World]) -> Vec<World> {
    let tasks: BTreeSet<_> = worlds
        .iter()
        .map(|w| w.task.clone())
        .filter(|t| !t.is_empty())
        .collect();
    if tasks.len() < 2 {
        return Vec::new();
    }
    let held = tasks.iter().next_back().cloned().unwrap_or_default();
    worlds.iter().filter(|w| w.task == held).cloned().collect()
}

fn compile_worlds(events: &[Event], proj: &Projection) -> Vec<World> {
    let mut grouped: BTreeMap<String, Vec<Decision>> = BTreeMap::new();
    for event in events {
        if event.kind != "decision" {
            continue;
        }
        let Some(decision) = decision_from(event) else {
            continue;
        };
        grouped
            .entry(decision.episode_id.clone())
            .or_default()
            .push(decision);
    }
    let mut worlds = Vec::new();
    for (episode_id, mut decisions) in grouped {
        decisions.sort_by_key(|d| d.seq);
        let ts = decisions.first().map(|d| d.ts).unwrap_or(0);
        let task = decisions
            .iter()
            .find(|d| !d.task.is_empty())
            .map(|d| d.task.clone())
            .unwrap_or_default();
        let agent_ids: BTreeSet<_> = decisions.iter().map(|d| d.agent_id.clone()).collect();
        let censored = agent_ids.iter().any(|id| {
            proj.agents
                .get(id)
                .map(|a| a.state != AgentState::CheckedOut.as_str())
                .unwrap_or(true)
        });
        worlds.push(World {
            episode_id,
            task,
            ts,
            agent_ids,
            decisions,
            censored,
        });
    }
    worlds.sort_by(|a, b| a.ts.cmp(&b.ts).then(a.episode_id.cmp(&b.episode_id)));
    worlds
}

fn decision_from(event: &Event) -> Option<Decision> {
    let body = &event.body;
    let legal = body
        .get("legal_actions")?
        .as_array()?
        .iter()
        .filter_map(|item| {
            Some(Action {
                id: item.get("id").and_then(Value::as_str)?.to_string(),
                overlapping: item
                    .get("overlapping")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                kind: item
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("continue")
                    .to_string(),
                paths: strings(item, "paths"),
                causal_parents: strings(item, "causal_parents"),
            })
        })
        .collect::<Vec<_>>();
    let fingerprints = body
        .get("fingerprints")
        .and_then(Value::as_object)
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    Some(Decision {
        seq: event.seq,
        id: body.get("decision_id").and_then(Value::as_str)?.to_string(),
        agent_id: event.agent_id.clone(),
        episode_id: body
            .get("episode_id")
            .and_then(Value::as_str)
            .unwrap_or("episode")
            .to_string(),
        task: body
            .get("task")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        legal,
        selected: strings(body, "selected"),
        fingerprints,
        ts: event.ts,
    })
}

fn compile_outcomes(events: &[Event]) -> BTreeMap<(String, String), Outcome> {
    let mut out = BTreeMap::new();
    for event in events {
        if event.kind != "outcome" {
            continue;
        }
        let Some(decision) = event.body.get("decision_id").and_then(Value::as_str) else {
            continue;
        };
        let Some(action) = event.body.get("action").and_then(Value::as_str) else {
            continue;
        };
        let mut metrics = BTreeMap::new();
        if let Some(obj) = event.body.get("metrics").and_then(Value::as_object) {
            for (k, v) in obj {
                if let Some(n) = v.as_f64() {
                    metrics.insert(k.clone(), n);
                }
            }
        }
        out.insert(
            (decision.to_string(), action.to_string()),
            Outcome {
                metrics,
                censored: event
                    .body
                    .get("censored")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            },
        );
    }
    out
}

fn strings(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::izakaya::presence;
    use serde_json::json;

    fn isolated(name: &str) -> std::path::PathBuf {
        let cache =
            std::env::temp_dir().join(format!("wk_iza_r_cache_{name}_{}", std::process::id()));
        let root =
            std::env::temp_dir().join(format!("wk_iza_r_root_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("XDG_CACHE_HOME", &cache);
        std::env::set_var("WORDKEEP_IZAKAYA_NOW", "5000");
        root
    }

    fn close(root: &Path) {
        std::env::remove_var("XDG_CACHE_HOME");
        std::env::remove_var("WORDKEEP_IZAKAYA_NOW");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn incumbent_reproduces_and_hides_unselected_outcomes() {
        let _g = crate::cache::test_env_lock();
        let root = isolated("inc");
        let inn =
            presence::check_in(&root, &json!({"agent_id": "alpha", "observe_git": false})).unwrap();
        let lease = inn["lease_id"].as_str().unwrap();
        let legal = json!([
            {"id": "a1", "overlapping": true, "kind": "continue", "paths": ["src/a.rs"]},
            {"id": "secret", "overlapping": false, "kind": "root", "paths": ["src/b.rs"]}
        ]);
        presence::record_decision(
            &root,
            &json!({
                "agent_id": "alpha", "lease_id": lease, "episode_id": "ep1", "decision_id": "d1",
                "legal_actions": legal, "selected": ["a1"],
                "fingerprints": {"model": "test", "evaluator": "gates"},
                "observe_git": false
            }),
        )
        .unwrap();
        presence::record_outcome(
            &root,
            &json!({
                "agent_id": "alpha", "lease_id": lease, "decision_id": "d1", "action": "a1",
                "metrics": {"correctness": 1, "integration": 1, "score": 3}
            }),
        )
        .unwrap();
        presence::record_outcome(
            &root,
            &json!({
                "agent_id": "alpha", "lease_id": lease, "decision_id": "d1", "action": "secret",
                "metrics": {"future_secret": 12345, "correctness": 0}
            }),
        )
        .unwrap();
        presence::check_out(
            &root,
            &json!({"agent_id": "alpha", "lease_id": lease, "reason": "completed"}),
        )
        .unwrap();
        let report = evaluate(&root, &incumbent_spec(), "coordination").unwrap();
        let text = report["text"].as_str().unwrap().to_string();
        assert!(text.contains("d1:a1"), "{text}");
        assert!(!text.contains("future_secret"), "{text}");
        assert!(
            !report["revealed_actions"].to_string().contains("secret"),
            "{report}"
        );
        let again = evaluate(&root, &incumbent_spec(), "coordination").unwrap();
        assert_eq!(report["report_hash"], again["report_hash"]);
        assert_eq!(report["beta_sweep"], false);
        close(&root);
    }

    #[test]
    fn unsupported_action_gets_no_reward_and_open_work_is_censored() {
        let _g = crate::cache::test_env_lock();
        let root = isolated("oos");
        let inn =
            presence::check_in(&root, &json!({"agent_id": "alpha", "observe_git": false})).unwrap();
        let lease = inn["lease_id"].as_str().unwrap();
        presence::record_decision(&root, &json!({
            "agent_id": "alpha", "lease_id": lease, "episode_id": "ep-open", "decision_id": "d9",
            "legal_actions": [{"id": "keep", "overlapping": true, "kind": "continue"}, {"id": "leap", "overlapping": false, "kind": "root"}],
            "selected": ["keep"]
        })).unwrap();
        presence::record_outcome(
            &root,
            &json!({
                "agent_id": "alpha", "lease_id": lease, "decision_id": "d9", "action": "leap",
                "metrics": {"future_secret": 99, "correctness": 1}
            }),
        )
        .unwrap();
        let open = evaluate(
            &root,
            &json!({
                "id": "avoid-overlap",
                "version": 1,
                "selection": "score",
                "max_batch": 1,
                "prefer": [{"feature": "non_overlapping", "weight": 5}]
            }),
            "coordination",
        )
        .unwrap();
        assert!(
            open["censored_episodes"].to_string().contains("ep-open"),
            "{open}"
        );
        assert!(!open.to_string().contains("future_secret"), "{open}");
        presence::check_out(&root, &json!({"agent_id": "alpha", "lease_id": lease})).unwrap();
        let closed = evaluate(
            &root,
            &json!({
                "id": "avoid-overlap",
                "version": 1,
                "selection": "score",
                "max_batch": 1,
                "prefer": [{"feature": "non_overlapping", "weight": 5}]
            }),
            "coordination",
        )
        .unwrap();
        let blob = closed.to_string();
        assert!(blob.contains("out_of_support"), "{blob}");
        assert!(!blob.contains("future_secret"), "{blob}");
        assert_eq!(closed["promotion_ok"], false);
        close(&root);
    }
}
