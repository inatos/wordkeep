//! Wiki-facing helpers for the Elucifia tab. Routes are mounted in `web`.

use elucifia_core::{
    aeron_host, chat_turn, dispatch, fetch_peer_status, menu_model, peer_base, post_peer_note,
    post_peer_restart, stream_peer_events, unreachable_peer, ChatRequest,
};
use serde_json::{json, Value};
use std::path::Path;

pub fn handle(root: &Path, method: &str, route: &str, body: &str) -> (u16, Value) {
    dispatch(root, method, route, body)
}

pub fn peer_status(root: &Path) -> Value {
    let menu = menu_model(root);
    match fetch_peer_status(&peer_base(&aeron_host(root))) {
        Ok(value) => value,
        Err(_) => unreachable_peer(&menu),
    }
}

pub fn stream_peer(root: &Path, on_chunk: impl FnMut(&str) -> bool) -> Result<(), String> {
    stream_peer_events(&peer_base(&aeron_host(root)), on_chunk)
}

pub fn peer_restart(root: &Path) -> Value {
    match post_peer_restart(&peer_base(&aeron_host(root))) {
        Ok(mut value) => {
            if let Some(lines) = value.get_mut("lines").and_then(Value::as_array_mut) {
                lines.insert(
                    0,
                    json!({
                        "lane": "feed",
                        "ok": true,
                        "detail": "Event stream reconnected"
                    }),
                );
            }
            if let Some(obj) = value.as_object_mut() {
                obj.insert("reachable".into(), json!(true));
            }
            value
        }
        Err(_) => json!({
            "reachable": false,
            "ok": false,
            "lines": [
                {"lane": "feed", "ok": false, "detail": "no peer to restart"},
                {"lane": "ollama", "ok": false, "detail": "no peer to restart"},
                {"lane": "copper", "ok": false, "detail": "no peer to restart"},
                {"lane": "voice", "ok": false, "detail": "no peer to restart"}
            ]
        }),
    }
}

pub fn chat_stream(root: &Path, body: &str, on_token: &mut dyn FnMut(&str)) -> Result<Value, String> {
    let parsed: Value = serde_json::from_str(body).unwrap_or_else(|_| json!({}));
    let force_stub = parsed.get("stub").and_then(Value::as_bool).unwrap_or(false);
    let model = parsed
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if !force_stub {
        let host = aeron_host(root);
        let noted = if model.trim().is_empty() {
            menu_model(root)
        } else {
            model.clone()
        };
        let line = format!("POST /api/chat {noted}");
        std::thread::spawn(move || post_peer_note(&peer_base(&host), "desktop", &line));
    }
    let outcome = chat_turn(
        root,
        &ChatRequest {
            message: parsed
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            model,
            thread_id: parsed
                .get("thread_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            force_stub,
            local_endpoint: parsed.get("local").and_then(Value::as_bool).unwrap_or(false),
        },
        on_token,
    )?;
    let mut value = serde_json::to_value(&outcome).map_err(|e| e.to_string())?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert("done".into(), json!(true));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn status_route_names_seraphina() {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!("elucifia-wiki-{n}"));
        std::fs::create_dir_all(&root).unwrap();
        let (code, value) = handle(&root, "GET", "/status", "");
        assert_eq!(code, 200);
        assert_eq!(value["name"], "Seraphina Elucifia");
        let _ = std::fs::remove_dir_all(&root);
    }
}
