//! MCP tools for the `/shrift` idea-keeper.

use serde_json::{json, Value};
use std::path::Path;
use wordkeep_knowledge::shrift::{self, Idea};

use crate::{coeffects, stats};

fn optional_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn required_str(args: &Value, key: &str) -> Result<String, String> {
    optional_str(args, key).ok_or_else(|| format!("{key} is required"))
}

fn string_array(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    x.as_str()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn format_idea_line(idea: &Idea) -> String {
    let tags = if idea.tags.is_empty() {
        String::new()
    } else {
        format!(" [{}]", idea.tags.join(", "))
    };
    format!(
        "  [{status}] {slug} — {fresh} ({age}d) · touched {touched}{tags}\n    {title}\n    {path}\n",
        status = idea.status,
        slug = idea.slug,
        fresh = idea.freshness,
        age = idea.age_days,
        touched = idea.touched,
        tags = tags,
        title = idea.title,
        path = idea.path
    )
}

fn filter_ideas(
    ideas: Vec<Idea>,
    status: Option<&str>,
    freshness: Option<&str>,
    tag: Option<&str>,
    query: &str,
) -> Vec<Idea> {
    ideas
        .into_iter()
        .filter(|idea| {
            if let Some(s) = status {
                if !idea.status.eq_ignore_ascii_case(s) {
                    return false;
                }
            }
            if let Some(f) = freshness {
                if !idea.freshness.eq_ignore_ascii_case(f) {
                    return false;
                }
            }
            if let Some(t) = tag {
                if !idea.tags.iter().any(|x| x.eq_ignore_ascii_case(t)) {
                    return false;
                }
            }
            if !query.is_empty() {
                let q = query.to_ascii_lowercase();
                let hay =
                    format!("{} {} {}", idea.slug, idea.title, idea.body).to_ascii_lowercase();
                if !hay.contains(&q) {
                    return false;
                }
            }
            true
        })
        .collect()
}

/// List ideas (digest by default).
pub fn list(root: &Path, args: &Value) -> Result<String, String> {
    let status = optional_str(args, "status");
    let freshness = optional_str(args, "freshness");
    let tag = optional_str(args, "tag");
    let query = optional_str(args, "query").unwrap_or_default();
    let semantic = args
        .get("semantic")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let digest = args.get("digest").and_then(Value::as_bool).unwrap_or(true);
    let max = args
        .get("max")
        .and_then(Value::as_u64)
        .unwrap_or(if digest { 15 } else { u64::MAX })
        .max(1) as usize;
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .map(|b| b as usize);

    let mut ideas = shrift::load_all(root)?;
    let bookmarks = shrift::load_bookmarks(root);

    if semantic && !query.is_empty() {
        ideas.sort_by(|a, b| {
            let sa = shrift::jaccard_similarity(&query, &format!("{}\n{}", a.title, a.body));
            let sb = shrift::jaccard_similarity(&query, &format!("{}\n{}", b.title, b.body));
            sb.partial_cmp(&sa)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.touched.cmp(&a.touched))
        });
    }

    let ideas = filter_ideas(
        ideas,
        status.as_deref(),
        freshness.as_deref(),
        tag.as_deref(),
        if semantic { "" } else { &query },
    );
    let metrics = shrift::metrics(&ideas, &bookmarks);

    let mut out = format!(
        "shrift_list - {} idea(s) (bookmarked={})\n  by status: {:?}\n  by freshness: {:?}\n",
        metrics.total, metrics.bookmarked, metrics.by_status, metrics.by_freshness
    );
    if digest {
        out.push_str("top:\n");
        if ideas.is_empty() {
            out.push_str("  (no matching ideas)\n");
        } else {
            for idea in ideas.iter().take(max) {
                let star = if bookmarks.entries.contains_key(&idea.slug) {
                    "*"
                } else {
                    " "
                };
                out.push_str(&format!(
                    " {star}[{status}] {slug} — {fresh} · {title}\n",
                    status = idea.status,
                    slug = idea.slug,
                    fresh = idea.freshness,
                    title = idea.title
                ));
            }
            if ideas.len() > max {
                out.push_str(&format!("  … ({} more)\n", ideas.len() - max));
            }
        }
        out.push_str(
            "hint: digest:false for full rows; filter status/freshness/tag/query; semantic:true for hybrid rank\n",
        );
    } else {
        for idea in ideas.iter().take(max) {
            out.push_str(&format_idea_line(idea));
        }
    }

    if let Some(budget) = budget {
        let max_chars = budget.saturating_mul(4);
        if out.len() > max_chars {
            out.truncate(max_chars);
            out.push_str("\n… (truncated by token_budget)\n");
        }
    }

    stats::record("shrift_list", 128, (out.len() / 4) as u64);
    Ok(out)
}

