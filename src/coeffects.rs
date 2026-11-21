//! Spatial coeffects — reactive cache invalidation for write tools (FIG-2026-005).
//!
//! Coeffect names come from `resources/capabilities.json`. When a tool with declared
//! coeffects mutates durable state, dependent caches are notified here.

use serde_json::{json, Value};
use std::path::Path;
use std::sync::OnceLock;

use crate::{knowledge, mas};

static CAPABILITIES: OnceLock<Value> = OnceLock::new();

fn capabilities() -> &'static Value {
    CAPABILITIES.get_or_init(|| {
        serde_json::from_str(include_str!("../resources/capabilities.json"))
            .expect("capabilities.json must parse")
    })
}

/// Context for coeffect notify (paths and/or MAS session slug).
pub struct NotifyCtx<'a> {
    pub rel_paths: &'a [&'a str],
    pub session: Option<&'a str>,
}

impl<'a> NotifyCtx<'a> {
    pub const EMPTY: NotifyCtx<'a> = NotifyCtx {
        rel_paths: &[],
        session: None,
    };
}

/// True when `tool` declares any durable `effects` in the capabilities manifest.
pub fn tool_has_effects(tool: &str) -> bool {
    capabilities()
        .get("tools")
        .and_then(|tools| tools.get(tool))
        .and_then(|t| t.get("effects"))
        .and_then(Value::as_array)
        .is_some_and(|a| !a.is_empty())
}

/// MCP 2025-11 tool annotations derived from `resources/capabilities.json` effects & hints.
pub fn tool_annotations(tool: &str) -> Value {
    let cap = capabilities();
    let entry = cap.get("tools").and_then(|tools| tools.get(tool));
    let effects = entry
        .and_then(|t| t.get("effects"))
        .and_then(Value::as_array);
    let has_effects = effects.is_some_and(|a| !a.is_empty());
    let ann = entry.and_then(|t| t.get("annotations"));

    let read_only = ann
        .and_then(|a| a.get("readOnlyHint"))
        .and_then(Value::as_bool)
        .unwrap_or(!has_effects);

    let default_destructive = !read_only
        && effects.is_some_and(|arr| {
            arr.iter().filter_map(Value::as_str).any(|e| {
                e.starts_with("kill-") || e.starts_with("reset-") || e.starts_with("delete-")
            })
        });
    let destructive = if read_only {
        false
    } else {
        ann.and_then(|a| a.get("destructiveHint"))
            .and_then(Value::as_bool)
            .unwrap_or(default_destructive)
    };

    let default_idempotent = if read_only {
        true
    } else {
        !effects.is_some_and(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .any(|e| e.starts_with("spawn-"))
        })
    };
    let idempotent = ann
        .and_then(|a| a.get("idempotentHint"))
        .and_then(Value::as_bool)
        .unwrap_or(default_idempotent);

    let listed_open_world = cap
        .get("open_world_tools")
        .and_then(Value::as_array)
        .is_some_and(|arr| arr.iter().any(|v| v.as_str() == Some(tool)));
    let effect_open_world = effects.is_some_and(|arr| {
        arr.iter()
            .filter_map(Value::as_str)
            .any(|e| e.contains("remote"))
    });
    let open_world = ann
        .and_then(|a| a.get("openWorldHint"))
        .and_then(Value::as_bool)
        .unwrap_or(listed_open_world || effect_open_world);

    json!({
        "readOnlyHint": read_only,
        "destructiveHint": destructive,
        "idempotentHint": idempotent,
        "openWorldHint": open_world,
    })
}

/// Invalidate caches declared as coeffects for `tool` after a write.
pub fn notify(root: &Path, tool: &str, ctx: NotifyCtx<'_>) {
    let Some(coeffects) = capabilities()
        .get("tools")
        .and_then(|tools| tools.get(tool))
        .and_then(|t| t.get("coeffects"))
        .and_then(Value::as_array)
    else {
        return;
    };
    for c in coeffects {
        let Some(name) = c.as_str() else {
            continue;
        };
        match name {
            "knowledge-chunks-cache" => {
                for rel in ctx.rel_paths {
                    knowledge::invalidate_markdown_chunks_in(root, rel);
                }
            }
            "mas-session" => {
                if let Some(session) = ctx.session {
                    mas::evict_session_cache(root, session);
                }
            }
            "workspace-cache" => {
                mas::evict_workspace_session_cache(root);
            }
            // Disk-backed stores (defects.json, runs.json, effect journals): readers load fresh.
            "config" | "defects" | "runs" | "effect-journal" => {}
            other => {
                let _ = other;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_json_loads() {
        assert!(capabilities().get("tools").is_some());
        let upsert = capabilities()
            .get("tools")
            .and_then(|t| t.get("knowledge_upsert"))
            .and_then(|t| t.get("coeffects"))
            .and_then(Value::as_array);
        assert!(
            upsert.is_some_and(|a| a.iter().any(|v| v == "knowledge-chunks-cache")),
            "knowledge_upsert must declare knowledge-chunks-cache coeffect"
        );
        assert!(tool_has_effects("run_record"));
        assert!(tool_has_effects("tacet"));
        assert!(!tool_has_effects("repo_map"));

        let ro = tool_annotations("repo_map");
        assert_eq!(ro["readOnlyHint"], true);
        assert_eq!(ro["destructiveHint"], false);
        assert_eq!(ro["idempotentHint"], true);
        assert_eq!(ro["openWorldHint"], false);

        let copper = tool_annotations("copper_status");
        assert_eq!(copper["readOnlyHint"], true);
        assert_eq!(copper["openWorldHint"], true);

        let rr = tool_annotations("run_record");
        assert_eq!(rr["readOnlyHint"], false);
        assert_eq!(rr["destructiveHint"], false);
        assert_eq!(rr["idempotentHint"], true);

        let cancel = tool_annotations("task_cancel");
        assert_eq!(cancel["readOnlyHint"], false);
        assert_eq!(cancel["destructiveHint"], true);
        assert_eq!(cancel["openWorldHint"], true);
    }

    #[test]
    fn workspace_cache_notify_evicts_mas_session() {
        let _guard = crate::cache::test_env_lock();
        let root = std::env::temp_dir().join(format!("wk_coeffect_mas_{}", std::process::id()));
        let cache =
            std::env::temp_dir().join(format!("wk_coeffect_mas_cache_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&cache);
        std::env::set_var("XDG_CACHE_HOME", &cache);
        std::fs::create_dir_all(root.join(".wordkeep/notes")).unwrap();
        mas::get_or_create_session_for_test(&root, "coe-test").unwrap();
        notify(
            &root,
            "mas_post",
            NotifyCtx {
                rel_paths: &[],
                session: Some("coe-test"),
            },
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&cache);
    }
}
