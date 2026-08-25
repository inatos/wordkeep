//! Live RuntimeHub: census merge, token, capture ring, attach pid.

use crate::indexer::manifest_path;
use crate::runtime::budget;
use crate::runtime::maps::{
    address_segments_json, committed_bytes, read_pid_maps, read_pid_status, regions_json,
    top_regions,
};
use crate::runtime::numa;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::watch;

const RING: usize = 120;
const TOP_N: usize = 48;
const MAP_SEGMENTS: usize = 512;
const MAX_CAPTURE_SAMPLES: usize = 1_800;
const MAX_CAPTURES: usize = 16;
const OVERLAY_TTL_NS: u64 = 5_000_000_000;

#[derive(Clone)]
pub struct RuntimeHub {
    inner: Arc<Mutex<Inner>>,
    updates: watch::Sender<Value>,
}

struct Inner {
    pub token: String,
    pub enabled: bool,
    pub attach_enabled: bool,
    pub attach_pid: Option<u32>,
    pub seq: u64,
    pub dropped: u64,
    pub coverage_start_ns: u64,
    pub snapshot: Value,
    pub ring: VecDeque<Value>,
    pub capture: Option<CaptureSession>,
    pub overlay: Option<Value>,
}

struct CaptureSession {
    id: String,
    path: PathBuf,
    started_ns: u64,
    samples: usize,
}