/// Show one idea with similar + design-link suggestions.
pub fn show(root: &Path, args: &Value) -> Result<String, String> {
    let slug = required_str(args, "slug")?;
    let idea = shrift::load_one(root, &slug)?;
    let all = shrift::load_all(root)?;
    let similar = shrift::similar_ideas(&all, &idea.title, &idea.body, 0.25)
        .into_iter()
        .filter(|(s, _)| s != &slug)
        .collect::<Vec<_>>();
    let suggestions = shrift::suggest_design_links(root, &idea.title, &idea.body, 5);
    let bookmarks = shrift::load_bookmarks(root);
    let starred = bookmarks.entries.contains_key(&slug);

    let mut out = format!(
        "shrift_show - {slug}\n  status: {status}\n  freshness: {fresh} ({age}d)\n  touched: {touched}\n  created: {created}\n  source: {source}\n  bookmarked: {starred}\n  path: {path}\n  tags: {tags}\n  links: {links}\n  title: {title}\n\n{body}\n",
        slug = idea.slug,
        status = idea.status,
        fresh = idea.freshness,
        age = idea.age_days,
        touched = idea.touched,
        created = idea.created,
        source = idea.source,
        starred = starred,
        path = idea.path,
        tags = idea.tags.join(", "),
        links = idea.links.join(", "),
        title = idea.title,
        body = idea.body
    );
    if !idea.archive_reason.is_empty() {
        out.push_str(&format!("\narchive_reason: {}\n", idea.archive_reason));
    }
    if !similar.is_empty() {
        out.push_str("\nsimilar:\n");
        for (s, score) in similar {
            out.push_str(&format!("  - {s} ({score:.2})\n"));
        }
    }
    if !suggestions.is_empty() {
        out.push_str("\nsuggested design links (confirm before writing):\n");
        for path in suggestions {
            out.push_str(&format!("  - {path}\n"));
        }
    }
    stats::record("shrift_show", 256, (out.len() / 4) as u64);
    Ok(out)
}

/// Capture a new idea (or update when `slug` is given).
pub fn upsert(root: &Path, args: &Value) -> Result<String, String> {
    let title = required_str(args, "title")?;
    let body = optional_str(args, "body").unwrap_or_default();
    let tags = string_array(args, "tags");
    let source = optional_str(args, "source").unwrap_or_else(|| "shrift".into());
    let slug_opt = optional_str(args, "slug");

    let all = shrift::load_all(root)?;
    let similar = shrift::similar_ideas(&all, &title, &body, 0.35);

    let (action, idea) = if let Some(slug) = &slug_opt {
        let idea = shrift::update(
            root,
            slug,
            Some(&title),
            if body.is_empty() { None } else { Some(&body) },
            if tags.is_empty() { None } else { Some(&tags) },
            Some(&source),
        )?;
        ("updated", idea)
    } else {
        (
            "created",
            shrift::capture(root, &title, &body, &tags, &source)?,
        )
    };

    let mut out = format!(
        "shrift_upsert - {action} {}\n  path: {}\n  status: {}\n",
        idea.slug, idea.path, idea.status
    );
    if !similar.is_empty() {
        out.push_str("dedupe warning (not blocked):\n");
        for (s, score) in similar.iter().filter(|(s, _)| *s != idea.slug).take(5) {
            out.push_str(&format!("  - {s} ({score:.2})\n"));
        }
    }
    let suggestions = shrift::suggest_design_links(root, &idea.title, &idea.body, 3);
    if !suggestions.is_empty() {
        out.push_str("suggested links (confirm before writing via shrift_status links):\n");
        for path in suggestions {
            out.push_str(&format!("  - {path}\n"));
        }
    }
    coeffects::notify(
        root,
        "shrift_upsert",
        coeffects::NotifyCtx {
            rel_paths: &[idea.path.as_str(), shrift::LEDGER_REL],
            session: None,
        },
    );
    stats::record("shrift_upsert", 64, (out.len() / 4) as u64);
    Ok(out)
}

pub fn touch(root: &Path, args: &Value) -> Result<String, String> {
    let slug = required_str(args, "slug")?;
    let idea = shrift::touch(root, &slug)?;
    let out = format!(
        "shrift_touch - {slug} touched={} freshness={}\n  path: {}\n",
        idea.touched, idea.freshness, idea.path
    );
    coeffects::notify(
        root,
        "shrift_touch",
        coeffects::NotifyCtx {
            rel_paths: &[idea.path.as_str(), shrift::LEDGER_REL],
            session: None,
        },
    );
    stats::record("shrift_touch", 32, (out.len() / 4) as u64);
    Ok(out)
}

