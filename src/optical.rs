//! Optical Compression Tier (OCT) — DeepSeek-OCR-inspired page render + decode.
//!
//! Text remains Source of Truth (SoT). Optics provide a ≤10× lossy skim with
//! exact fallback. Offline eval uses a deterministic 5×7 glyph OCR so CI does
//! not need a GPU; live encode may route through Palaestra `task_submit`
//! (`deepseek-ocr`) when Copper/Aeron is healthy.

use crate::{config, copper_health, stats, task_lan};
use image::{Rgba, RgbaImage};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// DeepSeek-OCR native resolution modes → vision token budgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisionMode {
    Tiny,  // 512² → 64
    Small, // 640² → 100
    Base,  // 1024² → 256
    Large, // 1280² → 400
}

impl VisionMode {
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "tiny" | "512" => Some(Self::Tiny),
            "small" | "640" => Some(Self::Small),
            "base" | "1024" | "" => Some(Self::Base),
            "large" | "1280" => Some(Self::Large),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tiny => "tiny",
            Self::Small => "small",
            Self::Base => "base",
            Self::Large => "large",
        }
    }

    pub fn vision_tokens(self) -> u64 {
        match self {
            Self::Tiny => 64,
            Self::Small => 100,
            Self::Base => 256,
            Self::Large => 400,
        }
    }

    pub fn edge_px(self) -> u32 {
        match self {
            Self::Tiny => 512,
            Self::Small => 640,
            Self::Base => 1024,
            Self::Large => 1280,
        }
    }
}

/// Default policy: never exceed 10× for promote-able skims.
pub const MAX_RATIO_DEFAULT: f64 = 10.0;
/// Near-lossless gate used by eval / policy (paper ~97% @ ≤10×).
pub const PRECISION_GATE_10X: f64 = 0.97;

/// 5×7 monospace glyph bitmaps for printable ASCII (space..~).
/// Rows are bitmasks; bit 4 is leftmost column.
const FONT_W: u32 = 5;
const FONT_H: u32 = 7;
const CELL_PAD_X: u32 = 1;
const CELL_PAD_Y: u32 = 1;
const CELL_W: u32 = FONT_W + CELL_PAD_X;
const CELL_H: u32 = FONT_H + CELL_PAD_Y;

fn glyph_bits(ch: char) -> Option<[u8; 7]> {
    // Compact subset: digits, letters, and common punctuation for docs/code.
    let c = ch as u8;
    if !(0x20..=0x7e).contains(&c) {
        return None;
    }
    Some(GLYPHS[(c - 0x20) as usize])
}