impl RuntimeHub {
    pub fn new(enabled: bool, attach_enabled: bool) -> Self {
        let token = format!("{:016x}", now_ns().wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let initial = json!({ "available": false, "message": "no census yet" });
        let (updates, _) = watch::channel(json!({
            "schema_version": 1,
            "seq": 0,
            "changed": initial
        }));
        Self {
            inner: Arc::new(Mutex::new(Inner {
                token,
                enabled,
                attach_enabled,
                attach_pid: None,
                seq: 0,
                dropped: 0,
                coverage_start_ns: now_ns(),
                snapshot: initial,
                ring: VecDeque::with_capacity(RING),
                capture: None,
                overlay: None,
            })),
            updates,
        }
    }

    #[cfg(test)]
    pub fn disabled() -> Self {
        Self::new(false, false)
    }

    #[cfg(test)]
    pub fn token(&self) -> String {
        self.inner
            .lock()
            .map(|g| g.token.clone())
            .unwrap_or_default()
    }

    pub fn enabled(&self) -> bool {
        self.inner.lock().map(|g| g.enabled).unwrap_or(false)
    }

    pub fn attach_enabled(&self) -> bool {
        self.inner.lock().map(|g| g.attach_enabled).unwrap_or(false)
    }

    pub fn check_token(&self, got: Option<&str>) -> bool {
        let Ok(g) = self.inner.lock() else {
            return false;
        };
        got == Some(g.token.as_str())
    }

    pub fn set_attach_pid(&self, pid: Option<u32>) -> Result<(), String> {
        let mut g = self.inner.lock().map_err(|e| e.to_string())?;
        if !g.attach_enabled {
            return Err("attach disabled (pass --runtime-attach)".into());
        }
        g.attach_pid = pid;
        g.overlay = None;
        g.coverage_start_ns = now_ns();
        Ok(())
    }

    pub fn attach_pid(&self) -> Option<u32> {
        self.inner.lock().ok().and_then(|g| g.attach_pid)
    }

    pub fn snapshot(&self) -> Value {
        self.inner
            .lock()
            .map(|g| g.snapshot.clone())
            .unwrap_or(json!({ "available": false }))
    }

    pub fn subscribe(&self) -> watch::Receiver<Value> {
        self.updates.subscribe()
    }

    pub fn refresh(&self, root: &Path) {
        let Ok(mut g) = self.inner.lock() else {
            return;
        };
        if !g.enabled {
            let previous = g.snapshot.clone();
            let snap = json!({
                "available": false,
                "message": "runtime disabled on non-loopback binds"
            });
            let update = stream_delta(&previous, &snap);
            g.snapshot = snap;
            drop(g);
            self.updates.send_replace(update);
            return;
        }
        let previous = g.snapshot.clone();
        g.seq = g.seq.saturating_add(1);
        let pid = g.attach_pid.unwrap_or_else(std::process::id);
        let source = if g.attach_pid.is_some() {
            "attach"
        } else {
            "self"
        };
        let mut snap = census_os(pid, source);
        snap["schema_version"] = json!(1);
        snap["seq"] = json!(g.seq);
        snap["dropped"] = json!(g.dropped);
        snap["tier"] = json!("census");
        snap["token"] = json!(g.token);
        snap["attach_enabled"] = json!(g.attach_enabled);
        snap["coverage_start_ns"] = json!(g.coverage_start_ns);
        snap["numa"] = numa::topology_json();

        if let Some(coop) = load_cooperative(root) {
            snap["source"] = json!("betwixt");
            if let Some(obj) = snap.as_object_mut() {
                if let Some(c) = coop.as_object() {
                    for (k, v) in c {
                        if k == "regions" {
                            continue;
                        }
                        if (k == "capabilities" || k == "quality")
                            && v.is_object()
                            && obj.get(k).is_some_and(Value::is_object)
                        {
                            if let (Some(target), Some(extra)) =
                                (obj.get_mut(k).and_then(Value::as_object_mut), v.as_object())
                            {
                                for (nested_key, nested_value) in extra {
                                    target.insert(nested_key.clone(), nested_value.clone());
                                }
                            }
                            continue;
                        }
                        obj.insert(k.clone(), v.clone());
                    }
                }
            }
            snap["cooperative"] = json!(true);
        } else {
            snap["cooperative"] = json!(false);
        }
        if let Some(overlay) = g.overlay.as_ref() {
            if overlay_applies(overlay, pid) {
                merge_runtime_overlay(&mut snap, overlay);
            }
        }
        if let Some(spec) = budget::load(root) {
            snap["budget"] = budget::evaluate(&spec, &snap);
        }

        snap["capturing"] = json!(g.capture.is_some());
        if let Some(capture) = g.capture.as_ref() {
            snap["capture_id"] = json!(capture.id);
        }
        let mut finish_capture = false;
        let mut capture_failed = false;
        if let Some(capture) = g.capture.as_mut() {
            let clean = sanitized_capture(&snap);
            if append_jsonl(&capture.path, &clean).is_ok() {
                capture.samples += 1;
            } else {
                capture_failed = true;
            }
            finish_capture = capture.samples >= MAX_CAPTURE_SAMPLES;
        }
        if capture_failed {
            g.dropped = g.dropped.saturating_add(1);
        }
        if finish_capture {
            if let Some(capture) = g.capture.take() {
                let _ = write_capture_meta(&capture, true);
            }
            snap["capturing"] = json!(false);
            snap["capture_truncated"] = json!(true);
        }

        let seq = g.seq;
        if g.ring.len() == RING {
            g.ring.pop_front();
        }
        let presented_ms = snap.pointer("/frame/presented_ms").and_then(Value::as_f64);
        g.ring.push_back(json!({
            "seq": seq,
            "ts_ns": snap.get("ts_ns").cloned().unwrap_or(json!(0)),
            "rss_bytes": snap.get("rss_bytes").cloned().unwrap_or(json!(null)),
            "committed_va_bytes": snap.get("committed_va_bytes").cloned().unwrap_or(json!(null)),
            "flecs_unused_bytes": snap.pointer("/flecs/bytes_table_components_unused").cloned().unwrap_or(json!(null)),
            "jolt_temp_used": snap.pointer("/jolt/temp_used").cloned().unwrap_or(json!(null)),
            "presented_ms": presented_ms,
            "hitch": presented_ms.is_some_and(|ms| ms >= 25.0),
        }));
        snap["timeline"] = json!(g.ring.iter().cloned().collect::<Vec<_>>());
        let _ = write_runtime_latest(root, &sanitized_capture(&snap));
        let update = stream_delta(&previous, &snap);
        g.snapshot = snap;
        drop(g);
        self.updates.send_replace(update);
    }

    pub fn ingest(&self, mut value: Value) -> Result<(), String> {
        let mut g = self.inner.lock().map_err(|e| e.to_string())?;
        if !g.enabled {
            return Err("runtime disabled".into());
        }
        let ver = value
            .get("schema_version")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if ver != 1 {
            return Err("unsupported schema_version".into());
        }
        let previous = g.snapshot.clone();
        g.seq = g.seq.saturating_add(1);
        if value.get("kind").and_then(Value::as_str) == Some("runtime_overlay") {
            for key in ["lifetime", "locality"] {
                if let Some(section) = value.get(key) {
                    if !section.is_object() {
                        return Err(format!("{key} overlay must be an object"));
                    }
                }
            }
            value["_received_at_ns"] = json!(now_ns());
            g.overlay = Some(value.clone());
            let pid = g.attach_pid.unwrap_or_else(std::process::id);
            if overlay_applies(&value, pid) {
                merge_runtime_overlay(&mut g.snapshot, &value);
            }
            g.snapshot["seq"] = json!(g.seq);
            let update = stream_delta(&previous, &g.snapshot);
            drop(g);
            self.updates.send_replace(update);
            return Ok(());
        }
        g.snapshot = value;
        g.snapshot["seq"] = json!(g.seq);
        g.snapshot["token"] = json!(g.token);
        let update = stream_delta(&previous, &g.snapshot);
        drop(g);
        self.updates.send_replace(update);
        Ok(())
    }

    pub fn set_capturing(&self, root: &Path, on: bool) -> Result<Value, String> {
        let mut g = self.inner.lock().map_err(|e| e.to_string())?;
        if on {
            if let Some(capture) = g.capture.as_ref() {
                return Ok(capture_status(capture, true, false));
            }
            let dir = capture_dir(root)?;
            prune_captures(&dir)?;
            let started_ns = now_ns();
            let id = started_ns.to_string();
            let path = dir.join(format!("capture-{id}.jsonl"));
            fs::write(&path, b"").map_err(|e| format!("create {}: {e}", path.display()))?;
            let mut samples = 0;
            if g.snapshot.get("available").and_then(Value::as_bool) == Some(true) {
                append_jsonl(&path, &sanitized_capture(&g.snapshot))?;
                samples = 1;
            }
            let capture = CaptureSession {
                id,
                path,
                started_ns,
                samples,
            };
            let status = capture_status(&capture, true, false);
            write_capture_meta(&capture, false)?;
            g.capture = Some(capture);
            Ok(status)
        } else if let Some(capture) = g.capture.take() {
            write_capture_meta(&capture, false)?;
            Ok(capture_status(&capture, false, false))
        } else {
            Ok(json!({"capturing": false}))
        }
    }

    pub fn captures(&self, root: &Path) -> Result<Value, String> {
        let active = self
            .inner
            .lock()
            .ok()
            .and_then(|g| g.capture.as_ref().map(|c| c.id.clone()));
        list_captures(root, active.as_deref())
    }

    pub fn diff_capture(&self, root: &Path, base: &str, current: &str) -> Result<Value, String> {
        let a = load_capture_snapshot(root, base, Some(self.snapshot()))?;
        let b = load_capture_snapshot(root, current, Some(self.snapshot()))?;
        Ok(snapshot_diff(base, &a, current, &b))
    }
}

fn merge_runtime_overlay(snapshot: &mut Value, overlay: &Value) {
    for key in ["lifetime", "locality"] {
        if let Some(value) = overlay.get(key) {
            snapshot[key] = value.clone();
        }
    }
    for key in ["alloc_hooks", "pmc"] {
        if let Some(value) = overlay.pointer(&format!("/capabilities/{key}")) {
            snapshot["capabilities"][key] = value.clone();
        }
    }
    if let Some(quality) = overlay.get("quality").and_then(Value::as_object) {
        for (key, value) in quality {
            snapshot["quality"][key] = value.clone();
        }
    }
    if let Some(dropped) = overlay.get("dropped") {
        snapshot["overlay_dropped"] = dropped.clone();
    }
    if let Some(overhead) = overlay.get("estimated_overhead_pct") {
        snapshot["estimated_overhead_pct"] = overhead.clone();
    }
}

fn overlay_applies(overlay: &Value, pid: u32) -> bool {
    let pid_matches = overlay
        .get("pid")
        .and_then(Value::as_u64)
        .is_none_or(|overlay_pid| overlay_pid == pid as u64);
    let fresh = overlay
        .get("_received_at_ns")
        .and_then(Value::as_u64)
        .is_some_and(|received| now_ns().saturating_sub(received) <= OVERLAY_TTL_NS);
    pid_matches && fresh
}

fn stream_delta(previous: &Value, next: &Value) -> Value {
    const AGGREGATES: &[&str] = &[
        "available",
        "message",
        "seq",
        "dropped",
        "ts_ns",
        "tier",
        "source",
        "os",
        "pid",
        "name",
        "cooperative",
        "attach_enabled",
        "capturing",
        "capture_id",
        "capture_truncated",
        "coverage_start_ns",
        "rss_bytes",
        "peak_rss_bytes",
        "swap_bytes",
        "committed_va_bytes",
        "quality",
        "capabilities",
        "flecs",
        "jolt",
        "pools",
        "frame",
        "hints",
        "timeline",
        "lifetime",
        "locality",
        "gpu",
        "budget",
        "alloc_streams",
        "overlay_dropped",
        "estimated_overhead_pct",
    ];
    let mut changed = serde_json::Map::new();
    for key in AGGREGATES {
        if previous.get(*key) != next.get(*key) {
            changed.insert(
                (*key).to_string(),
                next.get(*key).cloned().unwrap_or(Value::Null),
            );
        }
    }
    if previous.get("regions") != next.get("regions")
        || previous.get("address_map") != next.get("address_map")
    {
        changed.insert(
            "regions".into(),
            next.get("regions").cloned().unwrap_or_else(|| json!([])),
        );
        changed.insert(
            "address_map".into(),
            next.get("address_map")
                .cloned()
                .unwrap_or_else(|| json!({"segments": []})),
        );
        changed.insert(
            "kinds".into(),
            next.get("kinds").cloned().unwrap_or_else(|| json!({})),
        );
        changed.insert("map_dirty".into(), json!(true));
    }
    if previous.get("numa") != next.get("numa") {
        changed.insert(
            "numa".into(),
            next.get("numa").cloned().unwrap_or(Value::Null),
        );
    }
    json!({
        "schema_version": 1,
        "seq": next.get("seq").cloned().unwrap_or(json!(0)),
        "changed": changed
    })
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

fn census_os(pid: u32, source: &str) -> Value {
    let regions = read_pid_maps(pid).unwrap_or_default();
    let (rss, peak, swap) = read_pid_status(pid).unwrap_or((None, None, None));
    let committed = committed_bytes(&regions);
    let (top, other_b, other_n) = top_regions(&regions, TOP_N);
    let mut kinds: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for r in &regions {
        *kinds.entry(r.kind.clone()).or_insert(0) += r.size();
    }
    json!({
        "available": true,
        "ts_ns": now_ns(),
        "source": source,
        "os": std::env::consts::OS,
        "pid": pid,
        "name": std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "unknown".into()),
        "rss_bytes": rss,
        "peak_rss_bytes": peak,
        "swap_bytes": swap,
        "committed_va_bytes": committed,
        "quality": {
            "rss_bytes": if rss.is_some() { "exact" } else { "unavailable" },
            "committed_va_bytes": "exact",
            "heap_used_bytes": "unavailable"
        },
        "capabilities": {
            "cooperative": false,
            "alloc_hooks": false,
            "pmc": false,
            "peek": cfg!(unix) || cfg!(windows),
            "numa": cfg!(any(target_os = "linux", windows))
        },
        "kinds": kinds,
        "regions": regions_json(&top, other_b, other_n),
        "address_map": address_segments_json(&regions, MAP_SEGMENTS),
        "hints": hints(&regions, rss, committed),
        "flecs": {},
        "jolt": {},
        "pools": [],
        "ecs_suggestions": []
    })
}

fn hints(regions: &[crate::runtime::maps::Region], rss: Option<u64>, committed: u64) -> Vec<Value> {
    let mut h = Vec::new();
    let tiny = regions
        .iter()
        .filter(|r| r.size() < 4096 * 16 && r.kind == "anon")
        .count();
    if tiny > 200 {
        h.push(json!({
            "kind": "warn",
            "label": "many small anon mappings",
            "detail": format!("{tiny} regions <64KiB — VA scatter, not necessarily heap fragmentation")
        }));
    }
    if let Some(rss) = rss {
        if committed > 0 && rss * 4 < committed {
            h.push(json!({
                "kind": "info",
                "label": "committed >> RSS",
                "detail": "large reserved VA; residency overlay needed before calling it waste"
            }));
        }
    }
    h
}

pub fn cooperative_path(root: &Path) -> PathBuf {
    root.join(".wordkeep/runtime/latest.json")
}

fn load_cooperative(root: &Path) -> Option<Value> {
    let p = cooperative_path(root);
    let bytes = fs::read(&p).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn capture_dir(root: &Path) -> Result<PathBuf, String> {
    let dir = manifest_path(root)
        .parent()
        .map(|p| p.join("runtime"))
        .ok_or_else(|| "workspace cache path has no parent".to_string())?;
    fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    Ok(dir)
}

fn sanitized_capture(value: &Value) -> Value {
    let mut clean = value.clone();
    if let Some(obj) = clean.as_object_mut() {
        obj.remove("token");
    }
    clean
}

fn capture_status(capture: &CaptureSession, capturing: bool, truncated: bool) -> Value {
    json!({
        "id": capture.id,
        "capturing": capturing,
        "started_ns": capture.started_ns,
        "samples": capture.samples,
        "truncated": truncated,
        "file": capture.path.file_name().and_then(|s| s.to_str()).unwrap_or_default()
    })
}

fn write_capture_meta(capture: &CaptureSession, truncated: bool) -> Result<(), String> {
    let path = capture.path.with_extension("json");
    let tmp = path.with_extension("json.tmp");
    let mut meta = capture_status(capture, false, truncated);
    meta["ended_ns"] = json!(now_ns());
    let bytes = serde_json::to_vec_pretty(&meta).map_err(|e| e.to_string())?;
    fs::write(&tmp, bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, &path).map_err(|e| format!("rename {}: {e}", path.display()))
}

fn write_runtime_latest(root: &Path, snapshot: &Value) -> Result<(), String> {
    let path = capture_dir(root)?.join("latest.json");
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec(snapshot).map_err(|e| e.to_string())?;
    fs::write(&tmp, bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, &path).map_err(|e| format!("rename {}: {e}", path.display()))
}

fn prune_captures(dir: &Path) -> Result<(), String> {
    let mut captures: Vec<_> = fs::read_dir(dir)
        .map_err(|e| format!("read {}: {e}", dir.display()))?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.path().extension().and_then(|s| s.to_str()) == Some("jsonl")
                && entry.file_name().to_string_lossy().starts_with("capture-")
        })
        .collect();
    captures.sort_by_key(|entry| {
        entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(UNIX_EPOCH)
    });
    let remove_count = captures
        .len()
        .saturating_sub(MAX_CAPTURES.saturating_sub(1));
    for entry in captures.into_iter().take(remove_count) {
        let path = entry.path();
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(path.with_extension("json"));
    }
    Ok(())
}

