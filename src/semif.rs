//! SemIf-style semantic decisions for Wordkeep tooling.
//!
//! Mirrors the [SemIf/OpenJev](https://github.com/TheoLeeCJ/SemIf-OpenJev) request
//! shape (`state` + runtime `question` + typed `options` → probabilities) without
//! decoding an answer sentence. v1 ships a deterministic keyword-overlap
//! [`HeuristicScorer`]; GGUF / torch sidecars plug in via [`Scorer`] later.
//!
//! Host-agnostic MCP tooling — not a host application runtime.

use crate::{config, stats};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

const SCORER_NAME: &str = "heuristic";
const SCORER_REVISION: &str = "v1";
const HEAD_RERANK: usize = 24;

/// One typed option in a SemIf decision.
#[derive(Clone, Debug)]
pub struct OptionSpec {
    pub id: String,
    pub description: String,
}

/// Single decision request (SemIf row shape).
#[derive(Clone, Debug)]
pub struct DecisionRequest {
    pub id: String,
    pub state: String,
    pub question: String,
    pub options: Vec<OptionSpec>,
}

/// Per-option score in a decision response.
#[derive(Clone, Debug)]
pub struct OptionScore {
    pub id: String,
    pub probability: f64,
    pub raw_score: f64,
}

/// Auditable decision result.
#[derive(Clone, Debug)]
pub struct DecisionResult {
    pub id: String,
    pub options: Vec<OptionScore>,
    pub chosen: String,
    pub prompt_sha256: String,
    pub scorer: String,
    pub scorer_revision: String,
    pub timing_us: u64,
    pub calibrated: bool,
    pub fallback: bool,
}

/// Pluggable option scorer (direct logit readout in upstream SemIf; heuristic here).
pub trait Scorer: Send + Sync {
    fn name(&self) -> &'static str;
    fn revision(&self) -> &'static str;
    fn score_one(&self, req: &DecisionRequest) -> Result<Vec<(String, f64)>, String>;
    fn score_batch(&self, state: &str, batch: &[DecisionRequest]) -> Result<Vec<Vec<(String, f64)>>, String> {
        batch
            .iter()
            .map(|r| {
                let mut req = r.clone();
                if req.state.is_empty() {
                    req.state = state.to_string();
                }
                self.score_one(&req)
            })
            .collect()
    }
}

/// Deterministic keyword-overlap scorer. Softmax over option logits derived from
/// token overlap of each option (id + description) with state ∪ question.
pub struct HeuristicScorer;

impl Scorer for HeuristicScorer {
    fn name(&self) -> &'static str {
        SCORER_NAME
    }

    fn revision(&self) -> &'static str {
        SCORER_REVISION
    }

    fn score_one(&self, req: &DecisionRequest) -> Result<Vec<(String, f64)>, String> {
        if req.options.is_empty() {
            return Err("options must be nonempty".into());
        }
        let ctx = tokenize(&format!("{} {}", req.state, req.question));
        let mut ctx_tf: HashMap<&str, u32> = HashMap::new();
        for t in &ctx {
            *ctx_tf.entry(t.as_str()).or_insert(0) += 1;
        }
        let mut raw: Vec<(String, f64)> = Vec::with_capacity(req.options.len());
        for opt in &req.options {
            let opt_toks = tokenize(&format!("{} {}", opt.id, opt.description));
            let mut score = 0.5_f64; // floor so softmax is defined even with zero overlap
            for t in &opt_toks {
                if let Some(&f) = ctx_tf.get(t.as_str()) {
                    score += 1.0 + (f as f64).ln_1p();
                }
            }
            // Prefer options whose description shares a higher fraction of unique tokens.
            let uniq = opt_toks.len().max(1) as f64;
            let hits = opt_toks
                .iter()
                .filter(|t| ctx_tf.contains_key(t.as_str()))
                .count() as f64;
            score += hits / uniq;
            raw.push((opt.id.clone(), score));
        }
        Ok(raw)
    }
}

/// Resolve the configured scorer. Unknown backend → heuristic with fallback flag
/// set by the caller.
pub fn resolve_scorer(root: &Path) -> (Box<dyn Scorer>, bool /* unknown_backend */) {
    let backend = config::semif_backend(root);
    match backend.as_str() {
        "heuristic" | "" => (Box::new(HeuristicScorer), false),
        // Future: "llamacpp" | "torch" | "gguf"
        other => {
            eprintln!(
                "[wordkeep] semif backend {other:?} unavailable; using heuristic"
            );
            (Box::new(HeuristicScorer), true)
        }
    }
}

