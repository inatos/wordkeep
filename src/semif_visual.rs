//! Hybrid vision-native SemIf draft + gated true-logit scaffold.
//!
//! v1 ships a hybrid path: typed visual evidence (PNG paths + hashes) drafts
//! option logits from OCR-decoded page text blended with exact state, then
//! abstains when evidence is clipped / missing / hash-mismatched. True
//! image-token logits stay behind the `visual_logits` feature (no multimodal
//! weights in this crate yet).

use crate::optical;
use crate::semif::{DecisionRequest, OptionSpec, ScoreBundle, Scorer};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

/// Typed visual evidence attached to a DecisionRequest (not stringified into state).
#[derive(Clone, Debug, Default)]
pub struct VisualEvidence {
    pub png_paths: Vec<PathBuf>,
    pub sha256: Vec<String>,
    #[allow(dead_code)] // reserved for multipage decide provenance
    pub page_spans: Vec<String>,
    #[allow(dead_code)]
    pub provenance: String,
    pub clipped: bool,
    /// Protocol version for Copper/Aeron vision payloads (reject unversioned).
    pub protocol_version: u32,
}

impl VisualEvidence {
    pub const PROTOCOL_V1: u32 = 1;

    pub fn from_args(args: &Value) -> Result<Option<Self>, String> {
        let Some(v) = args.get("visual_evidence") else {
            return Ok(None);
        };
        let obj = v
            .as_object()
            .ok_or_else(|| "visual_evidence must be an object".to_string())?;
        let protocol_version = obj
            .get("protocol_version")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        if protocol_version == 0 {
            return Err(
                "visual_evidence.protocol_version required (v1); reject unversioned vision payloads"
                    .into(),
            );
        }
        if protocol_version != Self::PROTOCOL_V1 {
            return Err(format!(
                "unsupported visual_evidence.protocol_version={protocol_version}"
            ));
        }
        let png_paths: Vec<PathBuf> = obj
            .get("png_paths")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(PathBuf::from))
                    .collect()
            })
            .unwrap_or_default();
        let sha256: Vec<String> = obj
            .get("sha256")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let page_spans: Vec<String> = obj
            .get("page_spans")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let provenance = obj
            .get("provenance")
            .and_then(Value::as_str)
            .unwrap_or("caller")
            .to_string();
        let clipped = obj
            .get("clipped")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok(Some(Self {
            png_paths,
            sha256,
            page_spans,
            provenance,
            clipped,
            protocol_version,
        }))
    }

    pub fn verify_hashes(&self) -> Result<(), String> {
        if self.png_paths.is_empty() {
            return Err("visual_evidence.png_paths empty".into());
        }
        if !self.sha256.is_empty() && self.sha256.len() != self.png_paths.len() {
            return Err("visual_evidence.sha256 length must match png_paths".into());
        }
        for (i, path) in self.png_paths.iter().enumerate() {
            if !path.is_file() {
                return Err(format!("missing visual evidence png: {}", path.display()));
            }
            if let Some(expect) = self.sha256.get(i) {
                let bytes = fs::read(path).map_err(|e| e.to_string())?;
                let got = format!("{:x}", Sha256::digest(&bytes));
                if &got != expect {
                    return Err(format!(
                        "visual evidence hash mismatch at {}: got {got} want {expect}",
                        path.display()
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Why a vision draft abstained (exact-text scorer should take over).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VisionAbstain {
    Clipped,
    MissingSource,
    HashMismatch(String),
    EmptyDecode,
    Protocol,
}

impl VisionAbstain {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Clipped => "clipped",
            Self::MissingSource => "missing_source",
            Self::HashMismatch(_) => "hash_mismatch",
            Self::EmptyDecode => "empty_decode",
            Self::Protocol => "protocol",
        }
    }
}

/// Hybrid draft scorer: OCR-decode evidence pages, keyword-overlap vs options.
/// Not true vision logits — those require `visual_logits` + multimodal engine.
pub struct VisualDraftScorer {
    #[allow(dead_code)] // reserved for GPU OCR root staging
    pub root: PathBuf,
}

impl VisualDraftScorer {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    /// Draft or abstain. On Ok(ScoreBundle) caller may commit if conformal; on
    /// Err(VisionAbstain) exact-text scorer must run.
    pub fn draft_or_abstain(
        &self,
        req: &DecisionRequest,
    ) -> Result<ScoreBundle, VisionAbstain> {
        let Some(ev) = &req.visual_evidence else {
            return Err(VisionAbstain::MissingSource);
        };
        if ev.protocol_version != VisualEvidence::PROTOCOL_V1 {
            return Err(VisionAbstain::Protocol);
        }
        if ev.clipped {
            return Err(VisionAbstain::Clipped);
        }
        if let Err(e) = ev.verify_hashes() {
            if e.contains("hash mismatch") {
                return Err(VisionAbstain::HashMismatch(e));
            }
            return Err(VisionAbstain::MissingSource);
        }
        let mut decoded = String::new();
        for png in &ev.png_paths {
            // Prefer sidecar exact page text (deterministic). Else glyph OCR.
            // GPU OCR only when WORDKEEP_VISUAL_GPU=1 (avoid LAN stalls in decide).
            let text = decode_evidence_png(png);
            decoded.push_str(&text);
            decoded.push('\n');
        }
        if decoded.trim().is_empty() {
            return Err(VisionAbstain::EmptyDecode);
        }
        // Blend OCR skim + exact state for keyword overlap (hybrid).
        let blended = format!("{}\n{}", req.state, decoded);
        let raw = keyword_logits(&blended, &req.question, &req.options);
        Ok(ScoreBundle {
            raw,
            already_normalized: false,
            averaged_probs: None,
            prompt_sha256: None,
            cascade_source: Some("visual-hybrid-draft".into()),
            cascade_set_size: None,
        })
    }
}

impl Scorer for VisualDraftScorer {
    fn name(&self) -> &'static str {
        "visual-hybrid"
    }
    fn revision(&self) -> &'static str {
        "v1-hybrid"
    }
    fn score_one(&self, req: &DecisionRequest) -> Result<Vec<(String, f64)>, String> {
        self.draft_or_abstain(req)
            .map(|b| b.raw)
            .map_err(|a| format!("vision abstain: {}", a.as_str()))
    }
    fn score_detailed(&self, req: &DecisionRequest) -> Result<ScoreBundle, String> {
        self.draft_or_abstain(req)
            .map_err(|a| format!("vision abstain: {}", a.as_str()))
    }
}

fn decode_evidence_png(png: &Path) -> String {
    let sidecar = png.with_extension("page.json");
    if sidecar.is_file() {
        if let Ok(raw) = fs::read_to_string(&sidecar) {
            if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                if let Some(t) = v.get("text").and_then(Value::as_str) {
                    return t.to_string();
                }
            }
        }
    }
    match image::image_dimensions(png) {
        Ok((w, h)) => {
            let cols = (w.saturating_sub(16) / 6).max(1);
            let rows = (h.saturating_sub(16) / 8).max(1);
            optical::glyph_ocr_decode(png, cols, rows).unwrap_or_default()
        }
        Err(_) => String::new(),
    }
}

fn keyword_logits(state: &str, question: &str, options: &[OptionSpec]) -> Vec<(String, f64)> {
    let hay = format!("{state} {question}").to_ascii_lowercase();
    let tokens: Vec<&str> = hay
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 2)
        .collect();
    options
        .iter()
        .map(|o| {
            let needle = format!("{} {}", o.id, o.description).to_ascii_lowercase();
            let ntoks: Vec<&str> = needle
                .split(|c: char| !c.is_alphanumeric())
                .filter(|t| t.len() > 2)
                .collect();
            let hits = ntoks.iter().filter(|t| tokens.contains(t)).count() as f64;
            let score = hits / (ntoks.len().max(1) as f64);
            (o.id.clone(), score + 1e-6)
        })
        .collect()
}

