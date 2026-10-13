//! Config-declared telemetry sidecar providers for the wiki dashboard.
//!
//! Reads opaque JSON sidecars listed under `telemetry_providers` in
//! `.wordkeep/config.json` and projects card values via JSON Pointers.
//! Producers (out-of-tree) own the sidecar schema; this module never
//! interprets fabric-specific fields.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
struct CardSpec {
    kind: String,
    title: String,
    value: Option<String>,
    detail: Option<String>,
    series: Option<String>,
}

#[derive(Debug, Clone)]
struct ProviderSpec {
    name: String,
    sidecar: PathBuf,
    refresh_secs: u64,
    stale_secs: u64,
    cards: Vec<CardSpec>,
}

/// Load provider payloads for `/api/dashboard`. Always returns an array (possibly empty).
pub(crate) fn build_providers(root: &Path) -> Value {
    match load_specs(root) {
        Ok(specs) => {
            let providers: Vec<Value> = specs.iter().map(|s| resolve_provider(root, s)).collect();
            Value::Array(providers)
        }
        Err(err) => json!([{
            "name": "config",
            "available": false,
            "reason": err,
            "cards": []
        }]),
    }
}

fn load_specs(root: &Path) -> Result<Vec<ProviderSpec>, String> {
    let path = root.join(".wordkeep/config.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes =
        std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let cfg: Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("parse {}: {e}", path.display()))?;
    let Some(arr) = cfg.get("telemetry_providers").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for (i, entry) in arr.iter().enumerate() {
        let name = entry
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("telemetry_providers[{i}].name required"))?
            .to_string();
        let sidecar_rel = entry
            .get("sidecar")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("telemetry_providers[{i}].sidecar required"))?;
        let sidecar_rel = validate_rel(sidecar_rel)
            .map_err(|e| format!("telemetry_providers[{i}].sidecar: {e}"))?;
        let refresh_secs = entry
            .get("refresh_secs")
            .and_then(Value::as_u64)
            .unwrap_or(30)
            .max(1);
        let stale_secs = entry
            .get("stale_secs")
            .and_then(Value::as_u64)
            .unwrap_or(refresh_secs.saturating_mul(3))
            .max(refresh_secs);
        let cards_arr = entry
            .get("cards")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut cards = Vec::with_capacity(cards_arr.len());
        for (j, card) in cards_arr.iter().enumerate() {
            let kind = card
                .get("kind")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("telemetry_providers[{i}].cards[{j}].kind required"))?
                .to_ascii_lowercase();
            if !matches!(
                kind.as_str(),
                "metric" | "sparkline" | "donut" | "bar" | "activity"
            ) {
                return Err(format!(
                    "telemetry_providers[{i}].cards[{j}].kind unknown: {kind}"
                ));
            }
            let title = card
                .get("title")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("telemetry_providers[{i}].cards[{j}].title required"))?
                .to_string();
            let value = opt_ptr(card, "value");
            let detail = opt_ptr(card, "detail");
            let series = opt_ptr(card, "series");
            if kind == "metric" && value.is_none() {
                return Err(format!(
                    "telemetry_providers[{i}].cards[{j}] metric requires value"
                ));
            }
            if kind != "metric" && series.is_none() {
                return Err(format!(
                    "telemetry_providers[{i}].cards[{j}] {kind} requires series"
                ));
            }
            cards.push(CardSpec {
                kind,
                title,
                value,
                detail,
                series,
            });
        }
        out.push(ProviderSpec {
            name,
            sidecar: root.join(&sidecar_rel),
            refresh_secs,
            stale_secs,
            cards,
        });
    }
    Ok(out)
}

