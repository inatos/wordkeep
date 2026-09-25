//! Token-budgeted Runtime Memory Health snapshots and comparisons.
//!
//! The MCP process stays network-free. It reads the merged snapshot and bounded
//! captures written by `wordkeep-wiki`, falling back to Betwixt's cooperative
//! `.wordkeep/runtime/latest.json` census.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::workspace;

const DEFAULT_BUDGET: usize = 1_600;
const DEFAULT_TOP: usize = 12;

pub fn snapshot(root: &Path, args: &Value) -> Result<String, String> {
    let id = args
        .get("capture")
        .and_then(Value::as_str)
        .unwrap_or("live");
    let value = load_snapshot(root, id)?;
    let top = usize_arg(args, "top", DEFAULT_TOP).min(64);
    let budget = usize_arg(args, "token_budget", DEFAULT_BUDGET);
    Ok(clip(render_snapshot(id, &value, top), budget))
}

pub fn memory_diff(root: &Path, args: &Value) -> Result<String, String> {
    let base = args
        .get("base")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            format!(
                "base is required (capture id digits). Produce captures via wiki Health → Runtime \
                 (Capture), then pass the numeric id. Available: {}",
                available_captures_hint(root)
            )
        })?;
    let current = args
        .get("current")
        .and_then(Value::as_str)
        .unwrap_or("live");
    let a = load_snapshot(root, base)?;
    let b = load_snapshot(root, current)?;
    let budget = usize_arg(args, "token_budget", DEFAULT_BUDGET);
    Ok(clip(render_diff(base, &a, current, &b), budget))
}

pub fn locality_hotspots(root: &Path, args: &Value) -> Result<String, String> {
    let id = args
        .get("capture")
        .and_then(Value::as_str)
        .unwrap_or("live");
    let value = load_snapshot(root, id)?;
    let top = usize_arg(args, "top", DEFAULT_TOP).min(64);
    let min_samples = usize_arg(args, "min_samples", 1) as u64;
    let budget = usize_arg(args, "token_budget", DEFAULT_BUDGET);
    Ok(clip(render_locality(id, &value, top, min_samples), budget))
}

fn runtime_dir(root: &Path) -> PathBuf {
    workspace::workspace_dir(root).join("runtime")
}

fn valid_capture_id(id: &str) -> bool {
    id == "live" || (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
}

fn load_snapshot(root: &Path, id: &str) -> Result<Value, String> {
    if !valid_capture_id(id) {
        return Err(format!(
            "capture id must be `live` or digits; got `{id}`. Available: {}",
            available_captures_hint(root)
        ));
    }
    if id == "live" {
        let merged = runtime_dir(root).join("latest.json");
        if merged.is_file() {
            return read_json(&merged);
        }
        let cooperative = root.join(".wordkeep/runtime/latest.json");
        if cooperative.is_file() {
            return read_json(&cooperative);
        }
        return Err(
            "no runtime snapshot. Produce one via: (1) wiki Health → Runtime while Betwixt runs, \
             or (2) run Betwixt from the workspace root (writes `.wordkeep/runtime/latest.json`). \
             Then call runtime_snapshot / memory_diff / locality_hotspots. Soft-route: after a \
             Tracy hitch hunt, compare two wiki Capture ids with memory_diff."
                .into(),
        );
    }
    let path = runtime_dir(root).join(format!("capture-{id}.jsonl"));
    if !path.is_file() {
        return Err(format!(
            "no capture `{id}` at {}. Produce numbered captures via wiki Health → Runtime → \
             Capture (writes capture-<id>.jsonl). Available: {}. Soft-route: use capture=`live` \
             for the merged census, or runtime_snapshot first to confirm a snapshot exists.",
            path.display(),
            available_captures_hint(root)
        ));
    }
    read_last_jsonl(&path)
}

fn available_captures_hint(root: &Path) -> String {
    let mut ids = list_capture_ids(root);
    let live = runtime_dir(root).join("latest.json").is_file()
        || root.join(".wordkeep/runtime/latest.json").is_file();
    if live {
        ids.insert(0, "live".into());
    }
    if ids.is_empty() {
        "(none — open wiki Health → Runtime or run Betwixt)".into()
    } else {
        ids.join(", ")
    }
}

fn list_capture_ids(root: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(runtime_dir(root)) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let id = name.strip_prefix("capture-")?.strip_suffix(".jsonl")?;
            if valid_capture_id(id) && id != "live" {
                Some(id.to_string())
            } else {
                None
            }
        })
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

