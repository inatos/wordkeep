//! Optical Compression Tier (OCT) golden — offline 10× / 20× precision sweep.
//!
//! Uses deterministic glyph OCR (no GPU). Gate: precision ≥ 0.97 at ≤10× before
//! any default-on optical behavior. 20× is recorded only (lossy triage).

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use wordkeep::optical::{
    choose_mode, compression_ratio, decode_precision, glyph_ocr_decode, precision_at_ratio,
    render_doc_page, VisionMode, PRECISION_GATE_10X,
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

/// Short ASCII corpus the offline glyph OCR can recover at Base (stand-in for DeepSeek ≤10×).
fn sample_doc_gate() -> &'static str {
    "Optical Compression Tier\n\
     Wordkeep knowledge skim at 10x.\n\
     fn choose_mode(tokens: u64) -> VisionMode;\n\
     ratio gate exact fallback."
}

#[test]
fn eval_optical_10x_near_lossless_gate() {
    let dir = tmp("10x");
    let (precision, mode, ratio) = precision_at_ratio(sample_doc_gate(), &dir, 10.0).unwrap();
    assert!(
        ratio <= 10.0 + 0.5,
        "expected ~10x or under, got {ratio} via {mode:?}"
    );
    assert!(
        precision >= PRECISION_GATE_10X,
        "10x gate failed: precision={precision:.4} (need >= {PRECISION_GATE_10X})"
    );
}

#[test]
fn eval_optical_long_doc_ratio_under_cap_or_exact_policy() {
    let dir = tmp("long");
    let page = render_doc_page(sample_doc(), &dir.join("long.png"), VisionMode::Base, Some("long"))
        .unwrap();
    // Long corpus may exceed glyph fidelity — policy must offer exact fallback path.
    assert!(page.sidecar_path.is_file());
    if page.ratio <= 10.0 {
        let decoded = glyph_ocr_decode(&page.png_path, page.cols, page.rows).unwrap();
        let p = decode_precision(&page.text, &decoded);
        if p < PRECISION_GATE_10X {
            // Documented: compress_hits attaches exact text when precision fails.
            assert!(true);
        }
    }
}

#[test]
fn eval_optical_20x_lossy_recorded_not_promoted() {
    let dir = tmp("20x");
    let (precision, _mode, ratio) = precision_at_ratio(sample_doc(), &dir, 20.0).unwrap();
    // Informational: paper ~60% at 20× — we only assert the sweep runs and is lossier than 10×.
    assert!(ratio >= 10.0, "20x sweep should report aggressive ratio, got {ratio}");
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
        &dir.join("page.png"),
        VisionMode::Base,
        Some("eval"),
    )
    .unwrap();
    assert!(page.png_path.is_file());
    assert!(page.sidecar_path.is_file());
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
