//! Host-agnostic MCP surface for Ereshkigal decrees (`.esk` / TOML libraries).
//!
//! `check` / `lint` run in-process via `ereshkigal-lang`. `decide` uses Wordkeep's
//! shared resolved SemIf scorer by default (no duplicate unconfigured serve child).
//! `run` / `test` / `stats` still talk to `ereshkigal serve --stdio` when needed;
//! set `semif.decree_subprocess: true` to force subprocess for `decide` too.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;
use std::time::Instant;

use crate::config;
use crate::semif::{self, DecisionRequest, OptionSpec};
use crate::semif_telemetry;
use crate::stats;

static BRIDGE: Mutex<Option<Bridge>> = Mutex::new(None);

struct Bridge {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

fn decrees_dir(root: &Path) -> Result<PathBuf, String> {
    config::semif_decrees_path(root).ok_or_else(|| {
        "no decrees library: set semif.decrees in .wordkeep/config.json or pass lib".into()
    })
}

fn resolve_lib(root: &Path, args: &Value) -> Result<PathBuf, String> {
    if let Some(p) = args.get("lib").and_then(Value::as_str) {
        let path = PathBuf::from(p);
        if path.is_absolute() {
            return Ok(path);
        }
        return Ok(root.join(path));
    }
    decrees_dir(root)
}

pub fn dispatch(root: &Path, args: &Value) -> Result<String, String> {
    let t0 = Instant::now();
    let method = args
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("check");
    // Feedback journal does not need a decrees library.
    if method == "feedback" {
        let result = feedback_journal(root, args);
        let elapsed_us = t0.elapsed().as_micros() as u64;
        let text_result = match &result {
            Ok(v) => Ok(serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".into())),
            Err(e) => Err(e.clone()),
        };
        let returned = match &text_result {
            Ok(t) => (t.len() as u64).div_ceil(4).max(1),
            Err(_) => 0,
        };
        stats::record_ext(
            "decree",
            stats::RecordMeta {
                baseline: 64,
                returned,
                bytes_read: 0,
                cache_hits: 0,
                cache_misses: 0,
                reason: None,
            },
        );
        let outcome = stats::classify_outcome(&text_result, returned);
        semif_telemetry::record_event(json!({
            "kind": "decree",
            "method": method,
            "lib": Value::Null,
            "timing_us": elapsed_us,
            "outcome": outcome.as_str(),
            "returned_tokens": returned,
        }));
        return text_result;
    }
    let lib = resolve_lib(root, args)?;
    let result = match method {
        "check" => {
            let library = ereshkigal_lang::Library::load(&lib).map_err(|e| e.to_string())?;
            Ok(json!({
                "ok": true,
                "lib": lib.display().to_string(),
                "decrees": library.decrees.len(),
                "programs": library.programs.len(),
                "recipe": library.recipe,
            }))
        }
        "lint" => {
            let library = ereshkigal_lang::Library::load(&lib).map_err(|e| e.to_string())?;
            ereshkigal_lang::lint::lint_library(&library).map_err(|e| e.to_string())?;
            Ok(json!({"ok": true, "lib": lib.display().to_string()}))
        }
        "schema" => {
            let schema = ereshkigal_lang::library_schema_json().map_err(|e| e.to_string())?;
            Ok(json!({
                "ok": true,
                "lib": lib.display().to_string(),
                "schema": serde_json::from_str::<Value>(&schema).unwrap_or(json!(schema)),
            }))
        }
        "decide" if !config::semif_decree_subprocess(root) => decide_shared(root, &lib, args),
        "stats" | "run" | "test" | "decide" => rpc(root, &lib, method, args),
        other => Err(format!(
            "unknown decree method {other}; use check, lint, schema, run, test, decide, stats, feedback"
        )),
    };
    let elapsed_us = t0.elapsed().as_micros() as u64;
    let text_result = match &result {
        Ok(v) => Ok(serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".into())),
        Err(e) => Err(e.clone()),
    };
    let returned = match &text_result {
        Ok(t) => (t.len() as u64).div_ceil(4).max(1),
        Err(_) => 0,
    };
    stats::record_ext(
        "decree",
        stats::RecordMeta {
            baseline: 64,
            returned,
            bytes_read: 0,
            cache_hits: 0,
            cache_misses: 0,
            reason: None,
        },
    );
    // finish-style classify for events.jsonl honesty
    let outcome = stats::classify_outcome(&text_result, returned);
    semif_telemetry::record_event(json!({
        "kind": "decree",
        "method": method,
        "lib": lib.display().to_string(),
        "timing_us": elapsed_us,
        "outcome": outcome.as_str(),
        "returned_tokens": returned,
    }));
    // Mirror instrumented finish for MCP events.jsonl when decree is called directly.
    let _ = elapsed_us;
    text_result
}

fn decide_shared(root: &Path, lib: &Path, args: &Value) -> Result<Value, String> {
    let library = ereshkigal_lang::Library::load(lib).map_err(|e| e.to_string())?;
    let name = args
        .get("decree")
        .or_else(|| args.get("name"))
        .and_then(Value::as_str)
        .ok_or_else(|| "decide requires decree name".to_string())?;
    let decree = library
        .decree(name)
        .map_err(|e| e.to_string())?;
    let state = args.get("state").cloned().unwrap_or(json!(""));
    let state_str = match &state {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    };
    let options: Vec<OptionSpec> = decree
        .options
        .iter()
        .map(|o| OptionSpec {
            id: o.id.clone(),
            description: o.description.clone(),
        })
        .collect();
    if options.is_empty() {
        return Err(format!("decree {name} has no options"));
    }
    let req = DecisionRequest {
        id: format!("{name}-live"),
        state: state_str,
        question: decree.question.clone(),
        options,
        visual_evidence: None,
    };
    let body = json!({
        "id": req.id,
        "state": state,
        "question": decree.question,
        "options": decree.options.iter().map(|o| json!({
            "id": o.id,
            "description": o.description,
        })).collect::<Vec<_>>(),
    });
    // Ensure semif.enabled for decide path.
    let text = semif::decide(root, &body).or_else(|e| {
        // Heuristic air-gap when semif disabled or backend missing.
        let (scorer, unknown) = match semif::resolve_scorer(root) {
            semif::ResolvedScorer::Ready(s, u) => (s, u),
            semif::ResolvedScorer::Unavailable(_) => {
                return Err(e);
            }
        };
        let decided = semif::decide_one(scorer.as_ref(), &req, unknown);
        Ok(serde_json::to_string_pretty(&json!({
            "chosen": decided.chosen,
            "options": decided.options.iter().map(|o| json!({
                "id": o.id,
                "probability": o.probability,
                "raw_score": o.raw_score,
            })).collect::<Vec<_>>(),
            "prompt_sha256": decided.prompt_sha256,
            "scorer": decided.scorer,
            "scorer_revision": decided.scorer_revision,
            "timing_us": decided.timing_us,
            "calibrated": decided.calibrated,
            "fallback": decided.fallback,
            "cascade_source": decided.cascade_source,
            "abstain": decided.abstain,
        }))
        .unwrap_or_else(|_| "{}".into()))
    })?;
    let mut v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if let Some(obj) = v.as_object_mut() {
        obj.insert("ok".into(), json!(true));
        obj.insert("decree".into(), json!(name));
        obj.insert("shared_scorer".into(), json!(true));
    }
    Ok(v)
}

fn feedback_journal(root: &Path, args: &Value) -> Result<Value, String> {
    if crate::tacet::is_enabled(root) {
        return Ok(json!({"ok": true, "tacet_suppressed": true}));
    }
    let path = root.join(".wordkeep/ereshkigal-feedback.jsonl");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let row = json!({
        "ts": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        "prompt_sha256": args.get("prompt_sha256"),
        "chosen": args.get("chosen"),
        "outcome": args.get("outcome"),
        "override_id": args.get("override_id"),
        "decree": args.get("decree"),
    });
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("feedback journal: {e}"))?;
    writeln!(f, "{}", serde_json::to_string(&row).map_err(|e| e.to_string())?)
        .map_err(|e| format!("feedback write: {e}"))?;
    Ok(json!({"ok": true, "path": path.display().to_string()}))
}

fn rpc(root: &Path, lib: &Path, method: &str, args: &Value) -> Result<Value, String> {
    let mut g = BRIDGE.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        *g = Some(spawn(root, lib)?);
    }
    let br = g.as_mut().unwrap();
    let id = br.next_id;
    br.next_id += 1;
    let mut params = args.clone();
    if let Some(obj) = params.as_object_mut() {
        obj.remove("method");
        obj.remove("lib");
    }
    let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    writeln!(br.stdin, "{}", serde_json::to_string(&req).map_err(|e| e.to_string())?)
        .map_err(|e| format!("serve stdin: {e}"))?;
    br.stdin.flush().map_err(|e| format!("serve flush: {e}"))?;
    let mut line = String::new();
    br.stdout
        .read_line(&mut line)
        .map_err(|e| format!("serve stdout: {e}"))?;
    let resp: Value = serde_json::from_str(&line).map_err(|e| format!("serve json: {e}"))?;
    if let Some(err) = resp.get("error") {
        return Err(err.to_string());
    }
    Ok(resp.get("result").cloned().unwrap_or(resp))
}

fn spawn(root: &Path, lib: &Path) -> Result<Bridge, String> {
    let bin = serve_bin(root)?;
    let mut cmd = Command::new(&bin);
    cmd.arg("serve")
        .arg("--stdio")
        .arg("--lib")
        .arg(lib)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(gguf) = config::semif_gguf_path(root) {
        cmd.arg("--gguf").arg(gguf);
    }
    let mut child = cmd.spawn().map_err(|e| {
        format!(
            "failed to spawn {} (build tools/ereshkigal or set ERESHKIGAL_BIN): {e}",
            bin.display()
        )
    })?;
    let stdin = child.stdin.take().ok_or("serve stdin missing")?;
    let stdout = child.stdout.take().ok_or("serve stdout missing")?;
    Ok(Bridge {
        _child: child,
        stdin,
        stdout: BufReader::new(stdout),
        next_id: 1,
    })
}

fn serve_bin(root: &Path) -> Result<PathBuf, String> {
    if let Some(p) = config::semif_serve_bin(root) {
        if !p.is_file() && p.as_os_str() != "ereshkigal" {
            eprintln!(
                "[wordkeep] decree serve_bin not found at {} (build tools/ereshkigal target-vulkan or set ERESHKIGAL_BIN)",
                p.display()
            );
        }
        return Ok(p);
    }
    Ok(PathBuf::from("ereshkigal"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "wk_decree_{}_{}",
            tag,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(p.join(".wordkeep")).unwrap();
        p
    }

    #[test]
    fn check_lint_and_schema_std_decrees() {
        let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let lib = crate_dir.join("../ereshkigal/decrees");
        if !lib.is_dir() {
            return;
        }
        let root = tmp("check");
        fs::write(
            root.join(".wordkeep/config.json"),
            format!(r#"{{"semif":{{"decrees":{}}}}}"#, json!(lib.display().to_string())),
        )
        .unwrap();
        let text = dispatch(&root, &json!({"method": "check"})).unwrap();
        assert!(text.contains("\"ok\": true"), "{text}");
        let lint = dispatch(&root, &json!({"method": "lint"})).unwrap();
        assert!(lint.contains("\"ok\": true"), "{lint}");
        let schema = dispatch(&root, &json!({"method": "schema"})).unwrap();
        assert!(schema.contains("\"ok\": true"), "{schema}");
        assert!(
            schema.contains("decree") || schema.contains("schema"),
            "{schema}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn decree_response_classifies_ok_with_returned_tokens() {
        let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let lib = crate_dir.join("../ereshkigal/decrees");
        if !lib.is_dir() {
            return;
        }
        let root = tmp("telemetry");
        fs::write(
            root.join(".wordkeep/config.json"),
            format!(r#"{{"semif":{{"decrees":{}}}}}"#, json!(lib.display().to_string())),
        )
        .unwrap();
        let text = dispatch(&root, &json!({"method": "check"})).unwrap();
        let returned = (text.len() as u64).div_ceil(4).max(1);
        let outcome = crate::stats::classify_outcome(&Ok(text.clone()), returned);
        assert_eq!(
            outcome,
            crate::stats::Outcome::Ok,
            "non-empty decree JSON must not classify empty (returned={returned})"
        );
        assert_eq!(
            crate::stats::classify_outcome(&Ok(text), 0),
            crate::stats::Outcome::Empty,
            "document: missing stats::record → returned=0 → Empty"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn decide_shared_heuristic_non_empty() {
        let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let lib = crate_dir.join("../ereshkigal/decrees");
        if !lib.is_dir() {
            return;
        }
        let root = tmp("decide_shared");
        fs::write(
            root.join(".wordkeep/config.json"),
            format!(
                r#"{{"semif":{{"enabled":true,"backend":"heuristic","decrees":{}}}}}"#,
                json!(lib.display().to_string())
            ),
        )
        .unwrap();
        // Pick first enum decree from library.
        let library = ereshkigal_lang::Library::load(&lib).unwrap();
        let Some(d) = library.decrees.values().find(|d| !d.options.is_empty()) else {
            return;
        };
        let text = dispatch(
            &root,
            &json!({
                "method": "decide",
                "decree": d.name,
                "state": {"note": "shared scorer path smoke"},
            }),
        )
        .expect("decide shared");
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["shared_scorer"], true, "{text}");
        assert!(
            v["chosen"].as_str().map(|s| !s.is_empty()).unwrap_or(false) || v["abstain"] == true,
            "{text}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn feedback_journal_appends_row() {
        let root = tmp("feedback_ok");
        let text = dispatch(
            &root,
            &json!({
                "method": "feedback",
                "prompt_sha256": "deadbeef",
                "chosen": "promote",
                "outcome": "accept",
            }),
        )
        .expect("feedback");
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["ok"], true);
        let path = root.join(".wordkeep/ereshkigal-feedback.jsonl");
        let body = fs::read_to_string(&path).expect("journal");
        assert!(body.contains("deadbeef"));
        assert!(body.contains("promote"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn feedback_journal_respects_tacet() {
        let root = tmp("feedback_tacet");
        crate::tacet::save(
            &root,
            &crate::tacet::State {
                enabled: true,
                mode: crate::tacet::Mode::Session,
                updated_secs: 1,
            },
        )
        .unwrap();
        let text = dispatch(
            &root,
            &json!({
                "method": "feedback",
                "prompt_sha256": "should-not-write",
                "chosen": "hold",
                "outcome": "reject",
            }),
        )
        .expect("tacet feedback");
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["tacet_suppressed"], true);
        let path = root.join(".wordkeep/ereshkigal-feedback.jsonl");
        assert!(!path.exists(), "tacet must not create feedback journal");
        let _ = fs::remove_dir_all(&root);
    }
}
