//! Optional Ereshkigal GGUF adapter (letter logits, no answer decoding).

use crate::semif::{DecisionRequest, OptionSpec, ScoreBundle, Scorer};
use crate::semif_cascade;
use crate::{config, semif_telemetry};
use serde_json::json;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

struct Live {
    draft: Mutex<ereshkigal_core::score::Scorer>,
    /// Same draft GGUF with `n_gpu_layers=0` for accuracy escalate (tandem).
    cpu_draft: Option<Mutex<ereshkigal_core::score::Scorer>>,
    /// Verify GGUF path + load knobs; engine is loaded on first escalate (VRAM).
    verify_cfg: Option<VerifyCfg>,
    /// `None` = not attempted; `Some(Ok)` = loaded; `Some(Err)` = failed once.
    verify: Mutex<Option<Result<Mutex<ereshkigal_core::score::Scorer>, String>>>,
    n_gpu_layers_used: u32,
    tandem: bool,
}

struct VerifyCfg {
    path: std::path::PathBuf,
    tok_src: String,
    tok_rev: String,
    threads: i32,
    gpu: u32,
    n_seq: u32,
}

static SLOT: Mutex<Option<Result<Arc<Live>, String>>> = Mutex::new(None);
static WARM: Mutex<bool> = Mutex::new(false);

