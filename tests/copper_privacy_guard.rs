//! Forward-only privacy guard: no NEW "copper" hits under tools/wordkeep.
//!
//! Existing Copper-shaped seams (`copper_health.rs`, `copper_status`, task wire)
//! are grandfathered. Telemetry providers must stay fabric-agnostic.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn grandfather_paths(root: &Path) -> BTreeSet<String> {
    let path = root.join("tests/fixtures/copper_grandfather.txt");
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    raw.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.replace('\\', "/"))
        .collect()
}

#[test]
fn no_new_copper_hits_outside_grandfather() {
    let root = repo_root();
    let allowed = grandfather_paths(&root);
    assert!(
        !allowed.is_empty(),
        "missing tests/fixtures/copper_grandfather.txt"
    );

    // Prefer ripgrep; fall back to walking if unavailable.
    let output = Command::new("rg")
        .args([
            "-il",
            "copper",
            "--glob",
            "!**/target/**",
            "--glob",
            "!**/node_modules/**",
            "--glob",
            "!**/dist/**",
            ".",
        ])
        .current_dir(&root)
        .output();

    let hits: Vec<String> = match output {
        Ok(out) if out.status.success() || out.status.code() == Some(1) => {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(|l| {
                    let rel = l.trim().trim_start_matches("./");
                    format!("tools/wordkeep/{}", rel.replace('\\', "/"))
                })
                .filter(|l| !l.ends_with('/'))
                .collect()
        }
        _ => walk_copper_hits(&root),
    };

    let mut unexpected = Vec::new();
    for hit in &hits {
        if !allowed.contains(hit.as_str()) {
            unexpected.push(hit.clone());
        }
    }
    unexpected.sort();
    assert!(
        unexpected.is_empty(),
        "new copper hits outside grandfather list (keep Wordkeep fabric-agnostic):\n  {}\n\
         If intentional legacy, add to tests/fixtures/copper_grandfather.txt — \
         prefer opaque sidecars over new Copper symbols.",
        unexpected.join("\n  ")
    );
}

fn walk_copper_hits(root: &Path) -> Vec<String> {
    let mut hits = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name == "target" || name == "node_modules" || name == "dist" {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            // Skip obvious binaries.
            if bytes.contains(&0) {
                continue;
            }
            let Ok(text) = std::str::from_utf8(&bytes) else {
                continue;
            };
            if text.to_ascii_lowercase().contains("copper") {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                hits.push(format!("tools/wordkeep/{rel}"));
            }
        }
    }
    hits
}
