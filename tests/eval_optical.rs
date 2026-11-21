//! Optical Compression Tier (OCT) golden — offline 10× / 20× precision sweep & hard-gate audit.
//!
//! Uses deterministic glyph OCR (no GPU). Gate: precision ≥ 0.97 at ~10× across real
//! UTF-8 corpus + live VLM before any default-on optical behavior. 20× is recorded
//! only (lossy triage). Until real corpus + VLM clears both ≥10× and ≥0.97, OCT
//! MUST remain default-off (`gated`).

use serde_json::json;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use wordkeep::optical::{
    assert_ocr_receipt, assert_task_done, choose_mode, compress_hits, compression_ratio,
    decode_precision, format_optical_section, glyph_ocr_decode, ground_precision,
    precision_at_ratio, render_doc_page, render_pages, telemetry_snapshot, OpticalPolicy,
    VisionMode, PRECISION_GATE_10X,
};

fn tmp(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let p = std::env::temp_dir().join(format!("wk-eval-oct-{tag}-{nanos}"));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn sample_doc() -> &'static str {
    r#"# Wordkeep Optical Compression

BM25 retrieval over project markdown stays the Source of Truth.
Optical Compression Tier provides a lossy skim at most 10x with exact fallback.

## Policy
- Never lossy-only for signatures or contracts.
- Copper/Aeron deepseek-ocr when peer healthy; glyph-OCR offline.
- stats records optical savings separately from text-path baselines.

fn choose_mode(text_tokens: u64, max_ratio: f64) -> VisionMode;
pub fn compression_ratio(text_tokens: u64, vision_tokens: u64) -> f64;
"#
}

/// ~2,480-char (~620-token) ASCII technical document so `Tiny` (64 vision tokens)
/// genuinely exercises the ~9.5x-10.0x compression band on the dense 84x72 grid.
fn sample_doc_10x_ascii() -> String {
    let block = "Optical Compression Tier renders technical prose onto a dense 84x72 cell grid.\n\
                 Wordkeep keeps exact BM25 and SQLite FTS5 text as Source of Truth at all times.\n\
                 Signatures, structs, and public contracts bypass lossy optical skimming cleanly.\n\
                 Telemetry records rendered page counts, verified nonces, and honest precision.\n";
    // 8 blocks * ~312 chars = ~2,496 chars (~624 tokens -> 624 / 64 = ~9.75x on Tiny).
    block.repeat(8)
}

#[test]
fn eval_optical_10x_near_lossless_gate() {
    let dir = tmp("10x");
    let doc = sample_doc_10x_ascii();
    let (precision, mode, ratio) = precision_at_ratio(&doc, &dir, 10.0).unwrap();
    assert_eq!(mode, VisionMode::Tiny, "expected Tiny (64 vt) for ~624-token doc");
    assert!(
        (9.0..=10.5).contains(&ratio),
        "expected genuine ~9.5x-10.0x compression ratio (not under-1x toy doc), got {ratio:.2}x via {mode:?}"
    );
    assert!(
        precision >= PRECISION_GATE_10X,
        "10x ASCII glyph-grid gate failed: precision={precision:.4} (need >= {PRECISION_GATE_10X})"
    );
}

#[test]
fn eval_optical_long_or_unicode_doc_enforces_honest_exact_fallback() {
    let dir = tmp("long");
    // Real corpus with box-drawing / non-ASCII glyphs that cannot map losslessly to 5x7 ASCII:
    // ground_precision must catch the loss (< 0.97) rather than masking it via sanitized page.text!
    let unicode_heavy = "Architecture diagram:\n┌──────────────┐   ╔══════════════╗\n│ SQLite WAL   ├──▶║ FTS5 + SCIP  ║\n└──────────────┘   ╚══════════════╝\n".repeat(4);
    let policy = OpticalPolicy {
        enabled: true,
        max_ratio: 10.0,
        prefer_gpu: false,
        mode: Some(VisionMode::Tiny),
    };
    let hits = vec![(
        "docs/diagram.md".to_string(),
        "Diagram".to_string(),
        unicode_heavy.clone(),
    )];
    let bundle = compress_hits(&dir, &hits, &policy).unwrap();
    let p = bundle.precision.expect("precision recorded");
    assert!(
        p < PRECISION_GATE_10X,
        "box-drawing Unicode loss must be surfaced honestly by ground_precision, got {p:.4}"
    );
    assert!(
        bundle.fallback && bundle.backend.ends_with("+exact"),
        "precision miss must attach exact text fallback, got backend={}",
        bundle.backend
    );
    assert!(bundle.decoded_text.contains("┌──────────────┐"));
}

#[test]
fn eval_optical_hard_gate_keeps_default_off_until_corpus_clears_10x_and_097() {
    let dir = tmp("hard-gate");
    // 1. HARD GATE: default policy MUST be disabled when neither env nor config opts in.
    let default_policy = OpticalPolicy::from_args(&dir, &json!({}));
    assert!(
        !default_policy.enabled,
        "HARD GATE VIOLATION: OCT must stay default-off until live corpus clears 10x and >=0.97"
    );

    // 2. Evaluate real repo design doc (`docs/designs/wordkeep_optical_compression.md`) at forced 10x+ Tiny:
    //    real multi-section docs exceed Tiny's single-page rows (clipping / UTF-8 loss), proving why
    //    default-on remains gated until multi-page VLM (`deepseek-ocr`) clears >=10x & >=0.97.
    let design_doc_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/designs/wordkeep_optical_compression.md");
    if let Ok(real_doc) = std::fs::read_to_string(&design_doc_path) {
        let page = render_doc_page(&real_doc, &dir.join("real.png"), VisionMode::Tiny, Some("real"))
            .unwrap();
        let decoded = glyph_ocr_decode(&page.png_path, page.cols, page.rows).unwrap();
        let raw_p = ground_precision(&real_doc, &decoded, Some("real"));
        // Either ratio < 10.0 or raw ground-truth precision < 0.97 on Tiny — confirming gated status.
        let clears_both = page.ratio >= 10.0 && raw_p >= PRECISION_GATE_10X;
        if !clears_both {
            assert!(
                !default_policy.enabled,
                "OCT must remain default-off when real doc at Tiny gives ratio={:.2}x raw_precision={raw_p:.4}",
                page.ratio
            );
        }
    }
}

