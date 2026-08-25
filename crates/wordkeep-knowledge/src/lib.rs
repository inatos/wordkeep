//! Shared Markdown parsing and heading-aware chunking for Wordkeep.
//!
//! The parser intentionally implements only the small Markdown surface needed by
//! Wordkeep's indexes. It is deterministic, dependency-free, and aware of fenced
//! code blocks so source snippets such as `#include` never become document
//! headings.

use std::collections::HashMap;
use std::path::PathBuf;

/// Cross-platform parent directory for Wordkeep caches.
///
/// Keep this shared by the MCP and wiki crates so runtime captures resolve to
/// the same workspace on Windows (`LOCALAPPDATA`) and Unix (`XDG_CACHE_HOME`).
pub fn global_cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("LOCALAPPDATA")
                .filter(|path| !path.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|path| !path.is_empty())
                .map(|home| PathBuf::from(home).join(".cache"))
        })
        .unwrap_or_else(std::env::temp_dir)
}

/// Root directory for all Wordkeep cache data.
pub fn wordkeep_cache_dir() -> PathBuf {
    global_cache_dir().join("wordkeep")
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frontmatter {
    pub values: HashMap<String, String>,
}

impl Frontmatter {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub heading: String,
    pub heading_level: u8,
    pub heading_path: Vec<String>,
    pub anchor: String,
    pub ordinal: u32,
    pub start_line: usize,
    pub end_line: usize,
    pub body: String,
    pub links: Vec<WikiLink>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WikiLink {
    pub raw: String,
    pub target: String,
    pub is_external: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedDoc {
    pub path: String,
    pub frontmatter: Frontmatter,
    pub sections: Vec<Section>,
    pub tags: Vec<String>,
}

#[derive(Debug)]
struct SectionBuilder {
    heading: String,
    heading_level: u8,
    heading_path: Vec<String>,
    anchor: String,
    ordinal: u32,
    start_line: usize,
    body: String,
}

/// Parse Markdown into heading-delimited sections.
pub fn parse_markdown(path: &str, src: &str) -> ParsedDoc {
    let lines: Vec<&str> = src.lines().collect();
    let (frontmatter, content_start) = parse_frontmatter(&lines);
    let tags = frontmatter.get("tags").map(parse_tags).unwrap_or_default();

    let mut sections = Vec::new();
    let mut current: Option<SectionBuilder> = None;
    let mut hierarchy: Vec<(u8, String)> = Vec::new();
    let mut anchor_ordinals: HashMap<String, u32> = HashMap::new();
    let mut fence: Option<(u8, usize)> = None;

    for (index, line) in lines.iter().enumerate().skip(content_start) {
        let line_number = index + 1;

        if let Some((marker, width)) = fence {
            append_body(&mut current, line, line_number);
            if is_fence_close(line, marker, width) {
                fence = None;
            }
            continue;
        }

        if let Some(open) = fence_open(line) {
            append_body(&mut current, line, line_number);
            fence = Some(open);
            continue;
        }

        if let Some((level, heading)) = atx_heading(line) {
            if let Some(section) = finish_section(current.take(), line_number.saturating_sub(1)) {
                if section.heading_level == 0 {
                    anchor_ordinals.insert(section.anchor.clone(), 1);
                }
                sections.push(section);
            }

            while hierarchy
                .last()
                .is_some_and(|(parent_level, _)| *parent_level >= level)
            {
                hierarchy.pop();
            }
            hierarchy.push((level, heading.clone()));
            let heading_path = hierarchy.iter().map(|(_, name)| name.clone()).collect();

            let base_anchor = slugify(&heading);
            let ordinal = anchor_ordinals
                .entry(base_anchor.clone())
                .and_modify(|value| *value += 1)
                .or_insert(1);
            let anchor = if *ordinal == 1 {
                base_anchor
            } else {
                format!("{base_anchor}#{ordinal}")
            };

            current = Some(SectionBuilder {
                heading,
                heading_level: level,
                heading_path,
                anchor,
                ordinal: *ordinal,
                start_line: line_number,
                body: String::new(),
            });
        } else {
            append_body(&mut current, line, line_number);
        }
    }

    if let Some(section) = finish_section(current, lines.len()) {
        sections.push(section);
    }

    ParsedDoc {
        path: path.to_string(),
        frontmatter,
        sections,
        tags,
    }
}

/// Convert a heading into a stable, URL-friendly slug.
pub fn slugify(heading: &str) -> String {
    let mut slug = String::new();
    let mut pending_separator = false;

    for character in heading.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() || character == '_' {
            if pending_separator && !slug.is_empty() {
                slug.push('-');
            }
            pending_separator = false;
            slug.push(character);
        } else {
            pending_separator = true;
        }
    }

    if slug.is_empty() {
        "section".to_string()
    } else {
        slug
    }
}

/// Return the body-bearing chunks used by the existing BM25 index.
pub fn chunk_bodies(path: &str, src: &str) -> Vec<(String, String)> {
    parse_markdown(path, src)
        .sections
        .into_iter()
        .filter(|section| !section.body.trim().is_empty())
        .map(|section| (section.heading, section.body))
        .collect()
}

fn parse_frontmatter(lines: &[&str]) -> (Frontmatter, usize) {
    let Some(first) = lines.iter().position(|line| !line.trim().is_empty()) else {
        return (Frontmatter::default(), lines.len());
    };
    if lines[first].trim_start_matches('\u{feff}').trim() != "---" {
        return (Frontmatter::default(), 0);
    }

    let Some(end) = lines
        .iter()
        .enumerate()
        .skip(first + 1)
        .find_map(|(index, line)| (line.trim() == "---").then_some(index))
    else {
        return (parse_frontmatter_values(&lines[first + 1..]), lines.len());
    };

    (parse_frontmatter_values(&lines[first + 1..end]), end + 1)
}

fn parse_frontmatter_values(lines: &[&str]) -> Frontmatter {
    let mut values = HashMap::new();
    let mut list_key: Option<String> = None;

    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if let Some(item) = trimmed.strip_prefix("- ") {
            if let Some(key) = &list_key {
                let item = unquote(item.trim());
                if !item.is_empty() {
                    values
                        .entry(key.clone())
                        .and_modify(|value: &mut String| {
                            if !value.is_empty() {
                                value.push_str(", ");
                            }
                            value.push_str(item);
                        })
                        .or_insert_with(|| item.to_string());
                }
            }
            continue;
        }

        let Some((raw_key, raw_value)) = trimmed.split_once(':') else {
            list_key = None;
            continue;
        };
        let key = raw_key.trim().to_ascii_lowercase();
        if key.is_empty() {
            list_key = None;
            continue;
        }
        let value = unquote(raw_value.trim()).to_string();
        list_key = value.is_empty().then(|| key.clone());
        values.insert(key, value);
    }

    Frontmatter { values }
}

fn parse_tags(value: &str) -> Vec<String> {
    let value = value.trim();
    let value = value
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(value);
    let mut tags = Vec::new();
    for raw in value.split(',') {
        let tag = unquote(raw.trim()).trim();
        if !tag.is_empty() && !tags.iter().any(|existing| existing == tag) {
            tags.push(tag.to_string());
        }
    }
    tags
}

fn unquote(value: &str) -> &str {
    if value.len() >= 2 {
        if let Some(inner) = value
            .strip_prefix('"')
            .and_then(|inner| inner.strip_suffix('"'))
        {
            return inner;
        }
        if let Some(inner) = value
            .strip_prefix('\'')
            .and_then(|inner| inner.strip_suffix('\''))
        {
            return inner;
        }
    }
    value
}

fn append_body(current: &mut Option<SectionBuilder>, line: &str, line_number: usize) {
    let section = current.get_or_insert_with(|| SectionBuilder {
        heading: "(intro)".to_string(),
        heading_level: 0,
        heading_path: Vec::new(),
        anchor: "intro".to_string(),
        ordinal: 1,
        start_line: line_number,
        body: String::new(),
    });
    section.body.push_str(line);
    section.body.push('\n');
}

fn finish_section(builder: Option<SectionBuilder>, end_line: usize) -> Option<Section> {
    let builder = builder?;
    let body = trim_blank_lines(&builder.body);
    if builder.heading_level == 0 && body.is_empty() {
        return None;
    }
    let links = if builder.heading_level == 0 {
        extract_links(&body)
    } else {
        extract_links(&format!("{}\n{body}", builder.heading))
    };
    Some(Section {
        heading: builder.heading,
        heading_level: builder.heading_level,
        heading_path: builder.heading_path,
        anchor: builder.anchor,
        ordinal: builder.ordinal,
        start_line: builder.start_line,
        end_line: end_line.max(builder.start_line),
        body,
        links,
    })
}

fn trim_blank_lines(body: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let first = lines
        .iter()
        .position(|line| !line.trim().is_empty())
        .unwrap_or(lines.len());
    let end = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map(|index| index + 1)
        .unwrap_or(first);
    lines[first..end].join("\n")
}

fn atx_heading(line: &str) -> Option<(u8, String)> {
    let indent = line.bytes().take_while(|byte| *byte == b' ').count();
    if indent > 3 {
        return None;
    }
    let candidate = &line[indent..];
    let level = candidate.bytes().take_while(|byte| *byte == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &candidate[level..];
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }

    let mut heading = rest.trim().to_string();
    let without_hashes = heading.trim_end_matches('#');
    if without_hashes.len() != heading.len()
        && without_hashes
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace)
    {
        heading = without_hashes.trim_end().to_string();
    }
    if heading.is_empty() {
        heading = "(section)".to_string();
    }
    Some((level as u8, heading))
}