fn verify_tokenizer(gguf: &Path, draft_src: &str, draft_rev: &str) -> (String, String) {
    let name = gguf.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if name.contains("Qwen3.5-4B") || name.contains("qwen35") || name.contains("Qwen3.5") {
        (
            "Qwen/Qwen3.5-4B".into(),
            "851bf6e806efd8d0a36b00ddf55e13ccb7b8cd0a".into(),
        )
    } else {
        (draft_src.to_string(), draft_rev.to_string())
    }
}

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
    adapter: Option<&Path>,
    adapter_scale: f32,
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
            adapter: adapter.map(|p| p.to_path_buf()),
            adapter_scale,
        };
        match ereshkigal_core::EngineOwned::load(cfg) {
            Ok((engine, tok)) => {
                return Ok((ereshkigal_core::score::Scorer::new(engine, tok), layers));
            }
            Err(e) => {
                let msg = e.to_string();
                // Vocab / tokenizer mismatches will not heal by dropping GPU layers.
                let retry_cpu = layers > 0
                    && !msg.contains("vocabulary disagrees")
                    && !msg.contains("not a shared single token")
                    && !msg.contains("lora_adapter");
                if retry_cpu {
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
    let t0 = Instant::now();
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
    let tandem = config::semif_tandem(root) && gpu > 0;
    let adapter_path = config::semif_adapter_path(root).and_then(|p| {
        if p.is_file() {
            Some(p)
        } else {
            eprintln!(
                "[wordkeep] semif.adapter missing ({}); continuing without LoRA",
                p.display()
            );
            None
        }
    });
    let adapter_ref = adapter_path.as_deref();
    let adapter_scale = config::semif_adapter_scale(root);
    match load_one(
        &gguf,
        &tok_src,
        &tok_rev,
        threads,
        gpu,
        n_seq,
        adapter_ref,
        adapter_scale,
    ) {
        Ok((draft, used_layers)) => {
            let cpu_draft = if tandem && used_layers > 0 {
                match load_one(
                    &gguf,
                    &tok_src,
                    &tok_rev,
                    threads,
                    0,
                    n_seq,
                    adapter_ref,
                    adapter_scale,
                ) {
                    Ok((s, _)) => Some(Mutex::new(s)),
                    Err(e) => {
                        eprintln!(
                            "[wordkeep] tandem CPU draft load failed ({e}); continuing GPU-only"
                        );
                        None
                    }
                }
            } else {
                None
            };
            let verify_path = config::semif_gguf_verify_path(root);
            let verify_cfg = verify_path.as_ref().and_then(|p| {
                if !p.is_file() {
                    return None;
                }
                let (v_src, v_rev) = verify_tokenizer(p, &tok_src, &tok_rev);
                Some(VerifyCfg {
                    path: p.clone(),
                    tok_src: v_src,
                    tok_rev: v_rev,
                    threads,
                    gpu,
                    n_seq,
                })
            });
            eprintln!(
                "[wordkeep] semif engines: gpu_layers={used_layers} tandem={} verify={} adapter={}",
                cpu_draft.is_some(),
                if verify_cfg.is_some() {
                    "lazy"
                } else {
                    "false"
                },
                adapter_path.is_some()
            );
            let cold_ms = t0.elapsed().as_millis() as u64;
            let tandem_live = tandem && cpu_draft.is_some();
            semif_telemetry::write_status(
                root,
                json!({
                    "gguf_loaded": true,
                    "gguf_verify_loaded": false,
                    "gguf_verify_lazy": verify_cfg.is_some(),
                    "adapter_loaded": adapter_path.is_some(),
                    "adapter": adapter_path.as_ref().map(|p| p.display().to_string()),
                    "adapter_scale": adapter_scale,
                    "n_gpu_layers_used": used_layers,
                    "tandem": tandem_live,
                    "cpu_draft_loaded": cpu_draft.is_some(),
                    "cold_load_ms": cold_ms,
                    "warm": false,
                    "load_error": null,
                }),
            );
            if let Ok(mut w) = WARM.lock() {
                *w = false;
            }
            Ok(Live {
                draft: Mutex::new(draft),
                cpu_draft,
                verify_cfg,
                verify: Mutex::new(None),
                n_gpu_layers_used: used_layers,
                tandem: tandem_live,
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
        Ok(live) => {
            if let Ok(mut w) = WARM.lock() {
                if !*w {
                    *w = true;
                    semif_telemetry::write_status(
                        root,
                        json!({
                            "warm": true,
                            "n_gpu_layers_used": live.n_gpu_layers_used,
                            "tandem": live.tandem,
                            "cpu_draft_loaded": live.cpu_draft.is_some(),
                        }),
                    );
                }
            }
            Ok(Arc::clone(live))
        }
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

fn score_engine(
    engine: &Mutex<ereshkigal_core::score::Scorer>,
    mode: &str,
    row: &ereshkigal_core::DecisionRow,
) -> Result<ereshkigal_core::ScoreResult, String> {
    let mut g = engine.lock().map_err(|e| e.to_string())?;
    if mode == "serial" {
        g.score_serial(row).map_err(|e| e.to_string())
    } else {
        g.score_direct(row).map_err(|e| e.to_string())
    }
}

pub struct EreshkigalScorer {
    pub mode: String,
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

    /// Draft-only forward (Vulkan when linked). Cascade/tandem runs in [`escalate_cascade`].
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
        let draft_res = score_engine(&live.draft, &self.mode, &row)?;
        Ok(ScoreBundle {
            raw: logits_of(&draft_res, &req.options),
            already_normalized: false,
            averaged_probs: None,
            prompt_sha256: Some(draft_res.prompt_sha256.clone()),
            cascade_source: Some("draft".into()),
            cascade_set_size: None,
        })
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

fn bundle_probs(bundle: &ScoreBundle) -> Vec<f64> {
    if let Some(ref avg) = bundle.averaged_probs {
        avg.iter().map(|(_, p)| *p).collect()
    } else {
        crate::semif::softmax_labeled(&bundle.raw)
            .into_iter()
            .map(|(_, p)| p)
            .collect()
    }
}

/// Mark conformal commit on a draft/adaptive bundle. Returns true when draft wins.
pub fn try_commit_draft(bundle: &mut ScoreBundle, qhat: f64) -> bool {
    let probs = bundle_probs(bundle);
    let (commit, set_n) = semif_cascade::should_commit_draft(&probs, qhat);
    bundle.cascade_set_size = Some(set_n);
    if !commit {
        return false;
    }
    if bundle.cascade_source.as_deref() == Some("cascade-adaptive") {
        // keep adaptive label
    } else if bundle.averaged_probs.is_some() {
        bundle.cascade_source = Some("cascade-adaptive".into());
    } else {
        bundle.cascade_source = Some("cascade-draft".into());
    }
    true
}

fn try_commit_labeled(bundle: &mut ScoreBundle, qhat: f64, source: &str) -> bool {
    let probs = bundle_probs(bundle);
    let (commit, set_n) = semif_cascade::should_commit_draft(&probs, qhat);
    bundle.cascade_set_size = Some(set_n);
    if commit {
        bundle.cascade_source = Some(source.into());
    }
    commit
}

fn score_cpu_row(live: &Live, req: &DecisionRequest) -> Result<Option<ScoreBundle>, String> {
    if !live.tandem {
        return Ok(None);
    }
    let Some(cpu) = &live.cpu_draft else {
        return Ok(None);
    };
    let row = to_row(req);
    let cpu_res = score_engine(cpu, "direct", &row)?;
    Ok(Some(ScoreBundle {
        raw: logits_of(&cpu_res, &req.options),
        already_normalized: false,
        averaged_probs: None,
        prompt_sha256: Some(cpu_res.prompt_sha256.clone()),
        cascade_source: Some("cascade-cpu".into()),
        cascade_set_size: None,
    }))
}

fn apply_verify_row(
    live: &Live,
    root: &Path,
    req: &DecisionRequest,
    bundle: &mut ScoreBundle,
) -> Result<(), String> {
    let row = to_row(req);
    match score_with_verify(live, root, &row)? {
        Some(verify_res) => {
            bundle.raw = logits_of(&verify_res, &req.options);
            bundle.averaged_probs = None;
            bundle.already_normalized = false;
            bundle.prompt_sha256 = Some(verify_res.prompt_sha256);
            bundle.cascade_source = Some("cascade-verify".into());
        }
        None => {
            bundle.cascade_source = Some("cascade-skipped".into());
        }
    }
    Ok(())
}

/// CPU then verify wavefront over residual indices (lazy 4B load once).
pub fn escalate_residuals(
    root: &Path,
    reqs: &[DecisionRequest],
    bundles: &mut [ScoreBundle],
    residual: &[usize],
    qhat: f64,
) -> Result<(), String> {
    if residual.is_empty() {
        return Ok(());
    }
    let live = ensure_arc(root)?;
    let mut need_verify: Vec<usize> = Vec::with_capacity(residual.len());
    for &i in residual {
        match score_cpu_row(&live, &reqs[i])? {
            Some(mut cpu_b) => {
                if try_commit_labeled(&mut cpu_b, qhat, "cascade-cpu") {
                    bundles[i] = cpu_b;
                } else {
                    bundles[i] = cpu_b;
                    need_verify.push(i);
                }
            }
            None => need_verify.push(i),
        }
    }
    for &i in &need_verify {
        apply_verify_row(&live, root, &reqs[i], &mut bundles[i])?;
    }
    Ok(())
}

/// After draft (+ optional adaptive permute), escalate CPU then 4B verify.
pub fn escalate_cascade(
    root: &Path,
    req: &DecisionRequest,
    mut bundle: ScoreBundle,
    qhat: f64,
) -> Result<ScoreBundle, String> {
    if try_commit_draft(&mut bundle, qhat) {
        return Ok(bundle);
    }
    let mut bundles = vec![bundle];
    escalate_residuals(root, std::slice::from_ref(req), &mut bundles, &[0], qhat)?;
    Ok(bundles.remove(0))
}

/// Lazily load the verify GGUF on first escalate that needs it (keeps prefetch off the 4B).
fn score_with_verify(
    live: &Live,
    root: &Path,
    row: &ereshkigal_core::DecisionRow,
) -> Result<Option<ereshkigal_core::ScoreResult>, String> {
    let mut g = live.verify.lock().map_err(|e| e.to_string())?;
    if g.is_none() {
        let Some(cfg) = &live.verify_cfg else {
            return Ok(None);
        };
        eprintln!(
            "[wordkeep] lazy-loading gguf_verify {}",
            cfg.path.display()
        );
        match load_one(
            &cfg.path,
            &cfg.tok_src,
            &cfg.tok_rev,
            cfg.threads,
            cfg.gpu,
            cfg.n_seq,
            None, // verify stays bare — LoRA targets draft 0.6B only
            1.0,
        ) {
            Ok((s, layers)) => {
                eprintln!("[wordkeep] gguf_verify loaded (gpu_layers={layers})");
                *g = Some(Ok(Mutex::new(s)));
                semif_telemetry::write_status(
                    root,
                    json!({
                        "gguf_verify_loaded": true,
                        "gguf_verify_lazy": true,
                        "gguf_verify_error": null,
                    }),
                );
            }
            Err(e) => {
                eprintln!("[wordkeep] gguf_verify load failed ({e}); cascade-verify disabled");
                *g = Some(Err(e.clone()));
                semif_telemetry::write_status(
                    root,
                    json!({
                        "gguf_verify_loaded": false,
                        "gguf_verify_lazy": true,
                        "gguf_verify_error": e,
                    }),
                );
                return Ok(None);
            }
        }
    }
    match g.as_ref().expect("verify slot filled") {
        Ok(verify) => Ok(Some(score_engine(verify, "direct", row)?)),
        Err(_) => Ok(None),
    }
}

/// Load GGUF engines for `root`. Returns Err if the draft checkpoint is missing.
pub fn ensure_loaded(root: &Path) -> Result<(), String> {
    ensure_arc(root).map(|_| ())
}

/// True after at least one successful `ensure_arc` / score path warmed the slot.
pub fn is_warm() -> bool {
    WARM.lock().map(|g| *g).unwrap_or(false)
}
