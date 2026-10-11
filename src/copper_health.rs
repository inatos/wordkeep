//! Shared Copper peer health client for SemIf / task_* / copper_status.
//!
//! Status file: `.cache/palaestra/copper_health.json` (written by
//! `tools/copper/scripts/copper_health.sh`). Not job SoT — `task_*` remains SoT.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

const DEFAULT_STALE_SECS: u64 = 45;
const DEFAULT_COOLDOWN_SECS: u64 = 20;

#[derive(Debug, Clone)]
pub struct CopperHealth {
    pub ok: bool,
    pub peer: String,
    pub infer_daemon: String,
    pub rtt_ms: Option<f64>,
    pub caps: String,
    pub reason: String,
    pub ts: String,
    pub path: PathBuf,
}

impl CopperHealth {
    pub fn peer_up(&self) -> bool {
        self.ok && self.peer == "up" && self.caps != "missing"
    }

    pub fn lane_l_ready(&self) -> bool {
        self.peer_up() && self.infer_daemon == "up"
    }

    pub fn can_probe(&self) -> bool {
        self.caps != "missing"
    }
}

fn status_path(root: &Path) -> PathBuf {
    if let Ok(dir) = std::env::var("COPPER_HEALTH_DIR") {
        return PathBuf::from(dir).join("copper_health.json");
    }
    root.join(".cache/palaestra/copper_health.json")
}

fn cooldown_path(root: &Path) -> PathBuf {
    if let Ok(dir) = std::env::var("COPPER_HEALTH_DIR") {
        return PathBuf::from(dir).join("copper_health.cooldown");
    }
    root.join(".cache/palaestra/copper_health.cooldown")
}

fn stale_secs() -> u64 {
    std::env::var("COPPER_HEALTH_STALE_SEC")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_STALE_SECS)
}

fn cooldown_secs() -> u64 {
    std::env::var("COPPER_HEALTH_COOLDOWN_SEC")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_COOLDOWN_SECS)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn parse_doc(path: &Path, v: &Value) -> CopperHealth {
    CopperHealth {
        ok: v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false),
        peer: v
            .get("peer")
            .and_then(|x| x.as_str())
            .unwrap_or("down")
            .to_string(),
        infer_daemon: v
            .get("infer_daemon")
            .and_then(|x| x.as_str())
            .unwrap_or("down")
            .to_string(),
        rtt_ms: v.get("rtt_ms").and_then(|x| x.as_f64()),
        caps: v
            .get("caps")
            .and_then(|x| x.as_str())
            .unwrap_or("unknown")
            .to_string(),
        reason: v
            .get("reason")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        ts: v
            .get("ts")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        path: path.to_path_buf(),
    }
}