/// Route: visual draft → commit if confident, else exact-text scorer.
/// Returns (bundle, abstained, abstain_reason).
pub fn score_hybrid(
    root: &Path,
    text_scorer: &dyn Scorer,
    req: &DecisionRequest,
) -> Result<(ScoreBundle, bool, Option<String>), String> {
    if req.visual_evidence.is_none() {
        let b = text_scorer.score_detailed(req)?;
        return Ok((b, false, None));
    }
    let visual = VisualDraftScorer::new(root);
    match visual.draft_or_abstain(req) {
        Ok(draft) => {
            // Commit only when top option is clearly ahead (simple conformal stand-in).
            let mut sorted = draft.raw.clone();
            sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            let top = sorted.first().map(|(_, s)| *s).unwrap_or(0.0);
            let second = sorted.get(1).map(|(_, s)| *s).unwrap_or(0.0);
            let margin = top - second;
            if margin >= 0.15 && top >= 0.35 {
                Ok((draft, false, None))
            } else {
                // Low confidence → exact-text verify.
                let mut exact = text_scorer.score_detailed(req)?;
                exact.cascade_source = Some("visual-hybrid→exact".into());
                Ok((exact, true, Some("low_visual_margin".into())))
            }
        }
        Err(a) => {
            let mut exact = text_scorer.score_detailed(req)?;
            exact.cascade_source = Some(format!("visual-abstain:{}", a.as_str()));
            Ok((exact, true, Some(a.as_str().into())))
        }
    }
}

