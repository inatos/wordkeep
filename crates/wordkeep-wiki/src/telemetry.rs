use crate::indexer::manifest_path;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const RING: usize = 40;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct RecentSearch {
    ts: u64,
    latency_ms: u64,
    hit_count: u64,
    no_result: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    q: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    clicked_rank: Option<u32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct TelemetryStore {
    version: u32,
    searches: u64,
    no_results: u64,
    clicks: u64,
    click_rank_sum: u64,
    latency_sum_ms: u64,
    recent: Vec<RecentSearch>,
}

static LOCK: Mutex<()> = Mutex::new(());

fn store_path(root: &Path) -> PathBuf {
    // Sit next to the wiki manifest under the same workspace cache directory.
    manifest_path(root).with_file_name("wiki_search_telemetry.json")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn load(path: &Path) -> TelemetryStore {
    let Ok(bytes) = std::fs::read(path) else {
        return TelemetryStore {
            version: 1,
            ..TelemetryStore::default()
        };
    };
    serde_json::from_slice(&bytes).unwrap_or(TelemetryStore {
        version: 1,
        ..TelemetryStore::default()
    })
}

fn save(path: &Path, store: &TelemetryStore) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create telemetry dir {}: {error}", parent.display()))?;
    }
    let bytes = serde_json::to_vec_pretty(store)
        .map_err(|error| format!("serialize search telemetry: {error}"))?;
    std::fs::write(path, bytes).map_err(|error| format!("write {}: {error}", path.display()))
}

pub(crate) fn record_search(
    root: &Path,
    latency_ms: u64,
    hit_count: u64,
    query: Option<String>,
) -> Result<(), String> {
    let _guard = LOCK
        .lock()
        .map_err(|_| "search telemetry lock poisoned".to_string())?;
    let path = store_path(root);
    let mut store = load(&path);
    store.version = 1;
    store.searches = store.searches.saturating_add(1);
    store.latency_sum_ms = store.latency_sum_ms.saturating_add(latency_ms);
    let no_result = hit_count == 0;
    if no_result {
        store.no_results = store.no_results.saturating_add(1);
    }
    store.recent.push(RecentSearch {
        ts: now_secs(),
        latency_ms,
        hit_count,
        no_result,
        q: query,
        clicked_rank: None,
    });
    if store.recent.len() > RING {
        let drain = store.recent.len() - RING;
        store.recent.drain(0..drain);
    }
    save(&path, &store)
}

pub(crate) fn record_click(root: &Path, rank: u32) -> Result<(), String> {
    let _guard = LOCK
        .lock()
        .map_err(|_| "search telemetry lock poisoned".to_string())?;
    let path = store_path(root);
    let mut store = load(&path);
    store.version = 1;
    store.clicks = store.clicks.saturating_add(1);
    store.click_rank_sum = store.click_rank_sum.saturating_add(u64::from(rank));
    if let Some(entry) = store.recent.iter_mut().rev().find(|entry| !entry.no_result) {
        entry.clicked_rank = Some(rank);
    }
    save(&path, &store)
}

pub(crate) fn summary(root: &Path) -> Value {
    let path = store_path(root);
    let store = {
        let _guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        load(&path)
    };
    let avg_latency = if store.searches == 0 {
        0.0
    } else {
        store.latency_sum_ms as f64 / store.searches as f64
    };
    let no_result_rate = if store.searches == 0 {
        0.0
    } else {
        store.no_results as f64 / store.searches as f64
    };
    let avg_click_rank = if store.clicks == 0 {
        Value::Null
    } else {
        json!((store.click_rank_sum as f64 / store.clicks as f64 * 10.0).round() / 10.0)
    };
    json!({
        "available": store.searches > 0 || store.clicks > 0,
        "path": path,
        "searches": store.searches,
        "no_results": store.no_results,
        "no_result_rate": (no_result_rate * 1000.0).round() / 1000.0,
        "clicks": store.clicks,
        "avg_click_rank": avg_click_rank,
        "avg_latency_ms": (avg_latency * 10.0).round() / 10.0,
        "recent": store.recent,
        "retain_queries_note": "(opt-in: WIKI_RETAIN_QUERIES=1 · wiki.retain_search_queries)"
    })
}