/// Score one decision; returns a fully filled [`DecisionResult`].
pub fn decide_one(scorer: &dyn Scorer, req: &DecisionRequest, force_fallback: bool) -> DecisionResult {
    let t0 = Instant::now();
    let prompt = render_prompt(req);
    let prompt_sha256 = sha256_hex(&prompt);
    let (raw, fallback) = match scorer.score_one(req) {
        Ok(v) => (v, force_fallback),
        Err(e) => {
            eprintln!("[wordkeep] semif scorer error, uniform fallback: {e}");
            let n = req.options.len().max(1) as f64;
            (
                req.options
                    .iter()
                    .map(|o| (o.id.clone(), 1.0 / n))
                    .collect(),
                true,
            )
        }
    };
    let probs = softmax_labeled(&raw);
    let chosen = probs
        .iter()
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(id, _)| id.clone())
        .unwrap_or_default();
    let options: Vec<OptionScore> = raw
        .iter()
        .zip(probs.iter())
        .map(|((id, raw_score), (_, probability))| OptionScore {
            id: id.clone(),
            probability: *probability,
            raw_score: *raw_score,
        })
        .collect();
    DecisionResult {
        id: req.id.clone(),
        options,
        chosen,
        prompt_sha256,
        scorer: scorer.name().into(),
        scorer_revision: scorer.revision().into(),
        timing_us: t0.elapsed().as_micros() as u64,
        calibrated: false,
        fallback,
    }
}

/// MCP entry: parse args → score → JSON text.
pub fn decide(root: &Path, args: &Value) -> Result<String, String> {
    if !config::semif_enabled(root) {
        return Err("semif disabled in .wordkeep/config.json (semif.enabled=false)".into());
    }
    let (scorer, unknown) = resolve_scorer(root);

    // Shared-state batch: { state, batch: [{id, question, options}, ...] }
    if let Some(batch_val) = args.get("batch").and_then(Value::as_array) {
        let state = state_to_string(args.get("state"))?;
        if state.is_empty() {
            return Err("state is required for batch decisions".into());
        }
        let mut reqs = Vec::with_capacity(batch_val.len());
        for (i, row) in batch_val.iter().enumerate() {
            let mut req = parse_request_row(row, Some(&state))?;
            if req.id.is_empty() {
                req.id = format!("batch-{i}");
            }
            reqs.push(req);
        }
        let t0 = Instant::now();
        // Prefers Scorer::score_batch (serial for heuristic; parallel for future backends).
        let batch_raw = match scorer.score_batch(&state, &reqs) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[wordkeep] semif batch scorer error, per-row fallback: {e}");
                let results: Vec<_> = reqs
                    .iter()
                    .map(|r| decide_one(scorer.as_ref(), r, true))
                    .collect();
                let out = json!({
                    "mode": "batch",
                    "state_chars": state.len(),
                    "count": results.len(),
                    "timing_us": t0.elapsed().as_micros() as u64,
                    "results": results.iter().map(result_to_json).collect::<Vec<_>>(),
                });
                let text = serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?;
                stats::record("semantic_decide", 64, (text.len() / 4) as u64);
                return Ok(text);
            }
        };
        let mut results = Vec::with_capacity(reqs.len());
        for (req, raw) in reqs.iter().zip(batch_raw.into_iter()) {
            let prompt = render_prompt(req);
            let probs = softmax_labeled(&raw);
            let chosen = probs
                .iter()
                .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(id, _)| id.clone())
                .unwrap_or_default();
            let options: Vec<OptionScore> = raw
                .iter()
                .zip(probs.iter())
                .map(|((id, raw_score), (_, probability))| OptionScore {
                    id: id.clone(),
                    probability: *probability,
                    raw_score: *raw_score,
                })
                .collect();
            results.push(DecisionResult {
                id: req.id.clone(),
                options,
                chosen,
                prompt_sha256: sha256_hex(&prompt),
                scorer: scorer.name().into(),
                scorer_revision: scorer.revision().into(),
                timing_us: 0, // filled below as batch wall time share
                calibrated: false,
                fallback: unknown,
            });
        }
        let timing_us = t0.elapsed().as_micros() as u64;
        let per = timing_us / results.len().max(1) as u64;
        for r in &mut results {
            r.timing_us = per;
        }
        let out = json!({
            "mode": "batch",
            "state_chars": state.len(),
            "count": results.len(),
            "timing_us": timing_us,
            "results": results.iter().map(result_to_json).collect::<Vec<_>>(),
        });
        let text = serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?;
        stats::record("semantic_decide", 64, (text.len() / 4) as u64);
        return Ok(text);
    }

    let req = parse_request_row(args, None)?;
    if req.options.is_empty() {
        return Err("options must be a nonempty array of {id, description}".into());
    }
    let result = decide_one(scorer.as_ref(), &req, unknown);
    let mut out = result_to_json(&result);
    if let Some(k) = args.get("top_k").and_then(Value::as_u64) {
        let k = k.max(1) as usize;
        if let Some(arr) = out.get_mut("options").and_then(|v| v.as_array_mut()) {
            arr.sort_by(|a, b| {
                let pa = a.get("probability").and_then(Value::as_f64).unwrap_or(0.0);
                let pb = b.get("probability").and_then(Value::as_f64).unwrap_or(0.0);
                pb.partial_cmp(&pa).unwrap_or(std::cmp::Ordering::Equal)
            });
            arr.truncate(k);
        }
    }
    let text = serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?;
    stats::record("semantic_decide", 64, (text.len() / 4) as u64);
    Ok(text)
}