/// Precomputed 5×7 glyphs for ASCII 0x20..=0x7E (95 entries).
#[rustfmt::skip]
const GLYPHS: [[u8; 7]; 95] = {
    let mut g = [[0u8; 7]; 95];
    // space
    g[0] = [0, 0, 0, 0, 0, 0, 0];
    // ! # $ % & ' ( ) * + , - . / 0-9 : ; < = > ? @ A-Z [ \ ] ^ _ ` a-z { | } ~
    // Hand-authored dense set — enough for recoverability eval.
    macro_rules! set {
        ($i:expr, $($r:expr),+) => {{ g[$i] = [$($r),+]; }};
    }
    set!(1, 0x04, 0x04, 0x04, 0x04, 0x04, 0x00, 0x04); // !
    set!(2, 0x0a, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00); // "
    set!(3, 0x0a, 0x1f, 0x0a, 0x0a, 0x1f, 0x0a, 0x00); // #
    set!(4, 0x04, 0x0f, 0x14, 0x0e, 0x05, 0x1e, 0x04); // $
    set!(5, 0x19, 0x19, 0x02, 0x04, 0x08, 0x13, 0x13); // %
    set!(6, 0x08, 0x14, 0x14, 0x08, 0x15, 0x12, 0x0d); // &
    set!(7, 0x04, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00); // '
    set!(8, 0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02); // (
    set!(9, 0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08); // )
    set!(10, 0x00, 0x0a, 0x04, 0x1f, 0x04, 0x0a, 0x00); // *
    set!(11, 0x00, 0x04, 0x04, 0x1f, 0x04, 0x04, 0x00); // +
    set!(12, 0x00, 0x00, 0x00, 0x00, 0x04, 0x04, 0x08); // ,
    set!(13, 0x00, 0x00, 0x00, 0x1f, 0x00, 0x00, 0x00); // -
    set!(14, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0c, 0x0c); // .
    set!(15, 0x01, 0x01, 0x02, 0x04, 0x08, 0x10, 0x10); // /
    set!(16, 0x0e, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0e); // 0
    set!(17, 0x04, 0x0c, 0x04, 0x04, 0x04, 0x04, 0x0e); // 1
    set!(18, 0x0e, 0x11, 0x01, 0x06, 0x08, 0x10, 0x1f); // 2
    set!(19, 0x1f, 0x01, 0x02, 0x06, 0x01, 0x11, 0x0e); // 3
    set!(20, 0x02, 0x06, 0x0a, 0x12, 0x1f, 0x02, 0x02); // 4
    set!(21, 0x1f, 0x10, 0x1e, 0x01, 0x01, 0x11, 0x0e); // 5
    set!(22, 0x06, 0x08, 0x10, 0x1e, 0x11, 0x11, 0x0e); // 6
    set!(23, 0x1f, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08); // 7
    set!(24, 0x0e, 0x11, 0x11, 0x0e, 0x11, 0x11, 0x0e); // 8
    set!(25, 0x0e, 0x11, 0x11, 0x0f, 0x01, 0x02, 0x0c); // 9
    set!(26, 0x00, 0x0c, 0x0c, 0x00, 0x0c, 0x0c, 0x00); // :
    set!(27, 0x00, 0x0c, 0x0c, 0x00, 0x0c, 0x04, 0x08); // ;
    set!(28, 0x02, 0x04, 0x08, 0x10, 0x08, 0x04, 0x02); // <
    set!(29, 0x00, 0x00, 0x1f, 0x00, 0x1f, 0x00, 0x00); // =
    set!(30, 0x08, 0x04, 0x02, 0x01, 0x02, 0x04, 0x08); // >
    set!(31, 0x0e, 0x11, 0x01, 0x06, 0x04, 0x00, 0x04); // ?
    set!(32, 0x0e, 0x11, 0x17, 0x15, 0x17, 0x10, 0x0e); // @
    set!(33, 0x0e, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11); // A
    set!(34, 0x1e, 0x11, 0x11, 0x1e, 0x11, 0x11, 0x1e); // B
    set!(35, 0x0e, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0e); // C
    set!(36, 0x1e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1e); // D
    set!(37, 0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x1f); // E
    set!(38, 0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x10); // F
    set!(39, 0x0e, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0f); // G
    set!(40, 0x11, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11); // H
    set!(41, 0x0e, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0e); // I
    set!(42, 0x01, 0x01, 0x01, 0x01, 0x11, 0x11, 0x0e); // J
    set!(43, 0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11); // K
    set!(44, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1f); // L
    set!(45, 0x11, 0x1b, 0x15, 0x15, 0x11, 0x11, 0x11); // M
    set!(46, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11); // N
    set!(47, 0x0e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e); // O
    set!(48, 0x1e, 0x11, 0x11, 0x1e, 0x10, 0x10, 0x10); // P
    set!(49, 0x0e, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0d); // Q
    set!(50, 0x1e, 0x11, 0x11, 0x1e, 0x14, 0x12, 0x11); // R
    set!(51, 0x0f, 0x10, 0x10, 0x0e, 0x01, 0x01, 0x1e); // S
    set!(52, 0x1f, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04); // T
    set!(53, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e); // U
    set!(54, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0a, 0x04); // V
    set!(55, 0x11, 0x11, 0x11, 0x15, 0x15, 0x1b, 0x11); // W
    set!(56, 0x11, 0x11, 0x0a, 0x04, 0x0a, 0x11, 0x11); // X
    set!(57, 0x11, 0x11, 0x0a, 0x04, 0x04, 0x04, 0x04); // Y
    set!(58, 0x1f, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1f); // Z
    set!(59, 0x0e, 0x08, 0x08, 0x08, 0x08, 0x08, 0x0e); // [
    set!(60, 0x10, 0x10, 0x08, 0x04, 0x02, 0x01, 0x01); // \
    set!(61, 0x0e, 0x02, 0x02, 0x02, 0x02, 0x02, 0x0e); // ]
    set!(62, 0x04, 0x0a, 0x11, 0x00, 0x00, 0x00, 0x00); // ^
    set!(63, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1f); // _
    set!(64, 0x08, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00); // `
    set!(65, 0x00, 0x00, 0x0e, 0x01, 0x0f, 0x11, 0x0f); // a
    set!(66, 0x10, 0x10, 0x1e, 0x11, 0x11, 0x11, 0x1e); // b
    set!(67, 0x00, 0x00, 0x0e, 0x10, 0x10, 0x11, 0x0e); // c
    set!(68, 0x01, 0x01, 0x0f, 0x11, 0x11, 0x11, 0x0f); // d
    set!(69, 0x00, 0x00, 0x0e, 0x11, 0x1f, 0x10, 0x0e); // e
    set!(70, 0x06, 0x08, 0x08, 0x1e, 0x08, 0x08, 0x08); // f
    set!(71, 0x00, 0x00, 0x0f, 0x11, 0x0f, 0x01, 0x0e); // g
    set!(72, 0x10, 0x10, 0x1e, 0x11, 0x11, 0x11, 0x11); // h
    set!(73, 0x04, 0x00, 0x0c, 0x04, 0x04, 0x04, 0x0e); // i
    set!(74, 0x02, 0x00, 0x06, 0x02, 0x02, 0x12, 0x0c); // j
    set!(75, 0x10, 0x10, 0x12, 0x14, 0x18, 0x14, 0x12); // k
    set!(76, 0x0c, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0e); // l
    set!(77, 0x00, 0x00, 0x1a, 0x15, 0x15, 0x11, 0x11); // m
    set!(78, 0x00, 0x00, 0x1e, 0x11, 0x11, 0x11, 0x11); // n
    set!(79, 0x00, 0x00, 0x0e, 0x11, 0x11, 0x11, 0x0e); // o
    set!(80, 0x00, 0x00, 0x1e, 0x11, 0x1e, 0x10, 0x10); // p
    set!(81, 0x00, 0x00, 0x0f, 0x11, 0x0f, 0x01, 0x01); // q
    set!(82, 0x00, 0x00, 0x16, 0x19, 0x10, 0x10, 0x10); // r
    set!(83, 0x00, 0x00, 0x0f, 0x10, 0x0e, 0x01, 0x1e); // s
    set!(84, 0x08, 0x08, 0x1e, 0x08, 0x08, 0x08, 0x06); // t
    set!(85, 0x00, 0x00, 0x11, 0x11, 0x11, 0x11, 0x0f); // u
    set!(86, 0x00, 0x00, 0x11, 0x11, 0x11, 0x0a, 0x04); // v
    set!(87, 0x00, 0x00, 0x11, 0x11, 0x15, 0x15, 0x0a); // w
    set!(88, 0x00, 0x00, 0x11, 0x0a, 0x04, 0x0a, 0x11); // x
    set!(89, 0x00, 0x00, 0x11, 0x11, 0x0f, 0x01, 0x0e); // y
    set!(90, 0x00, 0x00, 0x1f, 0x02, 0x04, 0x08, 0x1f); // z
    set!(91, 0x06, 0x08, 0x08, 0x10, 0x08, 0x08, 0x06); // {
    set!(92, 0x04, 0x04, 0x04, 0x00, 0x04, 0x04, 0x04); // |
    set!(93, 0x0c, 0x02, 0x02, 0x01, 0x02, 0x02, 0x0c); // }
    set!(94, 0x08, 0x15, 0x02, 0x00, 0x00, 0x00, 0x00); // ~
    g
};

