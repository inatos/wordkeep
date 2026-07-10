//! include_graph - one hop of `#include` edges for a C/C++ header.
//!
//! Before touching a widely-included header you want the blast radius: what the
//! header pulls in (its includees) and who pulls the header in (its includers).
//! This is a deliberately cheap line scanner - no preprocessor, no tree-sitter -
//! that reads each C/C++ file once, extracts its `#include "..."` / `#include
//! <...>` lines, and matches them against the requested header by basename (and
//! by path suffix when the query contains a `/`). That is enough to scope an ABI
//! or header change without compiling the translation units.

use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;

use crate::lang::Lang;
use crate::{stats, walk};

/// A `#include` directive: the quoted/angled path and whether it was angled.
struct Include {
    path: String,
    angle: bool,
}

pub fn build(root: &Path, args: &Value) -> Result<String, String> {
    let header = args
        .get("header")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if header.is_empty() {
        return Err("header is required".into());
    }
    let paths = crate::config::paths_from_args(root, args)?;
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(1200) as usize;

    // Match by basename always; when the query has a slash, also require the
    // included/target path to end with that normalized suffix.
    let want = normalize(header);
    let want_base = basename(&want).to_string();
    let want_suffix = if want.contains('/') {
        Some(want.clone())
    } else {
        None
    };
    let matches_target = |candidate: &str| -> bool {
        let c = normalize(candidate);
        if basename(&c) != want_base {
            return false;
        }
        match &want_suffix {
            Some(sfx) => c == *sfx || c.ends_with(&format!("/{sfx}")),
            None => true,
        }
    };

    let prune = walk::prune_set(root);
    // includees: union of the target header(s)' own #include lines.
    let mut includees: BTreeSet<String> = BTreeSet::new();
    // includers: files that include the target, with the include directive seen.
    let mut includers: Vec<(String, String)> = Vec::new();
    let mut target_files: BTreeSet<String> = BTreeSet::new();
    let mut scanned_bytes = 0u64;

    for p in &paths {
        let base = root.join(p);
        for entry in walk::files(&base, &prune) {
            let path = entry.path();
            if Lang::from_path(path) != Some(Lang::Cpp) {
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                scanned_bytes += meta.len();
            }
            let Ok(src) = std::fs::read_to_string(path) else {
                continue;
            };
            let rel = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            let incs = scan_includes(&src);

            let is_target = matches_target(&rel);
            if is_target {
                target_files.insert(rel.clone());
                for inc in &incs {
                    let brace = if inc.angle { ('<', '>') } else { ('"', '"') };
                    includees.insert(format!("{}{}{}", brace.0, inc.path, brace.1));
                }
            } else if let Some(inc) = incs.iter().find(|i| matches_target(&i.path)) {
                let brace = if inc.angle { ('<', '>') } else { ('"', '"') };
                includers.push((rel, format!("{}{}{}", brace.0, inc.path, brace.1)));
            }
        }
    }

    includers.sort();
    let out = render(
        header,
        &paths,
        &target_files,
        &includees,
        &includers,
        budget,
    );
    stats::record("include_graph", scanned_bytes / 4, (out.len() / 4) as u64);
    Ok(out)
}

/// Extract `#include` directives from a translation unit, in file order.
fn scan_includes(src: &str) -> Vec<Include> {
    let mut out = Vec::new();
    for line in src.lines() {
        if let Some((path, angle)) = parse_include(line) {
            out.push(Include { path, angle });
        }
    }
    out
}

/// Parse a single `#include "x"` or `#include <x>` line; `None` otherwise.
fn parse_include(line: &str) -> Option<(String, bool)> {
    let t = line.trim_start();
    let rest = t
        .strip_prefix('#')?
        .trim_start()
        .strip_prefix("include")?
        .trim_start();
    let (close, angle) = match rest.chars().next()? {
        '"' => ('"', false),
        '<' => ('>', true),
        _ => return None,
    };
    let after = &rest[1..];
    let end = after.find(close)?;
    Some((after[..end].trim().to_string(), angle))
}

/// Normalize a path to forward slashes and strip a leading `./`.
fn normalize(p: &str) -> String {
    let s = p.replace('\\', "/");
    s.strip_prefix("./").unwrap_or(&s).to_string()
}

/// Last path segment.
fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

