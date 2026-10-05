//! Optional Ereshkigal GGUF adapter (letter logits, no answer decoding).

use crate::semif::{DecisionRequest, OptionSpec, ScoreBundle, Scorer};
use crate::semif_cascade;
use crate::{config, semif_telemetry};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

struct Live {
    draft: Mutex<ereshkigal_core::score::Scorer>,
    verify: Option<Mutex<ereshkigal_core::score::Scorer>>,
    gpu_layers: u32,
    gguf: String,
}

static SLOT: Mutex<Option<Result<Arc<Live>, String>>> = Mutex::new(None);

fn miss_is_retryable(err: &str) -> bool {
    err.contains("not found") || err.contains("not set") || err.contains("GGUF not found")
}

fn load_one(
    gguf: &Path,
    tokenizer_source: &str,
    tokenizer_revision: &str,
    threads: i32,
    n_gpu_layers: u32,
    n_seq_max: u32,
) -> Result<(ereshkigal_core::score::Scorer, u32), String> {
    let mut layers = n_gpu_layers;
    loop {
        let cfg = ereshkigal_core::EngineConfig {
            gguf: gguf.to_path_buf(),
            tokenizer_source: tokenizer_source.to_string(),
            tokenizer_revision: tokenizer_revision.to_string(),
            max_prompt_tokens: 4096,
            threads: if threads <= 0 { 4 } else { threads },
            n_gpu_layers: layers,
            n_seq_max: n_seq_max.max(1),
            embeddings: false,
            adapter: None,
        };
        match ereshkigal_core::EngineOwned::load(cfg) {
            Ok((engine, tok)) => {
                return Ok((ereshkigal_core::score::Scorer::new(engine, tok), layers));
            }
            Err(e) => {
                let msg = e.to_string();
                if layers > 0 {
                    eprintln!(
                        "[wordkeep] semif GPU load failed ({msg}); retrying CPU n_gpu_layers=0"
                    );
                    layers = 0;
                    continue;
                }
                return Err(msg);
            }
        }
    }
}

fn load_live(root: &Path) -> Result<Live, String> {
    let Some(gguf) = config::semif_gguf_path(root) else {
        return Err("semif.gguf / ERESHKIGAL_GGUF is not set".into());
    };
    if !gguf.is_file() {
        return Err(format!("GGUF not found: {}", gguf.display()));
    }
    let tok_src = config::semif_tokenizer_source(root);
    let tok_rev = config::semif_tokenizer_revision(root);
    let threads = config::semif_threads(root);
    let gpu = config::semif_n_gpu_layers(root);
    let n_seq = config::semif_n_seq_max(root);
    match load_one(&gguf, &tok_src, &tok_rev, threads, gpu, n_seq) {
        Ok((draft, used_layers)) => {
            let verify_path = config::semif_gguf_verify_path(root);
            let verify = verify_path.as_ref().and_then(|p| {
                if p.is_file() {
                    load_one(p, &tok_src, &tok_rev, threads, gpu, n_seq)
                        .ok()
                        .map(|(s, _)| Mutex::new(s))
                } else {
                    None
                }
            });
            semif_telemetry::write_status(
                root,
                json!({
                    "gguf_loaded": true,
                    "gguf_verify_loaded": verify.is_some(),
                    "n_gpu_layers_used": used_layers,
                    "load_error": null,
                }),
            );
            Ok(Live {
                draft: Mutex::new(draft),
                verify,
                gpu_layers: used_layers,
                gguf: gguf.display().to_string(),
            })
        }
        Err(e) => {
            semif_telemetry::write_status(
                root,
                json!({
                    "gguf_loaded": false,
                    "load_error": e,
                }),
            );
            Err(e)
        }
    }
}

fn ensure_arc(root: &Path) -> Result<Arc<Live>, String> {
    let mut g = SLOT.lock().map_err(|e| e.to_string())?;
    let retry = match g.as_ref() {
        None => true,
        Some(Err(e)) if miss_is_retryable(e) => true,
        Some(_) => false,
    };
    if retry {
        *g = Some(load_live(root).map(Arc::new));
    }
    match g.as_ref().expect("slot filled") {
        Ok(live) => Ok(Arc::clone(live)),
        Err(e) => Err(e.clone()),
    }
}

fn to_row(req: &DecisionRequest) -> ereshkigal_core::DecisionRow {
    ereshkigal_core::DecisionRow {
        id: req.id.clone(),
        state: serde_json::Value::String(req.state.clone()),
        question: req.question.clone(),
        options: req
            .options
            .iter()
            .map(|o| ereshkigal_core::OptionSpec {
                id: o.id.clone(),
                description: o.description.clone(),
            })
            .collect(),
    }
}

