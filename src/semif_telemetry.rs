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
    let hist = cascade_histogram_from_events();
    let mut doc = json!({
        "cargo_feature": cargo_feature(),
        "page_visible": cargo_feature(),
        "backend": crate::config::semif_backend(root),
        "mode": crate::config::semif_mode(root),
        "debias": crate::config::semif_debias(root),
        "tandem": crate::config::semif_tandem(root),
        "adaptive_qhat": crate::config::semif_adaptive_qhat(root),
        "adaptive_margin_min": crate::config::semif_adaptive_margin_min(root),
        "n_gpu_layers": crate::config::semif_n_gpu_layers(root),
        "cascade_routing": crate::config::semif_cascade_routing(root),
        "knowledge_cascade": crate::config::semif_knowledge_cascade(root),
        "cascade_histogram": hist,
        "updated": now_secs(),
        "gguf": crate::config::semif_gguf_path(root).map(|p| p.display().to_string()),
        "gguf_verify": crate::config::semif_gguf_verify_path(root).map(|p| p.display().to_string()),
        "decrees": crate::config::semif_decrees_path(root).map(|p| p.display().to_string()),
        "serve_bin": crate::config::semif_serve_bin(root).map(|p| p.display().to_string()),
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

fn cascade_histogram_from_events() -> Value {
    let path = cache::dir().join(EVENTS);
    let mut hist = serde_json::Map::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return Value::Object(hist);
    };
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("kind").and_then(|k| k.as_str()) != Some("decide") {
            continue;
        }
        let key = v
            .get("cascade_source")
            .and_then(|c| c.as_str())
            .unwrap_or("none");
        let entry = hist.entry(key.to_string()).or_insert(json!(0));
        if let Some(n) = entry.as_u64() {
            *entry = json!(n + 1);
        }
    }
    Value::Object(hist)
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
