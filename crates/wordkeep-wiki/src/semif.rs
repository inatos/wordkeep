//! Ereshkigal / SemIf dashboard payload (status + events from MCP cache).

use crate::indexer::global_cache_dir;
use serde_json::{json, Value};

pub(crate) fn build() -> Result<Value, String> {
    let dir = global_cache_dir().join("wordkeep");
    let status_path = dir.join("semif-status.json");
    let events_path = dir.join("semif-events.jsonl");
    let status = std::fs::read_to_string(&status_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .unwrap_or_else(|| json!({ "cargo_feature": false, "page_visible": true }));
    // Wiki always ships the Dashboard page. MCP `cargo_feature` / GGUF load is
    // telemetry on the board, not a reason to hide the tab.
    let page_visible = true;
    let mut events = Vec::new();
    if let Ok(text) = std::fs::read_to_string(&events_path) {
        for line in text.lines().rev().take(80) {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                events.push(v);
            }
        }
        events.reverse();
    }
    Ok(json!({
        "available": true,
        "page_visible": page_visible,
        "status": status,
        "events": events,
        "bakeoff": [
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
        ]
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bakeoff_has_honest_captions() {
        let v = build().unwrap();
        let rows = v["bakeoff"].as_array().unwrap();
        assert_eq!(rows.len(), 4);
        assert!(rows[3]["caption"].as_str().unwrap().contains("Different checkpoint"));
    }
}
