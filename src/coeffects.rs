//! Spatial coeffects — reactive cache invalidation for write tools (FIG-2026-005).
//!
//! Coeffect names come from `resources/capabilities.json`. When a tool with declared
//! coeffects mutates durable state, dependent caches are notified here.

use serde_json::Value;
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
                    knowledge::invalidate_markdown_chunks(&root.join(rel));
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
