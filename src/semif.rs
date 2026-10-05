//! SemIf-style semantic decisions for Wordkeep tooling.
//!
//! Mirrors the [SemIf/OpenJev](https://github.com/TheoLeeCJ/SemIf-OpenJev) request
//! shape (`state` + runtime `question` + typed `options` → probabilities) without
//! decoding an answer sentence. Production scoring is optional Ereshkigal GGUF
//! (`semif.backend = ereshkigal`); [`HeuristicScorer`] remains for explicit
//! backend=heuristic and air-gapped JSON-shape tests.
//!
//! Host-agnostic MCP tooling — not a host application runtime.

use crate::{config, semif_debias, semif_telemetry, stats};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

const SCORER_NAME: &str = "heuristic";
const SCORER_REVISION: &str = "v1";
/// Ereshkigal letter logits are A–P (2–16). Knowledge rerank uses a short BM25 head.
const LETTER_OPTION_CAP: usize = 16;
const HEAD_RERANK: usize = 5;
const KNOWLEDGE_DESC_CHARS: usize = 180;
const KNOWLEDGE_STATE: &str = "BM25 retrieval head; options are candidate documents.";

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
    pub cascade_source: Option<String>,
    pub cascade_set_size: Option<usize>,
}

/// Inner scorer output (logits or already-normalized probabilities).
#[derive(Clone, Debug)]
pub struct ScoreBundle {
    pub raw: Vec<(String, f64)>,
    pub already_normalized: bool,
    /// Averaged softmax from permute/PriDe extra cycles; identity logits stay in `raw`.
    pub averaged_probs: Option<Vec<(String, f64)>>,
    pub prompt_sha256: Option<String>,
    pub cascade_source: Option<String>,
    pub cascade_set_size: Option<usize>,
}

impl ScoreBundle {
    fn from_raw(raw: Vec<(String, f64)>) -> Self {
        Self {
            raw,
            already_normalized: false,
            averaged_probs: None,
            prompt_sha256: None,
            cascade_source: None,
            cascade_set_size: None,
        }
    }

    fn probs(&self) -> Vec<(String, f64)> {
        if let Some(avg) = &self.averaged_probs {
            avg.clone()
        } else if self.already_normalized {
            self.raw.clone()
        } else {
            softmax_labeled(&self.raw)
        }
    }
}