/// Gated scaffold for true image-token logits (no weights in v1).
#[cfg(feature = "visual_logits")]
pub trait VisualLogitEngine: Send + Sync {
    fn score_image_options(
        &self,
        png_paths: &[PathBuf],
        question: &str,
        options: &[OptionSpec],
    ) -> Result<ScoreBundle, String>;
}

#[cfg(feature = "visual_logits")]
pub fn visual_logits_available() -> bool {
    true
}

#[cfg(not(feature = "visual_logits"))]
pub fn visual_logits_available() -> bool {
    false
}

/// Telemetry tip for agents / decrees.
pub fn modality_tip(req: &DecisionRequest) -> Value {
    json!({
        "kind": "visual_modality",
        "has_visual_evidence": req.visual_evidence.is_some(),
        "visual_logits_feature": visual_logits_available(),
        "protocol": VisualEvidence::PROTOCOL_V1,
        "routing": ["text", "vision", "hybrid", "exact"],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::optical::{render_doc_page, VisionMode};
    use crate::semif::HeuristicScorer;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp() -> PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("wk-vis-{n}"));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn abstain_on_clipped() {
        let dir = tmp();
        let page = render_doc_page(
            "deployment succeeded health checks passed",
            &dir.join("p.png"),
            VisionMode::Base,
            Some("t"),
        )
        .unwrap();
        let bytes = fs::read(&page.png_path).unwrap();
        let hash = format!("{:x}", Sha256::digest(&bytes));
        let req = DecisionRequest {
            id: "v1".into(),
            state: "The deployment completed. Health checks passed.".into(),
            question: "Did deployment succeed?".into(),
            options: vec![
                OptionSpec {
                    id: "yes".into(),
                    description: "deployment succeeded".into(),
                },
                OptionSpec {
                    id: "no".into(),
                    description: "deployment failed".into(),
                },
            ],
            visual_evidence: Some(VisualEvidence {
                png_paths: vec![page.png_path.clone()],
                sha256: vec![hash],
                page_spans: vec!["0".into()],
                provenance: "test".into(),
                clipped: true,
                protocol_version: VisualEvidence::PROTOCOL_V1,
            }),
        };
        let scorer = VisualDraftScorer::new(&dir);
        assert_eq!(scorer.draft_or_abstain(&req).unwrap_err(), VisionAbstain::Clipped);
        let text = HeuristicScorer;
        let (bundle, abstained, reason) = score_hybrid(&dir, &text, &req).unwrap();
        assert!(abstained);
        assert_eq!(reason.as_deref(), Some("clipped"));
        assert!(!bundle.raw.is_empty());
    }

    #[test]
    fn hybrid_draft_on_clean_page() {
        let dir = tmp();
        let page = render_doc_page(
            "The deployment completed at 14:02. Health checks passed in all zones.",
            &dir.join("p.png"),
            VisionMode::Base,
            Some("t"),
        )
        .unwrap();
        let bytes = fs::read(&page.png_path).unwrap();
        let hash = format!("{:x}", Sha256::digest(&bytes));
        let req = DecisionRequest {
            id: "v2".into(),
            state: "ops log excerpt".into(),
            question: "Did deployment succeed?".into(),
            options: vec![
                OptionSpec {
                    id: "yes".into(),
                    description: "deployment completed health checks passed".into(),
                },
                OptionSpec {
                    id: "no".into(),
                    description: "rollback initiated failure".into(),
                },
            ],
            visual_evidence: Some(VisualEvidence {
                png_paths: vec![page.png_path.clone()],
                sha256: vec![hash],
                page_spans: vec!["0".into()],
                provenance: "test".into(),
                clipped: false,
                protocol_version: VisualEvidence::PROTOCOL_V1,
            }),
        };
        let scorer = VisualDraftScorer::new(&dir);
        let draft = scorer.draft_or_abstain(&req).unwrap();
        assert_eq!(draft.cascade_source.as_deref(), Some("visual-hybrid-draft"));
        let yes = draft.raw.iter().find(|(id, _)| id == "yes").unwrap().1;
        let no = draft.raw.iter().find(|(id, _)| id == "no").unwrap().1;
        assert!(yes >= no, "yes={yes} no={no}");
    }

    #[test]
    fn reject_unversioned_payload() {
        let v = json!({"png_paths": ["/tmp/x.png"]});
        let err = VisualEvidence::from_args(&json!({"visual_evidence": v})).unwrap_err();
        assert!(err.contains("protocol_version"), "{err}");
    }
}
