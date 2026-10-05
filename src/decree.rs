//! Host-agnostic MCP surface for Ereshkigal decrees (`.esk` / TOML libraries).
//!
//! `check` / `lint` run in-process via `ereshkigal-lang`. `run` / `test` / `decide` /
//! `stats` talk to a resident `ereshkigal serve --stdio` child (separate pipes from
//! Wordkeep MCP stdin) when a binary and GGUF are available.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;
use std::time::Instant;

use crate::config;
use crate::semif_telemetry;

static BRIDGE: Mutex<Option<Bridge>> = Mutex::new(None);

struct Bridge {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

fn decrees_dir(root: &Path) -> Result<PathBuf, String> {
    config::semif_decrees_path(root).ok_or_else(|| {
        "no decrees library: set semif.decrees in .wordkeep/config.json or pass lib".into()
    })
}

fn resolve_lib(root: &Path, args: &Value) -> Result<PathBuf, String> {
    if let Some(p) = args.get("lib").and_then(Value::as_str) {
        let path = PathBuf::from(p);
        if path.is_absolute() {
            return Ok(path);
        }
        return Ok(root.join(path));
    }
    decrees_dir(root)
}

pub fn dispatch(root: &Path, args: &Value) -> Result<String, String> {
    let t0 = Instant::now();
    let method = args
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("check");
    let lib = resolve_lib(root, args)?;
    let out = match method {
        "check" => {
            let library = ereshkigal_lang::Library::load(&lib).map_err(|e| e.to_string())?;
            json!({
                "ok": true,
                "lib": lib.display().to_string(),
                "decrees": library.decrees.len(),
                "programs": library.programs.len(),
                "recipe": library.recipe,
            })
        }
        "lint" => {
            let library = ereshkigal_lang::Library::load(&lib).map_err(|e| e.to_string())?;
            ereshkigal_lang::lint::lint_library(&library).map_err(|e| e.to_string())?;
            json!({"ok": true, "lib": lib.display().to_string()})
        }
        "schema" => {
            let schema = ereshkigal_lang::library_schema_json().map_err(|e| e.to_string())?;
            json!({
                "ok": true,
                "lib": lib.display().to_string(),
                "schema": serde_json::from_str::<Value>(&schema).unwrap_or(json!(schema)),
            })
        }
        "stats" | "run" | "test" | "decide" => rpc(root, &lib, method, args)?,
        other => {
            return Err(format!(
                "unknown decree method {other}; use check, lint, schema, run, test, decide, stats"
            ))
        }
    };
    semif_telemetry::record_event(json!({
        "kind": "decree",
        "method": method,
        "lib": lib.display().to_string(),
        "timing_us": t0.elapsed().as_micros() as u64,
    }));
    Ok(serde_json::to_string_pretty(&out).unwrap_or_else(|_| "{}".into()))
}

fn rpc(root: &Path, lib: &Path, method: &str, args: &Value) -> Result<Value, String> {
    let mut g = BRIDGE.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        *g = Some(spawn(root, lib)?);
    }
    let br = g.as_mut().unwrap();
    let id = br.next_id;
    br.next_id += 1;
    let mut params = args.clone();
    if let Some(obj) = params.as_object_mut() {
        obj.remove("method");
        obj.remove("lib");
    }
    let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    writeln!(br.stdin, "{}", serde_json::to_string(&req).map_err(|e| e.to_string())?)
        .map_err(|e| format!("serve stdin: {e}"))?;
    br.stdin.flush().map_err(|e| format!("serve flush: {e}"))?;
    let mut line = String::new();
    br.stdout
        .read_line(&mut line)
        .map_err(|e| format!("serve stdout: {e}"))?;
    let resp: Value = serde_json::from_str(&line).map_err(|e| format!("serve json: {e}"))?;
    if let Some(err) = resp.get("error") {
        return Err(err.to_string());
    }
    Ok(resp.get("result").cloned().unwrap_or(resp))
}

fn spawn(root: &Path, lib: &Path) -> Result<Bridge, String> {
    let bin = serve_bin(root)?;
    let mut cmd = Command::new(&bin);
    cmd.arg("serve")
        .arg("--stdio")
        .arg("--lib")
        .arg(lib)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(gguf) = config::semif_gguf_path(root) {
        cmd.arg("--gguf").arg(gguf);
    }
    let mut child = cmd.spawn().map_err(|e| {
        format!(
            "failed to spawn {} (build tools/ereshkigal or set ERESHKIGAL_BIN): {e}",
            bin.display()
        )
    })?;
    let stdin = child.stdin.take().ok_or("serve stdin missing")?;
    let stdout = child.stdout.take().ok_or("serve stdout missing")?;
    Ok(Bridge {
        _child: child,
        stdin,
        stdout: BufReader::new(stdout),
        next_id: 1,
    })
}

fn serve_bin(root: &Path) -> Result<PathBuf, String> {
    // Resolve order lives in `config::semif_serve_bin` (env → config →
    // target-vulkan when Wordkeep is Vulkan-built → target/release → PATH).
    if let Some(p) = config::semif_serve_bin(root) {
        if !p.is_file() && p.as_os_str() != "ereshkigal" {
            eprintln!(
                "[wordkeep] decree serve_bin not found at {} (build tools/ereshkigal target-vulkan or set ERESHKIGAL_BIN)",
                p.display()
            );
        }
        return Ok(p);
    }
    Ok(PathBuf::from("ereshkigal"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "wk_decree_{}_{}",
            tag,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(p.join(".wordkeep")).unwrap();
        p
    }

    #[test]
    fn check_lint_and_schema_std_decrees() {
        let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let lib = crate_dir.join("../ereshkigal/decrees");
        if !lib.is_dir() {
            return;
        }
        let root = tmp("check");
        fs::write(
            root.join(".wordkeep/config.json"),
            format!(r#"{{"semif":{{"decrees":{}}}}}"#, json!(lib.display().to_string())),
        )
        .unwrap();
        let text = dispatch(&root, &json!({"method": "check"})).unwrap();
        assert!(text.contains("\"ok\": true"), "{text}");
        let lint = dispatch(&root, &json!({"method": "lint"})).unwrap();
        assert!(lint.contains("\"ok\": true"), "{lint}");
        let schema = dispatch(&root, &json!({"method": "schema"})).unwrap();
        assert!(schema.contains("\"ok\": true"), "{schema}");
        assert!(
            schema.contains("decree") || schema.contains("schema"),
            "{schema}"
        );
        let _ = fs::remove_dir_all(&root);
    }
}
