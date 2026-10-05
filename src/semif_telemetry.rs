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
    // Start from any prior status so load extras (cpu_draft_loaded, cold_load_ms,
    // n_gpu_layers_used, warm, …) survive empty resolve refreshes.
    let mut doc = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .filter(|v| v.is_object())
        .unwrap_or_else(|| json!({}));
    let hist = cascade_histogram_from_events();
    #[allow(unused_mut)]
      let mut core = json!({
          "cargo_feature": cargo_feature(),
          "page_visible": cargo_feature(),
          "backend": crate::config::semif_backend(root),
          "mode": crate::config::semif_mode(root),
          "debias": crate::config::semif_debias(root),
          "tandem": crate::config::semif_tandem(root),
          "n_gpu_layers": crate::config::semif_n_gpu_layers(root),
          "n_seq_max": crate::config::semif_n_seq_max(root),
          "adapter_scale": crate::config::semif_adapter_scale(root),
          "temperature": crate::config::semif_temperature(root),
          "cascade_routing": crate::config::semif_cascade_routing(root),
          "knowledge_cascade": crate::config::semif_knowledge_cascade(root),
          "cascade_histogram": hist,
          "updated": now_secs(),
          "gguf": crate::config::semif_gguf_path(root).map(|p| p.display().to_string()),
          "gguf_verify": crate::config::semif_gguf_verify_path(root).map(|p| p.display().to_string()),
          "adapter": crate::config::semif_adapter_path(root).map(|p| p.display().to_string()),
          "decrees": crate::config::semif_decrees_path(root).map(|p| p.display().to_string()),
          "serve_bin": crate::config::semif_serve_bin(root).map(|p| p.display().to_string()),
      });
      #[cfg(any(test, feature = "ereshkigal"))]
      if let Some(obj) = core.as_object_mut() {
          obj.insert(
              "adaptive_qhat".into(),
              json!(crate::config::semif_adaptive_qhat(root)),
          );
          obj.insert(
              "adaptive_margin_min".into(),
              json!(crate::config::semif_adaptive_margin_min(root)),
          );
      }
    if let (Some(dst), Some(src)) = (doc.as_object_mut(), core.as_object()) {
        for (k, v) in src {
            dst.insert(k.clone(), v.clone());
        }
    }
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
    // Cap by rewriting only when over limit; avoid full reparse on every append
    // by checking file line count cheaply via byte scan when small.
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    // Heuristic: ~200 bytes/line → skip read when clearly under cap.
    if meta.len() < (EVENT_CAP as u64) * 80 {
        return;
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn empty_write_status_keeps_load_extras() {
        let cache = std::env::temp_dir().join(format!(
            "wk_semif_status_{}_{}",
            std::process::id(),
            now_secs()
        ));
        let root = std::env::temp_dir().join(format!(
            "wk_semif_root_{}_{}",
            std::process::id(),
            now_secs()
        ));
        let _ = std::fs::remove_dir_all(&cache);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".wordkeep")).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(
            root.join(".wordkeep/config.json"),
            r#"{"semif":{"enabled":true,"backend":"heuristic","debias":"none"}}"#,
        )
        .unwrap();
        let prev = std::env::var_os("XDG_CACHE_HOME");
        std::env::set_var("XDG_CACHE_HOME", &cache);

        write_status(
            &root,
            json!({
                "cpu_draft_loaded": true,
                "cold_load_ms": 1234,
                "n_gpu_layers_used": 99,
                "warm": false,
            }),
        );
        let path: PathBuf = cache::dir().join(STATUS);
        let first: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let updated_first = first["updated"].as_u64();

        write_status(&root, json!({}));
        let second: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

        assert_eq!(second["cpu_draft_loaded"], true, "{second}");
        assert_eq!(second["cold_load_ms"], 1234, "{second}");
        assert_eq!(second["n_gpu_layers_used"], 99, "{second}");
        assert_eq!(second["warm"], false, "{second}");
        assert_eq!(second["backend"], "heuristic", "{second}");
        assert!(second["updated"].as_u64().is_some());
        // Core refresh should still rewrite updated when the clock ticks; either
        // way the key must remain present after an empty merge.
        let _ = updated_first;

        match prev {
            Some(v) => std::env::set_var("XDG_CACHE_HOME", v),
            None => std::env::remove_var("XDG_CACHE_HOME"),
        }
        let _ = std::fs::remove_dir_all(&cache);
        let _ = std::fs::remove_dir_all(&root);
    }
}
