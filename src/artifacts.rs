//! Artifact metadata index over configured ignored artifact roots.
//!
//! Indexes path/mtime/size/type, shallow PNG dimensions, and shallow JSON
//! keys. Does **not** grade image quality. Never follows symlinks outside the
//! workspace.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::{config, stats, workspace};

const STORE_VERSION: u64 = 1;
const CACHE_FILE: &str = "artifacts.json";
const MAX_SCAN: usize = 5000;

#[derive(Clone, Debug)]
struct Artifact {
    path: String,
    size: u64,
    mtime_ns: u64,
    kind: String,
    width: Option<u32>,
    height: Option<u32>,
    json_keys: Vec<String>,
}

fn artifact_to_value(a: &Artifact) -> Value {
    json!({
        "path": a.path,
        "size": a.size,
        "mtime_ns": a.mtime_ns,
        "kind": a.kind,
        "width": a.width,
        "height": a.height,
        "json_keys": a.json_keys,
    })
}

fn artifact_from_value(v: &Value) -> Option<Artifact> {
    Some(Artifact {
        path: v.get("path").and_then(Value::as_str)?.to_string(),
        size: v.get("size").and_then(Value::as_u64).unwrap_or(0),
        mtime_ns: v.get("mtime_ns").and_then(Value::as_u64).unwrap_or(0),
        kind: v
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("file")
            .to_string(),
        width: v.get("width").and_then(Value::as_u64).map(|n| n as u32),
        height: v.get("height").and_then(Value::as_u64).map(|n| n as u32),
        json_keys: v
            .get("json_keys")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn classify(path: &Path) -> String {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "png".into(),
        "jpg" | "jpeg" => "jpeg".into(),
        "webp" => "webp".into(),
        "gif" => "gif".into(),
        "json" => "json".into(),
        "log" | "txt" => "text".into(),
        "blend" | "blend1" => "blend".into(),
        "glb" | "gltf" => "mesh".into(),
        "bkkbp" => "bundle".into(),
        _ => "file".into(),
    }
}

/// Read PNG IHDR width/height without external deps.
fn png_dimensions(path: &Path) -> Option<(u32, u32)> {
    let mut f = File::open(path).ok()?;
    let mut buf = [0u8; 24];
    f.read_exact(&mut buf).ok()?;
    if &buf[0..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    if &buf[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes([buf[16], buf[17], buf[18], buf[19]]);
    let h = u32::from_be_bytes([buf[20], buf[21], buf[22], buf[23]]);
    Some((w, h))
}

fn shallow_json_keys(path: &Path) -> Vec<String> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    // Cap read size for metadata
    if bytes.len() > 256 * 1024 {
        return Vec::new();
    }
    let Ok(v) = serde_json::from_slice::<Value>(&bytes) else {
        return Vec::new();
    };
    match v {
        Value::Object(map) => map.keys().take(32).cloned().collect(),
        Value::Array(arr) => {
            if let Some(Value::Object(map)) = arr.first() {
                map.keys().take(32).cloned().collect()
            } else {
                vec!["[array]".into()]
            }
        }
        _ => Vec::new(),
    }
}

fn is_under_workspace(root: &Path, path: &Path) -> bool {
    let Ok(canon_root) = root.canonicalize() else {
        return path.starts_with(root);
    };
    match path.canonicalize() {
        Ok(c) => c.starts_with(&canon_root),
        Err(_) => path.starts_with(root),
    }
}

fn walk_artifacts(root: &Path, bases: &[String]) -> Vec<Artifact> {
    let mut out = Vec::new();
    for base in bases {
        let start = root.join(base);
        if !start.exists() {
            continue;
        }
        // Do not follow the start if it is a symlink escaping the workspace.
        if start
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
            && !is_under_workspace(root, &start)
        {
            continue;
        }
        let mut stack = vec![start];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in rd.flatten() {
                if out.len() >= MAX_SCAN {
                    return out;
                }
                let path = entry.path();
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                // Never follow symlinks.
                if meta.file_type().is_symlink() {
                    continue;
                }
                if meta.is_dir() {
                    if is_under_workspace(root, &path) {
                        stack.push(path);
                    }
                    continue;
                }
                if !meta.is_file() {
                    continue;
                }
                if !is_under_workspace(root, &path) {
                    continue;
                }
                let rel = config::rel_path(root, &path);
                let kind = classify(&path);
                let (width, height) = if kind == "png" {
                    png_dimensions(&path)
                        .map(|(w, h)| (Some(w), Some(h)))
                        .unwrap_or((None, None))
                } else {
                    (None, None)
                };
                let json_keys = if kind == "json" {
                    shallow_json_keys(&path)
                } else {
                    Vec::new()
                };
                out.push(Artifact {
                    path: rel,
                    size: meta.len(),
                    mtime_ns: crate::cache::mtime_ns(&meta),
                    kind,
                    width,
                    height,
                    json_keys,
                });
            }
        }
    }
    out
}

fn cache_path(root: &Path) -> PathBuf {
    workspace::workspace_file(root, CACHE_FILE)
}

fn load_cache(root: &Path) -> Option<Vec<Artifact>> {
    let v = workspace::read_json(&cache_path(root)).ok()??;
    let arr = v.get("artifacts").and_then(Value::as_array)?;
    Some(arr.iter().filter_map(artifact_from_value).collect())
}

fn save_cache(root: &Path, artifacts: &[Artifact]) -> Result<(), String> {
    let doc = json!({
        "version": STORE_VERSION,
        "workspace_id": workspace::workspace_id(root),
        "updated": workspace::now_secs(),
        "artifacts": artifacts.iter().map(artifact_to_value).collect::<Vec<_>>(),
    });
    workspace::ensure_workspace_dir(root)?;
    workspace::write_atomic_json(&cache_path(root), &doc)
}

/// Scan / query artifact metadata.
pub fn index(root: &Path, args: &Value) -> Result<String, String> {
    let refresh = args
        .get("refresh")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let kind_filter = args
        .get("kind")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase());
    let newest = args.get("newest").and_then(Value::as_bool).unwrap_or(false);
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(1200) as usize;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(40)
        .max(1) as usize;

    let roots = if let Some(arr) = args.get("roots").and_then(Value::as_array) {
        let raw: Vec<String> = arr
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect();
        config::normalize_search_paths(&raw)?
    } else {
        config::artifact_roots(root)
    };
    if roots.is_empty() {
        return Ok(
            "artifact_index: no artifact_roots configured in .wordkeep/config.json \
             (and no roots argument). Evidence roots only — images are not graded."
                .into(),
        );
    }

    let artifacts = if refresh {
        let scanned = walk_artifacts(root, &roots);
        save_cache(root, &scanned)?;
        scanned
    } else if let Some(cached) = load_cache(root) {
        cached
    } else {
        let scanned = walk_artifacts(root, &roots);
        let _ = save_cache(root, &scanned);
        scanned
    };

    let mut filtered: Vec<&Artifact> = artifacts.iter().collect();
    if let Some(k) = &kind_filter {
        filtered.retain(|a| a.kind == *k);
    }
    if !query.is_empty() {
        filtered.retain(|a| {
            a.path.to_ascii_lowercase().contains(&query)
                || a.json_keys
                    .iter()
                    .any(|k| k.to_ascii_lowercase().contains(&query))
        });
    }
    if newest {
        filtered.sort_by_key(|a| std::cmp::Reverse(a.mtime_ns));
    } else {
        filtered.sort_by_key(|a| a.path.clone());
    }
    filtered.truncate(limit);

    let mut kind_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for a in &artifacts {
        *kind_counts.entry(a.kind.as_str()).or_default() += 1;
    }

    let mut out = format!(
        "artifact_index - {} indexed, showing {}, roots {:?}\n\
         (metadata only; does not grade image quality)\n",
        artifacts.len(),
        filtered.len(),
        roots
    );
    if !kind_counts.is_empty() {
        out.push_str("kinds:");
        for (k, n) in &kind_counts {
            out.push_str(&format!(" {k}={n}"));
        }
        out.push('\n');
    }
    let mut used = out.len() / 4;
    for a in filtered {
        let dims = match (a.width, a.height) {
            (Some(w), Some(h)) => format!(" {w}x{h}"),
            _ => String::new(),
        };
        let keys = if a.json_keys.is_empty() {
            String::new()
        } else {
            format!(
                " keys=[{}]",
                a.json_keys
                    .iter()
                    .take(8)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(",")
            )
        };
        let block = format!("  {}  {}  {}B{}{}\n", a.path, a.kind, a.size, dims, keys);
        let tok = block.len() / 4;
        if used + tok > budget && used > out.len() / 4 {
            out.push_str("… (truncated by token_budget)\n");
            break;
        }
        used += tok;
        out.push_str(&block);
    }
    stats::record(
        "artifact_index",
        artifacts.len() as u64 * 16,
        (out.len() / 4) as u64,
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn indexes_png_and_json() {
        let _guard = crate::cache::test_env_lock();
        let root = std::env::temp_dir().join(format!("wk_art_{}", std::process::id()));
        let cache = std::env::temp_dir().join(format!("wk_art_cache_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&cache);
        fs::create_dir_all(root.join(".wordkeep")).unwrap();
        fs::create_dir_all(root.join("artifacts")).unwrap();
        std::env::set_var("XDG_CACHE_HOME", &cache);
        // Minimal 1x1 PNG
        let png: Vec<u8> = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00,
            0x00, 0x90, 0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08,
            0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x00, 0x03, 0x00, 0x01, 0x00, 0x05, 0xFE,
            0x02, 0xFE, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        fs::write(root.join("artifacts/a.png"), &png).unwrap();
        fs::write(
            root.join("artifacts/meta.json"),
            r#"{"score":1,"name":"t"}"#,
        )
        .unwrap();
        fs::write(
            root.join(".wordkeep/config.json"),
            r#"{"artifact_roots":["artifacts"]}"#,
        )
        .unwrap();
        let out = index(&root, &json!({"refresh": true})).unwrap();
        assert!(out.contains("a.png"));
        assert!(out.contains("1x1"));
        assert!(out.contains("meta.json"));
        assert!(out.contains("score"));
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&cache);
    }
}