fn capture_id_from_path(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let id = stem.strip_prefix("capture-")?;
    valid_capture_id(id).then(|| id.to_string())
}

fn valid_capture_id(id: &str) -> bool {
    id == "live" || (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
}

fn list_captures(root: &Path, active: Option<&str>) -> Result<Value, String> {
    let dir = capture_dir(root)?;
    let mut captures = Vec::new();
    for entry in fs::read_dir(&dir)
        .map_err(|e| format!("read {}: {e}", dir.display()))?
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(id) = capture_id_from_path(&path) else {
            continue;
        };
        let metadata = entry.metadata().ok();
        let bytes = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
        let meta = fs::read(path.with_extension("json"))
            .ok()
            .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok())
            .unwrap_or_else(|| json!({}));
        captures.push(json!({
            "id": id,
            "bytes": bytes,
            "samples": meta.get("samples").cloned().unwrap_or(Value::Null),
            "started_ns": meta.get("started_ns").cloned().unwrap_or(Value::Null),
            "ended_ns": meta.get("ended_ns").cloned().unwrap_or(Value::Null),
            "truncated": meta.get("truncated").cloned().unwrap_or(json!(false)),
            "capturing": active == Some(id.as_str())
        }));
    }
    captures.sort_by_key(|value| {
        std::cmp::Reverse(
            value
                .get("started_ns")
                .and_then(Value::as_u64)
                .or_else(|| value.get("id").and_then(Value::as_str)?.parse().ok())
                .unwrap_or(0),
        )
    });
    Ok(json!({
        "captures": captures,
        "max_captures": MAX_CAPTURES,
        "max_samples": MAX_CAPTURE_SAMPLES
    }))
}

