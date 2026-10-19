//! MCP savings dashboard payload for the wiki GUI.
//!
//! Mirrors the terminal `wordkeep dashboard` overview (tools / activity / health)
//! using on-disk `savings.json`, so the GUI works without the `dashboard` Cargo
//! feature. Also projects config-declared telemetry providers (opaque sidecars).

use crate::indexer::global_cache_dir;
use crate::providers;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Dashboard tool table window. Lifetime `savings.json` totals stay available
/// as `lifetime_*` fields; the table itself is this many days of events.jsonl.
const WINDOW_DAYS: u64 = 30;

const LOW_YIELD_RATE_PCT: u64 = 25;
const LOW_YIELD_MIN_CALLS: u64 = 10;
const NET_NEGATIVE_BASELINE_FLOOR: u64 = 500;

pub(crate) fn build(root: &Path) -> Result<Value, String> {
    let providers = providers::build_providers(root);
    let path = global_cache_dir().join("wordkeep/savings.json");
    if !path.exists() {
        return Ok(json!({
            "available": false,
            "message": "No savings.json yet — run some wordkeep MCP tools first.",
            "terminal_hint": "cargo run -p wordkeep --features dashboard -- dashboard",
            "providers": providers
        }));
    }

    let bytes =
        std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let data: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;

    let now = now_secs();
    let since = data.get("since").and_then(Value::as_u64).unwrap_or(now);
    let tools_obj = data
        .get("tools")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let events = data
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut lifetime_tools: Vec<Value> = tools_obj
        .iter()
        .map(|(name, tool)| tool_row(name, tool))
        .collect();
    sort_by_returned(&mut lifetime_tools);

    let workspace_id = data
        .get("workspace_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let window = load_window(workspace_id, now);
    let (tools, window_label) = if window.is_empty() {
        (lifetime_tools, "lifetime aggregates".to_string())
    } else {
        let mut rows: Vec<Value> = window
            .iter()
            .map(|(name, win)| tool_row(name, &win.to_value()))
            .collect();
        sort_by_returned(&mut rows);
        (rows, format!("last {WINDOW_DAYS}d"))
    };

    let mut calls = 0u64;
    let mut baseline = 0u64;
    let mut returned = 0u64;
    for tool in &tools {
        calls = calls.saturating_add(tool.get("calls").and_then(Value::as_u64).unwrap_or(0));
        baseline = baseline.saturating_add(
            tool.get("baseline_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        );
        returned = returned.saturating_add(
            tool.get("returned_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        );
    }
    let saved = baseline.saturating_sub(returned);
    let pct = pct(saved, baseline);

    let activity: Vec<Value> = events
        .iter()
        .rev()
        .take(40)
        .filter_map(|event| {
            let tool = event.get("tool")?.as_str()?;
            let ts = event.get("ts")?.as_u64().unwrap_or(0);
            let baseline = event.get("baseline").and_then(Value::as_u64).unwrap_or(0);
            let returned = event.get("returned").and_then(Value::as_u64).unwrap_or(0);
            let elapsed_ms = event
                .get("elapsed_ms")
                .and_then(Value::as_u64)
                .or_else(|| {
                    event
                        .get("elapsed_us")
                        .and_then(Value::as_u64)
                        .map(|us| us / 1000)
                })
                .unwrap_or(0);
            let outcome = event.get("outcome").and_then(Value::as_str).unwrap_or("ok");
            let reason = event
                .get("reason")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            Some(json!({
                "ts": ts,
                "ago": ago(now, ts),
                "tool": tool,
                "elapsed_ms": elapsed_ms,
                "baseline": baseline,
                "returned": returned,
                "saved": baseline.saturating_sub(returned),
                "outcome": outcome,
                "reason": reason,
                "baseline_fmt": commafy(baseline),
                "returned_fmt": commafy(returned),
                "saved_fmt": commafy(baseline.saturating_sub(returned))
            }))
        })
        .collect();

    let optical = data.get("optical").cloned().unwrap_or_else(|| {
        json!({
            "calls": 0,
            "mas_calls": 0,
            "fallback_calls": 0,
            "avg_ratio": 0.0,
            "avg_precision": 0.0,
            "precision_samples": 0,
            "enabled_hint": "Opt-in: knowledge_search optical:true / session_handoff optical:true (default off until 10× gate)."
        })
    });

    Ok(json!({
        "available": true,
        "path": path.display().to_string(),
        "version": data.get("version").cloned().unwrap_or(Value::Null),
        "overview": {
            "calls": calls,
            "calls_fmt": commafy(calls),
            "label": "returned tokens",
            "baseline_label": "scanned_counterfactual",
            "window_days": if window.is_empty() { 0 } else { WINDOW_DAYS },
            "window_label": window_label,
            "baseline_tokens": baseline,
            "baseline_fmt": commafy(baseline),
            "returned_tokens": returned,
            "returned_fmt": commafy(returned),
            "saved_tokens": saved,
            "saved_fmt": commafy(saved),
            "reduction_pct": pct,
            "since": since,
            "since_label": elapsed(since, now),
            "now": now
        },
        "optical": optical,
        "tools": tools,
        "activity": activity,
        "health": health_signals(&tools),
        "providers": providers,
        "terminal_hint": "cargo run -p wordkeep --features dashboard -- dashboard"
    }))
}

fn sort_by_returned(tools: &mut [Value]) {
    tools.sort_by(|a, b| {
        let ra = a.get("returned_tokens").and_then(Value::as_u64).unwrap_or(0);
        let rb = b.get("returned_tokens").and_then(Value::as_u64).unwrap_or(0);
        rb.cmp(&ra).then_with(|| {
            let na = a.get("name").and_then(Value::as_str).unwrap_or("");
            let nb = b.get("name").and_then(Value::as_str).unwrap_or("");
            na.cmp(nb)
        })
    });
}

#[derive(Default)]
struct Win {
    calls: u64,
    baseline: u64,
    returned: u64,
    trunc: u64,
    errors: u64,
    invalid: u64,
    not_found: u64,
    low_yield: u64,
    total_us: u64,
    peak_us: u64,
    peak_returned: u64,
    peak_baseline: u64,
    peak_saved: u64,
    last_ts: u64,
}

impl Win {
    fn observe(&mut self, event: &Value) {
        let baseline = event.get("baseline").and_then(Value::as_u64).unwrap_or(0);
        let returned = event.get("returned").and_then(Value::as_u64).unwrap_or(0);
        let elapsed_us = event.get("elapsed_us").and_then(Value::as_u64).unwrap_or(0);
        let ts = event.get("ts").and_then(Value::as_u64).unwrap_or(0);
        self.calls += 1;
        self.baseline += baseline;
        self.returned += returned;
        self.total_us += elapsed_us;
        self.peak_us = self.peak_us.max(elapsed_us);
        self.peak_returned = self.peak_returned.max(returned);
        self.peak_baseline = self.peak_baseline.max(baseline);
        self.peak_saved = self.peak_saved.max(baseline.saturating_sub(returned));
        self.last_ts = self.last_ts.max(ts);
        match event.get("outcome").and_then(Value::as_str).unwrap_or("ok") {
            "truncated" => self.trunc += 1,
            "error" => self.errors += 1,
            "invalid" => self.invalid += 1,
            "not_found" => self.not_found += 1,
            "low_yield" => self.low_yield += 1,
            _ => {}
        }
    }

    fn to_value(&self) -> Value {
        json!({
            "calls": self.calls,
            "baseline_tokens": self.baseline,
            "returned_tokens": self.returned,
            "trunc_count": self.trunc,
            "error_count": self.errors,
            "invalid_count": self.invalid,
            "not_found_count": self.not_found,
            "low_yield_count": self.low_yield,
            "total_us": self.total_us,
            "peak_us": self.peak_us,
            "peak_saved": self.peak_saved,
            "peak_baseline": self.peak_baseline,
            "peak_returned": self.peak_returned,
            "last_ts": self.last_ts,
        })
    }
}

fn load_window(workspace_id: &str, now: u64) -> BTreeMap<String, Win> {
    let mut out = BTreeMap::new();
    if workspace_id.is_empty() {
        return out;
    }
    let path = global_cache_dir().join(format!("wordkeep/workspaces/{workspace_id}/events.jsonl"));
    let Ok(file) = std::fs::File::open(path) else {
        return out;
    };
    let cutoff = now.saturating_sub(WINDOW_DAYS * 86_400);
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { continue };
        let Ok(event) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let ts = event.get("ts").and_then(Value::as_u64).unwrap_or(0);
        if ts <= cutoff {
            continue;
        }
        let Some(tool) = event.get("tool").and_then(Value::as_str) else {
            continue;
        };
        out.entry(tool.to_string()).or_default().observe(&event);
    }
    out
}

fn tool_row(name: &str, tool: &Value) -> Value {
    let calls = tool.get("calls").and_then(Value::as_u64).unwrap_or(0);
    let baseline = tool
        .get("baseline_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let returned = tool
        .get("returned_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let saved = baseline.saturating_sub(returned);
    let total_ms = tool
        .get("total_us")
        .and_then(Value::as_u64)
        .map(|us| us / 1000)
        .or_else(|| tool.get("total_ms").and_then(Value::as_u64))
        .unwrap_or(0);
    let avg_ms = if calls == 0 { 0 } else { total_ms / calls };
    let trunc = tool.get("trunc_count").and_then(Value::as_u64).unwrap_or(0);
    let errors = tool.get("error_count").and_then(Value::as_u64).unwrap_or(0);
    let invalid = tool
        .get("invalid_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let not_found = tool
        .get("not_found_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let low_yield = tool
        .get("low_yield_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let peak_saved = tool.get("peak_saved").and_then(Value::as_u64).unwrap_or(0);
    let peak_baseline = tool
        .get("peak_baseline")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let peak_returned = tool
        .get("peak_returned")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let peak_ms = tool.get("peak_ms").and_then(Value::as_u64).unwrap_or(0);
    let last_ts = tool.get("last_ts").and_then(Value::as_u64).unwrap_or(0);
    let reduction = pct(saved, baseline);
    let inverted = baseline > 0 && returned >= baseline;

    json!({
        "name": name,
        "calls": calls,
        "calls_fmt": commafy(calls),
        "avg_ms": avg_ms,
        "trunc_count": trunc,
        "error_count": errors,
        "invalid_count": invalid,
        "not_found_count": not_found,
        "low_yield_count": low_yield,
        "baseline_tokens": baseline,
        "baseline_fmt": commafy(baseline),
        "returned_tokens": returned,
        "returned_fmt": commafy(returned),
        "saved": saved,
        "saved_fmt": commafy(saved),
        "reduction_pct": reduction,
        "bar": bar(reduction, 8),
        "inverted": inverted,
        "peak_saved": peak_saved,
        "peak_saved_fmt": commafy(peak_saved),
        "peak_baseline": peak_baseline,
        "peak_returned": peak_returned,
        "peak_ms": peak_ms,
        "last_ts": last_ts,
        "last_ago": if last_ts == 0 { "—".to_string() } else { ago(now_secs(), last_ts) }
    })
}

fn health_signals(tools: &[Value]) -> Value {
    let mut lines = Vec::new();

    if let Some(tool) = tools.iter().max_by_key(|tool| {
        tool.get("returned_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    }) {
        lines.push(json!({
            "kind": "info",
            "label": "most returned",
            "value": tool.get("returned_fmt").cloned().unwrap_or(Value::Null),
            "detail": tool.get("name").cloned().unwrap_or(Value::Null)
        }));
    }

    let peak = tools
        .iter()
        .max_by_key(|tool| tool.get("peak_saved").and_then(Value::as_u64).unwrap_or(0));
    if let Some(tool) = peak {
        lines.push(json!({
            "kind": "ok",
            "label": "peak counterfactual",
            "value": tool.get("peak_saved_fmt").cloned().unwrap_or(Value::Null),
            "detail": tool.get("name").cloned().unwrap_or(Value::Null)
        }));
    }

    if let Some(slow) = tools
        .iter()
        .filter(|tool| tool.get("calls").and_then(Value::as_u64).unwrap_or(0) > 0)
        .max_by_key(|tool| tool.get("avg_ms").and_then(Value::as_u64).unwrap_or(0))
    {
        lines.push(json!({
            "kind": "info",
            "label": "slowest (avg)",
            "value": format!("{}ms", slow.get("avg_ms").and_then(Value::as_u64).unwrap_or(0)),
            "detail": slow.get("name").cloned().unwrap_or(Value::Null)
        }));
    }

    if let Some(spike) = tools
        .iter()
        .filter(|tool| tool.get("peak_ms").and_then(Value::as_u64).unwrap_or(0) > 0)
        .max_by_key(|tool| tool.get("peak_ms").and_then(Value::as_u64).unwrap_or(0))
    {
        lines.push(json!({
            "kind": "info",
            "label": "peak single-call ms",
            "value": format!("{}ms", spike.get("peak_ms").and_then(Value::as_u64).unwrap_or(0)),
            "detail": spike.get("name").cloned().unwrap_or(Value::Null)
        }));
    }

    if let Some(busy) = tools
        .iter()
        .max_by_key(|tool| tool.get("calls").and_then(Value::as_u64).unwrap_or(0))
    {
        lines.push(json!({
            "kind": "info",
            "label": "busiest",
            "value": busy.get("calls_fmt").cloned().unwrap_or(Value::Null),
            "detail": busy.get("name").cloned().unwrap_or(Value::Null)
        }));
    }

    let low_yield: Vec<String> = tools
        .iter()
        .filter(|tool| {
            let calls = tool.get("calls").and_then(Value::as_u64).unwrap_or(0);
            let low = tool
                .get("low_yield_count")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            is_notable_low_yield(calls, low)
        })
        .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_string))
        .collect();
    if !low_yield.is_empty() {
        lines.push(json!({
            "kind": "warn",
            "label": "low-yield",
            "value": low_yield.join(", "),
            "detail": Value::Null
        }));
    }

    let inverted: Vec<String> = tools
        .iter()
        .filter(|tool| {
            tool.get("inverted")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .filter(|tool| {
            let baseline = tool
                .get("baseline_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let returned = tool
                .get("returned_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            is_net_negative(baseline, returned)
        })
        .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_string))
        .collect();
    if inverted.is_empty() {
        lines.push(json!({
            "kind": "ok",
            "label": "net-negative",
            "value": "none",
            "detail": Value::Null
        }));
    } else {
        lines.push(json!({
            "kind": "danger",
            "label": "net-negative",
            "value": inverted.join(", "),
            "detail": format!("{} tools", inverted.len())
        }));
    }

    json!({ "signals": lines })
}

fn is_notable_low_yield(calls: u64, low_yield_count: u64) -> bool {
    calls >= LOW_YIELD_MIN_CALLS && low_yield_count * 100 / calls.max(1) >= LOW_YIELD_RATE_PCT
}

fn is_net_negative(baseline: u64, returned: u64) -> bool {
    baseline >= NET_NEGATIVE_BASELINE_FLOOR && returned >= baseline
}

fn pct(saved: u64, baseline: u64) -> u64 {
    if baseline == 0 {
        return 0;
    }
    let p = (saved as f64 / baseline as f64 * 100.0).round() as u64;
    p.min(99)
}

fn bar(pct: u64, width: usize) -> String {
    let filled = (pct.min(100) as usize * width + 50) / 100;
    (0..width)
        .map(|i| if i < filled { '█' } else { '░' })
        .collect()
}

fn commafy(n: u64) -> String {
    const MILLION: u64 = 1_000_000;
    const BILLION: u64 = 1_000_000_000;
    if n >= MILLION {
        let (div, suffix) = if n >= BILLION {
            (BILLION as f64, "B")
        } else {
            (MILLION as f64, "M")
        };
        let value = n as f64 / div;
        let raw = format!("{value:.2}");
        let trimmed = raw.trim_end_matches('0').trim_end_matches('.');
        return format!("{trimmed}{suffix}");
    }
    let s = n.to_string();
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i != 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

fn elapsed(since: u64, now: u64) -> String {
    let secs = now.saturating_sub(since);
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let mins = (secs % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{mins}m")
    }
}

fn ago(now: u64, ts: u64) -> String {
    let secs = now.saturating_sub(ts);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commafy_and_bar_helpers() {
        assert_eq!(commafy(1200), "1,200");
        assert_eq!(bar(50, 4), "██░░");
        assert!(is_net_negative(600, 600));
        assert!(!is_notable_low_yield(5, 5));
    }

    #[test]
    fn window_fold_counts_returned_and_not_found() {
        let event = json!({
            "ts": 50,
            "tool": "repo_map",
            "baseline": 400,
            "returned": 90,
            "elapsed_us": 1000,
            "outcome": "not_found"
        });
        let mut win = Win::default();
        win.observe(&event);
        let row = tool_row("repo_map", &win.to_value());
        assert_eq!(row.get("returned_tokens").and_then(Value::as_u64), Some(90));
        assert_eq!(row.get("not_found_count").and_then(Value::as_u64), Some(1));
    }
}
