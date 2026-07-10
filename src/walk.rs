//! Directory pruning for tree walks.
//!
//! Keeps `repo_map`, `symbol_refs`, `call_graph`, and `knowledge_search` from
//! wandering into generated output or third-party code. The matcher is
//! deliberately lightweight: a builtin set of well-known directories plus the
//! *simple directory-name* rules found in the workspace root `.gitignore`
//! (e.g. `build/`, `target`, `/node_modules`). Glob rules and nested-path rules
//! are out of scope - this is a fast prefilter, not a full gitignore engine.

use std::collections::HashSet;
use std::path::Path;
use walkdir::{DirEntry, WalkDir};

const BUILTIN_PRUNE: [&str; 10] = [
    ".git",
    "build",
    "vendor",
    "target",
    "node_modules",
    ".cache",
    "bin",
    "obj",
    "publish",
    ".wineprefix",
];

/// Directory basenames to skip while walking `root`.
pub fn prune_set(root: &Path) -> HashSet<String> {
    let mut set: HashSet<String> = BUILTIN_PRUNE.iter().map(|s| s.to_string()).collect();
    if let Ok(text) = std::fs::read_to_string(root.join(".gitignore")) {
        for line in text.lines() {
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') || l.starts_with('!') {
                continue;
            }
            // Only simple directory names: strip a leading/trailing '/', then
            // reject anything with an interior path separator or glob metachar.
            let name = l.trim_start_matches('/').trim_end_matches('/');
            if name.is_empty()
                || name.contains('/')
                || name.contains('*')
                || name.contains('?')
                || name.contains('[')
            {
                continue;
            }
            set.insert(name.to_string());
        }
    }
    set
}

/// True when `entry` is a descendant directory whose basename is pruned. The
/// walk base itself (depth 0) is never pruned, so passing e.g. `build/`
/// explicitly as a root still works.
pub fn is_pruned(entry: &DirEntry, prune: &HashSet<String>) -> bool {
    entry.depth() > 0
        && entry.file_type().is_dir()
        && entry
            .file_name()
            .to_str()
            .map_or(false, |n| prune.contains(n))
}

/// Walk `base`, yielding files only and skipping pruned directory subtrees.
pub fn files<'a>(base: &Path, prune: &'a HashSet<String>) -> impl Iterator<Item = DirEntry> + 'a {
    WalkDir::new(base)
        .into_iter()
        .filter_entry(move |e| !is_pruned(e, prune))
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_set_has_builtins_and_parses_simple_gitignore_dirs() {
        let dir = std::env::temp_dir().join(format!("cbtest_walk_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(
            dir.join(".gitignore"),
            "# comment\nout/\nlogs\n/cache\n*.tmp\nsrc/generated/\n!keep\n",
        )
        .unwrap();

        let set = prune_set(&dir);
        // builtins
        assert!(set.contains("build"));
        assert!(set.contains("vendor"));
        assert!(set.contains("target"));
        // simple dir rules from .gitignore
        assert!(set.contains("out"));
        assert!(set.contains("logs"));
        assert!(set.contains("cache"));
        // glob and nested-path rules are ignored
        assert!(!set.contains("*.tmp"));
        assert!(!set.contains("src/generated"));
        assert!(!set.contains("keep"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn files_skips_pruned_subtrees() {
        let root = std::env::temp_dir().join(format!("cbtest_walkfiles_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("build")).unwrap();
        std::fs::write(root.join("src/a.cpp"), "int a;").unwrap();
        std::fs::write(root.join("build/gen.cpp"), "int g;").unwrap();

        let prune = prune_set(&root);
        let names: Vec<String> = files(&root, &prune)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"a.cpp".to_string()));
        assert!(!names.contains(&"gen.cpp".to_string()));

        let _ = std::fs::remove_dir_all(&root);
    }
}