fn load_capture_snapshot(root: &Path, id: &str, live: Option<Value>) -> Result<Value, String> {
    if id == "live" {
        return live.ok_or_else(|| "live snapshot unavailable".to_string());
    }
    if !valid_capture_id(id) {
        return Err("invalid capture id".into());
    }
    let path = capture_dir(root)?.join(format!("capture-{id}.jsonl"));
    let text = fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    text.lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .ok_or_else(|| "capture is empty".to_string())
        .and_then(|line| serde_json::from_str(line).map_err(|e| format!("parse capture: {e}")))
}

fn number_at(value: &Value, pointer: &str) -> Option<i64> {
    let n = value.pointer(pointer)?;
    n.as_i64()
        .or_else(|| n.as_u64().map(|v| v.min(i64::MAX as u64) as i64))
}

fn delta_metric(base: &Value, current: &Value, pointer: &str) -> Value {
    match (number_at(base, pointer), number_at(current, pointer)) {
        (Some(a), Some(b)) => json!({"base": a, "current": b, "delta": b.saturating_sub(a)}),
        _ => json!({"base": null, "current": null, "delta": null, "quality": "unavailable"}),
    }
}

fn pool_map(value: &Value) -> BTreeMap<String, i64> {
    value
        .get("pools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|pool| {
            Some((
                pool.get("name")?.as_str()?.to_string(),
                number_at(pool, "/in_use")?,
            ))
        })
        .collect()
}