fn approx_tokens(text: &str) -> u64 {
    (text.len() as u64).div_ceil(4).max(1)
}

/// Pick the smallest vision mode that keeps ratio ≤ `max_ratio`, or Large if none fit.
pub fn choose_mode(text_tokens: u64, max_ratio: f64) -> VisionMode {
    for mode in [
        VisionMode::Tiny,
        VisionMode::Small,
        VisionMode::Base,
        VisionMode::Large,
    ] {
        let vt = mode.vision_tokens() as f64;
        if vt > 0.0 && (text_tokens as f64) / vt <= max_ratio {
            return mode;
        }
    }
    VisionMode::Large
}

pub fn compression_ratio(text_tokens: u64, vision_tokens: u64) -> f64 {
    if vision_tokens == 0 {
        return f64::INFINITY;
    }
    text_tokens as f64 / vision_tokens as f64
}

#[derive(Debug, Clone)]
pub struct RenderedPage {
    pub png_path: PathBuf,
    pub sidecar_path: PathBuf,
    pub mode: VisionMode,
    pub text: String,
    pub text_tokens: u64,
    pub vision_tokens: u64,
    pub ratio: f64,
    pub cols: u32,
    pub rows: u32,
}

// Fields are consumed by eval / MCP callers; bin dead_code lint ignores lib usage.

/// Render markdown/code as a single-layout monospace page PNG + JSON sidecar.
pub fn render_doc_page(
    text: &str,
    out_png: &Path,
    mode: VisionMode,
    title: Option<&str>,
) -> Result<RenderedPage, String> {
    let edge = mode.edge_px();
    let margin = 8u32;
    let usable_w = edge.saturating_sub(margin * 2);
    let usable_h = edge.saturating_sub(margin * 2);
    let cols = (usable_w / CELL_W).max(1);
    let rows = (usable_h / CELL_H).max(1);

    let mut lines: Vec<String> = Vec::new();
    if let Some(t) = title {
        lines.push(format!("# {t}"));
        lines.push(String::new());
    }
    for raw in text.lines() {
        let sanitized: String = raw
            .chars()
            .map(|c| if (0x20..=0x7e).contains(&(c as u8)) { c } else { '?' })
            .collect();
        if sanitized.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut rest = sanitized.as_str();
        while !rest.is_empty() {
            let take = rest.chars().take(cols as usize).collect::<String>();
            let n = take.len();
            lines.push(take);
            rest = &rest[n..];
        }
    }
    // Fit into page rows (single layout — overflow truncated with marker).
    let mut page_lines: Vec<String> = lines.into_iter().take(rows as usize).collect();
    if page_lines.len() as u32 >= rows && text.lines().count() > rows as usize {
        if let Some(last) = page_lines.last_mut() {
            *last = "...(truncated)".to_string();
        }
    }
    while (page_lines.len() as u32) < rows {
        page_lines.push(String::new());
    }

    let mut img = RgbaImage::from_pixel(edge, edge, Rgba([250, 250, 248, 255]));
    let ink = Rgba([20, 22, 28, 255]);
    for (row, line) in page_lines.iter().enumerate() {
        for (col, ch) in line.chars().enumerate() {
            draw_glyph(
                &mut img,
                margin + col as u32 * CELL_W,
                margin + row as u32 * CELL_H,
                ch,
                ink,
            );
        }
    }

    if let Some(parent) = out_png.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    img.save(out_png)
        .map_err(|e| format!("save {}: {e}", out_png.display()))?;

    // Persist logical text without trailing blank pad rows (OCR compare + ratio).
    let page_text = page_lines
        .iter()
        .rev()
        .skip_while(|l| l.is_empty())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    let text_tokens = approx_tokens(&page_text);
    let vision_tokens = mode.vision_tokens();
    let ratio = compression_ratio(text_tokens, vision_tokens);

    let sidecar_path = out_png.with_extension("page.json");
    let meta = json!({
        "v": 1,
        "mode": mode.as_str(),
        "edge_px": edge,
        "cols": cols,
        "rows": rows,
        "text_tokens": text_tokens,
        "vision_tokens": vision_tokens,
        "ratio": ratio,
        "text": page_text,
        "png": out_png.display().to_string(),
    });
    fs::write(&sidecar_path, serde_json::to_string_pretty(&meta).unwrap_or_default())
        .map_err(|e| format!("write sidecar: {e}"))?;

    Ok(RenderedPage {
        png_path: out_png.to_path_buf(),
        sidecar_path,
        mode,
        text: page_text,
        text_tokens,
        vision_tokens,
        ratio,
        cols,
        rows,
    })
}

