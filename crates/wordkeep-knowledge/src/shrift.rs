//! Shrift idea-keeper store — Markdown ideas under `.wordkeep/notes/shrift/`.
//!
//! Shared by the MCP server and wiki companion. Ideas are Git-backed Markdown
//! with YAML-ish frontmatter (`created`, `touched`, `status`, `tags`, `source`,
//! `links`, `archive_reason`). A companion ledger lists every idea.

use crate::parse_markdown;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Fresh under this many days since `touched`.
pub const FRESH_DAYS: u64 = 14;
/// Due starts here; stale after [`STALE_DAYS`].
pub const DUE_DAYS: u64 = 14;
/// Stale after this many days since `touched`.
pub const STALE_DAYS: u64 = 30;
/// Dormant after this many days for seed/sprouted ideas (confirm-gated archive).
pub const DORMANT_DAYS: u64 = 90;

pub const DIR_REL: &str = ".wordkeep/notes/shrift";
pub const LEDGER_REL: &str = ".wordkeep/notes/shrift-ledger.md";
pub const BOOKMARKS_REL: &str = ".wordkeep/shrift-bookmarks.json";

const VALID_STATUS: &[&str] = &["seed", "sprouted", "planned", "implemented", "archived"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Idea {
    pub slug: String,
    pub title: String,
    pub status: String,
    pub created: String,
    pub touched: String,
    pub tags: Vec<String>,
    pub source: String,
    pub links: Vec<String>,
    pub archive_reason: String,
    pub body: String,
    pub path: String,
    pub age_days: u64,
    pub freshness: String,
}

#[derive(Clone, Debug, Default)]
pub struct Metrics {
    pub total: usize,
    pub by_status: BTreeMap<String, usize>,
    pub by_freshness: BTreeMap<String, usize>,
    pub tag_histogram: BTreeMap<String, usize>,
    pub bookmarked: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Bookmarks {
    /// slug → unix created secs
    pub entries: BTreeMap<String, u64>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn today_iso() -> String {
    let secs = now_secs();
    // Approximate YYYY-MM-DD from unix days (UTC). Good enough for TTL banding.
    let days = secs / 86_400;
    // 1970-01-01 + days via civil calendar approximation
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Howard Hinnant civil_from_days (proleptic Gregorian).
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

fn parse_iso_day(s: &str) -> Option<i64> {
    let parts: Vec<&str> = s.trim().split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    let y: i32 = parts[0].parse().ok()?;
    let m: u32 = parts[1].parse().ok()?;
    let d: u32 = parts[2].parse().ok()?;
    Some(days_from_civil(y, m, d))
}

fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy as u64;
    (era as i64) * 146_097 + doe as i64 - 719_468
}

pub fn freshness_for(touched: &str, status: &str) -> (u64, String) {
    let today = days_from_civil_today();
    let touched_day = parse_iso_day(touched).unwrap_or(today);
    let age = (today - touched_day).max(0) as u64;
    let band = if age < FRESH_DAYS {
        "fresh"
    } else if age < STALE_DAYS {
        "due"
    } else if age >= DORMANT_DAYS && matches!(status, "seed" | "sprouted") {
        "dormant"
    } else {
        "stale"
    };
    (age, band.to_string())
}

fn days_from_civil_today() -> i64 {
    let secs = now_secs();
    (secs / 86_400) as i64
}

pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars().take(64) {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if matches!(c, ' ' | '-' | '_' | '/') && !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "idea".to_string()
    } else {
        trimmed
    }
}

pub fn validate_status(s: &str) -> Result<(), String> {
    if VALID_STATUS.contains(&s) {
        Ok(())
    } else {
        Err(format!(
            "unknown status {s:?}; use {}",
            VALID_STATUS.join("|")
        ))
    }
}

pub fn next_status(current: &str) -> Option<&'static str> {
    match current {
        "seed" => Some("sprouted"),
        "sprouted" => Some("planned"),
        "planned" => Some("implemented"),
        "implemented" => Some("archived"),
        _ => None,
    }
}

pub fn dir(root: &Path) -> PathBuf {
    root.join(DIR_REL)
}

pub fn ledger_path(root: &Path) -> PathBuf {
    root.join(LEDGER_REL)
}

pub fn idea_path(root: &Path, slug: &str) -> PathBuf {
    dir(root).join(format!("{slug}.md"))
}

pub fn idea_rel(slug: &str) -> String {
    format!("{DIR_REL}/{slug}.md")
}

pub fn ensure_dirs(root: &Path) -> Result<(), String> {
    fs::create_dir_all(dir(root)).map_err(|e| format!("mkdir shrift: {e}"))?;
    let ledger = ledger_path(root);
    if !ledger.exists() {
        let stub = "# Shrift ledger\n\nIndex of ideas under `.wordkeep/notes/shrift/`.\n\
            Agents and the wiki refresh this on capture/touch/status.\n\n\
            ## Ideas\n\n";
        fs::write(&ledger, stub).map_err(|e| format!("write ledger: {e}"))?;
    }
    Ok(())
}

fn parse_list_value(raw: &str) -> Vec<String> {
    let t = raw.trim();
    if t.is_empty() {
        return Vec::new();
    }
    let inner = t
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(t);
    inner
        .split(',')
        .map(|part| {
            part.trim()
                .trim_matches('"')
                .trim_matches('\'')
                .trim()
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

fn idea_from_markdown(rel: &str, src: &str) -> Option<Idea> {
    let parsed = parse_markdown(rel, src);
    let slug = Path::new(rel)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("idea")
        .to_string();
    let status = parsed
        .frontmatter
        .get("status")
        .unwrap_or("seed")
        .to_string();
    let created = parsed.frontmatter.get("created").unwrap_or("").to_string();
    let touched = parsed
        .frontmatter
        .get("touched")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| created.clone());
    let source = parsed
        .frontmatter
        .get("source")
        .unwrap_or("shrift")
        .to_string();
    let archive_reason = parsed
        .frontmatter
        .get("archive_reason")
        .unwrap_or("")
        .to_string();
    let links = parsed
        .frontmatter
        .get("links")
        .map(parse_list_value)
        .unwrap_or_default();
    let tags = if parsed.tags.is_empty() {
        parsed
            .frontmatter
            .get("tags")
            .map(parse_list_value)
            .unwrap_or_default()
    } else {
        parsed.tags.clone()
    };
    let title = parsed
        .sections
        .iter()
        .find(|s| s.heading_level == 1)
        .map(|s| s.heading.clone())
        .unwrap_or_else(|| slug.replace('-', " "));
    let body: String = parsed
        .sections
        .iter()
        .map(|s| {
            if s.heading_level == 0 {
                s.body.clone()
            } else {
                format!(
                    "{} {}\n{}",
                    "#".repeat(s.heading_level as usize),
                    s.heading,
                    s.body
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let (age_days, freshness) = freshness_for(&touched, &status);
    Some(Idea {
        slug,
        title,
        status,
        created,
        touched,
        tags,
        source,
        links,
        archive_reason,
        body: body.trim().to_string(),
        path: rel.to_string(),
        age_days,
        freshness,
    })
}

pub fn load_all(root: &Path) -> Result<Vec<Idea>, String> {
    ensure_dirs(root)?;
    let mut ideas = Vec::new();
    let dir = dir(root);
    let entries = fs::read_dir(&dir).map_err(|e| format!("read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("readdir: {e}"))?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let slug = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("idea")
            .to_string();
        if slug.eq_ignore_ascii_case("README") {
            continue;
        }
        let src = fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let rel = idea_rel(&slug);
        if let Some(idea) = idea_from_markdown(&rel, &src) {
            ideas.push(idea);
        }
    }
    ideas.sort_by(|a, b| a.touched.cmp(&b.touched).then_with(|| a.slug.cmp(&b.slug)));
    Ok(ideas)
}

pub fn load_one(root: &Path, slug: &str) -> Result<Idea, String> {
    let path = idea_path(root, slug);
    let src = fs::read_to_string(&path).map_err(|e| format!("idea {slug} not found: {e}"))?;
    idea_from_markdown(&idea_rel(slug), &src).ok_or_else(|| format!("failed to parse {slug}"))
}

fn render_frontmatter(idea: &Idea) -> String {
    let tags = if idea.tags.is_empty() {
        String::new()
    } else {
        format!(
            "[{}]",
            idea.tags
                .iter()
                .map(|t| format!("\"{t}\""))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let links = if idea.links.is_empty() {
        String::new()
    } else {
        format!(
            "[{}]",
            idea.links
                .iter()
                .map(|t| format!("\"{t}\""))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let mut fm = format!(
        "---\ncreated: {}\ntouched: {}\nstatus: {}\nsource: {}\n",
        idea.created, idea.touched, idea.status, idea.source
    );
    if !tags.is_empty() {
        fm.push_str(&format!("tags: {tags}\n"));
    }
    if !links.is_empty() {
        fm.push_str(&format!("links: {links}\n"));
    }
    if !idea.archive_reason.is_empty() {
        fm.push_str(&format!(
            "archive_reason: \"{}\"\n",
            idea.archive_reason.replace('"', "'")
        ));
    }
    fm.push_str("---\n");
    fm
}

fn write_idea_file(root: &Path, idea: &Idea) -> Result<(), String> {
    ensure_dirs(root)?;
    let body = if idea.body.trim_start().starts_with('#') {
        idea.body.clone()
    } else {
        format!("# {}\n\n{}\n", idea.title, idea.body.trim())
    };
    let content = format!("{}{}", render_frontmatter(idea), body);
    fs::write(idea_path(root, &idea.slug), content)
        .map_err(|e| format!("write idea {}: {e}", idea.slug))
}

fn ledger_line(idea: &Idea) -> String {
    let tags = if idea.tags.is_empty() {
        String::new()
    } else {
        format!(" `{}`", idea.tags.join(","))
    };
    format!(
        "- [{status}] [{slug}]({path}) — {fresh} · touched {touched}{tags}",
        status = idea.status,
        slug = idea.slug,
        path = idea.path,
        fresh = idea.freshness,
        touched = idea.touched,
        tags = tags
    )
}

pub fn rewrite_ledger(root: &Path, ideas: &[Idea]) -> Result<(), String> {
    ensure_dirs(root)?;
    let mut body = String::from(
        "# Shrift ledger\n\nIndex of ideas under `.wordkeep/notes/shrift/`.\n\
         Agents and the wiki refresh this on capture/touch/status.\n\n## Ideas\n\n",
    );
    let mut sorted = ideas.to_vec();
    sorted.sort_by(|a, b| b.touched.cmp(&a.touched).then_with(|| a.slug.cmp(&b.slug)));
    if sorted.is_empty() {
        body.push_str("_No ideas yet. Capture with `/shrift` or `shrift_upsert`._\n");
    } else {
        for idea in &sorted {
            body.push_str(&ledger_line(idea));
            body.push('\n');
        }
    }
    fs::write(ledger_path(root), body).map_err(|e| format!("write ledger: {e}"))
}

pub fn capture(
    root: &Path,
    title: &str,
    body: &str,
    tags: &[String],
    source: &str,
) -> Result<Idea, String> {
    ensure_dirs(root)?;
    let mut slug = slugify(title);
    let mut n = 2u32;
    while idea_path(root, &slug).exists() {
        slug = format!("{}-{n}", slugify(title));
        n += 1;
        if n > 99 {
            return Err("too many colliding slugs".into());
        }
    }
    let today = today_iso();
    let idea = Idea {
        slug: slug.clone(),
        title: title.trim().to_string(),
        status: "seed".into(),
        created: today.clone(),
        touched: today,
        tags: tags.to_vec(),
        source: if source.is_empty() {
            "shrift".into()
        } else {
            source.into()
        },
        links: Vec::new(),
        archive_reason: String::new(),
        body: body.trim().to_string(),
        path: idea_rel(&slug),
        age_days: 0,
        freshness: "fresh".into(),
    };
    write_idea_file(root, &idea)?;
    let mut all = load_all(root)?;
    // Ensure the just-written idea is present (load_all should have it).
    if !all.iter().any(|i| i.slug == idea.slug) {
        all.push(idea.clone());
    }
    rewrite_ledger(root, &all)?;
    Ok(idea)
}

pub fn update(
    root: &Path,
    slug: &str,
    title: Option<&str>,
    body: Option<&str>,
    tags: Option<&[String]>,
    source: Option<&str>,
) -> Result<Idea, String> {
    let mut idea = load_one(root, slug)?;
    if let Some(t) = title {
        idea.title = t.trim().to_string();
    }
    if let Some(b) = body {
        idea.body = b.trim().to_string();
    }
    if let Some(t) = tags {
        idea.tags = t.to_vec();
    }
    if let Some(s) = source {
        idea.source = s.to_string();
    }
    idea.touched = today_iso();
    let (age, fresh) = freshness_for(&idea.touched, &idea.status);
    idea.age_days = age;
    idea.freshness = fresh;
    write_idea_file(root, &idea)?;
    let all = load_all(root)?;
    rewrite_ledger(root, &all)?;
    Ok(idea)
}

pub fn touch(root: &Path, slug: &str) -> Result<Idea, String> {
    let mut idea = load_one(root, slug)?;
    idea.touched = today_iso();
    let (age, fresh) = freshness_for(&idea.touched, &idea.status);
    idea.age_days = age;
    idea.freshness = fresh;
    write_idea_file(root, &idea)?;
    let all = load_all(root)?;
    rewrite_ledger(root, &all)?;
    Ok(idea)
}

pub fn set_status(
    root: &Path,
    slug: &str,
    status: &str,
    archive_reason: Option<&str>,
) -> Result<Idea, String> {
    validate_status(status)?;
    let mut idea = load_one(root, slug)?;
    idea.status = status.to_string();
    idea.touched = today_iso();
    if status == "archived" {
        if let Some(reason) = archive_reason {
            idea.archive_reason = reason.trim().to_string();
        }
    }
    let (age, fresh) = freshness_for(&idea.touched, &idea.status);
    idea.age_days = age;
    idea.freshness = fresh;
    write_idea_file(root, &idea)?;
    let all = load_all(root)?;
    rewrite_ledger(root, &all)?;
    Ok(idea)
}

pub fn set_links(root: &Path, slug: &str, links: &[String]) -> Result<Idea, String> {
    let mut idea = load_one(root, slug)?;
    idea.links = links.to_vec();
    idea.touched = today_iso();
    let (age, fresh) = freshness_for(&idea.touched, &idea.status);
    idea.age_days = age;
    idea.freshness = fresh;
    write_idea_file(root, &idea)?;
    let all = load_all(root)?;
    rewrite_ledger(root, &all)?;
    Ok(idea)
}

pub fn set_tags(root: &Path, slug: &str, tags: &[String]) -> Result<Idea, String> {
    let mut idea = load_one(root, slug)?;
    let mut unique = Vec::new();
    for tag in tags {
        let t = tag.trim();
        if t.is_empty() {
            continue;
        }
        if !unique
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(t))
        {
            unique.push(t.to_string());
        }
    }
    idea.tags = unique;
    idea.touched = today_iso();
    let (age, fresh) = freshness_for(&idea.touched, &idea.status);
    idea.age_days = age;
    idea.freshness = fresh;
    write_idea_file(root, &idea)?;
    let all = load_all(root)?;
    rewrite_ledger(root, &all)?;
    Ok(idea)
}

pub fn metrics(ideas: &[Idea], bookmarks: &Bookmarks) -> Metrics {
    let mut m = Metrics {
        total: ideas.len(),
        bookmarked: 0,
        ..Metrics::default()
    };
    for idea in ideas {
        *m.by_status.entry(idea.status.clone()).or_insert(0) += 1;
        *m.by_freshness.entry(idea.freshness.clone()).or_insert(0) += 1;
        for tag in &idea.tags {
            *m.tag_histogram.entry(tag.clone()).or_insert(0) += 1;
        }
        if bookmarks.entries.contains_key(&idea.slug) {
            m.bookmarked += 1;
        }
    }
    m
}

pub fn stalest(ideas: &[Idea], n: usize) -> Vec<Idea> {
    let mut candidates: Vec<Idea> = ideas
        .iter()
        .filter(|i| matches!(i.status.as_str(), "seed" | "sprouted"))
        .cloned()
        .collect();
    candidates.sort_by(|a, b| {
        b.age_days
            .cmp(&a.age_days)
            .then_with(|| a.touched.cmp(&b.touched))
    });
    candidates.truncate(n);
    candidates
}

pub fn review_queue(ideas: &[Idea]) -> Vec<Idea> {
    let mut q: Vec<Idea> = ideas
        .iter()
        .filter(|i| {
            matches!(i.freshness.as_str(), "due" | "stale" | "dormant")
                && i.status != "archived"
                && i.status != "implemented"
        })
        .cloned()
        .collect();
    q.sort_by(|a, b| {
        freshness_rank(&b.freshness)
            .cmp(&freshness_rank(&a.freshness))
            .then_with(|| b.age_days.cmp(&a.age_days))
    });
    q
}

fn freshness_rank(f: &str) -> u8 {
    match f {
        "dormant" => 0,
        "stale" => 1,
        "due" => 2,
        "fresh" => 3,
        _ => 9,
    }
}

/// Token-overlap Jaccard similarity in [0,1] — used when embeddings are off.
pub fn jaccard_similarity(a: &str, b: &str) -> f64 {
    let ta = tokenize(a);
    let tb = tokenize(b);
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let mut inter = 0usize;
    for t in &ta {
        if tb.contains(t) {
            inter += 1;
        }
    }
    let union = ta.len() + tb.len() - inter;
    if union == 0 {
        0.0
    } else {
        inter as f64 / union as f64
    }
}

fn tokenize(s: &str) -> Vec<String> {
    s.to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| t.len() > 2)
        .map(String::from)
        .collect()
}

pub fn similar_ideas(
    ideas: &[Idea],
    title: &str,
    body: &str,
    threshold: f64,
) -> Vec<(String, f64)> {
    let probe = format!("{title}\n{body}");
    let mut hits: Vec<(String, f64)> = ideas
        .iter()
        .map(|i| {
            let hay = format!("{}\n{}", i.title, i.body);
            (i.slug.clone(), jaccard_similarity(&probe, &hay))
        })
        .filter(|(_, score)| *score >= threshold)
        .collect();
    hits.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    hits.truncate(5);
    hits
}

/// Suggest design-doc paths from a simple keyword scan of `docs/designs`.
pub fn suggest_design_links(root: &Path, title: &str, body: &str, max: usize) -> Vec<String> {
    let designs = root.join("docs/designs");
    let Ok(entries) = walk_md(&designs) else {
        return Vec::new();
    };
    let probe = format!("{title} {body}").to_ascii_lowercase();
    let tokens: Vec<String> = tokenize(&probe);
    let mut scored: Vec<(String, usize)> = Vec::new();
    for path in entries {
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let mut score = 0usize;
        for t in &tokens {
            if name.contains(t) {
                score += 3;
            }
        }
        if let Ok(src) = fs::read_to_string(&path) {
            let lower = src.to_ascii_lowercase();
            for t in tokens.iter().take(12) {
                if lower.contains(t) {
                    score += 1;
                }
            }
        }
        if score > 0 {
            scored.push((rel, score));
        }
    }
    scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    scored.into_iter().take(max).map(|(p, _)| p).collect()
}

fn walk_md(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
        for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out)?;
            } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
                out.push(path);
            }
        }
        Ok(())
    }
    walk(dir, &mut out)?;
    Ok(out)
}

// --- Bookmarks (local user-state; Turso/libSQL migration target) ---

pub fn load_bookmarks(root: &Path) -> Bookmarks {
    let path = root.join(BOOKMARKS_REL);
    let Ok(src) = fs::read_to_string(&path) else {
        return Bookmarks::default();
    };
    parse_bookmarks_json(&src)
}

fn parse_bookmarks_json(src: &str) -> Bookmarks {
    // Minimal JSON object parser for {"bookmarks":{"slug":secs,...}} without serde.
    let mut entries = BTreeMap::new();
    let Some(start) = src.find('{') else {
        return Bookmarks { entries };
    };
    // Find "bookmarks" object
    if let Some(idx) = src[start..].find("\"bookmarks\"") {
        let rest = &src[start + idx..];
        if let Some(brace) = rest.find('{') {
            let inner = &rest[brace + 1..];
            let end = inner.find('}').unwrap_or(inner.len());
            let obj = &inner[..end];
            for part in obj.split(',') {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                let mut kv = part.splitn(2, ':');
                let key = kv.next().unwrap_or("").trim().trim_matches('"').to_string();
                let val = kv
                    .next()
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .unwrap_or(0);
                if !key.is_empty() {
                    entries.insert(key, val);
                }
            }
        }
    }
    Bookmarks { entries }
}

pub fn save_bookmarks(root: &Path, bookmarks: &Bookmarks) -> Result<(), String> {
    let mut body = String::from("{\n  \"version\": 1,\n  \"bookmarks\": {\n");
    let items: Vec<String> = bookmarks
        .entries
        .iter()
        .map(|(k, v)| format!("    \"{k}\": {v}"))
        .collect();
    body.push_str(&items.join(",\n"));
    if !items.is_empty() {
        body.push('\n');
    }
    body.push_str("  }\n}\n");
    if let Some(parent) = root.join(BOOKMARKS_REL).parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir bookmarks: {e}"))?;
    }
    fs::write(root.join(BOOKMARKS_REL), body).map_err(|e| format!("write bookmarks: {e}"))
}

pub fn toggle_bookmark(
    root: &Path,
    slug: &str,
    on: Option<bool>,
) -> Result<(bool, Bookmarks), String> {
    // Ensure idea exists.
    let _ = load_one(root, slug)?;
    let mut bm = load_bookmarks(root);
    let currently = bm.entries.contains_key(slug);
    let want = on.unwrap_or(!currently);
    if want {
        bm.entries.insert(slug.to_string(), now_secs());
    } else {
        bm.entries.remove(slug);
    }
    save_bookmarks(root, &bm)?;
    Ok((want, bm))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Hello World!"), "hello-world");
        assert_eq!(slugify("***"), "idea");
    }

    #[test]
    fn capture_touch_status_roundtrip() {
        let dir = env::temp_dir().join(format!("shrift-test-{}", now_secs()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let idea = capture(&dir, "Test Idea", "body text", &["tag-a".into()], "test").unwrap();
        assert_eq!(idea.status, "seed");
        assert!(idea_path(&dir, &idea.slug).exists());
        assert!(ledger_path(&dir).exists());
        let touched = touch(&dir, &idea.slug).unwrap();
        assert_eq!(touched.touched, today_iso());
        let promoted = set_status(&dir, &idea.slug, "sprouted", None).unwrap();
        assert_eq!(promoted.status, "sprouted");
        let archived = set_status(&dir, &idea.slug, "archived", Some("done elsewhere")).unwrap();
        assert_eq!(archived.archive_reason, "done elsewhere");
        let (on, _) = toggle_bookmark(&dir, &idea.slug, Some(true)).unwrap();
        assert!(on);
        let bm = load_bookmarks(&dir);
        assert!(bm.entries.contains_key(&idea.slug));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn jaccard_detects_overlap() {
        let s = jaccard_similarity("water mesh remesh voxel", "voxel water remesh probe");
        assert!(s > 0.3);
    }
}