fn fence_open(line: &str) -> Option<(u8, usize)> {
    let indent = line.bytes().take_while(|byte| *byte == b' ').count();
    if indent > 3 {
        return None;
    }
    let candidate = &line[indent..];
    let marker = *candidate.as_bytes().first()?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let width = candidate.bytes().take_while(|byte| *byte == marker).count();
    (width >= 3).then_some((marker, width))
}

fn is_fence_close(line: &str, marker: u8, minimum_width: usize) -> bool {
    let indent = line.bytes().take_while(|byte| *byte == b' ').count();
    if indent > 3 {
        return false;
    }
    let candidate = &line[indent..];
    let width = candidate.bytes().take_while(|byte| *byte == marker).count();
    width >= minimum_width && candidate[width..].trim().is_empty()
}

fn extract_links(body: &str) -> Vec<WikiLink> {
    let bytes = body.as_bytes();
    let mut links = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index..].starts_with(b"[[") {
            if let Some(relative_end) = body[index + 2..].find("]]") {
                let end = index + 2 + relative_end + 2;
                let raw = &body[index..end];
                let inner = &body[index + 2..end - 2];
                let target = inner
                    .split_once('|')
                    .map_or(inner, |(target, _)| target)
                    .trim();
                if !target.is_empty() {
                    links.push(WikiLink {
                        raw: raw.to_string(),
                        target: target.to_string(),
                        is_external: is_external_target(target),
                    });
                }
                index = end;
                continue;
            }
        }

        if bytes[index] == b'[' && (index == 0 || bytes[index - 1] != b'!') {
            if let Some(label_end_offset) = body[index + 1..].find(']') {
                let label_end = index + 1 + label_end_offset;
                if bytes.get(label_end + 1) == Some(&b'(') {
                    if let Some(end) = markdown_destination_end(bytes, label_end + 2) {
                        let raw = &body[index..=end];
                        let destination = &body[label_end + 2..end];
                        let target = markdown_target(destination);
                        if !target.is_empty() {
                            links.push(WikiLink {
                                raw: raw.to_string(),
                                target: target.to_string(),
                                is_external: is_external_target(target),
                            });
                        }
                        index = end + 1;
                        continue;
                    }
                }
            }
        }

        index += 1;
    }

    links
}

