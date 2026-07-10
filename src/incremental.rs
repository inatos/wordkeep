//! incremental - keep `tree-sitter` trees warm across tool calls.
//!
//! The higher-level navigation tools (`doc_comment`, `symbol_context`,
//! `diff_map`) repeatedly parse the same handful of files within a session. This
//! module memoizes the last-parsed tree per file, keyed by mtime, and feeds the
//! difference between the old and new contents through `Tree::edit` so a touched
//! file re-parses incrementally instead of from scratch.
//!
//! Scope is deliberate: only the symbol-navigation tools route through here, so
//! the warm set stays bounded to the files a lookup actually visits rather than
//! the whole repo. The mtime the caller already `stat`-ed is the cache key, so a
//! genuinely unchanged file skips IO and parsing entirely.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use tree_sitter::{InputEdit, Parser, Point, Tree};

use crate::lang::Lang;

/// One warm entry: the mtime it was parsed at, the *preprocessed* source that
/// was actually fed to `tree-sitter`, and the resulting tree.
struct Warm {
    mtime: u64,
    src: String,
    tree: Tree,
}

/// Process-global warm store. `Tree` is `Send`, so a plain `Mutex` is enough;
/// callers are single-threaded per parse but may interleave across tools.
static TREES: OnceLock<Mutex<HashMap<String, Warm>>> = OnceLock::new();

fn store() -> &'static Mutex<HashMap<String, Warm>> {
    TREES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Parse `path` reusing a warm tree when possible.
///
/// Returns the preprocessed source that was parsed plus its syntax tree. On an
/// mtime match the cached tree is cloned back with no IO or parsing; on a
/// changed file the previous tree seeds an incremental re-parse; on a cold file
/// a full parse is done. Returns `None` only if the language has no grammar or
/// the file cannot be read/parsed.
pub fn parse(parser: &mut Parser, lang: Lang, path: &Path, mtime: u64) -> Option<(String, Tree)> {
    let key = path.to_string_lossy().into_owned();

    // Warm hit: skip IO and parsing entirely.
    {
        let map = store().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(w) = map.get(&key) {
            if w.mtime == mtime {
                return Some((w.src.clone(), w.tree.clone()));
            }
        }
    }

    let ts = lang.ts_language()?;
    if parser.set_language(&ts).is_err() {
        return None;
    }
    let raw = std::fs::read_to_string(path).ok()?;
    let prepared = lang.preprocess(&raw).into_owned();

    // Grab a previous tree (if any) to seed an incremental re-parse.
    let seed = {
        let map = store().lock().unwrap_or_else(|e| e.into_inner());
        map.get(&key).map(|w| (w.src.clone(), w.tree.clone()))
    };

    let tree = match seed {
        // Content identical but mtime moved (e.g. `touch`): reuse as-is.
        Some((old_src, old_tree)) if old_src == prepared => old_tree,
        Some((old_src, mut old_tree)) => match diff_edit(&old_src, &prepared) {
            Some(edit) => {
                old_tree.edit(&edit);
                parser.parse(&prepared, Some(&old_tree))?
            }
            None => parser.parse(&prepared, None)?,
        },
        None => parser.parse(&prepared, None)?,
    };

    {
        let mut map = store().lock().unwrap_or_else(|e| e.into_inner());
        map.insert(
            key,
            Warm {
                mtime,
                src: prepared.clone(),
                tree: tree.clone(),
            },
        );
    }
    Some((prepared, tree))
}

/// The single `InputEdit` spanning everything that changed between `old` and
/// `new`: skip the common prefix and the common (non-overlapping) suffix, and
/// treat the middle as one replaced region. `tree-sitter` re-parses only the
/// affected subtree from this. Returns `None` when the two are identical.
fn diff_edit(old: &str, new: &str) -> Option<InputEdit> {
    let ob = old.as_bytes();
    let nb = new.as_bytes();

    let max_p = ob.len().min(nb.len());
    let mut p = 0;
    while p < max_p && ob[p] == nb[p] {
        p += 1;
    }

    // Common suffix length, not allowed to overlap the prefix on either side.
    let max_s = (ob.len() - p).min(nb.len() - p);
    let mut s = 0;
    while s < max_s && ob[ob.len() - 1 - s] == nb[nb.len() - 1 - s] {
        s += 1;
    }

    let start_byte = p;
    let old_end_byte = ob.len() - s;
    let new_end_byte = nb.len() - s;
    if start_byte == old_end_byte && start_byte == new_end_byte {
        return None;
    }

    Some(InputEdit {
        start_byte,
        old_end_byte,
        new_end_byte,
        start_position: point_at(ob, start_byte),
        old_end_position: point_at(ob, old_end_byte),
        new_end_position: point_at(nb, new_end_byte),
    })
}

/// Row/column (both 0-based, column in bytes) of a byte offset in `bytes`.
fn point_at(bytes: &[u8], off: usize) -> Point {
    let mut row = 0;
    let mut col = 0;
    for &b in &bytes[..off.min(bytes.len())] {
        if b == b'\n' {
            row += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    Point::new(row, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_edit_is_none_for_identical() {
        assert!(diff_edit("abc", "abc").is_none());
    }

    #[test]
    fn diff_edit_spans_the_changed_middle() {
        // "let x = 1;" -> "let x = 42;" : only the value region differs.
        let e = diff_edit("let x = 1;", "let x = 42;").expect("edit");
        assert_eq!(e.start_byte, 8); // after "let x = "
        assert_eq!(e.old_end_byte, 9); // old "1"
        assert_eq!(e.new_end_byte, 10); // new "42"
    }

    #[test]
    fn point_at_tracks_rows_and_columns() {
        let p = point_at(b"ab\ncd", 4);
        assert_eq!(p.row, 1);
        assert_eq!(p.column, 1);
    }

    #[test]
    fn incremental_reparse_matches_a_fresh_parse() {
        let dir = std::env::temp_dir().join(format!("wk_inc_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sample.rs");

        std::fs::write(&file, "fn a() {}\nfn b() {}\n").unwrap();
        let mut parser = Parser::new();
        let (_s1, t1) = parse(&mut parser, Lang::Rust, &file, 1).expect("cold parse");
        let cold_sexp = t1.root_node().to_sexp();

        // Change the body of `b`; re-parse incrementally under a new mtime.
        std::fs::write(&file, "fn a() {}\nfn b() { c(); }\n").unwrap();
        let (s2, t2) = parse(&mut parser, Lang::Rust, &file, 2).expect("warm parse");
        let inc_sexp = t2.root_node().to_sexp();

        // A fresh, unseeded parse of the same bytes must produce the same tree.
        let mut fresh = Parser::new();
        fresh
            .set_language(&Lang::Rust.ts_language().unwrap())
            .unwrap();
        let fresh_tree = fresh.parse(&s2, None).unwrap();
        assert_eq!(inc_sexp, fresh_tree.root_node().to_sexp());
        assert_ne!(cold_sexp, inc_sexp, "tree should reflect the edit");

        // mtime match returns the warm tree without touching disk.
        std::fs::remove_file(&file).unwrap();
        let (_s3, t3) = parse(&mut parser, Lang::Rust, &file, 2).expect("warm hit");
        assert_eq!(t3.root_node().to_sexp(), inc_sexp);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
