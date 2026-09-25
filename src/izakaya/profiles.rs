//! Declarative Izakaya discovery profiles.
//!
//! Profiles name gates, objectives, and run-metadata adapters. They never execute
//! shell commands. Integrated projects drop JSON files under `.wordkeep/izakaya/profiles/`.

use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

use crate::config;

#[derive(Clone, Debug)]
pub struct Objective {
    pub metric: String,
    pub direction: String,
    pub weight: f64,
}

#[derive(Clone, Debug)]
#[allow(dead_code)] // offline lab: run → metric adapters for record_outcome
pub struct RunAdapter {
    pub tag: String,
    pub metric: String,
    pub from: String,
    pub direction: String,
}

#[derive(Clone, Debug)]
pub struct Profile {
    pub id: String,
    pub version: u64,
    pub gates: Vec<String>,
    pub objectives: Vec<Objective>,
    pub features: Vec<String>,
    pub run_adapters: Vec<RunAdapter>,
}

pub fn builtin_coordination() -> Profile {
    Profile {
        id: "coordination".into(),
        version: 1,
        gates: vec!["correctness".into(), "integration".into()],
        objectives: vec![
            Objective {
                metric: "conflicts".into(),
                direction: "min".into(),
                weight: 1.0,
            },
            Objective {
                metric: "handoff_delay".into(),
                direction: "min".into(),
                weight: 1.0,
            },
            Objective {
                metric: "cost".into(),
                direction: "min".into(),
                weight: 0.5,
            },
            Objective {
                metric: "critical_path".into(),
                direction: "min".into(),
                weight: 1.0,
            },
        ],
        features: vec![
            "overlap".into(),
            "handoff".into(),
            "gate".into(),
            "cost".into(),
            "censoring".into(),
        ],
        run_adapters: Vec::new(),
    }
}

pub fn load(root: &Path, id: &str) -> Result<Profile, String> {
    if let Some(v) = read_profile_file(root, id)? {
        reject_executable(&v)?;
        let mut profile = profile_from_value(&v)?;
        if let Some(parent) = v.get("extends").and_then(Value::as_str) {
            if parent == id {
                return Err(format!("profile {id} cannot extend itself"));
            }
            let base = load(root, parent)?;
            profile = merge(base, profile);
        }
        if profile.id != id {
            return Err(format!(
                "profile id {} does not match requested {id}",
                profile.id
            ));
        }
        Ok(profile)
    } else if id == "coordination" {
        Ok(builtin_coordination())
    } else {
        Err(format!(
            "unknown discovery profile {id:?}; expected coordination or a file under {}",
            config::izakaya_profiles_dir(root)
        ))
    }
}

pub fn list_ids(root: &Path) -> Vec<String> {
    let mut ids = vec!["coordination".to_string()];
    let dir = root.join(config::izakaya_profiles_dir(root));
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Ok(raw) = std::fs::read_to_string(&path) {
                if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                    if reject_executable(&v).is_err() {
                        continue;
                    }
                    if let Some(id) = v.get("id").and_then(Value::as_str) {
                        if !ids.iter().any(|e| e == id) {
                            ids.push(id.to_string());
                        }
                    }
                }
            }
        }
    }
    ids.sort();
    ids
}

#[allow(dead_code)] // offline lab: used by record_outcome
pub fn apply_run_adapters(
    profile: &Profile,
    run: &crate::runs::Run,
    metrics: &mut BTreeMap<String, f64>,
) {
    for adapter in &profile.run_adapters {
        if !run.tags.iter().any(|t| t == &adapter.tag) {
            continue;
        }
        let value = match adapter.from.as_str() {
            "duration_ms" => run.duration_ms.map(|n| n as f64),
            "status_pass" => Some(if run.status == "passed" { 1.0 } else { 0.0 }),
            "summary_number" => first_number(&run.summary),
            _ => None,
        };
        if let Some(v) = value {
            if adapter.direction != "min" && adapter.direction != "max" {
                continue;
            }
            metrics.insert(adapter.metric.clone(), v);
        }
    }
}

fn merge(mut base: Profile, child: Profile) -> Profile {
    base.id = child.id;
    base.version = child.version;
    for gate in child.gates {
        if !base.gates.contains(&gate) {
            base.gates.push(gate);
        }
    }
    if !child.objectives.is_empty() {
        base.objectives = child.objectives;
    }
    for feature in child.features {
        if !base.features.contains(&feature) {
            base.features.push(feature);
        }
    }
    base.run_adapters.extend(child.run_adapters);
    base
}