fn kinds_map(value: &Value) -> BTreeMap<String, i64> {
    value
        .get("kinds")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(kind, bytes)| {
            Some((
                kind.clone(),
                bytes
                    .as_i64()
                    .or_else(|| bytes.as_u64().map(|v| v.min(i64::MAX as u64) as i64))?,
            ))
        })
        .collect()
}

fn map_deltas(base: BTreeMap<String, i64>, current: BTreeMap<String, i64>) -> Vec<Value> {
    let keys: BTreeSet<_> = base.keys().chain(current.keys()).cloned().collect();
    keys.into_iter()
        .map(|name| {
            let a = base.get(&name).copied().unwrap_or(0);
            let b = current.get(&name).copied().unwrap_or(0);
            json!({"name": name, "base": a, "current": b, "delta": b.saturating_sub(a)})
        })
        .collect()
}

fn snapshot_diff(base_id: &str, base: &Value, current_id: &str, current: &Value) -> Value {
    json!({
        "base": base_id,
        "current": current_id,
        "quality": "exact",
        "metrics": {
            "rss_bytes": delta_metric(base, current, "/rss_bytes"),
            "committed_va_bytes": delta_metric(base, current, "/committed_va_bytes"),
            "swap_bytes": delta_metric(base, current, "/swap_bytes"),
            "flecs_bytes": delta_metric(base, current, "/flecs/bytes"),
            "flecs_unused_bytes": delta_metric(base, current, "/flecs/bytes_table_components_unused"),
            "jolt_temp_used": delta_metric(base, current, "/jolt/temp_used")
        },
        "pools": map_deltas(pool_map(base), pool_map(current)),
        "kinds": map_deltas(kinds_map(base), kinds_map(current)),
        "base_seq": base.get("seq").cloned().unwrap_or(Value::Null),
        "current_seq": current.get("seq").cloned().unwrap_or(Value::Null)
    })
}

