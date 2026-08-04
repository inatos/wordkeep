//! outline - a token-budgeted table of contents for ONE file.
//!
//! Reuses `repo_map`'s per-language structural extractor (`symbols_located`),
//! then turns each symbol's byte offset into a line number so an agent can read
//! just the relevant range instead of the whole file. Covers every language
//! `repo_map` does: C/C++, GLSL, Rust, Python, C#, TypeScript/TSX/Svelte, and
//! Daslang. Svelte byte offsets line up because its preprocessing preserves byte
//! length (markup blanked, newlines kept).

use serde_json::Value;
use std::path::Path;
use tree_sitter::Parser;

use crate::config;
use crate::lang::Lang;
use crate::{repo_map, stats};

pub fn build(root: &Path, args: &Value) -> Result<String, String> {
    let file = args
        .get("file")
        .or_else(|| args.get("path"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if file.is_empty() {
        return Err("file (or path) is required".into());
    }
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(2400) as usize;

    let path = match config::resolve_file(root, file) {
        Ok(p) => p,
        Err(e) => {
            let out = format!(
                "outline - {file}: not found ({e}); pass a repo-relative path or widen default_paths in .wordkeep/config.json"
            );
            stats::record("outline", 0, (out.len() / 4) as u64);
            return Ok(out);
        }
    };
    let rel = config::rel_path(root, &path);
    let Some(lang) = Lang::from_path(&path) else {
        let out = format!("outline - {rel}: unsupported file type");
        stats::record("outline", 0, (out.len() / 4) as u64);
        return Ok(out);
    };
    let Ok(src) = std::fs::read_to_string(&path) else {
        let out = format!("outline - {rel}: cannot read file");
        stats::record("outline", 0, (out.len() / 4) as u64);
        return Ok(out);
    };

    let mut parser = Parser::new();
    let located = repo_map::symbols_located(&mut parser, lang, &src);

    let baseline = (src.len() / 4) as u64;
    if located.is_empty() {
        stats::record("outline", baseline, 0);
        return Ok(format!("outline - {rel}: no top-level symbols found"));
    }

    // One prefix scan of newline positions maps every byte offset to a line.
    let starts = line_starts(&src);
    let total = located.len();

    let mut out = format!("outline - {rel}  ({total} symbol(s))\n\n");
    let mut used = out.len() / 4;
    for (idx, (off, sym)) in located.iter().enumerate() {
        let line = line_of(&starts, *off);
        let row = format!("  {line:>5}  {sym}\n");
        let lt = row.len() / 4;
        if used + lt > budget && idx > 0 {
            out.push_str(&format!(
                "  … (+{} more; raise token_budget)\n",
                total - idx
            ));
            break;
        }
        used += lt;
        out.push_str(&row);
    }

    stats::record("outline", baseline, (out.len() / 4) as u64);
    Ok(out)
}

/// Byte offset at which each line begins (line 1 starts at 0).
fn line_starts(src: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in src.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// 1-based line number containing byte offset `off`.
fn line_of(starts: &[usize], off: usize) -> usize {
    match starts.binary_search(&off) {
        Ok(i) => i + 1,
        // `i` line starts precede `off`, so `off` sits on line `i`.
        Err(i) => i.max(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_mapping_is_one_based() {
        let src = "a\nbb\nccc\n";
        let starts = line_starts(src);
        assert_eq!(line_of(&starts, 0), 1); // 'a'
        assert_eq!(line_of(&starts, 2), 2); // first 'b'
        assert_eq!(line_of(&starts, 5), 3); // first 'c'
    }

    #[test]
    fn outlines_a_cpp_file_with_lines() {
        let dir = std::env::temp_dir().join(format!("cbtest_outline_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.cpp");
        // Widget on line 2, area() on line 3.
        std::fs::write(&f, "namespace demo {\nstruct Widget { int x; };\nint area(const Widget& w) { return w.x; }\n}\n").unwrap();
        let out = build(&dir, &serde_json::json!({ "file": "a.cpp" })).unwrap();
        assert!(out.contains("struct Widget"), "{out}");
        assert!(out.contains("area("), "{out}");
        assert!(
            out.lines().any(|l| l.contains("Widget") && l.contains('2')),
            "{out}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unsupported_extension_is_graceful() {
        let dir = std::env::temp_dir().join(format!("cbtest_outline2_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
        let out = build(&dir, &serde_json::json!({ "file": "a.txt" })).unwrap();
        assert!(out.contains("unsupported"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_returns_ok_with_hint() {
        let dir = std::env::temp_dir().join(format!("cbtest_outline3_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = build(&dir, &serde_json::json!({ "file": "no_such_file.rs" })).unwrap();
        assert!(out.contains("not found"), "{out}");
        assert!(out.contains("default_paths"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