fn read_json(path: &Path) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))
}

fn read_last_jsonl(path: &Path) -> Result<Value, String> {
    let file = fs::File::open(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut last = None;
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|e| format!("read {}: {e}", path.display()))?;
        if !line.trim().is_empty() {
            last = Some(line);
        }
    }
    let line = last.ok_or_else(|| format!("capture {} is empty", path.display()))?;
    serde_json::from_str(&line).map_err(|e| format!("parse {}: {e}", path.display()))
}

fn render_snapshot(id: &str, value: &Value, top: usize) -> String {
    let mut out = format!(
        "runtime_snapshot - {id}\nsource={} pid={} seq={} tier={} coverage_start_ns={}\n",
        text(value, "/source"),
        text(value, "/pid"),
        text(value, "/seq"),
        text(value, "/tier"),
        text(value, "/coverage_start_ns")
    );
    out.push_str(&format!(
        "RSS {} [{}]  committed VA {} [{}]  swap {}\n",
        bytes_at(value, "/rss_bytes"),
        text(value, "/quality/rss_bytes"),
        bytes_at(value, "/committed_va_bytes"),
        text(value, "/quality/committed_va_bytes"),
        bytes_at(value, "/swap_bytes")
    ));
    out.push_str(&format!(
        "Flecs {} (unused columns {})  Jolt temp {} / {}\n",
        bytes_at(value, "/flecs/bytes"),
        bytes_at(value, "/flecs/bytes_table_components_unused"),
        bytes_at(value, "/jolt/temp_used"),
        bytes_at(value, "/jolt/temp_cap")
    ));
    if value.get("gpu").is_some() {
        out.push_str(&format!(
            "GPU particles {} / {} [{}]  driver {}\n",
            text(value, "/gpu/particles"),
            text(value, "/gpu/particle_cap"),
            text(value, "/gpu/quality"),
            bytes_at(value, "/gpu/driver_available_kb")
        ));
    }
    if let Some(budget) = value.get("budget") {
        out.push_str(&format!(
            "budget {} ok={} checked={} unavailable={}\n",
            text(budget, "/name"),
            text(budget, "/ok"),
            text(budget, "/checked"),
            text(budget, "/unavailable")
        ));
        if let Some(violations) = budget.get("violations").and_then(Value::as_array) {
            for violation in violations.iter().take(top) {
                out.push_str(&format!(
                    "  {} actual={} limit={} [{}]\n",
                    text(violation, "/metric"),
                    text(violation, "/actual"),
                    text(violation, "/limit"),
                    text(violation, "/quality")
                ));
            }
        }
    }

    if let Some(pools) = value.get("pools").and_then(Value::as_array) {
        out.push_str("pools:\n");
        for pool in pools.iter().take(top) {
            out.push_str(&format!(
                "  {}: {} / {} [{}]\n",
                text(pool, "/name"),
                text(pool, "/in_use"),
                text(pool, "/cap"),
                text(pool, "/quality")
            ));
        }
    }

    if let Some(regions) = value.get("regions").and_then(Value::as_array) {
        out.push_str(&format!(
            "largest mappings (top {}):\n",
            top.min(regions.len())
        ));
        for region in regions.iter().take(top) {
            out.push_str(&format!(
                "  {:>10}  {:<7} {} {}\n",
                bytes_at(region, "/size"),
                text(region, "/kind"),
                text(region, "/perm"),
                text(region, "/path")
            ));
        }
    }
    if let Some(hints) = value.get("hints").and_then(Value::as_array) {
        if !hints.is_empty() {
            out.push_str("hints:\n");
            for hint in hints.iter().take(top) {
                out.push_str(&format!(
                    "  [{}] {} — {}\n",
                    text(hint, "/kind"),
                    text(hint, "/label"),
                    text(hint, "/detail")
                ));
            }
        }
    }
    out
}