/// Rerank BM25 candidates with the SemIf heuristic (knowledge_search hook).
///
/// Each candidate becomes an option; the query is the question; shared empty
/// state. Returns a new `(score, idx)` list (head only reordered). On error,
/// returns the original list unchanged.
pub fn rerank_knowledge(
    root: &Path,
    query: &str,
    candidates: &[(String /* label */, String /* text */)],
    scored: Vec<(f64, usize)>,
) -> Result<(Vec<(f64, usize)>, bool /* fallback */), String> {
    if !config::semif_enabled(root) || scored.len() <= 1 || candidates.is_empty() {
        return Ok((scored, false));
    }
    let head_n = scored.len().min(HEAD_RERANK).min(candidates.len());
    let (scorer, unknown) = resolve_scorer(root);
    let options: Vec<OptionSpec> = scored
        .iter()
        .take(head_n)
        .map(|(_, idx)| {
            let (label, text) = &candidates[*idx.min(&(candidates.len() - 1))];
            OptionSpec {
                id: format!("c{idx}"),
                description: format!("{label}\n{text}"),
            }
        })
        .collect();
    let req = DecisionRequest {
        id: "knowledge-rerank".into(),
        state: String::new(),
        question: query.to_string(),
        options,
    };
    let raw = match scorer.score_one(&req) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[wordkeep] semif knowledge rerank failed, BM25 kept: {e}");
            return Ok((scored, true));
        }
    };
    let probs = softmax_labeled(&raw);
    let max_bm = scored[..head_n]
        .iter()
        .map(|(s, _)| *s)
        .fold(f64::MIN, f64::max)
        .max(1e-9);
    let mut head: Vec<(f64, usize)> = probs
        .into_iter()
        .enumerate()
        .map(|(i, (_id, p))| {
            let idx = scored[i].1;
            let bm01 = (scored[i].0 / max_bm).clamp(0.0, 1.0);
            // Blend SemIf probability with normalized BM25 so we don't discard lexical signal.
            (0.55 * p + 0.45 * bm01, idx)
        })
        .collect();
    head.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut out = head;
    out.extend(scored.into_iter().skip(head_n));
    Ok((out, unknown))
}

fn parse_request_row(args: &Value, state_override: Option<&str>) -> Result<DecisionRequest, String> {
    let id = args
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("decision")
        .to_string();
    let state = if let Some(s) = state_override {
        s.to_string()
    } else {
        state_to_string(args.get("state"))?
    };
    if state.is_empty() && state_override.is_none() {
        // Allow empty state only when question alone carries context (knowledge hook).
        if args.get("question").and_then(Value::as_str).unwrap_or("").is_empty() {
            return Err("state is required (string or JSON object/array)".into());
        }
    }
    let question = args
        .get("question")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if question.is_empty() {
        return Err("question is required".into());
    }
    let options = parse_options(args.get("options"))?;
    Ok(DecisionRequest {
        id,
        state,
        question,
        options,
    })
}

fn parse_options(v: Option<&Value>) -> Result<Vec<OptionSpec>, String> {
    let arr = v.and_then(Value::as_array).ok_or("options must be an array")?;
    let mut out = Vec::with_capacity(arr.len());
    for (i, o) in arr.iter().enumerate() {
        let id = o
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let description = o
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if id.is_empty() {
            return Err(format!("options[{i}].id is required"));
        }
        if description.is_empty() {
            return Err(format!("options[{i}].description is required"));
        }
        out.push(OptionSpec { id, description });
    }
    Ok(out)
}

