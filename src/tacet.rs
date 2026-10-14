//! Workspace-scoped silence latch for adhoc `/tacet` turns and sessions.
//!
//! When enabled, MCP/CLI write tools with declared `effects` in
//! `resources/capabilities.json` refuse with `tacet_suppressed`. The `tacet`
//! tool itself is always allowed so agents can clear the latch.

use serde_json::{json, Value};
use std::path::Path;

use crate::coeffects;
use crate::workspace;

pub const SUPPRESSED: &str = "tacet_suppressed";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Session,
    Turn,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Session => "session",
            Mode::Turn => "turn",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "session" => Some(Mode::Session),
            "turn" => Some(Mode::Turn),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub enabled: bool,
    pub mode: Mode,
    pub updated_secs: u64,
}

impl Default for State {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: Mode::Session,
            updated_secs: 0,
        }
    }
}

fn store_path(root: &Path) -> std::path::PathBuf {
    workspace::workspace_file(root, "tacet.json")
}

pub fn load(root: &Path) -> Result<State, String> {
    match workspace::read_json(&store_path(root))? {
        None => Ok(State::default()),
        Some(v) => Ok(State {
            enabled: v.get("enabled").and_then(Value::as_bool).unwrap_or(false),
            mode: v
                .get("mode")
                .and_then(Value::as_str)
                .and_then(Mode::parse)
                .unwrap_or(Mode::Session),
            updated_secs: v.get("updated_secs").and_then(Value::as_u64).unwrap_or(0),
        }),
    }
}

pub fn save(root: &Path, state: &State) -> Result<(), String> {
    workspace::ensure_workspace_dir(root)?;
    workspace::write_atomic_json(
        &store_path(root),
        &json!({
            "enabled": state.enabled,
            "mode": state.mode.as_str(),
            "updated_secs": state.updated_secs,
        }),
    )
}

pub fn is_enabled(root: &Path) -> bool {
    load(root).map(|s| s.enabled).unwrap_or(false)
}

/// True when `tool` must be refused while tacet is active.
pub fn should_suppress(tool: &str) -> bool {
    if tool == "tacet" {
        return false;
    }
    coeffects::tool_has_effects(tool)
}

pub fn gate_write(root: &Path, tool: &str) -> Result<(), String> {
    if should_suppress(tool) && is_enabled(root) {
        Err(SUPPRESSED.to_string())
    } else {
        Ok(())
    }
}

/// MCP / shared handler: set, clear, or report latch state.
pub fn run(root: &Path, args: &Value) -> Result<String, String> {
    let status_only = args
        .get("status")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let has_enabled = args.get("enabled").and_then(Value::as_bool);
    let mode_arg = args.get("mode").and_then(Value::as_str).and_then(Mode::parse);

    if status_only || has_enabled.is_none() {
        let s = load(root)?;
        return Ok(format_status(&s));
    }

    let enabled = has_enabled.unwrap();
    let mode = mode_arg.unwrap_or(if enabled {
        Mode::Session
    } else {
        load(root).map(|s| s.mode).unwrap_or(Mode::Session)
    });
    let state = State {
        enabled,
        mode: if enabled { mode } else { Mode::Session },
        updated_secs: workspace::now_secs(),
    };
    save(root, &state)?;
    Ok(format_status(&state))
}

fn format_status(s: &State) -> String {
    if s.enabled {
        format!(
            "tacet: enabled mode={} updated_secs={}",
            s.mode.as_str(),
            s.updated_secs
        )
    } else {
        format!("tacet: disabled updated_secs={}", s.updated_secs)
    }
}

/// CLI: `wordkeep tacet on|off|status` (optional `--mode turn|session` with on).
pub fn cli(root: &Path, argv: &[String]) -> Result<String, String> {
    let mut mode = Mode::Session;
    let mut action: Option<&str> = None;
    let mut i = 0usize;
    while i < argv.len() {
        match argv[i].as_str() {
            "on" | "off" | "status" => {
                action = Some(argv[i].as_str());
                i += 1;
            }
            "--mode" => {
                i += 1;
                let Some(m) = argv.get(i).and_then(|s| Mode::parse(s)) else {
                    return Err("tacet --mode requires turn|session".into());
                };
                mode = m;
                i += 1;
            }
            "--help" | "-h" => {
                return Ok(
                    "wordkeep tacet on|off|status [--mode turn|session]\n\
                     Persist workspace silence latch; write tools return tacet_suppressed while on."
                        .into(),
                );
            }
            other => return Err(format!("unknown tacet arg: {other}")),
        }
    }
    match action.unwrap_or("status") {
        "status" => run(root, &json!({ "status": true })),
        "on" => run(
            root,
            &json!({ "enabled": true, "mode": mode.as_str() }),
        ),
        "off" => run(root, &json!({ "enabled": false })),
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static LOCK: Mutex<()> = Mutex::new(());

    fn temp_root(label: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "wk_tacet_{label}_{}",
            std::process::id()
        ));
        let cache = std::env::temp_dir().join(format!(
            "wk_tacet_cache_{label}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&cache);
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("XDG_CACHE_HOME", &cache);
        (root, cache)
    }

    #[test]
    fn latch_round_trip_and_gate() {
        let _g = LOCK.lock().unwrap();
        let _env = crate::cache::test_env_lock();
        let (root, cache) = temp_root("rt");
        assert!(!is_enabled(&root));
        assert!(gate_write(&root, "run_record").is_ok());
        run(&root, &json!({ "enabled": true, "mode": "turn" })).unwrap();
        let s = load(&root).unwrap();
        assert!(s.enabled);
        assert_eq!(s.mode, Mode::Turn);
        assert!(gate_write(&root, "run_record").is_err());
        assert!(gate_write(&root, "tacet").is_ok());
        assert!(gate_write(&root, "repo_map").is_ok());
        run(&root, &json!({ "enabled": false })).unwrap();
        assert!(!is_enabled(&root));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&cache);
    }
}