fn opt_ptr(card: &Value, key: &str) -> Option<String> {
    card.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn validate_rel(rel: &str) -> Result<String, String> {
    let norm = rel.trim().replace('\\', "/");
    if norm.is_empty() {
        return Err("path must not be empty".into());
    }
    if Path::new(&norm).is_absolute() || norm.starts_with('/') {
        return Err(format!("path must be relative: {norm}"));
    }
    if norm.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && norm.as_bytes().get(1) == Some(&b':')
    {
        return Err(format!("path must be relative: {norm}"));
    }
    if norm.split('/').any(|c| c == "..") {
        return Err("path must not contain ..".into());
    }
    Ok(norm)
}

fn resolve_provider(root: &Path, spec: &ProviderSpec) -> Value {
    let _ = root;
    let path = &spec.sidecar;
    if !path.exists() {
        return json!({
            "name": spec.name,
            "available": false,
            "reason": "unavailable",
            "sidecar": path.display().to_string(),
            "refresh_secs": spec.refresh_secs,
            "stale_secs": spec.stale_secs,
            "cards": []
        });
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            return json!({
                "name": spec.name,
                "available": false,
                "reason": format!("read error: {e}"),
                "sidecar": path.display().to_string(),
                "cards": []
            });
        }
    };
    let doc: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            return json!({
                "name": spec.name,
                "available": false,
                "reason": format!("parse error: {e}"),
                "sidecar": path.display().to_string(),
                "cards": []
            });
        }
    };

    let now = now_secs();
    let age_secs = sidecar_age_secs(path, &doc, now);
    let stale = age_secs > spec.stale_secs;
    let cards: Vec<Value> = spec
        .cards
        .iter()
        .map(|card| project_card(card, &doc))
        .collect();

    json!({
        "name": spec.name,
        "available": !stale,
        "reason": if stale { Value::String("stale".into()) } else { Value::Null },
        "sidecar": path.display().to_string(),
        "age_secs": age_secs,
        "refresh_secs": spec.refresh_secs,
        "stale_secs": spec.stale_secs,
        "ts": doc.get("ts").cloned().unwrap_or(Value::Null),
        "cards": cards
    })
}

fn sidecar_age_secs(path: &Path, doc: &Value, now: u64) -> u64 {
    if let Some(ts) = doc.get("ts").and_then(Value::as_u64) {
        return now.saturating_sub(ts);
    }
    if let Some(ts) = doc.get("ts").and_then(Value::as_f64) {
        return now.saturating_sub(ts as u64);
    }
    // ISO-8601 fallback: use mtime.
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| now.saturating_sub(d.as_secs()))
        .unwrap_or(u64::MAX)
}

fn project_card(card: &CardSpec, doc: &Value) -> Value {
    match card.kind.as_str() {
        "metric" => {
            let value = card
                .value
                .as_deref()
                .and_then(|p| json_pointer(doc, p))
                .map(display_scalar)
                .unwrap_or_else(|| "—".into());
            let detail = card
                .detail
                .as_deref()
                .and_then(|p| json_pointer(doc, p))
                .map(display_scalar);
            json!({
                "kind": "metric",
                "title": card.title,
                "value": value,
                "detail": detail
            })
        }
        "sparkline" => {
            let series = card
                .series
                .as_deref()
                .and_then(|p| json_pointer(doc, p))
                .map(normalize_spark)
                .unwrap_or_else(|| json!([]));
            json!({
                "kind": "sparkline",
                "title": card.title,
                "series": series
            })
        }
        "donut" | "bar" => {
            let series = card
                .series
                .as_deref()
                .and_then(|p| json_pointer(doc, p))
                .map(normalize_labeled)
                .unwrap_or_else(|| json!([]));
            json!({
                "kind": card.kind,
                "title": card.title,
                "series": series
            })
        }
        "activity" => {
            let series = card
                .series
                .as_deref()
                .and_then(|p| json_pointer(doc, p))
                .cloned()
                .unwrap_or_else(|| json!([]));
            json!({
                "kind": "activity",
                "title": card.title,
                "series": series
            })
        }
        other => json!({
            "kind": other,
            "title": card.title,
            "value": "—",
            "detail": "unknown kind"
        }),
    }
}

/// Minimal JSON Pointer (`/a/b/0`) — no `~` escaping required for our sidecars.
pub(crate) fn json_pointer<'a>(doc: &'a Value, pointer: &str) -> Option<&'a Value> {
    if pointer.is_empty() || pointer == "/" {
        return Some(doc);
    }
    let trimmed = pointer.strip_prefix('/').unwrap_or(pointer);
    let mut cur = doc;
    for part in trimmed.split('/') {
        if part.is_empty() {
            continue;
        }
        cur = match cur {
            Value::Object(map) => map.get(part)?,
            Value::Array(arr) => {
                let idx: usize = part.parse().ok()?;
                arr.get(idx)?
            }
            _ => return None,
        };
    }
    Some(cur)
}