fn render(
    header: &str,
    paths: &[String],
    target_files: &BTreeSet<String>,
    includees: &BTreeSet<String>,
    includers: &[(String, String)],
    budget: usize,
) -> String {
    let mut out = format!("include_graph - \"{header}\"  (roots {paths:?})\n");

    out.push_str(&format!(
        "\nresolved header file(s) ({}):\n",
        target_files.len()
    ));
    if target_files.is_empty() {
        out.push_str("  (no matching header found in scope - includers below still apply)\n");
    } else {
        for f in target_files {
            out.push_str(&format!("  {f}\n"));
        }
    }

    out.push_str(&format!(
        "\nincludees - what it includes ({}):\n",
        includees.len()
    ));
    if includees.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for inc in includees {
            out.push_str(&format!("  {inc}\n"));
        }
    }

    out.push_str(&format!(
        "\nincluders - who includes it ({}):\n",
        includers.len()
    ));
    if includers.is_empty() {
        out.push_str("  (none)\n");
    } else {
        let mut used = out.len() / 4;
        for (i, (rel, inc)) in includers.iter().enumerate() {
            let line = format!("  {rel}   {inc}\n");
            used += line.len() / 4;
            if used > budget && i > 0 {
                out.push_str(&format!(
                    "  … (+{} more, truncated by token_budget)\n",
                    includers.len() - i
                ));
                break;
            }
            out.push_str(&line);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_include_handles_quoted_and_angled() {
        assert_eq!(
            parse_include("#include \"a/b.h\""),
            Some(("a/b.h".to_string(), false))
        );
        assert_eq!(
            parse_include("  #  include <vector>"),
            Some(("vector".to_string(), true))
        );
        assert_eq!(parse_include("int x = 1;"), None);
        assert_eq!(parse_include("// #include \"x.h\""), None);
    }

    #[test]
    fn basename_and_normalize() {
        assert_eq!(basename("a/b/c.h"), "c.h");
        assert_eq!(basename("c.h"), "c.h");
        assert_eq!(normalize(".\\a\\b.h"), "a/b.h");
        assert_eq!(normalize("./x.h"), "x.h");
    }

    #[test]
    fn reports_includers_and_includees() {
        let dir = std::env::temp_dir().join(format!("cbtest_incgraph_{}", std::process::id()));
        let src = dir.join("src");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(src.join("core")).unwrap();
        std::fs::write(
            src.join("core/widget.h"),
            "#pragma once\n#include <vector>\n#include \"core/util.h\"\nstruct Widget {};\n",
        )
        .unwrap();
        std::fs::write(
            src.join("a.cpp"),
            "#include \"core/widget.h\"\nvoid use() {}\n",
        )
        .unwrap();
        std::fs::write(src.join("b.cpp"), "#include \"unrelated.h\"\n").unwrap();

        let out = build(
            &dir,
            &serde_json::json!({"header": "widget.h", "paths": ["src"]}),
        )
        .unwrap();
        // includees of widget.h
        assert!(out.contains("<vector>"), "{out}");
        assert!(out.contains("\"core/util.h\""), "{out}");
        // a.cpp includes widget.h; b.cpp does not
        assert!(out.contains("src/a.cpp"), "{out}");
        assert!(!out.contains("src/b.cpp"), "{out}");
        // resolved target
        assert!(out.contains("src/core/widget.h"), "{out}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_suffix_disambiguates_same_basename() {
        let dir = std::env::temp_dir().join(format!("cbtest_incgraph2_{}", std::process::id()));
        let src = dir.join("src");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(src.join("net")).unwrap();
        std::fs::create_dir_all(src.join("ui")).unwrap();
        std::fs::write(src.join("net/log.h"), "#pragma once\n").unwrap();
        std::fs::write(src.join("ui/log.h"), "#pragma once\n").unwrap();
        std::fs::write(src.join("a.cpp"), "#include \"net/log.h\"\n").unwrap();
        std::fs::write(src.join("b.cpp"), "#include \"ui/log.h\"\n").unwrap();

        // Querying the path suffix should select only the net includer.
        let out = build(
            &dir,
            &serde_json::json!({"header": "net/log.h", "paths": ["src"]}),
        )
        .unwrap();
        assert!(out.contains("src/a.cpp"), "{out}");
        assert!(!out.contains("src/b.cpp"), "{out}");
        assert!(out.contains("src/net/log.h"), "{out}");
        assert!(!out.contains("src/ui/log.h"), "{out}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