fn state_to_string(v: Option<&Value>) -> Result<String, String> {
    match v {
        None => Ok(String::new()),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(Value::Object(_)) | Some(Value::Array(_)) => {
            serde_json::to_string(v.unwrap()).map_err(|e| e.to_string())
        }
        Some(other) => Err(format!(
            "state must be a string, object, or array (got {})",
            other
        )),
    }
}

fn render_prompt(req: &DecisionRequest) -> String {
    let mut opts = String::new();
    for o in &req.options {
        opts.push_str(&format!("- {}: {}\n", o.id, o.description));
    }
    format!(
        "STATE:\n{}\n\nQUESTION:\n{}\n\nOPTIONS:\n{}",
        req.state, req.question, opts
    )
}

fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

fn softmax_labeled(raw: &[(String, f64)]) -> Vec<(String, f64)> {
    if raw.is_empty() {
        return Vec::new();
    }
    let max = raw
        .iter()
        .map(|(_, s)| *s)
        .fold(f64::NEG_INFINITY, f64::max);
    let exps: Vec<f64> = raw.iter().map(|(_, s)| (s - max).exp()).collect();
    let sum: f64 = exps.iter().sum::<f64>().max(1e-12);
    raw.iter()
        .zip(exps.iter())
        .map(|((id, _), e)| (id.clone(), e / sum))
        .collect()
}

fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| t.len() >= 2)
        .filter(|t| {
            !matches!(
                *t,
                "the"
                    | "and"
                    | "for"
                    | "with"
                    | "that"
                    | "this"
                    | "from"
                    | "are"
                    | "was"
                    | "but"
                    | "not"
                    | "you"
                    | "your"
                    | "use"
                    | "using"
                    | "into"
                    | "when"
                    | "which"
                    | "have"
                    | "has"
                    | "its"
                    | "can"
            )
        })
        .map(|t| t.to_string())
        .collect()
}

