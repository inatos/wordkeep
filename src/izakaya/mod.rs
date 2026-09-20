//! Izakaya: local agent presence plus an offline Dream-RSI-style replay lab.
//!
//! Presence is authoritative. Replay advice is read-only and cannot check agents
//! out, move Git refs, or turn advisory claims into locks.

mod model;
mod policy;
mod presence;
mod profiles;
mod replay;
mod store;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::mcp;

const HELP: &str = "\
wordkeep izakaya status [--agent ID] [--state checked_in|live_code|suspended|checked_out|stale] [--format json]
wordkeep izakaya check-in --agent ID [--task TEXT] [--claim PATH] [--force] [--no-git]
wordkeep izakaya update --agent ID --lease LEASE [--state live_code|suspended|checked_in] [--checkpoint REF]
wordkeep izakaya check-out --agent ID --lease LEASE [--reason completed|handed_off|abandoned] [--handoff-to ID]
wordkeep izakaya policy list
wordkeep izakaya policy evaluate --spec FILE [--profile ID]
wordkeep izakaya policy promote --spec FILE [--profile ID]
wordkeep izakaya policy retire --id POLICY
";

pub fn cli(root: &Path, argv: &[String]) -> Result<String, String> {
    if argv.is_empty() || argv[0] == "--help" || argv[0] == "help" {
        return Ok(HELP.to_string());
    }
    match argv[0].as_str() {
        "status" | "check-in" | "update" | "check-out" => {
            let mut args = parse_flags(&argv[1..])?;
            if argv[0] == "check-out"
                && (args.get("handoff_to").is_some() || args.get("to").is_some())
            {
                args["handoff"] = json!({
                    "to": args.get("handoff_to").cloned().unwrap_or(json!("")),
                    "summary": args.get("summary").cloned().unwrap_or(json!("")),
                    "mas_session": args.get("mas_session").cloned(),
                    "checkpoint": args.get("checkpoint").cloned(),
                });
                args["reason"] = json!("handed_off");
            }
            let format = args
                .get("format")
                .and_then(Value::as_str)
                .map(str::to_string);
            let value = match argv[0].as_str() {
                "status" => presence::status(root, &args)?,
                "check-in" => presence::check_in(root, &args)?,
                "update" => presence::update(root, &args)?,
                _ => presence::check_out(root, &args)?,
            };
            render(value, format.as_deref())
        }
        "policy" => policy_cli(root, &argv[1..]),
        other => Err(format!("unknown izakaya command {other}\n{HELP}")),
    }
}

fn policy_cli(root: &Path, argv: &[String]) -> Result<String, String> {
    if argv.is_empty() {
        return Err(HELP.into());
    }
    match argv[0].as_str() {
        "list" => render(policy::list(root)?, None),
        "evaluate" | "promote" => {
            let args = parse_flags(&argv[1..])?;
            let format = args
                .get("format")
                .and_then(Value::as_str)
                .map(str::to_string);
            let spec_path = args
                .get("spec")
                .and_then(Value::as_str)
                .ok_or("policy evaluate/promote requires --spec FILE")?;
            let spec = read_spec(root, spec_path)?;
            let profile = args
                .get("profile")
                .and_then(Value::as_str)
                .unwrap_or("coordination");
            let value = if argv[0] == "evaluate" {
                policy::evaluate(root, &spec, profile)?
            } else {
                policy::promote(root, &spec, profile)?
            };
            render(value, format.as_deref())
        }
        "retire" => {
            let args = parse_flags(&argv[1..])?;
            let format = args
                .get("format")
                .and_then(Value::as_str)
                .map(str::to_string);
            let id = args
                .get("id")
                .and_then(Value::as_str)
                .ok_or("policy retire requires --id")?;
            render(policy::retire(root, id)?, format.as_deref())
        }
        other => Err(format!("unknown policy command {other}")),
    }
}