fn draw_glyph(img: &mut RgbaImage, x: u32, y: u32, ch: char, color: Rgba<u8>) {
    let Some(bits) = glyph_bits(ch) else {
        return;
    };
    for (row, mask) in bits.iter().enumerate() {
        for col in 0..FONT_W {
            if (mask >> (FONT_W - 1 - col)) & 1 == 1 {
                let px = x + col;
                let py = y + row as u32;
                if px < img.width() && py < img.height() {
                    img.put_pixel(px, py, color);
                }
            }
        }
    }
}

/// Deterministic glyph OCR over a page rendered by [`render_doc_page`].
pub fn glyph_ocr_decode(png: &Path, cols: u32, rows: u32) -> Result<String, String> {
    let img = image::open(png)
        .map_err(|e| format!("open {}: {e}", png.display()))?
        .to_rgba8();
    let margin = 8u32;
    let mut lines = Vec::with_capacity(rows as usize);
    for row in 0..rows {
        let mut s = String::with_capacity(cols as usize);
        for col in 0..cols {
            let x = margin + col * CELL_W;
            let y = margin + row * CELL_H;
            s.push(recognize_cell(&img, x, y));
        }
        // Trim trailing spaces for fair comparison.
        lines.push(s.trim_end().to_string());
    }
    Ok(lines.join("\n").trim_end().to_string())
}

fn recognize_cell(img: &RgbaImage, x: u32, y: u32) -> char {
    let mut best = ' ';
    let mut best_score = i32::MIN;
    for code in 0x20u8..=0x7e {
        let ch = code as char;
        let Some(bits) = glyph_bits(ch) else {
            continue;
        };
        let mut score = 0i32;
        for (row, mask) in bits.iter().enumerate() {
            for col in 0..FONT_W {
                let px = x + col;
                let py = y + row as u32;
                if px >= img.width() || py >= img.height() {
                    continue;
                }
                let p = img.get_pixel(px, py);
                let dark = (p[0] as u16 + p[1] as u16 + p[2] as u16) < 200 * 3;
                let expect = ((mask >> (FONT_W - 1 - col)) & 1) == 1;
                if dark == expect {
                    score += 1;
                } else {
                    score -= 2;
                }
            }
        }
        if score > best_score {
            best_score = score;
            best = ch;
        }
    }
    best
}

/// Character-level precision of `decoded` vs `ground` (trims trailing whitespace).
pub fn decode_precision(ground: &str, decoded: &str) -> f64 {
    let g: Vec<char> = ground.trim_end().chars().collect();
    let d: Vec<char> = decoded.trim_end().chars().collect();
    if g.is_empty() {
        return if d.is_empty() { 1.0 } else { 0.0 };
    }
    let n = g.len().max(d.len());
    let mut ok = 0usize;
    for i in 0..n {
        let gc = g.get(i).copied().unwrap_or('\0');
        let dc = d.get(i).copied().unwrap_or('\0');
        if gc == dc {
            ok += 1;
        }
    }
    ok as f64 / n as f64
}

