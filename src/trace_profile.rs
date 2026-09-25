//! trace_profile - Tracy hitch workflow: max-sorted zones + diff_map + index_stale.
//!
//! Composes the perf-investigation loop agents use when a user attaches a `.tracy`
//! capture: surface hitch spikes (not just mean cost), map git blast radius, and
//! check whether wordkeep indexes need a rebuild.

use serde_json::{json, Value};
use std::path::Path;

use crate::{diff_map, index_stale, progress, stats, trace};

pub fn build(root: &Path, args: &Value) -> Result<String, String> {
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(2400) as usize;
    let per_section = budget / 3;

    progress::tick(0, Some(3), "trace_profile: summary");
    let mut trace_args = args.clone();
    if trace_args.get("sort_by").is_none() {
        trace_args["sort_by"] = json!("max");
    }
    if let Some(obj) = trace_args.as_object_mut() {
        obj.insert("token_budget".into(), json!(per_section));
        if !obj.contains_key("top") {
            obj.insert("top".into(), json!(18));
        }
    }

    let mut out = String::from("trace_profile - Tracy hitch workflow (sort_by=max default)\n\n");
    out.push_str("## 1. Zone ranking\n\n");
    match trace::summary(root, &trace_args) {
        Ok(t) => out.push_str(&t),
        Err(e) => out.push_str(&format!("trace_summary error: {e}\n")),
    }

    if args.get("baseline").and_then(Value::as_str).is_some() {
        out.push_str("\n\nTip: baseline diff shows regressions; max sort shows hitches.\n");
    } else {
        out.push_str("\n\nTip: user reports hitches → sort_by=max (default here). ");
        out.push_str("For regressions vs a prior capture, pass \"baseline\": \"tracy_N.tracy\".\n");
    }

    out.push_str("\n\n## 2. Git blast radius (diff_map)\n\n");
    let gitref = args
        .get("git_ref")
        .and_then(Value::as_str)
        .unwrap_or("HEAD");
    let paths = crate::config::paths_from_args(root, args)?;
    let diff_args = json!({
        "ref": gitref,
        "paths": paths,
        "token_budget": per_section,
        "max": args.get("diff_max").unwrap_or(&json!(25)),
    });
    match diff_map::build(root, &diff_args) {
        Ok(d) => out.push_str(&d),
        Err(e) => out.push_str(&format!("diff_map error: {e}\n")),
    }

    out.push_str("\n\n## 3. Index freshness (index_stale)\n\n");
    let stale_args = json!({ "ref": gitref, "token_budget": per_section / 2 });
    match index_stale::check(root, &stale_args) {
        Ok(s) => out.push_str(&s),
        Err(e) => out.push_str(&format!("index_stale error: {e}\n")),
    }

    stats::record(
        "trace_profile",
        per_section as u64 * 3,
        (out.len() / 4) as u64,
    );
    Ok(out)
}