fn logits_of(res: &ereshkigal_core::ScoreResult, options: &[OptionSpec]) -> Vec<(String, f64)> {
    options
        .iter()
        .enumerate()
        .map(|(i, o)| {
            let logit = res.option_logits.get(i).copied().unwrap_or(0.0);
            (o.id.clone(), logit)
        })
        .collect()
}

pub struct EreshkigalScorer {
    pub allow_cascade: bool,
    pub mode: String,
    pub qhat: f64,
}

impl Scorer for EreshkigalScorer {
    fn name(&self) -> &'static str {
        "ereshkigal"
    }

    fn revision(&self) -> &'static str {
        "direct-options-v1"
    }

    fn score_one(&self, req: &DecisionRequest) -> Result<Vec<(String, f64)>, String> {
        Ok(self.score_detailed(req)?.raw)
    }

    fn score_detailed(&self, req: &DecisionRequest) -> Result<ScoreBundle, String> {
        let live = SLOT
            .lock()
            .map_err(|e| e.to_string())?
            .as_ref()
            .ok_or_else(|| "ereshkigal engines not initialized".to_string())?
            .as_ref()
            .map_err(|e| e.clone())
            .map(Arc::clone)?;
        let row = to_row(req);
        let draft_res = {
            let mut g = live.draft.lock().map_err(|e| e.to_string())?;
            if self.mode == "serial" {
                g.score_serial(&row).map_err(|e| e.to_string())?
            } else {
                g.score_direct(&row).map_err(|e| e.to_string())?
            }
        };
        let mut bundle = ScoreBundle {
            raw: logits_of(&draft_res, &req.options),
            already_normalized: false,
            averaged_probs: None,
            prompt_sha256: Some(draft_res.prompt_sha256.clone()),
            cascade_source: Some("draft".into()),
            cascade_set_size: None,
        };
        if !self.allow_cascade {
            return Ok(bundle);
        }
        let (commit, set_n) =
            semif_cascade::should_commit_draft(&draft_res.probabilities, self.qhat);
        bundle.cascade_set_size = Some(set_n);
        if commit {
            bundle.cascade_source = Some("cascade-draft".into());
            return Ok(bundle);
        }
        let Some(verify) = &live.verify else {
            bundle.cascade_source = Some("cascade-skipped".into());
            return Ok(bundle);
        };
        let verify_res = {
            let mut g = verify.lock().map_err(|e| e.to_string())?;
            g.score_direct(&row).map_err(|e| e.to_string())?
        };
        bundle.raw = logits_of(&verify_res, &req.options);
        bundle.prompt_sha256 = Some(verify_res.prompt_sha256);
        bundle.cascade_source = Some("cascade-verify".into());
        Ok(bundle)
    }

    fn score_batch(
        &self,
        state: &str,
        batch: &[DecisionRequest],
    ) -> Result<Vec<Vec<(String, f64)>>, String> {
        let live = SLOT
            .lock()
            .map_err(|e| e.to_string())?
            .as_ref()
            .ok_or_else(|| "ereshkigal engines not initialized".to_string())?
            .as_ref()
            .map_err(|e| e.clone())
            .map(Arc::clone)?;
        if batch.len() <= 1 || self.mode == "direct" {
            return batch
                .iter()
                .map(|r| {
                    let mut req = r.clone();
                    if req.state.is_empty() {
                        req.state = state.to_string();
                    }
                    self.score_one(&req)
                })
                .collect();
        }
        let rows: Vec<ereshkigal_core::DecisionRow> = batch
            .iter()
            .map(|r| {
                let mut row = to_row(r);
                if row.state.as_str().unwrap_or("").is_empty() {
                    row.state = serde_json::Value::String(state.to_string());
                }
                row
            })
            .collect();
        let mut g = live.draft.lock().map_err(|e| e.to_string())?;
        let (results, _timing) = g.score_shared(&rows).map_err(|e| e.to_string())?;
        Ok(results
            .iter()
            .zip(batch.iter())
            .map(|(res, req)| logits_of(res, &req.options))
            .collect())
    }
}

/// Load GGUF engines for `root`. Returns Err if the draft checkpoint is missing.
pub fn ensure_loaded(root: &Path) -> Result<(), String> {
    ensure_arc(root).map(|_| ())
}

pub fn loaded_meta() -> (bool, Option<String>, u32, String) {
    match SLOT.lock() {
        Ok(g) => match g.as_ref() {
            None => (false, None, 0, String::new()),
            Some(Ok(l)) => (true, None, l.gpu_layers, l.gguf.clone()),
            Some(Err(err)) => (false, Some(err.clone()), 0, String::new()),
        },
        Err(_) => (false, Some("engine slot poisoned".into()), 0, String::new()),
    }
}

pub fn gguf_path_hint(root: &Path) -> Option<PathBuf> {
    config::semif_gguf_path(root)
}