/// Simulate higher compression by downscaling then upscaling (blur) before OCR.
pub fn precision_at_ratio(
    text: &str,
    work_dir: &Path,
    target_ratio: f64,
) -> Result<(f64, VisionMode, f64), String> {
    fs::create_dir_all(work_dir).map_err(|e| e.to_string())?;
    let tokens = approx_tokens(text);
    // Choose mode so ratio ≈ target (may exceed if text is huge).
    let mode = if target_ratio <= 10.0 {
        choose_mode(tokens, target_ratio)
    } else {
        // Force Tiny for aggressive 20× triage experiments.
        VisionMode::Tiny
    };
    let png = work_dir.join(format!("page_{}.png", mode.as_str()));
    let page = render_doc_page(text, &png, mode, Some("oct-eval"))?;
    let actual = page.ratio;
    // Artificial blur only for aggressive (>10×) triage experiments when the
    // natural page ratio is lower — never degrade the ≤10× near-lossless gate.
    if target_ratio > 10.0 && target_ratio > actual + 0.5 {
        let mut img = image::open(&page.png_path)
            .map_err(|e| e.to_string())?
            .to_rgba8();
        let factor = (actual / target_ratio).clamp(0.15, 1.0);
        let w = ((img.width() as f64) * factor).max(16.0) as u32;
        let h = ((img.height() as f64) * factor).max(16.0) as u32;
        let small = image::imageops::resize(&img, w, h, image::imageops::FilterType::Triangle);
        img = image::imageops::resize(
            &small,
            page.mode.edge_px(),
            page.mode.edge_px(),
            image::imageops::FilterType::Triangle,
        );
        img.save(&page.png_path).map_err(|e| e.to_string())?;
    }

    let decoded = glyph_ocr_decode(&page.png_path, page.cols, page.rows)?;
    let precision = decode_precision(&page.text, &decoded);
    let ratio = if target_ratio > 10.0 && target_ratio > actual {
        target_ratio
    } else {
        actual
    };
    Ok((precision, mode, ratio))
}

#[derive(Debug, Clone)]
pub struct OpticalPolicy {
    pub enabled: bool,
    pub max_ratio: f64,
    pub prefer_gpu: bool,
    pub mode: Option<VisionMode>,
}