pub fn read_cached(root: &Path) -> Option<CopperHealth> {
    let path = status_path(root);
    let text = fs::read_to_string(&path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    Some(parse_doc(&path, &v))
}

fn age_secs(h: &CopperHealth) -> Option<u64> {
    if h.ts.is_empty() {
        let meta = fs::metadata(&h.path).ok()?;
        let m = meta.modified().ok()?;
        let age = SystemTime::now().duration_since(m).ok()?;
        return Some(age.as_secs());
    }
    // Accept `2026-10-09T22:53:11Z`
    let s = h.ts.trim_end_matches('Z');
    // Minimal parse: treat as fresh if mtime-based fallback works when chrono absent.
    let _ = s;
    let meta = fs::metadata(&h.path).ok()?;
    let m = meta.modified().ok()?;
    let age = SystemTime::now().duration_since(m).ok()?;
    Some(age.as_secs())
}

fn run_refresh_script(root: &Path) {
    let script = root.join("tools/copper/scripts/copper_health.sh");
    if !script.is_file() {
        return;
    }
    // Hard cap — a wedged copper_health.sh must not stall MCP tool calls (OCT / SemIf).
    let mut child = match Command::new("bash")
        .arg(&script)
        .arg("--json")
        .arg("--no-notify")
        .current_dir(root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return,
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
}

pub fn refresh(root: &Path, force: bool) -> Option<CopperHealth> {
    let cached = read_cached(root);
    if let Some(ref h) = cached {
        if !force {
            if let Some(age) = age_secs(h) {
                let stale = stale_secs();
                if age <= stale {
                    return cached;
                }
                // Caps missing: slow refresh (avoid AF_PACKET spin).
                if h.caps == "missing" && age <= stale.saturating_mul(4) {
                    return cached;
                }
            }
        }
    } else if !force {
        // fall through to refresh
    }
    run_refresh_script(root);
    read_cached(root).or(cached)
}

pub fn in_cooldown(root: &Path) -> bool {
    let path = cooldown_path(root);
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return false,
    };
    let until: f64 = match text.trim().parse() {
        Ok(v) => v,
        Err(_) => return false,
    };
    (now_secs() as f64) < until
}

pub fn mark_down_cooldown(root: &Path) {
    let path = cooldown_path(root);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let until = (now_secs() as f64) + (cooldown_secs() as f64);
    let _ = fs::write(&path, format!("{until:.3}\n"));
}

pub fn clear_cooldown(root: &Path) {
    let _ = fs::remove_file(cooldown_path(root));
}

pub fn prefer_copper(root: &Path) -> bool {
    if in_cooldown(root) {
        return false;
    }
    let Some(h) = refresh(root, false) else {
        return false;
    };
    h.can_probe() && h.peer_up()
}

/// Cache-only Copper glance — never spawns `copper_health.sh` (safe for OCT / hot MCP).
pub fn prefer_copper_cached(root: &Path) -> bool {
    if in_cooldown(root) {
        return false;
    }
    let Some(h) = read_cached(root) else {
        return false;
    };
    h.can_probe() && h.peer_up()
}

/// Probe Aeron with a short ping (best-effort; no status file yet).
pub fn prefer_aeron(root: &Path) -> bool {
    let bin = std::env::var("AERON_CLI").unwrap_or_else(|_| {
        let rel = root.join("tools/aeron/target/release/aeron-cli");
        if rel.is_file() {
            rel.display().to_string()
        } else {
            "tools/aeron/target/release/aeron-cli".into()
        }
    });
    let cfg = std::env::var("AERON_CONFIG")
        .unwrap_or_else(|_| root.join("tools/aeron/aeron.toml").display().to_string());
    let path = Path::new(&bin);
    if !path.is_file() && !bin.contains('/') {
        // rely on PATH
    } else if !Path::new(&bin).is_file() {
        return false;
    }
    let mut child = match Command::new(&bin)
        .args(["--config", &cfg, "ping", "--count", "1"])
        .current_dir(root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(40));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

pub fn backoff_sleep(attempt: u32) {
    let base_ms = 100u64;
    let cap_ms = 1600u64;
    let delay = cap_ms.min(base_ms.saturating_mul(4u64.saturating_pow(attempt)));
    let jitter = (delay / 4).max(1);
    // Deterministic-ish jitter from attempt to avoid pulling rand into wordkeep.
    let j = (attempt as u64 * 37 + 11) % (jitter + 1);
    thread::sleep(Duration::from_millis(delay + j));
}

pub fn to_json(h: &CopperHealth) -> Value {
    serde_json::json!({
        "ok": h.ok,
        "peer": h.peer,
        "infer_daemon": h.infer_daemon,
        "rtt_ms": h.rtt_ms,
        "caps": h.caps,
        "reason": h.reason,
        "ts": h.ts,
        "peer_up": h.peer_up(),
        "lane_l_ready": h.lane_l_ready(),
        "cooldown": false, // filled by caller
        "sot_note": "Copper health is not job SoT; Wordkeep task_* remains SoT",
        "path": h.path.display().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_peer_up() {
        let dir = tempfile_dir("copper-health-parse");
        let path = dir.join("copper_health.json");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"ok":true,"peer":"up","infer_daemon":"up","rtt_ms":1.5,"caps":"ok","reason":"ok","ts":"2026-10-09T00:00:00Z"}}"#
        )
        .unwrap();
        std::env::set_var("COPPER_HEALTH_DIR", dir.display().to_string());
        let h = read_cached(Path::new("/tmp")).expect("health");
        assert!(h.peer_up());
        assert!(h.lane_l_ready());
        std::env::remove_var("COPPER_HEALTH_DIR");
    }

    fn tempfile_dir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("wk-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }
}
