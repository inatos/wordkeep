//! Structured defect registry under `.wordkeep/defects.json`.
//!
//! Defects track open eyeball/gate blockers across sessions. Unresolved defects
//! are injected into knowledge_search as boosted pseudo-chunks so they outrank
//! stale "gate passed" prose.

use serde_json::{json, Value};
use std::path::Path;

use crate::{stats, workspace};

const STORE_VERSION: u64 = 1;
const REL: &str = ".wordkeep/defects.json";

#[derive(Clone, Debug)]
pub struct Defect {
    pub id: String,
    pub status: String,
    pub subsystem: String,
    pub summary: String,
    pub acceptance: Vec<String>,
    pub evidence: Vec<String>,
    pub anchors: Vec<String>,
    pub tags: Vec<String>,
    pub created: u64,
    pub updated: u64,
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

fn validate_status(s: &str) -> Result<(), String> {
    match s {
        "open" | "gated" | "eyeball_fail" | "done" => Ok(()),
        other => Err(format!(
            "unknown status {other:?}; use open, gated, eyeball_fail, or done"
        )),
    }
}

fn status_rank(s: &str) -> u8 {
    match s {
        "eyeball_fail" => 0,
        "open" => 1,
        "gated" => 2,
        "done" => 3,
        _ => 9,
    }
}

fn slugify(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars().take(48) {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if matches!(c, ' ' | '-' | '_') && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

fn defect_to_value(d: &Defect) -> Value {
    json!({
        "id": d.id,
        "status": d.status,
        "subsystem": d.subsystem,
        "summary": d.summary,
        "acceptance": d.acceptance,
        "evidence": d.evidence,
        "anchors": d.anchors,
        "tags": d.tags,
        "created": d.created,
        "updated": d.updated,
    })
}

fn defect_from_value(v: &Value) -> Option<Defect> {
    Some(Defect {
        id: v.get("id").and_then(Value::as_str)?.to_string(),
        status: v
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("open")
            .to_string(),
        subsystem: v
            .get("subsystem")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        summary: v
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        acceptance: string_array(v, "acceptance"),
        evidence: string_array(v, "evidence"),
        anchors: string_array(v, "anchors"),
        tags: string_array(v, "tags"),
        created: v.get("created").and_then(Value::as_u64).unwrap_or(0),
        updated: v.get("updated").and_then(Value::as_u64).unwrap_or(0),
    })
}

fn store_path(root: &Path) -> std::path::PathBuf {
    root.join(REL)
}

fn load_all(root: &Path) -> Result<Vec<Defect>, String> {
    let path = store_path(root);
    let Some(v) = workspace::read_json(&path)? else {
        return Ok(Vec::new());
    };
    let arr = v
        .get("defects")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(arr.iter().filter_map(defect_from_value).collect())
}

fn save_all(root: &Path, defects: &[Defect]) -> Result<(), String> {
    let doc = json!({
        "version": STORE_VERSION,
        "updated": workspace::now_secs(),
        "defects": defects.iter().map(defect_to_value).collect::<Vec<_>>(),
    });
    workspace::write_atomic_json(&store_path(root), &doc)
}

/// Create or update a defect record.
pub fn upsert(root: &Path, args: &Value) -> Result<String, String> {
    let summary = required_str(args, "summary")?;
    let status = optional_str(args, "status").unwrap_or_else(|| "open".into());
    validate_status(&status)?;
    let subsystem = optional_str(args, "subsystem").unwrap_or_default();
    let acceptance = string_array(args, "acceptance");
    let evidence = string_array(args, "evidence");
    let anchors = string_array(args, "anchors");
    let tags = string_array(args, "tags");
    let now = workspace::now_secs();

    let mut defects = load_all(root)?;
    let id = if let Some(id) = optional_str(args, "id") {
        id
    } else {
        let base = if subsystem.is_empty() {
            slugify(&summary)
        } else {
            format!("{}-{}", slugify(&subsystem), slugify(&summary))
        };
        let base = if base.is_empty() {
            "defect".into()
        } else {
            base
        };
        let mut candidate = base.clone();
        let mut n = 2u32;
        while defects.iter().any(|d| d.id == candidate) {
            candidate = format!("{base}-{n}");
            n += 1;
        }
        candidate
    };

    let action = if let Some(existing) = defects.iter_mut().find(|d| d.id == id) {
        existing.status = status.clone();
        if !subsystem.is_empty() {
            existing.subsystem = subsystem.clone();
        }
        existing.summary = summary.clone();
        if !acceptance.is_empty() {
            existing.acceptance = acceptance.clone();
        }
        if !evidence.is_empty() {
            existing.evidence = evidence.clone();
        }
        if !anchors.is_empty() {
            existing.anchors = anchors.clone();
        }
        if !tags.is_empty() {
            existing.tags = tags.clone();
        }
        existing.updated = now;
        "updated"
    } else {
        defects.push(Defect {
            id: id.clone(),
            status: status.clone(),
            subsystem: subsystem.clone(),
            summary: summary.clone(),
            acceptance,
            evidence,
            anchors,
            tags,
            created: now,
            updated: now,
        });
        "created"
    };
    save_all(root, &defects)?;
    let out = format!("defect_upsert - {action} {id} status={status} {summary}");
    stats::record("defect_upsert", 64, (out.len() / 4) as u64);
    Ok(out)
}

/// List defects (default: unresolved), ordered eyeball_fail → open → gated → done.
pub fn list(root: &Path, args: &Value) -> Result<String, String> {
    let include_done = args
        .get("include_done")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let status_filter = optional_str(args, "status");
    let subsystem = optional_str(args, "subsystem");
    let tag = optional_str(args, "tag");
    let query = optional_str(args, "query").unwrap_or_default();

    let mut defects = load_all(root)?;
    defects.retain(|d| {
        if let Some(s) = &status_filter {
            if !d.status.eq_ignore_ascii_case(s) {
                return false;
            }
        } else if !include_done && d.status == "done" {
            return false;
        }
        if let Some(sub) = &subsystem {
            if !d.subsystem.eq_ignore_ascii_case(sub) {
                return false;
            }
        }
        if let Some(t) = &tag {
            if !d.tags.iter().any(|x| x.eq_ignore_ascii_case(t)) {
                return false;
            }
        }
        if !query.is_empty() {
            let q = query.to_ascii_lowercase();
            let hay = format!("{} {} {}", d.id, d.summary, d.subsystem).to_ascii_lowercase();
            if !hay.contains(&q) {
                return false;
            }
        }
        true
    });
    defects.sort_by(|a, b| {
        status_rank(&a.status)
            .cmp(&status_rank(&b.status))
            .then_with(|| b.updated.cmp(&a.updated))
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut out = format!("defect_list - {} defect(s)\n", defects.len());
    for d in &defects {
        out.push_str(&format!(
            "\n[{}] {} — {}\n  subsystem: {}\n  summary: {}\n",
            d.status, d.id, d.updated, d.subsystem, d.summary
        ));
        if !d.acceptance.is_empty() {
            out.push_str("  acceptance:\n");
            for a in &d.acceptance {
                out.push_str(&format!("    - {a}\n"));
            }
        }
        if !d.evidence.is_empty() {
            out.push_str(&format!("  evidence: {}\n", d.evidence.join("; ")));
        }
        if !d.anchors.is_empty() {
            out.push_str(&format!("  anchors: {}\n", d.anchors.join(", ")));
        }
        if !d.tags.is_empty() {
            out.push_str(&format!("  tags: {}\n", d.tags.join(", ")));
        }
    }
    if defects.is_empty() {
        out.push_str("\n(no matching defects)\n");
    }
    // Distill = on-disk store size (raw JSON), not a per-row guess — the list
    // output often exceeds a tiny synthetic baseline and falsely flipped net-negative.
    let store_tokens = std::fs::metadata(store_path(root))
        .map(|m| m.len() / 4)
        .unwrap_or(0) as u64;
    stats::record("defect_list", store_tokens.max(64), (out.len() / 4) as u64);
    Ok(out)
}

/// Unresolved defects for handoff / pressure / knowledge boost.
pub fn unresolved(root: &Path) -> Vec<Defect> {
    load_all(root)
        .unwrap_or_default()
        .into_iter()
        .filter(|d| d.status != "done")
        .collect()
}

/// Boost score multiplier for knowledge injection by status.
pub fn boost_for_status(status: &str) -> f64 {
    match status {
        "eyeball_fail" => 8.0,
        "open" => 4.0,
        "gated" => 2.0,
        _ => 0.0,
    }
}

/// Render a defect as searchable text for knowledge_search injection.
pub fn defect_chunk_text(d: &Defect) -> (String, String, String) {
    let heading = format!("DEFECT [{}] {}", d.status, d.id);
    let mut body = format!(
        "Defect {id} ({status}) in {subsystem}: {summary}\n",
        id = d.id,
        status = d.status,
        subsystem = if d.subsystem.is_empty() {
            "unspecified"
        } else {
            d.subsystem.as_str()
        },
        summary = d.summary
    );
    if !d.acceptance.is_empty() {
        body.push_str("Acceptance:\n");
        for a in &d.acceptance {
            body.push_str(&format!("- {a}\n"));
        }
    }
    if !d.evidence.is_empty() {
        body.push_str(&format!("Evidence: {}\n", d.evidence.join("; ")));
    }
    if !d.anchors.is_empty() {
        body.push_str(&format!("Anchors: {}\n", d.anchors.join(", ")));
    }
    (REL.to_string(), heading, body)
}

/// Path validation helper used by tests.
#[cfg(test)]
#[allow(dead_code)]
pub fn store_rel() -> &'static str {
    REL
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wk_def_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".wordkeep")).unwrap();
        dir
    }

    #[test]
    fn status_ordering_and_upsert() {
        let root = tmp();
        upsert(
            &root,
            &json!({"summary":"cloak axis","status":"open","subsystem":"kkbp"}),
        )
        .unwrap();
        upsert(
            &root,
            &json!({"summary":"gate ok","status":"gated","subsystem":"kkbp"}),
        )
        .unwrap();
        upsert(
            &root,
            &json!({"summary":"eyeball bad","status":"eyeball_fail","subsystem":"kkbp"}),
        )
        .unwrap();
        upsert(
            &root,
            &json!({"summary":"fixed","status":"done","subsystem":"kkbp"}),
        )
        .unwrap();
        let out = list(&root, &json!({})).unwrap();
        let eyeball = out.find("eyeball_fail").unwrap();
        let open = out.find("[open]").unwrap();
        let gated = out.find("[gated]").unwrap();
        assert!(eyeball < open && open < gated);
        assert!(!out.contains("[done]"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn boost_ranks_eyeball_highest() {
        assert!(boost_for_status("eyeball_fail") > boost_for_status("open"));
        assert!(boost_for_status("open") > boost_for_status("gated"));
        assert_eq!(boost_for_status("done"), 0.0);
    }
}
