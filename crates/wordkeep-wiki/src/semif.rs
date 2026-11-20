//! Ereshkigal / SemIf dashboard payload (status + events from MCP cache).

use crate::indexer::global_cache_dir;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

pub(crate) fn build() -> Result<Value, String> {
    let dir = global_cache_dir().join("wordkeep");
    let status_path = dir.join("semif-status.json");
    let events_path = dir.join("semif-events.jsonl");
    let status = std::fs::read_to_string(&status_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .unwrap_or_else(|| json!({ "cargo_feature": false, "page_visible": true }));
    let page_visible = true;
    let mut events = Vec::new();
    if let Ok(text) = std::fs::read_to_string(&events_path) {
        for line in text.lines().rev().take(200) {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                events.push(v);
            }
        }
        events.reverse();
    }
    let feedback = load_feedback_rows(&dir);
    let metrics = derive_metrics(&events, &feedback);
    Ok(json!({
        "available": true,
        "page_visible": page_visible,
        "status": status,
        "events": events.iter().rev().take(80).cloned().collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>(),
        "metrics": metrics,
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

fn load_feedback_rows(cache_dir: &Path) -> Vec<Value> {
    // Prefer workspace feedback journal next to cache; also accept cache-local copy.
    let candidates = [
        cache_dir.join("ereshkigal-feedback.jsonl"),
        cache_dir
            .parent()
            .map(|p| p.join("ereshkigal-feedback.jsonl"))
            .unwrap_or_else(|| cache_dir.join("ereshkigal-feedback.jsonl")),
    ];
    let mut out = Vec::new();
    for path in candidates {
        if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines() {
                if let Ok(v) = serde_json::from_str::<Value>(line) {
                    out.push(v);
                }
            }
            if !out.is_empty() {
                break;
            }
        }
    }
    out
}

fn derive_metrics(events: &[Value], feedback: &[Value]) -> Value {
    let mut timings_us: Vec<u64> = Vec::new();
    let mut backends: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut copper_blips = 0u64;
    let mut kv_slot = 0u64;
    let mut empty_chosen = 0u64;
    let mut decide_n = 0u64;
    let mut decree_ok = 0u64;
    let mut decree_empty = 0u64;
    let mut calibrated = 0u64;
    let mut fallback = 0u64;
    let mut think_invoked_n = 0u64;
    let mut think_flip_n = 0u64;
    let mut decide_by_sha: HashMap<String, Value> = HashMap::new();
    for ev in events {
        let kind = ev.get("kind").and_then(|k| k.as_str()).unwrap_or("");
        if kind == "copper_blip" {
            copper_blips += 1;
        }
        if kind == "decide" {
            decide_n += 1;
            if let Some(us) = ev.get("timing_us").and_then(|t| t.as_u64()) {
                timings_us.push(us);
            }
            let scorer = ev
                .get("scorer")
                .and_then(|s| s.as_str())
                .unwrap_or("unknown");
            let entry = backends.entry(scorer.to_string()).or_insert(json!(0));
            if let Some(n) = entry.as_u64() {
                *entry = json!(n + 1);
            }
            if ev.get("calibrated").and_then(|c| c.as_bool()) == Some(true) {
                calibrated += 1;
            }
            if ev.get("fallback").and_then(|c| c.as_bool()) == Some(true) {
                fallback += 1;
            }
            if ev.get("think_invoked").and_then(|c| c.as_bool()) == Some(true) {
                think_invoked_n += 1;
            }
            if ev.get("p0_vs_p1_flipped").and_then(|c| c.as_bool()) == Some(true) {
                think_flip_n += 1;
            }
            let chosen = ev.get("chosen").and_then(|c| c.as_str()).unwrap_or("");
            if chosen.is_empty() && ev.get("abstain").and_then(|a| a.as_bool()) != Some(true) {
                empty_chosen += 1;
            }
            let cascade = ev
                .get("cascade_source")
                .and_then(|c| c.as_str())
                .unwrap_or("");
            if cascade.contains("NoKvCacheSlot") || cascade.contains("KV") {
                kv_slot += 1;
            }
            if let Some(sha) = ev.get("prompt_sha256").and_then(|s| s.as_str()) {
                if !sha.is_empty() {
                    decide_by_sha.insert(sha.to_string(), ev.clone());
                }
            }
        }
        if kind == "decree" {
            match ev.get("outcome").and_then(|o| o.as_str()).unwrap_or("") {
                "ok" => decree_ok += 1,
                "empty" => decree_empty += 1,
                _ => {}
            }
        }
    }
    timings_us.sort_unstable();
    let p50 = percentile_us(&timings_us, 0.50);
    let p95 = percentile_us(&timings_us, 0.95);
    let idle_ratio = if decide_n == 0 {
        1.0
    } else {
        (fallback as f64) / (decide_n as f64)
    };
    let feedback_n = feedback.len() as u64;
    let (ece, brier, matched) = calibration_from_feedback(&decide_by_sha, feedback);
    json!({
        "decide_n": decide_n,
        "wall_p50_ms": p50.map(|u| (u as f64) / 1000.0),
        "wall_p95_ms": p95.map(|u| (u as f64) / 1000.0),
        "backends": backends,
        "copper_blips": copper_blips,
        "nokv_cache_slot": kv_slot,
        "empty_chosen": empty_chosen,
        "calibrated_n": calibrated,
        "fallback_n": fallback,
        "decree_ok": decree_ok,
        "decree_empty": decree_empty,
        "ereshkigal_idle_ratio": idle_ratio,
        "think_invoked_n": think_invoked_n,
        "think_flip_n": think_flip_n,
        "feedback_n": feedback_n,
        "calibration_matched_n": matched,
        "calibration_ece": ece,
        "calibration_brier": brier,
    })
}

/// Join decide events (with `probabilities`) to feedback outcomes by `prompt_sha256`.
/// Returns `(ece, brier, matched_n)` — Null ECE/Brier when zero matched labeled rows.
fn calibration_from_feedback(
    decide_by_sha: &HashMap<String, Value>,
    feedback: &[Value],
) -> (Value, Value, u64) {
    let mut confs: Vec<f64> = Vec::new();
    let mut corrects: Vec<bool> = Vec::new();
    let mut brier_sum = 0.0;
    let mut matched = 0u64;
    for fb in feedback {
        let sha = fb
            .get("prompt_sha256")
            .and_then(|s| s.as_str())
            .unwrap_or("");
        if sha.is_empty() {
            continue;
        }
        let Some(ev) = decide_by_sha.get(sha) else {
            continue;
        };
        let gold = fb
            .get("override_id")
            .and_then(|s| s.as_str())
            .or_else(|| fb.get("chosen").and_then(|s| s.as_str()))
            .unwrap_or("");
        if gold.is_empty() {
            continue;
        }
        let Some(probs) = ev.get("probabilities").and_then(|p| p.as_array()) else {
            continue;
        };
        if probs.is_empty() {
            continue;
        }
        let mut gold_p = 0.0;
        let mut max_p = 0.0;
        let mut max_id = "";
        for item in probs {
            let id = item.get("id").and_then(|x| x.as_str()).unwrap_or("");
            let p = item
                .get("probability")
                .and_then(|x| x.as_f64())
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            if p > max_p {
                max_p = p;
                max_id = id;
            }
            if id == gold {
                gold_p = p;
            }
        }
        matched += 1;
        confs.push(max_p);
        corrects.push(max_id == gold);
        // Brier on the gold one-hot vs predicted mass on gold.
        brier_sum += (gold_p - 1.0).powi(2);
        for item in probs {
            let id = item.get("id").and_then(|x| x.as_str()).unwrap_or("");
            if id == gold {
                continue;
            }
            let p = item
                .get("probability")
                .and_then(|x| x.as_f64())
                .unwrap_or(0.0);
            brier_sum += p.powi(2);
        }
    }
    if matched == 0 {
        return (Value::Null, Value::Null, 0);
    }
    let ece = equal_width_ece(&confs, &corrects, 10);
    let brier = brier_sum / matched as f64;
    (json!(ece), json!(brier), matched)
}

fn equal_width_ece(confs: &[f64], corrects: &[bool], bins: usize) -> f64 {
    let bins = bins.max(1);
    let mut bin_correct = vec![0.0; bins];
    let mut bin_conf = vec![0.0; bins];
    let mut bin_count = vec![0.0; bins];
    for (c, ok) in confs.iter().zip(corrects.iter()) {
        let idx = ((c * bins as f64).floor() as usize).min(bins - 1);
        bin_count[idx] += 1.0;
        bin_conf[idx] += c;
        if *ok {
            bin_correct[idx] += 1.0;
        }
    }
    let n = confs.len() as f64;
    let mut ece = 0.0;
    for i in 0..bins {
        if bin_count[i] <= 0.0 {
            continue;
        }
        let acc = bin_correct[i] / bin_count[i];
        let conf = bin_conf[i] / bin_count[i];
        ece += (bin_count[i] / n) * (acc - conf).abs();
    }
    ece
}

fn percentile_us(sorted: &[u64], q: f64) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let idx = ((sorted.len() as f64 - 1.0) * q).round() as usize;
    Some(sorted[idx.min(sorted.len() - 1)])
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
        assert!(v.get("metrics").is_some());
    }

    #[test]
    fn metrics_from_events_compute_p50() {
        let events = vec![
            json!({"kind":"decide","timing_us":1000,"scorer":"heuristic","chosen":"a","fallback":false}),
            json!({"kind":"decide","timing_us":3000,"scorer":"remote-myridos","chosen":"b","fallback":false,"calibrated":true,"think_invoked":true}),
            json!({"kind":"copper_blip"}),
            json!({"kind":"decree","outcome":"ok"}),
        ];
        let m = derive_metrics(&events, &[]);
        assert_eq!(m["decide_n"], 2);
        assert_eq!(m["copper_blips"], 1);
        assert_eq!(m["decree_ok"], 1);
        assert_eq!(m["think_invoked_n"], 1);
        assert!(m["wall_p50_ms"].as_f64().unwrap() >= 1.0);
        assert!(m["calibration_ece"].is_null());
        assert!(m["calibration_brier"].is_null());
    }

    /// Non-decorative: matched feedback must yield non-null ECE/Brier (not Null placeholders).
    #[test]
    fn metrics_ece_brier_from_feedback_not_null() {
        let events = vec![json!({
            "kind":"decide",
            "timing_us":1000,
            "scorer":"heuristic",
            "chosen":"promote",
            "prompt_sha256":"abc123",
            "probabilities":[
                {"id":"promote","probability":0.8},
                {"id":"hold","probability":0.2}
            ]
        })];
        let feedback = vec![json!({
            "prompt_sha256":"abc123",
            "chosen":"promote",
            "outcome":"accept"
        })];
        let m = derive_metrics(&events, &feedback);
        assert_eq!(m["calibration_matched_n"], 1);
        assert!(m["calibration_ece"].as_f64().is_some(), "{m}");
        assert!(m["calibration_brier"].as_f64().is_some(), "{m}");
        // Perfect argmax on gold → Brier of (0.8-1)^2 + 0.2^2 = 0.08
        let b = m["calibration_brier"].as_f64().unwrap();
        assert!((b - 0.08).abs() < 1e-9, "brier={b}");
    }
}