fn read_profile_file(root: &Path, id: &str) -> Result<Option<Value>, String> {
    let dir = root.join(config::izakaya_profiles_dir(root));
    let direct = dir.join(format!("{id}.json"));
    if direct.is_file() {
        let raw = std::fs::read_to_string(&direct)
            .map_err(|e| format!("read {}: {e}", direct.display()))?;
        return Ok(Some(
            serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", direct.display()))?,
        ));
    }
    if !dir.is_dir() {
        return Ok(None);
    }
    for entry in std::fs::read_dir(&dir).map_err(|e| format!("read {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let raw =
            std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let v: Value =
            serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
        if v.get("id").and_then(Value::as_str) == Some(id) {
            return Ok(Some(v));
        }
    }
    Ok(None)
}

fn profile_from_value(v: &Value) -> Result<Profile, String> {
    let id = v
        .get("id")
        .and_then(Value::as_str)
        .ok_or("profile.id is required")?
        .to_string();
    let version = v.get("version").and_then(Value::as_u64).unwrap_or(1);
    if version != 1 {
        return Err(format!("unsupported profile version {version}"));
    }
    let gates = strings(v, "gates");
    let features = strings(v, "features");
    let objectives = v
        .get("objectives")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|o| {
                    let metric = o.get("metric").and_then(Value::as_str)?.to_string();
                    let direction = o.get("direction").and_then(Value::as_str).unwrap_or("min");
                    if direction != "min" && direction != "max" {
                        return None;
                    }
                    Some(Objective {
                        metric,
                        direction: direction.to_string(),
                        weight: o.get("weight").and_then(Value::as_f64).unwrap_or(1.0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let run_adapters = v
        .get("run_adapters")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|o| {
                    Some(RunAdapter {
                        tag: o.get("tag").and_then(Value::as_str)?.to_string(),
                        metric: o.get("metric").and_then(Value::as_str)?.to_string(),
                        from: o
                            .get("from")
                            .and_then(Value::as_str)
                            .unwrap_or("summary_number")
                            .to_string(),
                        direction: o
                            .get("direction")
                            .and_then(Value::as_str)
                            .unwrap_or("max")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Profile {
        id,
        version,
        gates,
        objectives,
        features,
        run_adapters,
    })
}

fn strings(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub fn reject_executable(v: &Value) -> Result<(), String> {
    match v {
        Value::Object(map) => {
            for key in map.keys() {
                if matches!(key.as_str(), "exec" | "shell" | "command" | "cmd") {
                    return Err(format!(
                        "discovery profiles cannot declare {key}; adapters read recorded run metadata only"
                    ));
                }
            }
            for child in map.values() {
                reject_executable(child)?;
            }
            Ok(())
        }
        Value::Array(items) => {
            for child in items {
                reject_executable(child)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[allow(dead_code)] // offline lab helper for apply_run_adapters
fn first_number(text: &str) -> Option<f64> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit()
            || (bytes[i] == b'-' && bytes.get(i + 1).is_some_and(|c| c.is_ascii_digit()))
        {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            if let Ok(n) = text[start..i].parse::<f64>() {
                return Some(n);
            }
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_shell_profiles_and_loads_extension() {
        let root = std::env::temp_dir().join(format!("wk_iza_prof_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join(".wordkeep/izakaya/profiles");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bad.json"), r#"{"id":"bad","shell":"rm -rf /"}"#).unwrap();
        assert!(load(&root, "bad").is_err());
        std::fs::write(
            dir.join("betwixt-kernel.json"),
            r#"{"id":"betwixt-kernel","version":1,"extends":"coordination","gates":["correctness"],"objectives":[{"metric":"score","direction":"max","weight":1}],"run_adapters":[{"tag":"bench","metric":"score","from":"summary_number","direction":"max"}]}"#,
        )
        .unwrap();
        let p = load(&root, "betwixt-kernel").unwrap();
        assert!(p.gates.iter().any(|g| g == "correctness"));
        assert!(p.gates.iter().any(|g| g == "integration"));
        assert_eq!(p.objectives[0].metric, "score");
        assert_eq!(p.run_adapters[0].tag, "bench");
        let _ = std::fs::remove_dir_all(&root);
    }
}