fn render_diff(base_id: &str, base: &Value, current_id: &str, current: &Value) -> String {
    let mut out = format!("memory_diff - {base_id} → {current_id}\n");
    for (label, pointer) in [
        ("RSS", "/rss_bytes"),
        ("committed VA", "/committed_va_bytes"),
        ("swap", "/swap_bytes"),
        ("Flecs bytes", "/flecs/bytes"),
        (
            "Flecs unused columns",
            "/flecs/bytes_table_components_unused",
        ),
        ("Jolt temp", "/jolt/temp_used"),
    ] {
        out.push_str(&format_delta(
            label,
            number_at(base, pointer),
            number_at(current, pointer),
        ));
    }
    append_named_deltas(
        &mut out,
        "pools (in_use)",
        named_values(base, "/pools", "name", "in_use"),
        named_values(current, "/pools", "name", "in_use"),
        false,
    );
    append_named_deltas(
        &mut out,
        "mapping kinds",
        object_values(base, "/kinds"),
        object_values(current, "/kinds"),
        true,
    );
    out
}

fn render_locality(id: &str, value: &Value, top: usize, min_samples: u64) -> String {
    let Some(locality) = value.get("locality") else {
        return format!(
            "locality_hotspots - {id}\nquality=unavailable\nNo PMC/ETW/perf address samples are present; \
             VA adjacency is not treated as cache locality.\n\
             Soft-route: enable sampling in wiki Health → Runtime (PMC/ETW/perf) before Capture, \
             or use runtime_snapshot / memory_diff for RSS/VA/pool census without locality.\n"
        );
    };
    let quality = text(locality, "/quality");
    let Some(hotspots) = locality.get("hotspots").and_then(Value::as_array) else {
        return format!(
            "locality_hotspots - {id}\nquality={quality}\nNo attributed hotspot records.\n\
             Soft-route: re-capture with locality sampling enabled in wiki Health → Runtime.\n"
        );
    };
    let mut rows: Vec<_> = hotspots
        .iter()
        .filter(|row| number_at(row, "/samples").unwrap_or(0) >= min_samples as i64)
        .collect();
    rows.sort_by_key(|row| std::cmp::Reverse(number_at(row, "/samples").unwrap_or(0)));
    let mut out = format!(
        "locality_hotspots - {id}\nquality={quality} total_samples={}\n",
        text(locality, "/samples")
    );
    for row in rows.into_iter().take(top) {
        out.push_str(&format!(
            "  samples={} misses={} weight={} addr={} thread={} {} [{}]\n",
            text(row, "/samples"),
            text(row, "/misses"),
            text(row, "/weight"),
            text(row, "/addr"),
            text(row, "/thread"),
            text(row, "/symbol"),
            text(row, "/data_source")
        ));
    }
    out
}

fn append_named_deltas(
    out: &mut String,
    heading: &str,
    base: BTreeMap<String, i64>,
    current: BTreeMap<String, i64>,
    bytes: bool,
) {
    let keys: BTreeSet<_> = base.keys().chain(current.keys()).cloned().collect();
    if keys.is_empty() {
        return;
    }
    out.push_str(&format!("{heading}:\n"));
    for key in keys {
        let a = base.get(&key).copied().unwrap_or(0);
        let b = current.get(&key).copied().unwrap_or(0);
        let delta = b.saturating_sub(a);
        if bytes {
            out.push_str(&format!(
                "  {key}: {} → {} ({})\n",
                fmt_bytes(a),
                fmt_bytes(b),
                fmt_signed_bytes(delta)
            ));
        } else {
            out.push_str(&format!("  {key}: {a} → {b} ({delta:+})\n"));
        }
    }
}

fn named_values(value: &Value, pointer: &str, key: &str, metric: &str) -> BTreeMap<String, i64> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| {
            Some((
                row.get(key)?.as_str()?.to_string(),
                number_at(row, &format!("/{metric}"))?,
            ))
        })
        .collect()
}

fn object_values(value: &Value, pointer: &str) -> BTreeMap<String, i64> {
    value
        .pointer(pointer)
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(key, value)| numeric(value).map(|n| (key.clone(), n)))
        .collect()
}

fn format_delta(label: &str, base: Option<i64>, current: Option<i64>) -> String {
    match (base, current) {
        (Some(a), Some(b)) => format!(
            "{label}: {} → {} ({})\n",
            fmt_bytes(a),
            fmt_bytes(b),
            fmt_signed_bytes(b.saturating_sub(a))
        ),
        _ => format!("{label}: unavailable\n"),
    }
}

fn bytes_at(value: &Value, pointer: &str) -> String {
    number_at(value, pointer)
        .map(fmt_bytes)
        .unwrap_or_else(|| "unavailable".into())
}

fn number_at(value: &Value, pointer: &str) -> Option<i64> {
    numeric(value.pointer(pointer)?)
}

