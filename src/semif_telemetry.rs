//! SemIf / Ereshkigal status + rolling events for the wiki Dashboard.

use serde_json::{json, Value};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::cache;

const STATUS: &str = "semif-status.json";
const EVENTS: &str = "semif-events.jsonl";
const EVENT_CAP: usize = 200;

static WRITE: Mutex<()> = Mutex::new(());

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn cargo_feature() -> bool {
    cfg!(feature = "ereshkigal")
}

pub fn write_status(root: &Path, extra: Value) {
    let _g = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let path = cache::dir().join(STATUS);
    let mut doc = json!({
        "cargo_feature": cargo_feature(),
        "page_visible": cargo_feature(),
        "backend": crate::config::semif_backend(root),
        "mode": crate::config::semif_mode(root),
        "debias": crate::config::semif_debias(root),
        "n_gpu_layers": crate::config::semif_n_gpu_layers(root),
        "cascade_routing": crate::config::semif_cascade_routing(root),
        "knowledge_cascade": crate::config::semif_knowledge_cascade(root),
        "updated": now_secs(),
        "gguf": crate::config::semif_gguf_path(root).map(|p| p.display().to_string()),
        "gguf_verify": crate::config::semif_gguf_verify_path(root).map(|p| p.display().to_string()),
    });
    if let Some(obj) = extra.as_object() {
        if let Some(dst) = doc.as_object_mut() {
            for (k, v) in obj {
                dst.insert(k.clone(), v.clone());
            }
        }
    }
    let _ = std::fs::create_dir_all(cache::dir());
    if let Ok(text) = serde_json::to_string_pretty(&doc) {
        let _ = std::fs::write(path, text);
    }
}

pub fn record_event(ev: Value) {
    let _g = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let path = cache::dir().join(EVENTS);
    let _ = std::fs::create_dir_all(cache::dir());
    let mut line = ev;
    if line.get("ts").is_none() {
        line["ts"] = json!(now_secs());
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        if let Ok(s) = serde_json::to_string(&line) {
            let _ = writeln!(f, "{s}");
        }
    }
    trim_events(&path);
}

fn trim_events(path: &Path) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let mut lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    if lines.len() <= EVENT_CAP {
        return;
    }
    let skip = lines.len() - EVENT_CAP;
    lines.drain(0..skip);
    let _ = std::fs::write(path, lines.join("\n") + "\n");
}

/// Wiki / tests: load status + recent events from the shared cache dir.
pub fn load_dashboard_payload() -> Value {
    let status_path = cache::dir().join(STATUS);
    let events_path = cache::dir().join(EVENTS);
    let status = std::fs::read_to_string(&status_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .unwrap_or_else(|| {
            json!({
                "cargo_feature": cargo_feature(),
                "page_visible": cargo_feature(),
            })
        });
    let page_visible = status
        .get("page_visible")
        .and_then(Value::as_bool)
        .unwrap_or(cargo_feature());
    let mut events = Vec::new();
    if let Ok(text) = std::fs::read_to_string(&events_path) {
        for line in text.lines().rev().take(80) {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                events.push(v);
            }
        }
        events.reverse();
    }
    json!({
        "available": true,
        "page_visible": page_visible,
        "status": status,
        "events": events,
        "bakeoff": bakeoff_panel(),
    })
}

pub fn bakeoff_panel() -> Value {
    json!([
        {
            "value": "0.6B 144/144, max |Δp| ≈ 0",
            "caption": "1080 Ti 2026-10-04 21:42Z, same Q8. Esk 0.480 s/row vs Python SemIf 0.480 s/row (llamacpp CPU). Tight 1e-6 gate."
        },
        {
            "value": "bartowski 4B 3/3, max Δp 0.0053",
            "caption": "1080 Ti, same bartowski Q4_K_M, 1e-2 gate. ABI / Qwen3.5 hybrid KV, not a 1e-6 CI pin."
        },
        {
            "value": "Wordkeep 7-row p50 575 ms none / 2176 ms permute",
            "caption": "Fresh-process draft 0.6B on this 1080 Ti. Same GGUF hash. Not heuristic µs."
        },
        {
            "value": "SemIf published BF16 ~0.813",
            "caption": "Different checkpoint; not a 1080 Ti latency table and not “Rust beat SemIf”"
        }
    ])
}