pub fn mcp_tools(root: PathBuf) -> Vec<mcp::Tool> {
    vec![
        tool(&root, "izakaya_status", "Read-only Izakaya board: active agents, stale leases, advisory overlaps, handoffs, and messages. Does not create state. Filter with agent, state, path, since_seq. format text|json.", json!({"type":"object","properties":{
            "agent":{"type":"string"},
            "state":{"type":"string","description":"checked_in|live_code|suspended|checked_out|stale"},
            "path":{"type":"string"},
            "since_seq":{"type":"integer"},
            "include_checked_out":{"type":"boolean"},
            "format":{"type":"string","enum":["text","json"]},
            "token_budget":{"type":"integer"}
        }})),
        tool(&root, "izakaya_check_in", "Check in before the first live edit, or resume with lease_id. Returns a lease. Claims are advisory. Optional handoff_id accepts an offered handoff. Does not finalize MAS sessions.", json!({"type":"object","properties":{
            "agent_id":{"type":"string"},
            "role":{"type":"string"},
            "task":{"type":"string"},
            "summary":{"type":"string"},
            "claims":{"type":"array","items":{"type":"object","properties":{"path":{"type":"string"},"symbols":{"type":"array","items":{"type":"string"}},"intent":{"type":"string"}},"additionalProperties":true}},
            "mas_session":{"type":"string"},
            "handoff_id":{"type":"string"},
            "lease_id":{"type":"string"},
            "force":{"type":"boolean"},
            "ttl_secs":{"type":"integer"},
            "idempotency_key":{"type":"string"},
            "expected_revision":{"type":"integer"},
            "observe_git":{"type":"boolean"},
            "format":{"type":"string"},
            "token_budget":{"type":"integer"}
        },"required":["agent_id"]})),
        tool(&root, "izakaya_update", "Heartbeat or move among checked_in, live_code, and suspended. Suspend of dirty work requires checkpoint. Optional note is a directed message. Requires lease_id.", json!({"type":"object","properties":{
            "agent_id":{"type":"string"},
            "lease_id":{"type":"string"},
            "state":{"type":"string"},
            "summary":{"type":"string"},
            "claims":{"type":"array","items":{"type":"object","properties":{"path":{"type":"string"},"symbols":{"type":"array","items":{"type":"string"}},"intent":{"type":"string"}},"additionalProperties":true}},
            "checkpoint":{"type":"string"},
            "dirty_paths":{"type":"array","items":{"type":"string"}},
            "note":{"type":"object","additionalProperties":true},
            "ack_seqs":{"type":"array","items":{"type":"integer"}},
            "idempotency_key":{"type":"string"},
            "expected_revision":{"type":"integer"},
            "observe_git":{"type":"boolean"},
            "format":{"type":"string"},
            "token_budget":{"type":"integer"}
        },"required":["agent_id","lease_id"]})),
        tool(&root, "izakaya_check_out", "Release claims and close this incarnation. reason completed|handed_off|abandoned. handed_off requires a handoff capsule referencing MAS/notes; Izakaya does not call mas_finalize. Idempotent if already checked out.", json!({"type":"object","properties":{
            "agent_id":{"type":"string"},
            "lease_id":{"type":"string"},
            "reason":{"type":"string"},
            "handoff":{"type":"object","additionalProperties":true},
            "idempotency_key":{"type":"string"},
            "expected_revision":{"type":"integer"},
            "format":{"type":"string"},
            "token_budget":{"type":"integer"}
        },"required":["agent_id","lease_id"]})),
        tool(&root, "izakaya_record_decision", "Record the frontier that was actually visible: legal_actions, selected batch, policy/profile fingerprints, and budget. Required before replay can treat history as supported.", json!({"type":"object","properties":{
            "agent_id":{"type":"string"},
            "lease_id":{"type":"string"},
            "episode_id":{"type":"string"},
            "decision_id":{"type":"string"},
            "legal_actions":{"type":"array","items":{"type":"object","properties":{"id":{"type":"string"},"kind":{"type":"string"},"overlapping":{"type":"boolean"}},"additionalProperties":true}},
            "selected":{"type":"array","items":{"type":"string"}},
            "profile":{"type":"string"},
            "policy_id":{"type":"string"},
            "fingerprints":{"type":"object","additionalProperties":true},
            "budget":{"type":"integer"},
            "workers":{"type":"integer"},
            "task":{"type":"string"},
            "observation_hash":{"type":"string"},
            "idempotency_key":{"type":"string"},
            "format":{"type":"string"},
            "token_budget":{"type":"integer"}
        },"required":["agent_id","lease_id","legal_actions","selected"]})),
        tool(&root, "izakaya_record_outcome", "Attach measured metrics to a recorded action. Links run_record ids. Does not execute commands. Suspended/unknown results should set censored:true.", json!({"type":"object","properties":{
            "agent_id":{"type":"string"},
            "lease_id":{"type":"string"},
            "decision_id":{"type":"string"},
            "action":{"type":"string"},
            "metrics":{"type":"object","additionalProperties":true},
            "censored":{"type":"boolean"},
            "run_ids":{"type":"array","items":{"type":"string"}},
            "artifacts":{"type":"array","items":{"type":"string"}},
            "profile":{"type":"string"},
            "fingerprints":{"type":"object","additionalProperties":true},
            "idempotency_key":{"type":"string"},
            "format":{"type":"string"},
            "token_budget":{"type":"integer"}
        },"required":["agent_id","lease_id","decision_id","action"]})),
        tool(&root, "izakaya_replay", "Evaluate a declarative exploration policy by historical supported replay. Reports support, censoring, holdout gates, and Pareto axes. Does not promote and does not invent outcomes for unrecorded actions.", json!({"type":"object","properties":{
            "policy":{"type":"object","description":"Declarative spec. Omit for the incumbent.","additionalProperties":true},
            "incumbent":{"type":"boolean"},
            "profile":{"type":"string"},
            "format":{"type":"string"},
            "token_budget":{"type":"integer"}
        }})),
        tool(&root, "izakaya_advise", "Apply the promoted advisory policy to current presence and return recommendations. Read-only: cannot assign, suspend, check out, or run git.", json!({"type":"object","properties":{
            "format":{"type":"string"},
            "token_budget":{"type":"integer"}
        }})),
    ]
}

