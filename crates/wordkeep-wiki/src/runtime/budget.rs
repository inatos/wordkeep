//! Scenario budget evaluation against a Runtime Health snapshot.
//!
//! Limits are JSON pointers (`/rss_bytes`) or `pools.<Name>.in_use`. Missing
//! actuals are **unavailable**, not a silent pass.

use serde_json::{json, Value};

pub fn budget_path(root: &std::path::Path) -> std::path::PathBuf {
    if let Ok(path) = std::env::var("WORDKEEP_RUNTIME_BUDGET") {
        if !path.is_empty() {
            return std::path::PathBuf::from(path);
        }
    }
    root.join(".wordkeep/runtime/budget.json")
}

pub fn load(root: &std::path::Path) -> Option<Value> {
    let path = budget_path(root);
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn evaluate(budget: &Value, snapshot: &Value) -> Value {
    let name = budget
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("unnamed");
    let mut violations = Vec::new();
    let mut checked = 0u64;
    let mut unavailable = 0u64;

    if let Some(limits) = budget.get("limits").and_then(Value::as_object) {
        for (key, limit_value) in limits {
            let Some(limit) = numeric(limit_value) else {
                continue;
            };
            match resolve(snapshot, key) {
                Some(actual) => {
                    checked += 1;
                    if actual > limit {
                        violations.push(json!({
                            "metric": key,
                            "limit": limit,
                            "actual": actual,
                            "delta": actual - limit,
                            "quality": "exact"
                        }));
                    }
                }
                None => {
                    unavailable += 1;
                    violations.push(json!({
                        "metric": key,
                        "limit": limit,
                        "actual": null,
                        "delta": null,
                        "quality": "unavailable"
                    }));
                }
            }
        }
    }

    json!({
        "name": name,
        "ok": violations.iter().all(|v| v.get("quality").and_then(Value::as_str) != Some("exact")),
        "checked": checked,
        "unavailable": unavailable,
        "violations": violations,
        "quality": if unavailable > 0 && checked == 0 { "unavailable" } else { "exact" }
    })
}

fn resolve(snapshot: &Value, key: &str) -> Option<i64> {
    if let Some(rest) = key.strip_prefix("pools.") {
        let (name, field) = rest.split_once('.').unwrap_or((rest, "in_use"));
        let pools = snapshot.get("pools")?.as_array()?;
        for pool in pools {
            if pool.get("name").and_then(Value::as_str) == Some(name) {
                return numeric(pool.get(field)?);
            }
        }
        return None;
    }
    let pointer = if key.starts_with('/') {
        key.to_string()
    } else {
        format!("/{key}")
    };
    snapshot.pointer(&pointer).and_then(numeric)
}

fn numeric(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().map(|n| n.min(i64::MAX as u64) as i64))
        .or_else(|| value.as_f64().map(|n| n as i64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_over_limit_and_missing_as_unavailable() {
        let budget = json!({
            "name": "gym-idle",
            "limits": {
                "/rss_bytes": 100,
                "pools.EntityPool.in_use": 8,
                "/gpu/particles": 4
            }
        });
        let snap = json!({
            "rss_bytes": 250,
            "pools": [{"name": "EntityPool", "in_use": 2, "cap": 16}]
        });
        let result = evaluate(&budget, &snap);
        assert_eq!(result["name"], "gym-idle");
        assert_eq!(result["ok"], false);
        assert_eq!(result["checked"], 2);
        assert_eq!(result["unavailable"], 1);
        let metrics: Vec<_> = result["violations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["metric"].as_str().unwrap())
            .collect();
        assert!(metrics.contains(&"/rss_bytes"));
        assert!(metrics.contains(&"/gpu/particles"));
        assert!(!metrics.contains(&"pools.EntityPool.in_use"));
    }

    #[test]
    fn passes_when_under_limits() {
        let budget = json!({"name": "ok", "limits": {"/rss_bytes": 100}});
        let result = evaluate(&budget, &json!({"rss_bytes": 40}));
        assert_eq!(result["ok"], true);
        assert_eq!(result["violations"].as_array().unwrap().len(), 0);
    }
}