fn numeric(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().map(|n| n.min(i64::MAX as u64) as i64))
}

fn text(value: &Value, pointer: &str) -> String {
    match value.pointer(pointer) {
        Some(Value::String(s)) => {
            if s.is_empty() {
                "—".into()
            } else {
                s.clone()
            }
        }
        Some(Value::Null) | None => "unavailable".into(),
        Some(other) => other.to_string(),
    }
}

fn fmt_bytes(value: i64) -> String {
    let sign = if value < 0 { "-" } else { "" };
    let n = value.unsigned_abs() as f64;
    if n >= 1_073_741_824.0 {
        format!("{sign}{:.2} GiB", n / 1_073_741_824.0)
    } else if n >= 1_048_576.0 {
        format!("{sign}{:.2} MiB", n / 1_048_576.0)
    } else if n >= 1_024.0 {
        format!("{sign}{:.1} KiB", n / 1_024.0)
    } else {
        format!("{value} B")
    }
}

fn fmt_signed_bytes(value: i64) -> String {
    if value >= 0 {
        format!("+{}", fmt_bytes(value))
    } else {
        fmt_bytes(value)
    }
}

fn usize_arg(args: &Value, key: &str, default: usize) -> usize {
    args.get(key)
        .and_then(Value::as_u64)
        .map(|n| n.min(usize::MAX as u64) as usize)
        .unwrap_or(default)
}

fn clip(mut text: String, token_budget: usize) -> String {
    let cap = token_budget.max(64).saturating_mul(4);
    if text.len() <= cap {
        return text;
    }
    let mut end = cap.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str("\n… output clipped by token_budget\n");
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn snapshot_renders_quality_and_regions() {
        let value = json!({
            "source": "betwixt",
            "pid": 7,
            "seq": 2,
            "tier": "census",
            "rss_bytes": 2048,
            "committed_va_bytes": 4096,
            "quality": {"rss_bytes": "exact", "committed_va_bytes": "exact"},
            "regions": [{"size": 4096, "kind": "anon", "perm": "rw-", "path": null}]
        });
        let out = render_snapshot("live", &value, 4);
        assert!(out.contains("RSS 2.0 KiB [exact]"));
        assert!(out.contains("anon"));
    }

    #[test]
    fn snapshot_renders_gpu_and_budget_violations() {
        let value = json!({
            "gpu": {
                "quality": "estimated",
                "particles": 12,
                "particle_cap": 1024,
                "driver_available_kb": 2048
            },
            "budget": {
                "name": "gym-idle",
                "ok": false,
                "checked": 1,
                "unavailable": 0,
                "violations": [{
                    "metric": "/rss_bytes",
                    "actual": 200,
                    "limit": 100,
                    "quality": "exact"
                }]
            }
        });
        let out = render_snapshot("live", &value, 4);
        assert!(out.contains("GPU particles 12 / 1024 [estimated]"));
        assert!(out.contains("budget gym-idle ok=false"));
        assert!(out.contains("/rss_bytes"));
    }

    #[test]
    fn diff_keeps_negative_deltas() {
        let a = json!({"rss_bytes": 4096, "kinds": {"anon": 4096}});
        let b = json!({"rss_bytes": 2048, "kinds": {"anon": 1024}});
        let out = render_diff("a", &a, "b", &b);
        assert!(out.contains("-2.0 KiB"));
        assert!(out.contains("-3.0 KiB"));
    }

    #[test]
    fn locality_does_not_infer_from_va_map() {
        let out = render_locality("live", &json!({"regions": []}), 8, 1);
        assert!(out.contains("quality=unavailable"));
        assert!(out.contains("VA adjacency is not treated as cache locality"));
        assert!(out.contains("Soft-route"));
    }

    #[test]
    fn missing_capture_lists_available_and_soft_route() {
        let dir = std::env::temp_dir().join(format!(
            "wk_runtime_miss_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".wordkeep/runtime")).unwrap();
        fs::write(
            dir.join(".wordkeep/runtime/latest.json"),
            br#"{"schema_version":1,"source":"test"}"#,
        )
        .unwrap();
        // Point workspace runtime at empty cache dir so numbered capture misses.
        let err = load_snapshot(&dir, "999").unwrap_err();
        assert!(err.contains("no capture `999`"), "{err}");
        assert!(err.contains("Health → Runtime"), "{err}");
        assert!(err.contains("Available:"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }
}
