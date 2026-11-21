//! Directory pruning for tree walks.
//!
//! Keeps `repo_map`, `symbol_refs`, `call_graph`, and `knowledge_search` from
//! wandering into generated output, virtualenvs, or third-party code. Uses the
//! `ignore` crate so nested `.gitignore` files and glob rules are honored, plus
//! a builtin basename/prefix denylist for paths that often escape gitignore
//! (`.venv`, `.cargo-tmp`, `.worktrees`, `build-*`, …).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

const BUILTIN_PRUNE: &[&str] = &[
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
    "pacman-overlay",
    "__pycache__",
    // Virtualenvs / package installs (telemetry: 80%+ of DiskMap bloat)
    ".venv",
    "venv",
    "site-packages",
    ".cargo-tmp",
    ".worktrees",
    "dist",
    ".next",
    ".svelte-kit",
    ".obsidian",
    ".trash",
    ".mypy_cache",
    ".pytest_cache",
    ".tox",
    ".eggs",
    ".ruff_cache",
    ".turbo",
    "coverage",
    "htmlcov",
];

/// Directory basenames to skip while walking `root`.
///
/// Includes builtins, plus simple directory-name rules from the workspace-root
/// `.gitignore` (kept for callers that still filter with [`is_pruned_name`]).
pub fn prune_set(root: &Path) -> HashSet<String> {
    let mut set: HashSet<String> = BUILTIN_PRUNE.iter().map(|s| (*s).to_string()).collect();
    if let Ok(text) = std::fs::read_to_string(root.join(".gitignore")) {
        for line in text.lines() {
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') || l.starts_with('!') {
                continue;
            }
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

/// True when a directory basename should be pruned (builtins, prune set, or
/// `build-*` / `target-*` prefixes).
pub fn is_pruned_name(name: &str, prune: &HashSet<String>) -> bool {
    if prune.contains(name) {
        return true;
    }
    if name.starts_with("build-") || name.starts_with("target-") {
        return true;
    }
    false
}

fn should_skip_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(".min.js") || n.ends_with(".min.css") || n.ends_with(".map"))
}

/// Walk `base`, yielding files only and skipping pruned directory subtrees.
///
/// Honors nested `.gitignore` via the `ignore` crate. The walk base itself
/// (depth 0) is never pruned, so passing e.g. `build/` explicitly as a root
/// still works.
pub fn files(base: &Path, prune: &HashSet<String>) -> Vec<PathBuf> {
    let prune = prune.clone();
    let mut out = Vec::new();
    let walker = ignore::WalkBuilder::new(base)
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        // Temp-dir unit tests (and non-git overlays) still honor nested
        // `.gitignore` files when require_git is off.
        .require_git(false)
        .parents(true)
        .filter_entry(move |entry| {
            if entry.depth() == 0 {
                return true;
            }
            if entry.file_type().is_some_and(|t| t.is_dir()) {
                if let Some(name) = entry.file_name().to_str() {
                    return !is_pruned_name(name, &prune);
                }
            }
            true
        })
        .build();
    for entry in walker.flatten() {
        let ft = match entry.file_type() {
            Some(t) => t,
            None => continue,
        };
        if !ft.is_file() {
            continue;
        }
        let path = entry.into_path();
        if should_skip_file(&path) {
            continue;
        }
        out.push(path);
    }
    out
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
        assert!(set.contains("build"));
        assert!(set.contains("vendor"));
        assert!(set.contains("target"));
        assert!(set.contains(".cache"));
        assert!(set.contains("node_modules"));
        assert!(set.contains("pacman-overlay"));
        assert!(set.contains("__pycache__"));
        assert!(set.contains(".venv"));
        assert!(set.contains("site-packages"));
        assert!(set.contains(".cargo-tmp"));
        assert!(set.contains(".worktrees"));
        assert!(set.contains("dist"));
        assert!(set.contains("out"));
        assert!(set.contains("logs"));
        assert!(set.contains("cache"));
        // Simple-name parser still skips globs / nested paths (ignore crate
        // handles those at walk time).
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
        std::fs::create_dir_all(root.join(".venv/lib")).unwrap();
        std::fs::create_dir_all(root.join("build-wild-off")).unwrap();
        std::fs::write(root.join("src/a.cpp"), "int a;").unwrap();
        std::fs::write(root.join("build/gen.cpp"), "int g;").unwrap();
        std::fs::write(root.join(".venv/lib/x.py"), "x=1").unwrap();
        std::fs::write(root.join("build-wild-off/y.cpp"), "int y;").unwrap();

        let prune = prune_set(&root);
        let names: Vec<String> = files(&root, &prune)
            .into_iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"a.cpp".to_string()));
        assert!(!names.contains(&"gen.cpp".to_string()));
        assert!(!names.contains(&"x.py".to_string()));
        assert!(!names.contains(&"y.cpp".to_string()));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn files_honors_nested_gitignore() {
        let root = std::env::temp_dir().join(format!("cbtest_walknest_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("pkg/tmp")).unwrap();
        std::fs::write(root.join(".gitignore"), "").unwrap();
        std::fs::write(root.join("pkg/.gitignore"), "tmp/\n").unwrap();
        std::fs::write(root.join("pkg/keep.rs"), "fn k(){}").unwrap();
        std::fs::write(root.join("pkg/tmp/secret.rs"), "fn s(){}").unwrap();

        let prune = prune_set(&root);
        let names: Vec<String> = files(&root, &prune)
            .into_iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"keep.rs".to_string()));
        assert!(!names.contains(&"secret.rs".to_string()));

        let _ = std::fs::remove_dir_all(&root);
    }
}