fn append_jsonl(path: &Path, v: &Value) -> Result<(), String> {
    use std::io::Write;
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    writeln!(f, "{v}").map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_hub_reports_unavailable() {
        let h = RuntimeHub::disabled();
        h.refresh(Path::new("."));
        assert_eq!(h.snapshot()["available"], false);
    }

    #[test]
    fn ingest_rejects_bad_schema() {
        let h = RuntimeHub::new(true, false);
        assert!(h.ingest(json!({"schema_version": 99})).is_err());
    }

    #[test]
    fn overlay_ingest_preserves_census_and_adds_lifetime() {
        let h = RuntimeHub::new(true, true);
        h.ingest(json!({
            "schema_version": 1,
            "rss_bytes": 1024,
            "capabilities": {"numa": true}
        }))
        .unwrap();
        h.ingest(json!({
            "schema_version": 1,
            "kind": "runtime_overlay",
            "pid": std::process::id(),
            "capabilities": {"alloc_hooks": true},
            "lifetime": {"quality": "sampled", "live_bytes": 64}
        }))
        .unwrap();
        let snapshot = h.snapshot();
        assert_eq!(snapshot["rss_bytes"], 1024);
        assert_eq!(snapshot["capabilities"]["numa"], true);
        assert_eq!(snapshot["capabilities"]["alloc_hooks"], true);
        assert_eq!(snapshot["lifetime"]["live_bytes"], 64);
    }

    #[test]
    fn overlay_rejects_wrong_pid_and_stale_data() {
        let current = std::process::id();
        let wrong_pid = current.saturating_add(1);
        assert!(!overlay_applies(
            &json!({"pid": wrong_pid, "_received_at_ns": now_ns()}),
            current
        ));
        assert!(!overlay_applies(
            &json!({
                "pid": current,
                "_received_at_ns": now_ns().saturating_sub(OVERLAY_TTL_NS + 1)
            }),
            current
        ));
    }

    #[test]
    fn stream_delta_only_carries_changed_map_when_needed() {
        let a = json!({
            "seq": 1,
            "rss_bytes": 10,
            "regions": [{"start_u64": 1, "size": 4}],
            "kinds": {"anon": 4}
        });
        let b = json!({
            "seq": 2,
            "rss_bytes": 12,
            "regions": [{"start_u64": 1, "size": 4}],
            "kinds": {"anon": 4}
        });
        let delta = stream_delta(&a, &b);
        assert_eq!(delta["changed"]["rss_bytes"], 12);
        assert!(delta["changed"].get("regions").is_none());

        let c = json!({
            "seq": 3,
            "rss_bytes": 12,
            "regions": [{"start_u64": 1, "size": 8}],
            "kinds": {"anon": 8}
        });
        let delta = stream_delta(&b, &c);
        assert_eq!(delta["changed"]["map_dirty"], true);
        assert_eq!(delta["changed"]["regions"][0]["size"], 8);
    }

    #[test]
    fn captures_drop_runtime_token_and_diff_metrics() {
        let base = json!({
            "token": "secret",
            "rss_bytes": 100,
            "committed_va_bytes": 500,
            "flecs": {"bytes_table_components_unused": 20},
            "pools": [{"name": "EntityPool", "in_use": 2}],
            "kinds": {"anon": 300}
        });
        assert!(sanitized_capture(&base).get("token").is_none());

        let current = json!({
            "rss_bytes": 140,
            "committed_va_bytes": 520,
            "flecs": {"bytes_table_components_unused": 12},
            "pools": [{"name": "EntityPool", "in_use": 5}],
            "kinds": {"anon": 280}
        });
        let diff = snapshot_diff("a", &base, "live", &current);
        assert_eq!(diff["metrics"]["rss_bytes"]["delta"], 40);
        assert_eq!(diff["metrics"]["flecs_unused_bytes"]["delta"], -8);
        assert_eq!(diff["pools"][0]["delta"], 3);
        assert_eq!(diff["kinds"][0]["delta"], -20);
    }

    #[test]
    fn refresh_attaches_scenario_budget_when_present() {
        let dir = std::env::temp_dir().join(format!(
            "wordkeep-budget-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join(".wordkeep/runtime")).unwrap();
        std::fs::write(
            dir.join(".wordkeep/runtime/budget.json"),
            r#"{"name":"hub-test","limits":{"/rss_bytes":1}}"#,
        )
        .unwrap();
        let h = RuntimeHub::new(true, false);
        h.refresh(&dir);
        let budget = &h.snapshot()["budget"];
        assert_eq!(budget["name"], "hub-test");
        assert!(budget.get("ok").is_some());
        let _ = std::fs::remove_dir_all(dir);
    }
}
