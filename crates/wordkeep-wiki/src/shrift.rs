//! Shrift Ideas board for the wiki companion.
//!
//! Reads/writes `.wordkeep/notes/shrift/*.md` + ledger + local bookmarks.
//! Mutations share the same Markdown store as MCP `shrift_*` tools.

use serde_json::{json, Value};
use std::path::Path;
use wordkeep_knowledge::shrift::{self, Idea};

fn idea_json(idea: &Idea, bookmarked: bool) -> Value {
    json!({
        "slug": idea.slug,
        "title": idea.title,
        "status": idea.status,
        "created": idea.created,
        "touched": idea.touched,
        "tags": idea.tags,
        "source": idea.source,
        "links": idea.links,
        "archive_reason": idea.archive_reason,
        "path": idea.path,
        "age_days": idea.age_days,
        "freshness": idea.freshness,
        "bookmarked": bookmarked,
        "body_preview": idea.body.chars().take(280).collect::<String>(),
    })
}

/// Full board payload for `GET /api/shrift`.
pub fn board(root: &Path) -> Value {
    match board_inner(root) {
        Ok(v) => v,
        Err(error) => json!({
            "available": false,
            "error": error,
            "ideas": [],
            "metrics": {
                "total": 0,
                "bookmarked": 0,
                "by_status": {},
                "by_freshness": {},
                "tag_histogram": {}
            },
            "stalest": [],
            "dormant": [],
            "review_queue": []
        }),
    }
}

fn board_inner(root: &Path) -> Result<Value, String> {
    let ideas = shrift::load_all(root)?;
    let bookmarks = shrift::load_bookmarks(root);
    let metrics = shrift::metrics(&ideas, &bookmarks);
    let stalest = shrift::stalest(&ideas, 5);
    let dormant: Vec<Idea> = ideas
        .iter()
        .filter(|i| i.freshness == "dormant")
        .cloned()
        .collect();
    let review_queue = shrift::review_queue(&ideas);
    Ok(json!({
        "available": true,
        "ideas": ideas.iter().map(|i| idea_json(i, bookmarks.entries.contains_key(&i.slug))).collect::<Vec<_>>(),
        "metrics": {
            "total": metrics.total,
            "bookmarked": metrics.bookmarked,
            "by_status": metrics.by_status,
            "by_freshness": metrics.by_freshness,
            "tag_histogram": metrics.tag_histogram,
        },
        "stalest": stalest.iter().map(|i| idea_json(i, bookmarks.entries.contains_key(&i.slug))).collect::<Vec<_>>(),
        "dormant": dormant.iter().map(|i| idea_json(i, bookmarks.entries.contains_key(&i.slug))).collect::<Vec<_>>(),
        "review_queue": review_queue.iter().take(30).map(|i| idea_json(i, bookmarks.entries.contains_key(&i.slug))).collect::<Vec<_>>(),
        "ttl": {
            "fresh_days": shrift::FRESH_DAYS,
            "stale_days": shrift::STALE_DAYS,
            "dormant_days": shrift::DORMANT_DAYS
        }
    }))
}

pub fn touch(root: &Path, slug: &str) -> Result<Value, String> {
    let idea = shrift::touch(root, slug)?;
    let bookmarks = shrift::load_bookmarks(root);
    Ok(idea_json(&idea, bookmarks.entries.contains_key(&idea.slug)))
}

pub fn set_status(
    root: &Path,
    slug: &str,
    status: &str,
    archive_reason: Option<&str>,
    links: Option<&[String]>,
) -> Result<Value, String> {
    if status == "archived" {
        let idea = shrift::load_one(root, slug)?;
        if idea.freshness == "dormant" && archive_reason.unwrap_or("").is_empty() {
            return Err("archive_reason is required when archiving a dormant idea".into());
        }
    }
    let mut idea = shrift::set_status(root, slug, status, archive_reason)?;
    if let Some(links) = links {
        if !links.is_empty() {
            idea = shrift::set_links(root, slug, links)?;
        }
    }
    let bookmarks = shrift::load_bookmarks(root);
    Ok(idea_json(&idea, bookmarks.entries.contains_key(&idea.slug)))
}

pub fn promote(root: &Path, slug: &str) -> Result<Value, String> {
    let idea = shrift::load_one(root, slug)?;
    let Some(next) = shrift::next_status(&idea.status) else {
        return Err(format!(
            "cannot promote from status {:?} (already terminal or unknown)",
            idea.status
        ));
    };
    set_status(root, slug, next, None, None)
}

pub fn bookmark(root: &Path, slug: &str, on: Option<bool>) -> Result<Value, String> {
    let (enabled, _) = shrift::toggle_bookmark(root, slug, on)?;
    let idea = shrift::load_one(root, slug)?;
    Ok(json!({
        "slug": slug,
        "bookmarked": enabled,
        "idea": idea_json(&idea, enabled)
    }))
}

pub fn set_tags(root: &Path, slug: &str, tags: &[String]) -> Result<Value, String> {
    let idea = shrift::set_tags(root, slug, tags)?;
    let bookmarks = shrift::load_bookmarks(root);
    Ok(idea_json(&idea, bookmarks.entries.contains_key(&idea.slug)))
}

pub fn search(root: &Path, query: &str, semantic: bool) -> Result<Value, String> {
    let mut ideas = shrift::load_all(root)?;
    let bookmarks = shrift::load_bookmarks(root);
    if semantic && !query.is_empty() {
        ideas.sort_by(|a, b| {
            let sa = shrift::jaccard_similarity(query, &format!("{}\n{}", a.title, a.body));
            let sb = shrift::jaccard_similarity(query, &format!("{}\n{}", b.title, b.body));
            sb.partial_cmp(&sa)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.touched.cmp(&a.touched))
        });
    } else if !query.is_empty() {
        let q = query.to_ascii_lowercase();
        ideas.retain(|i| {
            format!("{} {} {}", i.slug, i.title, i.body)
                .to_ascii_lowercase()
                .contains(&q)
        });
    }
    Ok(json!({
        "query": query,
        "semantic": semantic,
        "hits": ideas.iter().take(40).map(|i| idea_json(i, bookmarks.entries.contains_key(&i.slug))).collect::<Vec<_>>()
    }))
}

pub fn upsert(
    root: &Path,
    title: &str,
    body: &str,
    tags: &[String],
    source: &str,
) -> Result<Value, String> {
    let all = shrift::load_all(root)?;
    let similar = shrift::similar_ideas(&all, title, body, 0.35);
    let idea = shrift::capture(root, title, body, tags, source)?;
    let suggestions = shrift::suggest_design_links(root, title, body, 5);
    Ok(json!({
        "idea": idea_json(&idea, false),
        "similar": similar.into_iter().map(|(slug, score)| json!({"slug": slug, "score": score})).collect::<Vec<_>>(),
        "suggested_links": suggestions
    }))
}