pub fn review(root: &Path, args: &Value) -> Result<String, String> {
    let max = args.get("max").and_then(Value::as_u64).unwrap_or(20) as usize;
    let ideas = shrift::load_all(root)?;
    let bookmarks = shrift::load_bookmarks(root);
    let metrics = shrift::metrics(&ideas, &bookmarks);
    let queue = shrift::review_queue(&ideas);
    let stalest = shrift::stalest(&ideas, 3);

    let mut out = format!(
        "shrift_review - queue={} total={} by_freshness={:?}\n",
        queue.len(),
        metrics.total,
        metrics.by_freshness
    );
    out.push_str("stalest seed/sprouted (handoff surface):\n");
    if stalest.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for idea in &stalest {
            out.push_str(&format!(
                "  - [{}] {} — {} ({}d) {}\n",
                idea.status, idea.slug, idea.freshness, idea.age_days, idea.title
            ));
        }
    }
    out.push_str("review queue:\n");
    if queue.is_empty() {
        out.push_str("  (empty — all fresh or terminal)\n");
    } else {
        for idea in queue.iter().take(max) {
            out.push_str(&format!(
                "  - [{}|{}] {} — {}d · {}\n",
                idea.status, idea.freshness, idea.slug, idea.age_days, idea.title
            ));
        }
        if queue.len() > max {
            out.push_str(&format!("  … ({} more)\n", queue.len() - max));
        }
    }
    out.push_str(
        "hint: shrift_touch / shrift_status to keep|promote|archive; dormant needs archive_reason\n",
    );
    stats::record("shrift_review", 128, (out.len() / 4) as u64);
    Ok(out)
}

pub fn status(root: &Path, args: &Value) -> Result<String, String> {
    let slug = required_str(args, "slug")?;
    let status = required_str(args, "status")?;
    let reason = optional_str(args, "archive_reason");
    let links = string_array(args, "links");

    if status == "archived" {
        let idea = shrift::load_one(root, &slug)?;
        if idea.freshness == "dormant" && reason.as_deref().unwrap_or("").is_empty() {
            return Err("archive_reason is required when archiving a dormant idea".into());
        }
    }

    let mut idea = shrift::set_status(root, &slug, &status, reason.as_deref())?;
    if !links.is_empty() {
        idea = shrift::set_links(root, &slug, &links)?;
    }
    let tags = string_array(args, "tags");
    if args.get("tags").is_some() {
        idea = shrift::set_tags(root, &slug, &tags)?;
    }
    let out = format!(
        "shrift_status - {slug} → {}\n  freshness: {}\n  path: {}\n  links: {}\n  tags: {}\n",
        idea.status,
        idea.freshness,
        idea.path,
        idea.links.join(", "),
        idea.tags.join(", ")
    );
    coeffects::notify(
        root,
        "shrift_status",
        coeffects::NotifyCtx {
            rel_paths: &[idea.path.as_str(), shrift::LEDGER_REL],
            session: None,
        },
    );
    stats::record("shrift_status", 32, (out.len() / 4) as u64);
    Ok(out)
}

pub fn bookmark(root: &Path, args: &Value) -> Result<String, String> {
    let slug = required_str(args, "slug")?;
    let on = args.get("on").and_then(Value::as_bool);
    let (enabled, bm) = shrift::toggle_bookmark(root, &slug, on)?;
    let out = format!(
        "shrift_bookmark - {slug} bookmarked={enabled} (total {})\n",
        bm.entries.len()
    );
    coeffects::notify(root, "shrift_bookmark", coeffects::NotifyCtx::EMPTY);
    stats::record("shrift_bookmark", 16, (out.len() / 4) as u64);
    Ok(out)
}

/// JSON board payload for tests / shared shape with the wiki.
#[allow(dead_code)]
pub fn board_json(root: &Path) -> Result<Value, String> {
    let ideas = shrift::load_all(root)?;
    let bookmarks = shrift::load_bookmarks(root);
    let metrics = shrift::metrics(&ideas, &bookmarks);
    let stalest = shrift::stalest(&ideas, 5);
    Ok(json!({
        "ideas": ideas.iter().map(|i| idea_json(i, bookmarks.entries.contains_key(&i.slug))).collect::<Vec<_>>(),
        "metrics": {
            "total": metrics.total,
            "bookmarked": metrics.bookmarked,
            "by_status": metrics.by_status,
            "by_freshness": metrics.by_freshness,
            "tag_histogram": metrics.tag_histogram,
        },
        "stalest": stalest.iter().map(|i| idea_json(i, bookmarks.entries.contains_key(&i.slug))).collect::<Vec<_>>(),
    }))
}

#[allow(dead_code)]
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
        "body_preview": idea.body.chars().take(240).collect::<String>(),
    })
}