fn display_scalar(v: &Value) -> String {
    match v {
        Value::Null => "—".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn normalize_spark(v: &Value) -> Value {
    let Some(arr) = v.as_array() else {
        return json!([]);
    };
    let mut out = Vec::new();
    for (i, item) in arr.iter().enumerate() {
        if let Some(n) = item.as_f64() {
            out.push(json!({"ts": i as u64, "value": n}));
            continue;
        }
        let ts = item
            .get("ts")
            .and_then(|t| t.as_u64().or_else(|| t.as_f64().map(|f| f as u64)))
            .unwrap_or(i as u64);
        let value = item
            .get("value")
            .and_then(Value::as_f64)
            .or_else(|| item.get("rtt_ms").and_then(Value::as_f64))
            .unwrap_or(0.0);
        let label = item.get("label").and_then(Value::as_str);
        out.push(json!({"ts": ts, "value": value, "label": label}));
    }
    Value::Array(out)
}

fn normalize_labeled(v: &Value) -> Value {
    let Some(arr) = v.as_array() else {
        return json!([]);
    };
    let mut out = Vec::new();
    for item in arr {
        let label = item
            .get("label")
            .or_else(|| item.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        let value = item
            .get("value")
            .or_else(|| item.get("count"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        out.push(json!({"label": label, "value": value}));
    }
    Value::Array(out)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wk_wiki_prov_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".wordkeep")).unwrap();
        fs::create_dir_all(dir.join(".cache/palaestra")).unwrap();
        dir
    }

    #[test]
    fn pointer_and_missing_sidecar() {
        let doc = json!({"presence": "up", "role": "desktop", "nested": {"a": 1}});
        assert_eq!(
            json_pointer(&doc, "/presence").and_then(Value::as_str),
            Some("up")
        );
        assert_eq!(
            json_pointer(&doc, "/nested/a").and_then(Value::as_u64),
            Some(1)
        );

        let dir = tmp("missing");
        fs::write(
            dir.join(".wordkeep/config.json"),
            r#"{
              "telemetry_providers": [{
                "name": "Fabric",
                "sidecar": ".cache/palaestra/telemetry_provider.json",
                "cards": [{"kind":"metric","title":"Peer","value":"/presence"}]
              }]
            }"#,
        )
        .unwrap();
        let providers = build_providers(&dir);
        let arr = providers.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["available"], false);
        assert_eq!(arr[0]["reason"], "unavailable");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fresh_sidecar_projects_cards() {
        let dir = tmp("fresh");
        let now = now_secs();
        fs::write(
            dir.join(".wordkeep/config.json"),
            r#"{
              "telemetry_providers": [{
                "name": "Fabric",
                "sidecar": ".cache/palaestra/telemetry_provider.json",
                "stale_secs": 120,
                "cards": [
                  {"kind":"metric","title":"Peer","value":"/presence","detail":"/role"},
                  {"kind":"sparkline","title":"RTT","series":"/rtt_ms_history"},
                  {"kind":"donut","title":"Backends","series":"/semif_backends"}
                ]
              }]
            }"#,
        )
        .unwrap();
        fs::write(
            dir.join(".cache/palaestra/telemetry_provider.json"),
            format!(
                r#"{{
                  "ts": {now},
                  "presence": "up",
                  "role": "desktop",
                  "rtt_ms_history": [{{"ts": {now}, "value": 1.5}}],
                  "semif_backends": [{{"label": "local", "value": 3}}]
                }}"#
            ),
        )
        .unwrap();
        let providers = build_providers(&dir);
        let p = &providers.as_array().unwrap()[0];
        assert_eq!(p["available"], true);
        let cards = p["cards"].as_array().unwrap();
        assert_eq!(cards[0]["value"], "up");
        assert_eq!(cards[0]["detail"], "desktop");
        assert_eq!(cards[1]["series"].as_array().unwrap().len(), 1);
        assert_eq!(cards[2]["series"][0]["label"], "local");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_sidecar_marked_unavailable() {
        let dir = tmp("stale");
        fs::write(
            dir.join(".wordkeep/config.json"),
            r#"{
              "telemetry_providers": [{
                "name": "Fabric",
                "sidecar": ".cache/palaestra/telemetry_provider.json",
                "stale_secs": 5,
                "cards": [{"kind":"metric","title":"Peer","value":"/presence"}]
              }]
            }"#,
        )
        .unwrap();
        fs::write(
            dir.join(".cache/palaestra/telemetry_provider.json"),
            r#"{"ts": 1, "presence": "up"}"#,
        )
        .unwrap();
        let providers = build_providers(&dir);
        let p = &providers.as_array().unwrap()[0];
        assert_eq!(p["available"], false);
        assert_eq!(p["reason"], "stale");
        // Cards still projected so the UI can show last-known values.
        assert_eq!(p["cards"][0]["value"], "up");
        let _ = fs::remove_dir_all(&dir);
    }
}
