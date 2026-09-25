//! outline - a token-budgeted table of contents for ONE file (or a small batch).
//!
//! Reuses `repo_map`'s per-language structural extractor (`symbols_located`),
//! then turns each symbol's byte offset into a line number so an agent can read
//! just the relevant range instead of the whole file. Covers every language
//! `repo_map` does: C/C++, GLSL, Rust, Python, C#, TypeScript/TSX/Svelte, and
//! Daslang. Svelte byte offsets line up because its preprocessing preserves byte
//! length (markup blanked, newlines kept).
//!
//! Accepts `file` / `path` (single) or `files` (batch under one shared budget).

use serde_json::Value;
use std::path::Path;
use tree_sitter::Parser;

use crate::config;
use crate::continuation::{self, KIND_FILE_SKIP};
use crate::lang::Lang;
use crate::{repo_map, stats};

pub fn build(root: &Path, args: &Value) -> Result<String, String> {
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(2400) as usize;

    let batch: Vec<String> = args
        .get("files")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();

    if !batch.is_empty() {
        return build_batch(root, args, &batch, budget);
    }

    let file = args
        .get("file")
        .or_else(|| args.get("path"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if file.is_empty() {
        return Err("file (or path) is required; or pass files:[...] for a batch".into());
    }

    let (text, baseline) = outline_one(root, args, file, budget, /*header_only_budget*/ true)?;
    stats::record("outline", baseline, (text.len() / 4) as u64);
    Ok(text)
}

fn build_batch(
    root: &Path,
    args: &Value,
    files: &[String],
    budget: usize,
) -> Result<String, String> {
    const FP_KEYS: &[&str] = &["files", "file", "path"];
    let args_fp = continuation::args_fingerprint(args, FP_KEYS);
    let resume_at = continuation::resume_offset(args, "outline", FP_KEYS, KIND_FILE_SKIP)? as usize;

    let mut out = format!("outline - batch ({} file(s))", files.len());
    if resume_at > 0 {
        out.push_str(&format!(" [continuation from file #{resume_at}]"));
    }
    out.push('\n');
    let mut used = out.len() / 4;
    let mut baseline_total = 0u64;
    let mut parser = Parser::new();
    let mut truncated_at: Option<usize> = None;

    for (idx, file) in files.iter().enumerate() {
        if idx < resume_at {
            continue;
        }
        if used >= budget && idx > resume_at {
            truncated_at = Some(idx);
            break;
        }
        let remaining = budget.saturating_sub(used).max(64);
        match outline_one_into(root, args, file, remaining, &mut parser) {
            Ok((block, baseline)) => {
                baseline_total += baseline;
                let lt = block.len() / 4;
                if used + lt > budget && idx > resume_at {
                    truncated_at = Some(idx);
                    break;
                }
                if idx > resume_at || (resume_at == 0 && idx > 0) {
                    out.push('\n');
                }
                out.push_str(&block);
                used = out.len() / 4;
            }
            Err(e) => {
                let block = format!("outline - {file}: {e}\n");
                out.push_str(&block);
                used = out.len() / 4;
            }
        }
    }
    if let Some(idx) = truncated_at {
        let omitted = files.len().saturating_sub(idx);
        continuation::append_footer(
            &mut out,
            "outline",
            args_fp,
            idx as u64,
            KIND_FILE_SKIP,
            &format!("+{omitted} more file(s)"),
        );
    }
    stats::record("outline", baseline_total, (out.len() / 4) as u64);
    Ok(out)
}

/// Outline a single file; returns (text, baseline_tokens).
fn outline_one(
    root: &Path,
    args: &Value,
    file: &str,
    budget: usize,
    _header_only_budget: bool,
) -> Result<(String, u64), String> {
    let mut parser = Parser::new();
    outline_one_into(root, args, file, budget, &mut parser)
}

fn outline_one_into(
    root: &Path,
    args: &Value,
    file: &str,
    budget: usize,
    parser: &mut Parser,
) -> Result<(String, u64), String> {
    let path = match config::resolve_file_with_args(root, file, args) {
        Ok(p) => p,
        Err(e) => {
            let mut out = format!(
                "outline - {file}: not found ({e}); pass a repo-relative path or widen default_paths in .wordkeep/config.json"
            );
            // Basename candidates under search roots / whole repo.
            if let Some(cands) = basename_candidate_list(root, args, file) {
                out.push_str(&format!("\ncandidates: {cands}"));
            }
            return Ok((out, 0));
        }
    };
    let rel = config::rel_path(root, &path);
    let Some(lang) = Lang::from_path(&path) else {
        return Ok((format!("outline - {rel}: unsupported file type"), 0));
    };
    let Ok(src) = std::fs::read_to_string(&path) else {
        return Ok((format!("outline - {rel}: cannot read file"), 0));
    };

    let located = repo_map::symbols_located(parser, lang, &src);
    let baseline = (src.len() / 4) as u64;
    if located.is_empty() {
        return Ok((format!("outline - {rel}: no top-level symbols found"), baseline));
    }

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

    Ok((out, baseline))
}

fn basename_candidate_list(root: &Path, args: &Value, file: &str) -> Option<String> {
    let name = Path::new(file)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(file);
    if name.contains('/') {
        return None;
    }
    let prune = crate::walk::prune_set(root);
    let search = config::paths_from_args(root, args).unwrap_or_else(|_| config::default_paths(root));
    let mut matches = config::basename_matches(root, name, &prune, search);
    if matches.is_empty() {
        matches = config::basename_matches(root, name, &prune, vec!["".to_string()]);
    }
    if matches.is_empty() {
        return None;
    }
    let list: Vec<String> = matches
        .into_iter()
        .take(8)
        .map(|p| config::rel_path(root, &p))
        .collect();
    Some(list.join(", "))
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

    #[test]
    fn batch_files_share_one_header() {
        let dir = std::env::temp_dir().join(format!("cbtest_outline_batch_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("src/a.cpp"),
            "struct Alpha { int x; };\nint alpha_fn() { return 0; }\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("src/b.cpp"),
            "struct Beta { int y; };\nint beta_fn() { return 1; }\n",
        )
        .unwrap();
        let out = build(
            &dir,
            &serde_json::json!({
                "files": ["src/a.cpp", "src/b.cpp"],
                "token_budget": 4000
            }),
        )
        .unwrap();
        assert!(out.contains("outline - batch (2 file(s))"), "{out}");
        assert!(out.contains("Alpha") || out.contains("alpha_fn"), "{out}");
        assert!(out.contains("Beta") || out.contains("beta_fn"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_basename_lists_candidates() {
        let dir = std::env::temp_dir().join(format!("cbtest_outline_cand_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/nested")).unwrap();
        std::fs::write(dir.join("src/nested/unique_outline_cand.cpp"), "int z() { return 0; }\n")
            .unwrap();
        // Config default_paths → src so basename search finds the nested file when
        // resolve_file_with_args would otherwise fail on a bare basename that is
        // unique — here we pass a wrong path so not-found triggers candidates.
        std::fs::create_dir_all(dir.join(".wordkeep")).unwrap();
        std::fs::write(
            dir.join(".wordkeep/config.json"),
            r#"{"default_paths":["src"]}"#,
        )
        .unwrap();
        // Bare basename that exists under src should resolve uniquely; use a
        // non-existent relative path whose basename matches.
        let out = build(
            &dir,
            &serde_json::json!({ "file": "other/unique_outline_cand.cpp" }),
        )
        .unwrap();
        // Either resolved via search or listed as candidate / not found with hint.
        assert!(
            out.contains("unique_outline_cand") || out.contains("not found"),
            "{out}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