impl OpticalPolicy {
    pub fn from_args(root: &Path, args: &Value) -> Self {
        let enabled = args
            .get("optical")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || config::optical_default_enabled(root);
        let max_ratio = args
            .get("optical_max_ratio")
            .and_then(Value::as_f64)
            .or_else(|| config::optical_max_ratio(root))
            .unwrap_or(MAX_RATIO_DEFAULT)
            .clamp(1.0, 20.0);
        let prefer_gpu = args
            .get("optical_gpu")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let mode = args
            .get("optical_mode")
            .and_then(Value::as_str)
            .and_then(VisionMode::from_str_loose);
        Self {
            enabled,
            max_ratio,
            prefer_gpu,
            mode,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OpticalHitBundle {
    pub pages: Vec<RenderedPage>,
    pub decoded_text: String,
    pub backend: String,
    /// Backend from job receipt (`deepseek-ocr` / `sidecar`); may differ from `backend` label.
    pub receipt_backend: Option<String>,
    pub fallback: bool,
    pub fallback_reason: Option<String>,
    pub png_paths: Vec<String>,
    pub ratio: f64,
    pub precision: Option<f64>,
    /// True when page render clipped the corpus (tail discarded).
    pub clipped: bool,
}

/// Result of a Palaestra deepseek-ocr attempt (honest receipt backend).
#[derive(Debug, Clone)]
pub struct OcrTaskResult {
    pub text: String,
    pub backend: String,
    pub nonce: String,
}

/// Fresh nonce for OCT artifacts / Palaestra receipt echo-through.
pub fn fresh_nonce() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    // Mix in pid so concurrent agents collide less.
    format!("{nanos}-{}", std::process::id())
}

/// Paginate `text` into one or more rendered pages under `out_dir`.
/// Single-column chronology; pages after the first are titled `title pN`.
/// When clipped, the last page ends with `...(truncated)` and `clipped` is true.
pub fn render_pages(
    text: &str,
    out_dir: &Path,
    mode: VisionMode,
    title: Option<&str>,
    max_pages: usize,
    file_prefix: &str,
) -> Result<(Vec<RenderedPage>, bool), String> {
    fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
    let edge = mode.edge_px();
    let margin = 8u32;
    let cols = ((edge.saturating_sub(margin * 2)) / CELL_W).max(1);
    let rows = ((edge.saturating_sub(margin * 2)) / CELL_H).max(1);
    let body_rows = rows.saturating_sub(2).max(1); // leave room for title + blank

    let mut wrapped: Vec<String> = Vec::new();
    for raw in text.lines() {
        let sanitized: String = raw
            .chars()
            .map(|c| if (0x20..=0x7e).contains(&(c as u8)) { c } else { '?' })
            .collect();
        if sanitized.is_empty() {
            wrapped.push(String::new());
            continue;
        }
        let mut rest = sanitized.as_str();
        while !rest.is_empty() {
            let take = rest.chars().take(cols as usize).collect::<String>();
            let n = take.len();
            wrapped.push(take);
            rest = &rest[n..];
        }
    }

    let max_pages = max_pages.max(1);
    let mut pages = Vec::new();
    let mut offset = 0usize;
    let mut clipped = false;
    let mut page_idx = 0usize;
    while offset < wrapped.len() && page_idx < max_pages {
        let end = (offset + body_rows as usize).min(wrapped.len());
        let chunk = &wrapped[offset..end];
        let more = end < wrapped.len();
        let will_clip = more && page_idx + 1 >= max_pages;
        let mut page_body = chunk.join("\n");
        if will_clip {
            page_body.push_str("\n...(truncated)");
            clipped = true;
        }
        let page_title = match title {
            Some(t) if page_idx == 0 => t.to_string(),
            Some(t) => format!("{t} p{}", page_idx + 1),
            None => format!("page {}", page_idx + 1),
        };
        let nonce = fresh_nonce();
        let png = out_dir.join(format!("{file_prefix}_{nonce}.png"));
        let page = render_doc_page(&page_body, &png, mode, Some(&page_title))?;
        pages.push(page);
        offset = end;
        page_idx += 1;
        if !more {
            break;
        }
    }
    if offset < wrapped.len() {
        clipped = true;
    }
    Ok((pages, clipped))
}

/// Decode a rendered page: GPU OCR when preferred+peer, else glyph OCR.
pub fn ocr_decode_page(
    root: &Path,
    page: &RenderedPage,
    prefer_gpu: bool,
) -> Result<(String, String /* backend */, Option<String> /* receipt */), String> {
    let peer_ready = copper_health::prefer_copper_cached(root);
    if prefer_gpu && peer_ready {
        match try_deepseek_ocr_task(root, &page.png_path) {
            Ok(r) => Ok((r.text, r.backend.clone(), Some(r.backend))),
            Err(e) => {
                let text = glyph_ocr_decode(&page.png_path, page.cols, page.rows)?;
                Ok((text, "glyph-ocr".into(), Some(format!("gpu-err:{e}"))))
            }
        }
    } else {
        let text = glyph_ocr_decode(&page.png_path, page.cols, page.rows)?;
        Ok((text, "glyph-ocr".into(), None))
    }
}

/// Build optical skim for knowledge hits. Falls back to concatenating exact text.
pub fn compress_hits(
    root: &Path,
    hits: &[(String /* path */, String /* heading */, String /* body */)],
    policy: &OpticalPolicy,
) -> Result<OpticalHitBundle, String> {
    if hits.is_empty() {
        return Ok(OpticalHitBundle {
            pages: Vec::new(),
            decoded_text: String::new(),
            backend: "none".into(),
            receipt_backend: None,
            fallback: true,
            fallback_reason: Some("no hits".into()),
            png_paths: Vec::new(),
            ratio: 0.0,
            precision: None,
            clipped: false,
        });
    }

    let mut corpus = String::new();
    for (i, (path, heading, body)) in hits.iter().enumerate() {
        if i > 0 {
            corpus.push_str("\n\n");
        }
        corpus.push_str(&format!("## {path} — {heading}\n{body}"));
    }

    let tokens = approx_tokens(&corpus);
    let mode = policy
        .mode
        .unwrap_or_else(|| choose_mode(tokens, policy.max_ratio));
    let ratio = compression_ratio(tokens, mode.vision_tokens());

    // Hard policy: if ratio would exceed max without forcing, fall back to exact text.
    if ratio > policy.max_ratio + 0.01 && policy.mode.is_none() {
        stats::record_optical(false, ratio, 0.0);
        return Ok(OpticalHitBundle {
            pages: Vec::new(),
            decoded_text: corpus,
            backend: "exact-text".into(),
            receipt_backend: None,
            fallback: true,
            fallback_reason: Some(format!(
                "ratio {ratio:.1}x exceeds max {:.1}x — exact fallback",
                policy.max_ratio
            )),
            png_paths: Vec::new(),
            ratio,
            precision: Some(1.0),
            clipped: false,
        });
    }

    let out_dir = root.join(".cache/wordkeep/optical");
    let (pages, clipped) = render_pages(
        &corpus,
        &out_dir,
        mode,
        Some("knowledge_search"),
        1,
        "knowledge",
    )?;
    let page = pages
        .first()
        .ok_or_else(|| "render_pages produced no page".to_string())?
        .clone();

    let mut backend = "glyph-ocr".to_string();
    let mut receipt_backend = None;
    let mut fallback = false;
    let mut fallback_reason = None;
    if clipped {
        fallback = true;
        fallback_reason = Some("page clipped — exact text attached for omitted tail".into());
    }

    // Cache-only peer glance — never stall MCP on copper_health.sh / aeron ping.
    let peer_ready = copper_health::prefer_copper_cached(root);
    let mut decoded = if policy.prefer_gpu && peer_ready {
        match try_deepseek_ocr_task(root, &page.png_path) {
            Ok(r) => {
                // Honest label: sidecar is not GPU OCR.
                backend = r.backend.clone();
                receipt_backend = Some(r.backend.clone());
                if r.backend != "deepseek-ocr" {
                    fallback = true;
                    fallback_reason = Some(format!(
                        "receipt backend={} (not deepseek-ocr)",
                        r.backend
                    ));
                }
                r.text
            }
            Err(e) => {
                fallback = true;
                fallback_reason = Some(format!("GPU OCR unavailable ({e}); glyph-ocr fallback"));
                glyph_ocr_decode(&page.png_path, page.cols, page.rows)?
            }
        }
    } else {
        if policy.prefer_gpu && !peer_ready {
            fallback = true;
            fallback_reason = Some("peer down (cached) — glyph-ocr / exact text path".into());
        }
        glyph_ocr_decode(&page.png_path, page.cols, page.rows)?
    };

    let precision = decode_precision(&page.text, &decoded);
    // Exact drill-down if precision fails the 10× gate.
    if precision < PRECISION_GATE_10X && page.ratio <= policy.max_ratio {
        fallback = true;
        fallback_reason = Some(format!(
            "precision {precision:.3} < {PRECISION_GATE_10X} — exact text attached"
        ));
        decoded = page.text.clone();
        backend = format!("{backend}+exact");
    }
    if clipped {
        // Attach full corpus so clipped skim never silently drops SoT.
        decoded = corpus.clone();
        backend = format!("{backend}+exact");
    }

    // Only count as GPU optical success when receipt says deepseek-ocr and no fallback.
    let gpu_ok = !fallback && backend == "deepseek-ocr";
    stats::record_optical(gpu_ok, page.ratio, precision);

    Ok(OpticalHitBundle {
        png_paths: vec![page.png_path.display().to_string()],
        ratio: page.ratio,
        precision: Some(precision),
        pages: vec![page],
        decoded_text: decoded,
        backend,
        receipt_backend,
        fallback,
        fallback_reason,
        clipped,
    })
}

/// Parse task_submit JSON and reject failed / non-done jobs (stale-guard).
pub fn assert_task_done(reply: &str) -> Result<Value, String> {
    let v: Value = serde_json::from_str(reply)
        .map_err(|e| format!("task reply not JSON: {e}"))?;
    let status = v
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("");
    let exit = v.get("exit_code").and_then(Value::as_i64).unwrap_or(-1);
    if status != "done" || exit != 0 {
        return Err(format!(
            "deepseek-ocr task not done (status={status} exit_code={exit})"
        ));
    }
    Ok(v)
}

/// Validate receipt.json against expected nonce/job; reject empty backend.
pub fn assert_ocr_receipt(receipt: &Value, expected_nonce: &str) -> Result<String, String> {
    let job = receipt.get("job").and_then(Value::as_str).unwrap_or("");
    if job != "deepseek-ocr" {
        return Err(format!("receipt job mismatch: {job}"));
    }
    let nonce = receipt
        .get("nonce")
        .and_then(Value::as_str)
        .unwrap_or("");
    if nonce != expected_nonce {
        return Err(format!(
            "receipt nonce mismatch: got {nonce:?} want {expected_nonce:?}"
        ));
    }
    let backend = receipt
        .get("backend")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if backend.is_empty() || backend == "empty" {
        return Err(format!("receipt backend rejected: {backend:?}"));
    }
    Ok(backend)
}

fn try_deepseek_ocr_task(root: &Path, png: &Path) -> Result<OcrTaskResult, String> {
    let nonce = fresh_nonce();
    let args = json!({
        "job": "deepseek-ocr",
        "wait": true,
    });
    let job_in = root.join(".cache/palaestra/jobs/deepseek-ocr");
    fs::create_dir_all(&job_in).map_err(|e| e.to_string())?;

    // Clear prior finals so a failed job cannot leave a stale decoded.txt readable.
    let _ = fs::remove_file(job_in.join("decoded.txt"));
    let _ = fs::remove_file(job_in.join("receipt.json"));

    let dest = job_in.join("input.png");
    let dest_tmp = job_in.join("input.png.tmp");
    fs::copy(png, &dest_tmp).map_err(|e| format!("copy input tmp: {e}"))?;
    fs::rename(&dest_tmp, &dest).map_err(|e| format!("rename input: {e}"))?;

    let sidecar = png.with_extension("page.json");
    if !sidecar.is_file() {
        return Err("missing page.json sidecar for GPU OCR staging".into());
    }
    fs::copy(&sidecar, job_in.join("page.json")).map_err(|e| format!("copy page.json: {e}"))?;
    fs::copy(&sidecar, job_in.join("input.page.json"))
        .map_err(|e| format!("copy input.page.json: {e}"))?;
    fs::write(job_in.join("input.nonce"), &nonce).map_err(|e| format!("write nonce: {e}"))?;
    fs::write(job_in.join("input.path"), dest.display().to_string())
        .map_err(|e| format!("write input.path: {e}"))?;

    let reply = task_lan::submit(root, &args)?;
    let _task = assert_task_done(&reply)?;

    let receipt_path = job_in.join("receipt.json");
    let remote_receipt = root.join(".cache/palaestra/remote/deepseek-ocr/receipt.json");
    let receipt_src = if receipt_path.is_file() {
        receipt_path.clone()
    } else if remote_receipt.is_file() {
        remote_receipt
    } else {
        return Err("deepseek-ocr finished without receipt.json".into());
    };
    let receipt_raw = fs::read_to_string(&receipt_src).map_err(|e| e.to_string())?;
    let receipt: Value =
        serde_json::from_str(&receipt_raw).map_err(|e| format!("receipt JSON: {e}"))?;
    let backend = assert_ocr_receipt(&receipt, &nonce)?;

    let decoded = job_in.join("decoded.txt");
    let remote_decoded = root.join(".cache/palaestra/remote/deepseek-ocr/decoded.txt");
    let text_path = if decoded.is_file() {
        decoded
    } else if remote_decoded.is_file() {
        remote_decoded
    } else {
        return Err("deepseek-ocr finished without decoded.txt".into());
    };
    let text = fs::read_to_string(&text_path).map_err(|e| e.to_string())?;
    if text.trim().is_empty() {
        return Err("deepseek-ocr returned empty decoded.txt".into());
    }
    Ok(OcrTaskResult {
        text,
        backend,
        nonce,
    })
}

/// Format optical section appended to knowledge_search replies.
pub fn format_optical_section(bundle: &OpticalHitBundle) -> String {
    let mut out = String::new();
    out.push_str("\n---\n");
    out.push_str(&format!(
        "optical: backend={} ratio={:.2}x fallback={}",
        bundle.backend, bundle.ratio, bundle.fallback
    ));
    if let Some(p) = bundle.precision {
        out.push_str(&format!(" precision={p:.3}"));
    }
    if let Some(rb) = &bundle.receipt_backend {
        out.push_str(&format!(" receipt_backend={rb}"));
    }
    if bundle.clipped {
        out.push_str(" clipped=true");
    }
    out.push('\n');
    if let Some(r) = &bundle.fallback_reason {
        out.push_str(&format!("optical_fallback: {r}\n"));
    }
    for p in &bundle.png_paths {
        out.push_str(&format!("optical_png: {p}\n"));
    }
    out.push_str("optical_decode:\n");
    out.push_str(&bundle.decoded_text);
    if !bundle.decoded_text.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// SemIf / decree helper: whether optical skim is allowed for this state.
pub fn policy_allows_optical(max_ratio: f64, estimated_ratio: f64, is_code_signature: bool) -> bool {
    if is_code_signature {
        return false;
    }
    estimated_ratio <= max_ratio + 1e-6
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("wordkeep-oct-{tag}-{nanos}"));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn roundtrip_glyph_ocr_near_lossless_at_base() {
        let dir = tmp("roundtrip");
        let text = "Optical Compression Tier\nfn choose_mode(tokens: u64) -> VisionMode;\nratio <= 10x gate.";
        let page = render_doc_page(text, &dir.join("p.png"), VisionMode::Base, Some("t")).unwrap();
        assert!(page.ratio <= 10.0, "ratio {}", page.ratio);
        let decoded = glyph_ocr_decode(&page.png_path, page.cols, page.rows).unwrap();
        let p = decode_precision(&page.text, &decoded);
        assert!(p >= PRECISION_GATE_10X, "precision {p} decoded={decoded:?}");
    }

    #[test]
    fn choose_mode_respects_max_ratio() {
        assert_eq!(choose_mode(500, 10.0), VisionMode::Tiny); // 500/64≈7.8
        assert_eq!(choose_mode(2000, 10.0), VisionMode::Base); // 2000/256≈7.8
        assert_eq!(choose_mode(5000, 10.0), VisionMode::Large); // all exceed → Large fallback
    }

    #[test]
    fn policy_blocks_code_signatures() {
        assert!(!policy_allows_optical(10.0, 5.0, true));
        assert!(policy_allows_optical(10.0, 5.0, false));
        assert!(!policy_allows_optical(10.0, 15.0, false));
    }

    #[test]
    fn assert_task_done_rejects_failed_child() {
        let ok = r#"{"status":"done","exit_code":0}"#;
        assert!(assert_task_done(ok).is_ok());
        let fail = r#"{"status":"failed","exit_code":23}"#;
        let err = assert_task_done(fail).unwrap_err();
        assert!(err.contains("not done"), "{err}");
    }

    #[test]
    fn assert_ocr_receipt_nonce_and_backend() {
        let good = json!({
            "job": "deepseek-ocr",
            "backend": "sidecar",
            "nonce": "abc-1",
        });
        assert_eq!(assert_ocr_receipt(&good, "abc-1").unwrap(), "sidecar");
        let mismatch = json!({
            "job": "deepseek-ocr",
            "backend": "deepseek-ocr",
            "nonce": "other",
        });
        assert!(assert_ocr_receipt(&mismatch, "abc-1").unwrap_err().contains("nonce"));
        let empty = json!({
            "job": "deepseek-ocr",
            "backend": "empty",
            "nonce": "abc-1",
        });
        assert!(assert_ocr_receipt(&empty, "abc-1").unwrap_err().contains("backend"));
    }

    #[test]
    fn render_pages_marks_clip_when_capped() {
        let dir = tmp("pages");
        let long = "line\n".repeat(400);
        let (pages, clipped) =
            render_pages(&long, &dir, VisionMode::Tiny, Some("t"), 1, "t").unwrap();
        assert_eq!(pages.len(), 1);
        assert!(clipped);
    }
}
