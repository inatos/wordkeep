//! MCP `notifications/progress` helpers for long tool calls.
//!
//! When the client passes `_meta.progressToken` on `tools/call` **and**
//! `WORDKEEP_MCP_PROGRESS=1`, handlers may call [`tick`] to emit progress
//! notifications on stdout before the final result. Otherwise ticks are
//! no-ops.
//!
//! Progress is **opt-in** because Cursor's Shared MCP client currently treats
//! `notifications/progress` for an unrecognized token as a transport error and
//! marks the server failed (`connection:transport_error` → live tool discovery
//! dead). Hosts that properly register `_meta.progressToken` can enable ticks.

use serde_json::{json, Value};
use std::cell::RefCell;
use std::io::{self, Write};
use std::sync::Mutex;

thread_local! {
    static TOKEN: RefCell<Option<Value>> = const { RefCell::new(None) };
}

/// Serializes progress writes so concurrent tests (or future threads) do not
/// interleave JSON-RPC lines on stdout.
static OUT_LOCK: Mutex<()> = Mutex::new(());

/// Whether `notifications/progress` emission is enabled (`WORDKEEP_MCP_PROGRESS=1`).
pub fn progress_enabled() -> bool {
    matches!(
        std::env::var("WORDKEEP_MCP_PROGRESS").ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES") | Some("on")
            | Some("ON")
    )
}

/// Acquire the shared stdout serialization lock (also used by `mcp` result writes).
pub fn stdout_lock() -> std::sync::MutexGuard<'static, ()> {
    OUT_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Install the progress token for the duration of a `tools/call` handler.
pub fn install(progress_token: Option<Value>) {
    if !progress_enabled() {
        TOKEN.with(|t| *t.borrow_mut() = None);
        return;
    }
    TOKEN.with(|t| {
        *t.borrow_mut() = progress_token.filter(|v| !v.is_null());
    });
}

/// Clear the thread-local progress token after a tool call finishes.
pub fn clear() {
    TOKEN.with(|t| {
        *t.borrow_mut() = None;
    });
}

/// Emit `notifications/progress` when a token is installed and progress is enabled.
pub fn tick(progress: u64, total: Option<u64>, message: &str) {
    if !progress_enabled() {
        return;
    }
    TOKEN.with(|t| {
        let Some(token) = t.borrow().clone() else {
            return;
        };
        let mut params = json!({
            "progressToken": token,
            "progress": progress,
        });
        if let Some(total) = total {
            params["total"] = json!(total);
        }
        if !message.is_empty() {
            params["message"] = json!(message);
        }
        let note = json!({
            "jsonrpc": "2.0",
            "method": "notifications/progress",
            "params": params,
        });
        let _ = write_notification(&note);
    });
}

fn write_notification(note: &Value) -> io::Result<()> {
    let _guard = stdout_lock();
    let mut out = io::stdout().lock();
    let s = serde_json::to_string(note).unwrap_or_else(|_| "{}".to_string());
    out.write_all(s.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

/// Extract `_meta.progressToken` from tools/call params (MCP 2025-06-18).
pub fn token_from_call_params(params: &Value) -> Option<Value> {
    params
        .get("_meta")
        .and_then(|m| m.get("progressToken"))
        .cloned()
        .filter(|v| !v.is_null())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_from_meta() {
        let params = json!({
            "name": "repo_map",
            "arguments": {},
            "_meta": { "progressToken": "abc-1" }
        });
        assert_eq!(
            token_from_call_params(&params),
            Some(json!("abc-1"))
        );
    }

    #[test]
    fn tick_without_token_is_noop() {
        clear();
        tick(1, Some(10), "scanning");
    }
}