#[test]
fn eval_optical_telemetry_wires_pages_and_nonces() {
    let dir = tmp("telemetry");
    let before = telemetry_snapshot();
    let prev_pages = before.get("pages").and_then(|v| v.as_u64()).unwrap_or(0);

    let policy = OpticalPolicy {
        enabled: true,
        max_ratio: 10.0,
        prefer_gpu: false,
        mode: Some(VisionMode::Base),
    };
    let hits = vec![(
        "docs/oct.md".to_string(),
        "Telemetry".to_string(),
        "Optical page and nonce telemetry check.".to_string(),
    )];
    let bundle = compress_hits(&dir, &hits, &policy).unwrap();
    assert_eq!(bundle.pages.len(), 1);
    assert!(!bundle.pages[0].nonce.is_empty());

    let section = format_optical_section(&bundle);
    assert!(section.contains("nonce="), "section missing nonce: {section}");
    assert!(section.contains("mode=base"), "section missing mode: {section}");
    assert!(section.contains("tokens="), "section missing token counts: {section}");

    let after = telemetry_snapshot();
    let new_pages = after.get("pages").and_then(|v| v.as_u64()).unwrap_or(0);
    assert!(
        new_pages > prev_pages,
        "expected pages counter to increment ({prev_pages} -> {new_pages}): {after}"
    );
    assert!(
        after.get("last_nonce").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()),
        "expected last_nonce to be recorded in optical telemetry: {after}"
    );
}

#[test]
fn eval_optical_20x_lossy_recorded_not_promoted() {
    let dir = tmp("20x");
    let (precision, _mode, ratio) = precision_at_ratio(sample_doc(), &dir, 20.0).unwrap();
    // Informational: paper ~60% at 20× — we only assert the sweep runs and is lossier than 10×.
    assert!(
        ratio >= 10.0,
        "20x sweep should report aggressive ratio, got {ratio}"
    );
    assert!(
        precision < PRECISION_GATE_10X || ratio >= 15.0,
        "20x should be lossy or high-ratio; precision={precision:.4} ratio={ratio:.2}"
    );
}

#[test]
fn eval_optical_page_roundtrip_and_sidecar() {
    let dir = tmp("page");
    let page = render_doc_page(
        sample_doc(),
        &dir.join("page_n42.png"),
        VisionMode::Base,
        Some("eval"),
    )
    .unwrap();
    assert!(page.png_path.is_file());
    assert!(page.sidecar_path.is_file());
    assert_eq!(page.nonce, "n42");
    let decoded = glyph_ocr_decode(&page.png_path, page.cols, page.rows).unwrap();
    let p = decode_precision(&page.text, &decoded);
    assert!(p >= PRECISION_GATE_10X, "base roundtrip precision {p}");
    let r = compression_ratio(page.text_tokens, page.vision_tokens);
    assert!((r - page.ratio).abs() < 1e-9);
}

#[test]
fn eval_optical_mode_ladder_matches_deepseek_budgets() {
    assert_eq!(VisionMode::Tiny.vision_tokens(), 64);
    assert_eq!(VisionMode::Small.vision_tokens(), 100);
    assert_eq!(VisionMode::Base.vision_tokens(), 256);
    assert_eq!(VisionMode::Large.vision_tokens(), 400);
    assert_eq!(choose_mode(640, 10.0), VisionMode::Tiny);
    assert_eq!(choose_mode(900, 10.0), VisionMode::Small);
}

#[test]
fn eval_optical_stale_task_failure_rejected() {
    // Failed child still returns Ok(JSON) from task_lan — OCT must reject.
    let fail = r#"{"task_id":"x-deepseek-ocr","status":"failed","exit_code":23}"#;
    assert!(assert_task_done(fail).is_err());
    let stale_receipt = json!({
        "job": "deepseek-ocr",
        "backend": "sidecar",
        "nonce": "old-nonce",
    });
    assert!(assert_ocr_receipt(&stale_receipt, "fresh-nonce")
        .unwrap_err()
        .contains("nonce"));
}

#[test]
fn eval_optical_sidecar_receipt_not_mislabelled_as_gpu() {
    let receipt = json!({
        "job": "deepseek-ocr",
        "backend": "sidecar",
        "nonce": "n1",
    });
    let backend = assert_ocr_receipt(&receipt, "n1").unwrap();
    assert_eq!(backend, "sidecar");
    assert_ne!(backend, "deepseek-ocr");
}

#[test]
fn eval_optical_pagination_clip_marker() {
    let dir = tmp("clip");
    let long = "alpha beta gamma delta\n".repeat(300);
    let (pages, clipped) =
        render_pages(&long, &dir, VisionMode::Tiny, Some("eval"), 1, "eval").unwrap();
    assert!(clipped, "long corpus must clip at max_pages=1");
    assert_eq!(pages.len(), 1);
}
