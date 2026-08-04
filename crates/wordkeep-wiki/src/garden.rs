use crate::config::WikiConfig;
use crate::indexer::{collect_docs, kind_for_path};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};
use wordkeep_knowledge::parse_markdown;

pub(crate) fn html_anchor_id(anchor: &str) -> String {
    anchor.replace('#', "-")
}

pub(crate) fn analyze(root: &Path, config: &WikiConfig) -> Result<Value, String> {
    let documents = collect_docs(root, config)?;
    let paths: BTreeSet<String> = documents.keys().cloned().collect();

    let mut inbound: BTreeMap<String, u32> = BTreeMap::new();
    for path in &paths {
        inbound.insert(path.clone(), 0);
    }

    let mut broken = Vec::new();
    let mut duplicate_headings = Vec::new();
    let mut tag_counts: BTreeMap<String, u32> = BTreeMap::new();

    for document in documents.values() {
        let Ok(markdown) = std::fs::read_to_string(&document.absolute) else {
            continue;
        };
        let parsed = parse_markdown(&document.relative, &markdown);
        for tag in &parsed.tags {
            *tag_counts.entry(tag.clone()).or_default() += 1;
        }
        for section in &parsed.sections {
            if section.ordinal > 1 {
                duplicate_headings.push(json!({
                    "path": document.relative,
                    "heading": section.heading,
                    "anchor": section.anchor,
                    "html_id": html_anchor_id(&section.anchor),
                    "ordinal": section.ordinal
                }));
            }
            for link in &section.links {
                if link.is_external {
                    continue;
                }
                let Some(target) = resolve_internal(&document.relative, &link.target, &paths)
                else {
                    broken.push(json!({
                        "source": document.relative,
                        "heading": section.heading,
                        "raw": link.raw,
                        "target": link.target
                    }));
                    continue;
                };
                *inbound.entry(target).or_default() += 1;
            }
        }
    }

    let mut orphans = Vec::new();
    for path in &paths {
        let count = inbound.get(path).copied().unwrap_or(0);
        if count == 0 && path != "README.md" {
            orphans.push(json!({
                "path": path,
                "kind": kind_for_path(path)
            }));
        }
    }

    broken.truncate(200);
    orphans.truncate(200);
    duplicate_headings.truncate(200);

    Ok(json!({
        "files": paths.len(),
        "broken_links": broken,
        "broken_count": broken.len(),
        "orphans": orphans,
        "orphan_count": orphans.len(),
        "duplicate_headings": duplicate_headings,
        "duplicate_heading_count": duplicate_headings.len(),
        "tags": tag_counts
    }))
}

fn resolve_internal(source: &str, raw_target: &str, paths: &BTreeSet<String>) -> Option<String> {
    let target = raw_target
        .split(['#', '?'])
        .next()
        .unwrap_or_default()
        .trim();
    if target.is_empty() {
        return Some(source.to_string());
    }
    let direct = target.trim_start_matches('/');
    for variant in path_variants(direct) {
        if paths.contains(&variant) {
            return Some(variant);
        }
    }
    let parent = Path::new(source).parent().unwrap_or_else(|| Path::new(""));
    let relative = normalize_lexical(&parent.join(target))?;
    for variant in path_variants(&relative) {
        if paths.contains(&variant) {
            return Some(variant);
        }
    }
    None
}

fn path_variants(path: &str) -> Vec<String> {
    let normalized = path.trim_start_matches("./").replace('\\', "/");
    let mut variants = vec![normalized.clone()];
    if Path::new(&normalized).extension().is_none() {
        variants.push(format!("{normalized}.md"));
        variants.push(format!("{normalized}.mdc"));
    }
    variants
}

fn normalize_lexical(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => parts.push(value.to_string_lossy().to_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_anchor_ids_flatten_ordinal_suffix() {
        assert_eq!(html_anchor_id("alpha#2"), "alpha-2");
        assert_eq!(html_anchor_id("safe"), "safe");
    }
}