/// Pluggable option scorer (direct logit readout in upstream SemIf; heuristic here).
pub trait Scorer: Send + Sync {
    fn name(&self) -> &'static str;
    fn revision(&self) -> &'static str;
    fn score_one(&self, req: &DecisionRequest) -> Result<Vec<(String, f64)>, String>;
    fn score_detailed(&self, req: &DecisionRequest) -> Result<ScoreBundle, String> {
        Ok(ScoreBundle::from_raw(self.score_one(req)?))
    }
    fn score_batch(
        &self,
        state: &str,
        batch: &[DecisionRequest],
    ) -> Result<Vec<Vec<(String, f64)>>, String> {
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

pub enum ResolvedScorer {
    Ready(Box<dyn Scorer>, bool /* unknown_backend */),
    Unavailable(String),
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

/// Resolve the configured scorer. `ereshkigal` without GGUF / without the Cargo
/// feature is [`ResolvedScorer::Unavailable`] (not a silent heuristic score).
pub fn resolve_scorer(root: &Path) -> ResolvedScorer {
    resolve_scorer_ex(root, true)
}

fn resolve_scorer_ex(root: &Path, for_decide: bool) -> ResolvedScorer {
    semif_telemetry::write_status(root, serde_json::json!({}));
    let backend = config::semif_backend(root);
    match backend.as_str() {
        "heuristic" | "" => ResolvedScorer::Ready(Box::new(HeuristicScorer), false),
        "ereshkigal" | "gguf" => resolve_ereshkigal(root, for_decide),
        other => {
            eprintln!("[wordkeep] semif backend {other:?} unavailable; using heuristic");
            ResolvedScorer::Ready(Box::new(HeuristicScorer), true)
        }
    }
}

fn resolve_ereshkigal(root: &Path, for_decide: bool) -> ResolvedScorer {
    #[cfg(feature = "ereshkigal")]
    {
        match crate::semif_gguf::ensure_loaded(root) {
            Ok(()) => {
                let allow_cascade = if for_decide {
                    config::semif_cascade_routing(root) == "conformal"
                } else {
                    config::semif_knowledge_cascade(root)
                };
                let scorer = crate::semif_gguf::EreshkigalScorer {
                    allow_cascade,
                    mode: config::semif_mode(root),
                    qhat: config::semif_cascade_qhat(root),
                };
                ResolvedScorer::Ready(Box::new(scorer), false)
            }
            Err(e) => ResolvedScorer::Unavailable(e),
        }
    }
    #[cfg(not(feature = "ereshkigal"))]
    {
        let _ = (root, for_decide);
        ResolvedScorer::Unavailable(
            "wordkeep built without --features ereshkigal (GGUF scorer unavailable)".into(),
        )
    }
}

fn score_with_policy(
    root: &Path,
    scorer: &dyn Scorer,
    req: &DecisionRequest,
) -> Result<ScoreBundle, String> {
    let mut ident = scorer.score_detailed(req)?;
    match config::semif_debias(root).as_str() {
        "none" | "" => Ok(ident),
        "pride" => {
            ident.averaged_probs =
                Some(semif_debias::pride_from_identity(scorer, req, &ident.raw)?);
            ident.already_normalized = false;
            Ok(ident)
        }
        _ => {
            ident.averaged_probs = Some(semif_debias::permute_from_identity(
                scorer, req, &ident.raw, 3,
            )?);
            ident.already_normalized = false;
            Ok(ident)
        }
    }
}

/// Score one decision; returns a fully filled [`DecisionResult`].
pub fn decide_one(scorer: &dyn Scorer, req: &DecisionRequest, force_fallback: bool) -> DecisionResult {
    decide_one_at(None, scorer, req, force_fallback)
}

fn decide_one_at(
    root: Option<&Path>,
    scorer: &dyn Scorer,
    req: &DecisionRequest,
    force_fallback: bool,
) -> DecisionResult {
    let t0 = Instant::now();
    let prompt = render_prompt(req);
    let heuristic_hash = sha256_hex(&prompt);
    let (bundle, fallback) = match root {
        Some(root) => match score_with_policy(root, scorer, req) {
            Ok(b) => (b, force_fallback),
            Err(e) => {
                eprintln!("[wordkeep] semif scorer error: {e}");
                return DecisionResult {
                    id: req.id.clone(),
                    options: Vec::new(),
                    chosen: String::new(),
                    prompt_sha256: heuristic_hash,
                    scorer: scorer.name().into(),
                    scorer_revision: scorer.revision().into(),
                    timing_us: t0.elapsed().as_micros() as u64,
                    calibrated: false,
                    fallback: true,
                    cascade_source: Some("error".into()),
                    cascade_set_size: None,
                };
            }
        },
        None => match scorer.score_detailed(req) {
            Ok(b) => (b, force_fallback),
            Err(e) => {
                eprintln!("[wordkeep] semif scorer error, uniform fallback: {e}");
                let n = req.options.len().max(1) as f64;
                (
                    ScoreBundle {
                        raw: req
                            .options
                            .iter()
                            .map(|o| (o.id.clone(), 1.0 / n))
                            .collect(),
                        already_normalized: true,
                        averaged_probs: None,
                        prompt_sha256: None,
                        cascade_source: None,
                        cascade_set_size: None,
                    },
                    true,
                )
            }
        },
    };
    let probs = bundle.probs();
    let chosen = probs
        .iter()
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(id, _)| id.clone())
        .unwrap_or_default();
    let options: Vec<OptionScore> = bundle
        .raw
        .iter()
        .zip(probs.iter())
        .map(|((id, raw_score), (_, probability))| OptionScore {
            id: id.clone(),
            probability: *probability,
            raw_score: *raw_score,
        })
        .collect();
    let prompt_sha256 = bundle
        .prompt_sha256
        .clone()
        .unwrap_or(heuristic_hash);
    let timing_us = t0.elapsed().as_micros() as u64;
    if let Some(root) = root {
        semif_telemetry::record_event(serde_json::json!({
            "id": req.id,
            "chosen": chosen,
            "timing_us": timing_us,
            "fallback": fallback,
            "scorer": scorer.name(),
            "cascade_source": bundle.cascade_source,
            "kind": "decide",
        }));
        let _ = root;
    }
    DecisionResult {
        id: req.id.clone(),
        options,
        chosen,
        prompt_sha256,
        scorer: scorer.name().into(),
        scorer_revision: scorer.revision().into(),
        timing_us,
        calibrated: false,
        fallback,
        cascade_source: bundle.cascade_source,
        cascade_set_size: bundle.cascade_set_size,
    }
}

/// MCP entry: parse args → score → JSON text.
pub fn decide(root: &Path, args: &Value) -> Result<String, String> {
    if !config::semif_enabled(root) {
        return Err("semif disabled in .wordkeep/config.json (semif.enabled=false)".into());
    }
    let (scorer, unknown) = match resolve_scorer(root) {
        ResolvedScorer::Ready(s, u) => (s, u),
        ResolvedScorer::Unavailable(e) => {
            return Err(format!("semif backend unavailable: {e}"));
        }
    };

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
                    .map(|r| decide_one_at(Some(root), scorer.as_ref(), r, true))
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
                cascade_source: None,
                cascade_set_size: None,
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
    let result = decide_one_at(Some(root), scorer.as_ref(), &req, unknown);
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
    let Some(req) = knowledge_rerank_request(query, candidates, &scored) else {
        return Ok((scored, false));
    };
    let head_n = req.options.len();
    let (scorer, unknown) = match resolve_scorer_ex(root, false) {
        ResolvedScorer::Ready(s, u) => (s, u),
        ResolvedScorer::Unavailable(e) => {
            eprintln!("[wordkeep] semif knowledge rerank unavailable, BM25 kept: {e}");
            semif_telemetry::record_event(serde_json::json!({
                "kind": "rerank",
                "fallback": true,
                "error": e,
            }));
            return Ok((scored, true));
        }
    };
    let allow_cascade = config::semif_knowledge_cascade(root);
    let bundle = if allow_cascade {
        score_with_policy(root, scorer.as_ref(), &req)
    } else {
        match config::semif_knowledge_debias(root).as_str() {
            "pride" => semif_debias::pride_average(scorer.as_ref(), &req).map(|raw| ScoreBundle {
                raw,
                already_normalized: true,
                averaged_probs: None,
                prompt_sha256: None,
                cascade_source: None,
                cascade_set_size: None,
            }),
            "permute" => {
                semif_debias::permute_average(scorer.as_ref(), &req, 3).map(|raw| ScoreBundle {
                    raw,
                    already_normalized: true,
                    averaged_probs: None,
                    prompt_sha256: None,
                    cascade_source: None,
                    cascade_set_size: None,
                })
            }
            _ => scorer.score_detailed(&req),
        }
    };
    let raw = match bundle {
        Ok(b) => b.probs(),
        Err(e) => {
            eprintln!("[wordkeep] semif knowledge rerank failed, BM25 kept: {e}");
            semif_telemetry::record_event(serde_json::json!({
                "kind": "rerank",
                "fallback": true,
                "error": e,
            }));
            return Ok((scored, true));
        }
    };
    let probs = raw;
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

/// Shape a BM25 head into a letter-logit row (2–16 short options, nonempty state).
pub(crate) fn knowledge_rerank_request(
    query: &str,
    candidates: &[(String, String)],
    scored: &[(f64, usize)],
) -> Option<DecisionRequest> {
    let head_n = scored
        .len()
        .min(HEAD_RERANK)
        .min(candidates.len())
        .min(LETTER_OPTION_CAP);
    if head_n < 2 {
        return None;
    }
    let mut options = Vec::with_capacity(head_n);
    for (_, idx) in scored.iter().take(head_n) {
        let idx = (*idx).min(candidates.len().saturating_sub(1));
        let (label, text) = &candidates[idx];
        let mut desc: String = format!("{label} {text}")
            .chars()
            .take(KNOWLEDGE_DESC_CHARS)
            .collect();
        if desc.trim().is_empty() {
            desc = format!("candidate {idx}");
        }
        options.push(OptionSpec {
            id: format!("c{idx}"),
            description: desc,
        });
    }
    let question = query.trim();
    Some(DecisionRequest {
        id: "knowledge-rerank".into(),
        state: KNOWLEDGE_STATE.into(),
        question: if question.is_empty() {
            "Which candidate is most relevant?".into()
        } else {
            question.to_string()
        },
        options,
    })
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

pub(crate) fn softmax_labeled(raw: &[(String, f64)]) -> Vec<(String, f64)> {
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
        "cascade_source": r.cascade_source,
        "cascade_set_size": r.cascade_set_size,
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
    fn knowledge_rerank_request_fits_letter_logit_shape() {
        let cands: Vec<(String, String)> = (0..20)
            .map(|i| (format!("heading-{i}"), "body ".repeat(200)))
            .collect();
        let scored: Vec<(f64, usize)> = (0..20).map(|i| (20.0 - i as f64, i)).collect();
        let req = knowledge_rerank_request("Which doc?", &cands, &scored).expect("head");
        assert_eq!(req.state, KNOWLEDGE_STATE);
        assert_eq!(req.options.len(), HEAD_RERANK);
        for o in &req.options {
            assert!(!o.id.is_empty());
            assert!(!o.description.trim().is_empty());
            assert!(o.description.chars().count() <= KNOWLEDGE_DESC_CHARS);
        }
        assert!(knowledge_rerank_request("q", &cands[..1], &scored[..1]).is_none());
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
            r#"{"semif":{"enabled":true,"backend":"heuristic","debias":"none"}}"#,
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
        let (scorer, unknown) = match resolve_scorer(&dir) {
            ResolvedScorer::Ready(s, u) => (s, u),
            ResolvedScorer::Unavailable(e) => panic!("expected heuristic fallback, got {e}"),
        };
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

    #[test]
    fn ereshkigal_backend_without_gguf_is_unavailable() {
        let dir = std::env::temp_dir().join(format!("wk_semif_noggu_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".wordkeep")).unwrap();
        std::fs::write(
            dir.join(".wordkeep/config.json"),
            r#"{"semif":{"enabled":true,"backend":"ereshkigal","debias":"none"}}"#,
        )
        .unwrap();
        match resolve_scorer(&dir) {
            ResolvedScorer::Unavailable(e) => {
                assert!(
                    e.contains("GGUF") || e.contains("ereshkigal") || e.contains("not set"),
                    "{e}"
                );
            }
            ResolvedScorer::Ready(_, _) => panic!("missing GGUF must not silently use heuristic"),
        }
        let err = decide(
            &dir,
            &json!({
                "state": "x",
                "question": "y?",
                "options": [{"id":"a","description":"A"},{"id":"b","description":"B"}]
            }),
        )
        .unwrap_err();
        assert!(err.contains("unavailable"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writes_or_checks_heuristic_baseline_decisions() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/semif/decisions.jsonl");
        let out_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/semif/heuristic-baseline-decisions.jsonl");
        let text = std::fs::read_to_string(&fixture).expect("fixture");
        let s = HeuristicScorer;
        let mut rows = Vec::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let v: Value = serde_json::from_str(line).unwrap();
            let req = parse_request_row(&v, None).unwrap();
            let r = decide_one(&s, &req, false);
            rows.push(json!({
                "id": r.id,
                "chosen": r.chosen,
                "prompt_sha256": r.prompt_sha256,
                "timing_us": r.timing_us,
                "fallback": r.fallback,
                "scorer": r.scorer,
                "scorer_revision": r.scorer_revision,
                "options": r.options.iter().map(|o| json!({
                    "id": o.id,
                    "probability": o.probability,
                    "raw_score": o.raw_score
                })).collect::<Vec<_>>(),
            }));
        }
        if std::env::var("SEMIF_WRITE_BASELINE").ok().as_deref() == Some("1") || !out_path.exists()
        {
            let mut buf = String::new();
            for row in &rows {
                buf.push_str(&serde_json::to_string(row).unwrap());
                buf.push('\n');
            }
            std::fs::write(&out_path, buf).unwrap();
        }
        let got: Vec<Value> = std::fs::read_to_string(&out_path)
            .unwrap()
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(got.len(), rows.len());
        for (g, r) in got.iter().zip(rows.iter()) {
            assert_eq!(g["id"], r["id"]);
            assert_eq!(g["chosen"], r["chosen"]);
            assert_eq!(g["prompt_sha256"], r["prompt_sha256"]);
            assert_eq!(g["scorer"], "heuristic");
        }
        let wk = Path::new(env!("CARGO_MANIFEST_DIR"));
        let _ = crate::runs::record(
            wk,
            &json!({
                "id": "semif-heuristic-baseline",
                "command": "cargo test --lib semif::tests::writes_or_checks_heuristic_baseline",
                "status": "passed",
                "exit_code": 0,
                "summary": format!("heuristic baseline {} decisions", rows.len()),
                "artifacts": [
                    "tests/fixtures/semif/heuristic-baseline-decisions.jsonl",
                    "tests/fixtures/semif/heuristic-baseline-knowledge.json"
                ],
                "tags": ["semif-heuristic-baseline", "semif"]
            }),
        );
    }

    #[test]
    fn writes_or_checks_heuristic_baseline_knowledge() {
        let _lock = crate::cache::test_env_lock();
        let out_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/semif/heuristic-baseline-knowledge.json");
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let root = root.canonicalize().unwrap_or(root);
        let queries = [
            "SemIf semantic_decide option probabilities",
            "Izakaya check in lease",
            "Flecs ECS module grouping",
            "knowledge_search BM25 retrieval",
            "wiki dashboard telemetry savings.json",
            "rollback determinism snapshot",
        ];
        let mut reports = Vec::new();
        for q in queries {
            let bm = crate::knowledge::search(
                &root,
                &json!({
                    "query": q,
                    "k": 5,
                    "semif": false,
                    "semantic": false,
                    "include_defects": false,
                    "roots": ["docs", "tools/wordkeep/docs"]
                }),
            )
            .unwrap_or_else(|e| e);
            let sm = crate::knowledge::search(
                &root,
                &json!({
                    "query": q,
                    "k": 5,
                    "semif": true,
                    "semantic": false,
                    "include_defects": false,
                    "roots": ["docs", "tools/wordkeep/docs"]
                }),
            )
            .unwrap_or_else(|e| e);
            let bm_paths = knowledge_paths(&bm);
            let sm_paths = knowledge_paths(&sm);
            reports.push(json!({
                "query": q,
                "bm25": bm_paths,
                "semif": sm_paths,
            }));
        }
        if std::env::var("SEMIF_WRITE_BASELINE").ok().as_deref() == Some("1") || !out_path.exists()
        {
            std::fs::write(&out_path, serde_json::to_string_pretty(&reports).unwrap()).unwrap();
        }
        assert!(out_path.exists());
        let disk: Value = serde_json::from_str(&std::fs::read_to_string(&out_path).unwrap()).unwrap();
        assert!(disk.as_array().map(|a| a.len()).unwrap_or(0) >= 1);
    }

    fn knowledge_paths(text: &str) -> Vec<String> {
        text.lines()
            .filter_map(|line| {
                let line = line.trim();
                if !line.starts_with('[') {
                    return None;
                }
                let rest = line.splitn(2, ']').nth(1)?;
                let path = rest.split(" - ").next()?.trim().to_string();
                if path.is_empty() {
                    None
                } else {
                    Some(path)
                }
            })
            .collect()
    }

    #[cfg(feature = "ereshkigal")]
    #[test]
    fn gguf_after_fixture_when_env_set() {
        let Ok(gguf) = std::env::var("ERESHKIGAL_GGUF") else {
            return;
        };
        if !Path::new(&gguf).is_file() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("wk_semif_gguf_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".wordkeep")).unwrap();
        std::fs::write(
            dir.join(".wordkeep/config.json"),
            format!(
                r#"{{"semif":{{"enabled":true,"backend":"ereshkigal","debias":"permute","gguf":{}}}}}"#,
                serde_json::to_string(&gguf).unwrap()
            ),
        )
        .unwrap();
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/semif/decisions.jsonl");
        let text = std::fs::read_to_string(&fixture).unwrap();
        let mut rows = Vec::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let v: Value = serde_json::from_str(line).unwrap();
            let parsed = decide(&dir, &v)
                .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()));
            match parsed {
                Ok(row) => rows.push(row),
                Err(e) => rows.push(json!({ "error": e, "id": v.get("id") })),
            }
        }
        if std::env::var("SEMIF_WRITE_AFTER").ok().as_deref() == Some("1") {
            let dest = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/semif/ereshkigal-after-decisions.jsonl");
            let mut buf = String::new();
            for row in &rows {
                buf.push_str(&serde_json::to_string(row).unwrap());
                buf.push('\n');
            }
            std::fs::write(&dest, buf).unwrap();

            let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
            let root = root.canonicalize().unwrap_or(root);
            let queries = [
                "SemIf semantic_decide option probabilities",
                "Izakaya check in lease",
                "Flecs ECS module grouping",
                "knowledge_search BM25 retrieval",
                "wiki dashboard telemetry savings.json",
                "rollback determinism snapshot",
            ];
            let mut reports = Vec::new();
            for q in queries {
                let bm = crate::knowledge::search(
                    &root,
                    &json!({
                        "query": q,
                        "k": 5,
                        "semif": false,
                        "semantic": false,
                        "include_defects": false,
                        "roots": ["docs", "tools/wordkeep/docs"]
                    }),
                )
                .unwrap_or_else(|e| e);
                let sm = crate::knowledge::search(
                    &root,
                    &json!({
                        "query": q,
                        "k": 5,
                        "semif": true,
                        "semantic": false,
                        "include_defects": false,
                        "roots": ["docs", "tools/wordkeep/docs"]
                    }),
                )
                .unwrap_or_else(|e| e);
                reports.push(json!({
                    "query": q,
                    "bm25": knowledge_paths(&bm),
                    "semif": knowledge_paths(&sm),
                }));
            }
            std::fs::write(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/semif/ereshkigal-after-knowledge.json"),
                serde_json::to_string_pretty(&reports).unwrap(),
            )
            .unwrap();
        }
        assert!(!rows.is_empty());
        assert!(
            rows.iter().all(|r| r.get("error").is_none()),
            "GGUF after rows had errors: {rows:?}"
        );
        let mut times: Vec<u64> = rows
            .iter()
            .filter_map(|r| r.get("timing_us").and_then(|t| t.as_u64()))
            .collect();
        times.sort_unstable();
        if !times.is_empty() {
            let p50 = times[times.len() / 2];
            eprintln!(
                "gguf permute fixture p50_ms={:.1} (n={})",
                p50 as f64 / 1000.0,
                times.len()
            );
        }
        std::fs::write(
            dir.join(".wordkeep/config.json"),
            format!(
                r#"{{"semif":{{"enabled":true,"backend":"ereshkigal","debias":"none","gguf":{}}}}}"#,
                serde_json::to_string(&gguf).unwrap()
            ),
        )
        .unwrap();
        let mut none_times = Vec::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let v: Value = serde_json::from_str(line).unwrap();
            if let Ok(t) = decide(&dir, &v) {
                if let Ok(row) = serde_json::from_str::<Value>(&t) {
                    if let Some(us) = row.get("timing_us").and_then(|x| x.as_u64()) {
                        none_times.push(us);
                    }
                }
            }
        }
        none_times.sort_unstable();
        if !none_times.is_empty() {
            let p50 = none_times[none_times.len() / 2];
            eprintln!(
                "gguf none fixture p50_ms={:.1} (n={})",
                p50 as f64 / 1000.0,
                none_times.len()
            );
        }
        let cands = vec![
            ("account".into(), "Password reset and email delivery.".into()),
            ("billing".into(), "Invoices and payment charges.".into()),
            ("sales".into(), "New purchase evaluation.".into()),
            ("docs".into(), "Markdown indexer and BM25.".into()),
            ("net".into(), "Remote API transport.".into()),
        ];
        let scored: Vec<(f64, usize)> = (0..cands.len()).map(|i| ((cands.len() - i) as f64, i)).collect();
        let (_order, fallback) =
            rerank_knowledge(&dir, "Which queue owns password reset?", &cands, scored)
                .expect("knowledge rerank");
        assert!(
            !fallback,
            "GGUF knowledge rerank fell back to BM25 after shaping"
        );
        for row in &rows {
            if row.get("error").is_some() {
                continue;
            }
            let sha = row["prompt_sha256"].as_str().unwrap_or("");
            assert_eq!(sha.len(), 64, "permute must keep GGUF prompt_sha256");
            assert_eq!(row["scorer"], "ereshkigal");
            let id = row["id"].as_str().unwrap_or("");
            let src = text.lines().find(|l| l.contains(id)).unwrap_or("");
            if let Ok(v) = serde_json::from_str::<Value>(src) {
                if let Ok(req) = parse_request_row(&v, None) {
                    let heuristic = sha256_hex(&render_prompt(&req));
                    assert_ne!(
                        sha, heuristic,
                        "permute must not replace GGUF hash with heuristic recipe hash"
                    );
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