fn result_to_json(r: &DecisionResult) -> Value {
    json!({
        "id": r.id,
        "chosen": r.chosen,
        "options": r.options.iter().map(|o| json!({
            "id": o.id,
            "probability": o.probability,
            "raw_score": o.raw_score,
        })).collect::<Vec<_>>(),
        "prompt_sha256": r.prompt_sha256,
        "scorer": r.scorer,
        "scorer_revision": r.scorer_revision,
        "timing_us": r.timing_us,
        "calibrated": r.calibrated,
        "fallback": r.fallback,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn support_req() -> DecisionRequest {
        DecisionRequest {
            id: "support-1".into(),
            state: "The deployment completed at 14:02 UTC. Health checks passed in all three zones. No rollback was initiated.".into(),
            question: "Is there evidence that the deployment succeeded?".into(),
            options: vec![
                OptionSpec {
                    id: "yes".into(),
                    description: "The deployment succeeded.".into(),
                },
                OptionSpec {
                    id: "no".into(),
                    description: "The deployment did not succeed.".into(),
                },
                OptionSpec {
                    id: "insufficient".into(),
                    description: "The evidence is insufficient to decide.".into(),
                },
            ],
        }
    }

    #[test]
    fn heuristic_argmax_stable_and_prompt_hash_deterministic() {
        let s = HeuristicScorer;
        let req = support_req();
        let a = decide_one(&s, &req, false);
        let b = decide_one(&s, &req, false);
        assert_eq!(a.chosen, b.chosen);
        assert_eq!(a.prompt_sha256, b.prompt_sha256);
        assert_eq!(a.prompt_sha256.len(), 64);
        assert!(!a.fallback);
        assert!(!a.calibrated);
        let sum: f64 = a.options.iter().map(|o| o.probability).sum();
        assert!((sum - 1.0).abs() < 1e-9, "softmax sum={sum}");
        // "yes" / succeeded should dominate given "deployment completed" + "health checks passed".
        assert_eq!(a.chosen, "yes", "expected yes, got {:?}", a);
    }

    #[test]
    fn fixture_decisions_score_under_100ms_p99() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/semif/decisions.jsonl");
        let text = std::fs::read_to_string(&path).expect("fixture");
        let s = HeuristicScorer;
        let mut timings = Vec::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let v: Value = serde_json::from_str(line).expect("jsonl");
            let req = parse_request_row(&v, None).expect("parse");
            let t0 = Instant::now();
            let r = decide_one(&s, &req, false);
            timings.push(t0.elapsed().as_micros() as u64);
            assert!(!r.chosen.is_empty());
            assert_eq!(r.options.len(), req.options.len());
        }
        timings.sort_unstable();
        let p99_idx = ((timings.len() as f64) * 0.99).ceil() as usize - 1;
        let p99 = timings[p99_idx.min(timings.len() - 1)];
        assert!(
            p99 < 100_000,
            "heuristic p99 {p99}µs exceeds 100ms on fixture"
        );
    }

    #[test]
    fn mcp_decide_single_and_batch() {
        let dir = std::env::temp_dir().join(format!("wk_semif_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".wordkeep")).unwrap();
        std::fs::write(
            dir.join(".wordkeep/config.json"),
            r#"{"semif":{"enabled":true,"backend":"heuristic"}}"#,
        )
        .unwrap();
        let args = json!({
            "id": "route-1",
            "state": "Customer asks to reset a forgotten password and says the reset email never arrived.",
            "question": "Which queue should handle this request?",
            "options": [
                {"id": "account_access", "description": "Account access and authentication support for password reset and email delivery."},
                {"id": "billing", "description": "Billing and payment support for invoices and charges."},
                {"id": "sales", "description": "Sales and product evaluation for new purchases."}
            ]
        });
        let text = decide(&dir, &args).expect("decide");
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["chosen"], "account_access");
        assert_eq!(v["scorer"], "heuristic");

        let batch = json!({
            "state": "Open CI failure in the docs indexer: markdown chunker drops fenced code; last run EXIT 1.",
            "batch": [
                {
                    "id": "route-issue",
                    "question": "Which subsystem owns this issue?",
                    "options": [
                        {"id": "knowledge", "description": "Documentation indexer / markdown chunking pipeline."},
                        {"id": "net", "description": "Network transport and remote API clients."},
                        {"id": "ui", "description": "Wiki front-end styling and layout."}
                    ]
                },
                {
                    "id": "escalate",
                    "question": "What should happen to this open visual-regression issue?",
                    "options": [
                        {"id": "keep_open", "description": "Keep the open visual issue until side-by-side passes with evidence."},
                        {"id": "close", "description": "Mark done without new visual evidence."}
                    ]
                }
            ]
        });
        let text = decide(&dir, &batch).expect("batch");
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["count"], 2);
        assert_eq!(v["results"][0]["chosen"], "knowledge");
        assert_eq!(v["results"][1]["chosen"], "keep_open");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_backend_falls_back_to_heuristic() {
        let dir = std::env::temp_dir().join(format!("wk_semif_fb_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".wordkeep")).unwrap();
        std::fs::write(
            dir.join(".wordkeep/config.json"),
            r#"{"semif":{"enabled":true,"backend":"torch"}}"#,
        )
        .unwrap();
        let (scorer, unknown) = resolve_scorer(&dir);
        assert!(unknown);
        let r = decide_one(scorer.as_ref(), &support_req(), unknown);
        assert!(r.fallback);
        assert_eq!(r.chosen, "yes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fixture_run_record_evidence() {
        use crate::runs;
        let dir = std::env::temp_dir().join(format!("wk_semif_run_{}", std::process::id()));
        let cache = std::env::temp_dir().join(format!("wk_semif_cache_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&cache);
        std::fs::create_dir_all(dir.join(".wordkeep")).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        // coeffects notify may touch the shared cache; keep it inside temp.
        let prev = std::env::var_os("XDG_CACHE_HOME");
        std::env::set_var("XDG_CACHE_HOME", &cache);
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/semif/decisions.jsonl");
        let text = std::fs::read_to_string(&path).expect("fixture");
        let s = HeuristicScorer;
        let mut n = 0u64;
        let t0 = Instant::now();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let v: Value = serde_json::from_str(line).unwrap();
            let req = parse_request_row(&v, None).unwrap();
            let _ = decide_one(&s, &req, false);
            n += 1;
        }
        let ms = t0.elapsed().as_millis() as u64;
        let out = runs::record(
            &dir,
            &json!({
                "id": "semif-fixture-heuristic",
                "command": "cargo test -p wordkeep semif::",
                "status": "passed",
                "exit_code": 0,
                "duration_ms": ms,
                "summary": format!("heuristic scored {n} fixture decisions"),
                "artifacts": ["tests/fixtures/semif/decisions.jsonl"],
                "tags": ["semif", "fixture", "light-audit"]
            }),
        );
        match prev {
            Some(v) => std::env::set_var("XDG_CACHE_HOME", v),
            None => std::env::remove_var("XDG_CACHE_HOME"),
        }
        let out = out.expect("run_record");
        assert!(out.contains("semif-fixture-heuristic"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&cache);
    }
}
