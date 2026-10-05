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
                "value": "CPU 0.6B 144/144, max |Δp| ≈ 0 @ 1e-6",
                "caption": "1080 Ti CPU ggml vs Python SemIf (2026-10-05). Bit-exact gate. ~0.49 s/row."
            },
            {
                "value": "Vulkan 0.6B 144/144 argmax+SHA, ~0.089 s/row",
                "caption": "1080 Ti Vulkan n_gpu_layers=99. Max |Δp|~0.25 vs Python — smoke gate only, never 1e-6."
            },
            {
                "value": "Vulkan bartowski 4B 3/3 @ 1e-2, ~0.51 s/row",
                "caption": "Same-host Vulkan verify path. ~6× vs CPU Python. Not a BF16 quality claim."
            },
            {
                "value": "Wordkeep permute p50 ~226 ms (Vulkan)",
                "caption": "Fresh-process draft 0.6B on this 1080 Ti. Adaptive skips cycles when sharp. Not heuristic µs."
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
        assert_eq!(rows.len(), 5);
        assert!(rows[1]["caption"].as_str().unwrap().contains("smoke gate"));
        assert!(rows[4]["caption"]
            .as_str()
            .unwrap()
            .contains("Different checkpoint"));
    }
}