fn markdown_destination_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut escaped = false;
    for (index, byte) in bytes.iter().enumerate().skip(start) {
        if escaped {
            escaped = false;
            continue;
        }
        match *byte {
            b'\\' => escaped = true,
            b'(' => depth += 1,
            b')' if depth == 0 => return Some(index),
            b')' => depth -= 1,
            _ => {}
        }
    }
    None
}

fn markdown_target(destination: &str) -> &str {
    let destination = destination.trim();
    if let Some(inner) = destination
        .strip_prefix('<')
        .and_then(|inner| inner.split_once('>').map(|(target, _)| target))
    {
        return inner.trim();
    }
    destination
        .split_ascii_whitespace()
        .next()
        .unwrap_or_default()
}

fn is_external_target(target: &str) -> bool {
    if target.starts_with("//") {
        return true;
    }
    let Some((scheme, _)) = target.split_once(':') else {
        return false;
    };
    !scheme.is_empty()
        && scheme.chars().enumerate().all(|(index, character)| {
            character.is_ascii_alphabetic()
                || (index > 0
                    && (character.is_ascii_digit() || matches!(character, '+' | '-' | '.')))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fenced_source_lines_do_not_become_headings() {
        let src = "# Build\n\
                   Before.\n\
                   ```cpp\n\
                   #include <vector>\n\
                   # this is source, not prose\n\
                   ```\n\
                   ~~~sh\n\
                   # shell comment\n\
                   ~~~\n\
                   ## Next\n\
                   After.\n";
        let parsed = parse_markdown("docs/build.md", src);
        let headings: Vec<&str> = parsed
            .sections
            .iter()
            .map(|section| section.heading.as_str())
            .collect();
        assert_eq!(headings, ["Build", "Next"]);
        assert!(parsed.sections[0].body.contains("#include <vector>"));
        assert!(parsed.sections[0].body.contains("# shell comment"));
    }

    #[test]
    fn parses_frontmatter_and_tag_forms_without_leaking_it() {
        let src =
            "\n---\ntitle: \"Rule\"\ntags:\n  - render\n  - \"lighting\"\n---\n# Topic\nBody\n";
        let parsed = parse_markdown(".cursor/rules/topic.mdc", src);
        assert_eq!(parsed.frontmatter.get("title"), Some("Rule"));
        assert_eq!(parsed.tags, ["render", "lighting"]);
        assert_eq!(parsed.sections[0].start_line, 8);
        assert!(!parsed.sections[0].body.contains("title:"));

        let inline = parse_markdown("n.md", "---\ntags: [one, 'two', one]\n---\ntext\n");
        assert_eq!(inline.tags, ["one", "two"]);
    }

    #[test]
    fn builds_heading_paths_and_duplicate_anchors() {
        let src = "# Alpha\none\n## Child\ntwo\n# Alpha\nthree\n### Deep\nfour\n";
        let parsed = parse_markdown("d.md", src);
        assert_eq!(parsed.sections[0].heading_path, ["Alpha"]);
        assert_eq!(parsed.sections[1].heading_path, ["Alpha", "Child"]);
        assert_eq!(parsed.sections[2].anchor, "alpha#2");
        assert_eq!(parsed.sections[2].ordinal, 2);
        assert_eq!(parsed.sections[3].heading_path, ["Alpha", "Deep"]);
        assert_eq!(parsed.sections[0].start_line, 1);
        assert_eq!(parsed.sections[0].end_line, 2);
    }

    #[test]
    fn extracts_markdown_and_wiki_links() {
        let src = "# Links\nSee [local](docs/page.md#part), [web](https://example.com/a(b)), \
                   [[notes/idea|Idea]], and [[https://example.org]].\n";
        let parsed = parse_markdown("links.md", src);
        let links = &parsed.sections[0].links;
        assert_eq!(links.len(), 4);
        assert_eq!(links[0].target, "docs/page.md#part");
        assert!(!links[0].is_external);
        assert_eq!(links[1].target, "https://example.com/a(b)");
        assert!(links[1].is_external);
        assert_eq!(links[2].target, "notes/idea");
        assert_eq!(links[2].raw, "[[notes/idea|Idea]]");
        assert!(links[3].is_external);

        let heading_link = parse_markdown("heading.md", "# [Guide](guide.md)\nBody\n");
        assert_eq!(heading_link.sections[0].links[0].target, "guide.md");
    }

    #[test]
    fn chunk_bodies_matches_bm25_shape_and_skips_empty_sections() {
        let chunks = chunk_bodies("x.md", "# Empty\n## Full\nsearchable text\n");
        assert_eq!(
            chunks,
            vec![("Full".to_string(), "searchable text".to_string())]
        );
    }

    #[test]
    fn slugify_collapses_separators_and_has_empty_fallback() {
        assert_eq!(slugify("Hello,  World!"), "hello-world");
        assert_eq!(slugify("C++ API"), "c-api");
        assert_eq!(slugify("---"), "section");
    }
}