fn tool(root: &Path, name: &'static str, description: &'static str, schema: Value) -> mcp::Tool {
    let root = root.to_path_buf();
    mcp::Tool {
        name,
        description,
        input_schema: schema,
        handler: Box::new(move |args| dispatch(name, &root, args)),
    }
}

fn dispatch(name: &str, root: &Path, args: &Value) -> Result<String, String> {
    let value = match name {
        "izakaya_status" => presence::status(root, args)?,
        "izakaya_check_in" => presence::check_in(root, args)?,
        "izakaya_update" => presence::update(root, args)?,
        "izakaya_check_out" => presence::check_out(root, args)?,
        "izakaya_record_decision" => presence::record_decision(root, args)?,
        "izakaya_record_outcome" => presence::record_outcome(root, args)?,
        "izakaya_replay" => {
            let profile = args
                .get("profile")
                .and_then(Value::as_str)
                .unwrap_or("coordination");
            let spec = if args
                .get("incumbent")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || args.get("policy").is_none()
            {
                replay::incumbent_spec()
            } else {
                args.get("policy").cloned().unwrap_or(Value::Null)
            };
            replay::evaluate(root, &spec, profile)?
        }
        "izakaya_advise" => policy::advise(root, args)?,
        other => return Err(format!("unknown izakaya tool {other}")),
    };
    let format = args.get("format").and_then(Value::as_str).unwrap_or("text");
    let mut text = if format == "json" {
        serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into())
    } else {
        value
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(1200) as usize;
    let max_chars = budget.saturating_mul(4).max(32);
    if text.len() > max_chars {
        let mut end = max_chars;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n…(truncated)\n");
    }
    crate::stats::record(name, 64, (text.len() / 4) as u64);
    Ok(text)
}

fn render(value: Value, format: Option<&str>) -> Result<String, String> {
    if format == Some("json") {
        return serde_json::to_string_pretty(&value).map_err(|e| e.to_string());
    }
    value
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "missing text".into())
}

fn read_spec(root: &Path, spec: &str) -> Result<Value, String> {
    let path = PathBuf::from(spec);
    let full = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    let raw =
        std::fs::read_to_string(&full).map_err(|e| format!("read {}: {e}", full.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", full.display()))
}

fn parse_flags(argv: &[String]) -> Result<Value, String> {
    let mut obj = serde_json::Map::new();
    let mut claims = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        let flag = argv[i].as_str();
        let take = |i: &mut usize| -> Result<String, String> {
            *i += 1;
            argv.get(*i)
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag {
            "--agent" => {
                obj.insert("agent_id".into(), json!(take(&mut i)?));
            }
            "--lease" => {
                obj.insert("lease_id".into(), json!(take(&mut i)?));
            }
            "--task" => {
                obj.insert("task".into(), json!(take(&mut i)?));
            }
            "--role" => {
                obj.insert("role".into(), json!(take(&mut i)?));
            }
            "--summary" => {
                obj.insert("summary".into(), json!(take(&mut i)?));
            }
            "--state" => {
                obj.insert("state".into(), json!(take(&mut i)?));
            }
            "--reason" => {
                obj.insert("reason".into(), json!(take(&mut i)?));
            }
            "--handoff-to" => {
                obj.insert("handoff_to".into(), json!(take(&mut i)?));
            }
            "--mas-session" => {
                obj.insert("mas_session".into(), json!(take(&mut i)?));
            }
            "--checkpoint" => {
                obj.insert("checkpoint".into(), json!(take(&mut i)?));
            }
            "--format" => {
                obj.insert("format".into(), json!(take(&mut i)?));
            }
            "--spec" => {
                obj.insert("spec".into(), json!(take(&mut i)?));
            }
            "--profile" => {
                obj.insert("profile".into(), json!(take(&mut i)?));
            }
            "--id" => {
                obj.insert("id".into(), json!(take(&mut i)?));
            }
            "--path" => {
                obj.insert("path".into(), json!(take(&mut i)?));
            }
            "--idempotency" => {
                obj.insert("idempotency_key".into(), json!(take(&mut i)?));
            }
            "--claim" => claims.push(json!(take(&mut i)?)),
            "--force" => {
                obj.insert("force".into(), json!(true));
            }
            "--no-git" => {
                obj.insert("observe_git".into(), json!(false));
            }
            "--include-checked-out" => {
                obj.insert("include_checked_out".into(), json!(true));
            }
            other => return Err(format!("unknown flag {other}")),
        }
        i += 1;
    }
    if !claims.is_empty() {
        obj.insert("claims".into(), Value::Array(claims));
    }
    Ok(Value::Object(obj))
}
