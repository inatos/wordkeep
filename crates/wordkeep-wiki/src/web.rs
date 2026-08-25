use crate::config::WikiConfig;
use crate::garden::{self, html_anchor_id};
use crate::indexer::{
    collect_docs, global_cache_dir, is_markdown_path, kind_for_path, load_manifest, watch_workspace,
};
use crate::meili::MeiliClient;
use crate::runtime::ecs_layout;
use crate::runtime::maps;
use crate::runtime::peek::{self, MAX_PEEK};
use crate::runtime::RuntimeHub;
use crate::telemetry;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::Html;
use axum::routing::{get, post};
use axum::{Json, Router};
use pulldown_cmark::{html, Event, Options, Parser, Tag};
use serde::Deserialize;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};
use tokio_stream::wrappers::WatchStream;
use tokio_stream::StreamExt;
use tower_http::services::ServeDir;
use wordkeep_knowledge::parse_markdown;

#[derive(Clone)]
struct AppState {
    root: PathBuf,
    config: WikiConfig,
    meili: MeiliClient,
    runtime: RuntimeHub,
}

type ApiError = (StatusCode, Json<Value>);
type ApiResult = Result<Json<Value>, ApiError>;

pub(crate) async fn serve(
    root: PathBuf,
    config: WikiConfig,
    meili: MeiliClient,
    bind: &str,
    allow_non_loopback: bool,
    watch: bool,
    runtime_attach: bool,
) -> Result<(), String> {
    let address = parse_bind(bind, allow_non_loopback)?;
    if let Err(error) = meili.ensure_index().await {
        eprintln!("wordkeep-wiki: Meilisearch unavailable at startup: {error}");
    }

    if watch {
        let watch_root = root.clone();
        let watch_config = config.clone();
        let watch_meili = meili.clone();
        tokio::spawn(async move {
            if let Err(error) = watch_workspace(watch_root, watch_config, watch_meili).await {
                eprintln!("wordkeep-wiki: watcher stopped: {error}");
            }
        });
    }

    let runtime_enabled = address.ip().is_loopback() && !allow_non_loopback;
    let runtime = RuntimeHub::new(runtime_enabled, runtime_enabled && runtime_attach);
    {
        let hub = runtime.clone();
        let root_c = root.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
            loop {
                tick.tick().await;
                hub.refresh(&root_c);
            }
        });
    }
    runtime.refresh(&root);

    let state = AppState {
        root,
        config,
        meili,
        runtime,
    };
    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/search", get(search))
        .route("/api/page", get(page).put(save_page))
        .route("/api/outline", get(outline))
        .route("/api/tree", get(tree))
        .route("/api/backlinks", get(backlinks))
        .route("/api/recent", get(recent))
        .route("/api/garden", get(garden_health))
        .route("/api/search-telemetry", get(search_telemetry))
        .route("/api/search-telemetry/click", post(search_click))
        .route("/api/stats", get(stats))
        .route("/api/dashboard", get(dashboard_api))
        .route("/api/runtime", get(runtime_get))
        .route("/api/runtime/stream", get(runtime_stream))
        .route("/api/runtime/ingest", post(runtime_ingest))
        .route("/api/runtime/capture", post(runtime_capture))
        .route("/api/runtime/captures", get(runtime_captures))
        .route("/api/runtime/diff", post(runtime_diff))
        .route("/api/runtime/attach", post(runtime_attach_api))
        .route("/api/runtime/peek", post(runtime_peek))
        .route("/api/runtime/ecs-optimize", post(runtime_ecs))
        .with_state(state);

    let dist = resolve_wiki_dist();
    let app = if let Some(dist) = dist {
        app.fallback_service(ServeDir::new(dist).append_index_html_on_directories(true))
    } else {
        app.fallback(fallback_page)
    };

    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|error| format!("bind {address}: {error}"))?;
    eprintln!("wordkeep-wiki: serving http://{address}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|error| format!("serve: {error}"))
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

fn resolve_wiki_dist() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("wiki/dist"));
            candidates.push(dir.join("../wiki/dist"));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("wiki/dist"));
    }
    candidates.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wiki/dist"));
    candidates.into_iter().find(|p| p.is_dir())
}

pub fn parse_bind(bind: &str, allow_non_loopback: bool) -> Result<SocketAddr, String> {
    let address = bind
        .parse::<SocketAddr>()
        .map_err(|error| format!("invalid bind address '{bind}': {error}"))?;
    if !address.ip().is_loopback() && !allow_non_loopback {
        return Err(format!(
            "refusing non-loopback bind {address}; pass --allow-non-loopback to opt in"
        ));
    }
    Ok(address)
}

async fn health(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    let meili = state.meili.health().await;
    let index = state.meili.index_stats().await;
    let manifest = load_manifest(&state.root).unwrap_or_default();
    let manifest_files = manifest.len();
    let manifest_chunks: usize = manifest.values().map(|entry| entry.chunk_ids.len()).sum();
    let search_telemetry = telemetry::summary(&state.root);

    match meili {
        Ok(meili) => (
            StatusCode::OK,
            Json(json!({
                "status": "ok",
                "project_name": state.config.project_name,
                "meilisearch": meili,
                "index": result_or_error(index),
                "manifest": {
                    "files": manifest_files,
                    "chunks": manifest_chunks
                },
                "search_telemetry": search_telemetry,
                "retain_search_queries": state.config.retain_search_queries,
                "read_only": state.config.read_only
            })),
        ),
        Err(error) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "status": "degraded",
                "project_name": state.config.project_name,
                "error": error,
                "index": result_or_error(index),
                "manifest": {
                    "files": manifest_files,
                    "chunks": manifest_chunks
                },
                "search_telemetry": search_telemetry,
                "retain_search_queries": state.config.retain_search_queries,
                "read_only": state.config.read_only
            })),
        ),
    }
}

fn result_or_error(result: Result<Value, String>) -> Value {
    match result {
        Ok(value) => json!({"ok": true, "data": value}),
        Err(error) => json!({"ok": false, "error": error}),
    }
}

#[derive(Default, Deserialize)]
struct SearchQuery {
    q: Option<String>,
    kind: Option<String>,
    root: Option<String>,
    tag: Option<String>,
    limit: Option<usize>,
}

async fn search(State(state): State<AppState>, Query(query): Query<SearchQuery>) -> ApiResult {
    let mut filters = Vec::new();
    if let Some(kind) = nonempty(query.kind) {
        filters.push(format!("kind = \"{}\"", filter_value(&kind)));
    }
    if let Some(root) = nonempty(query.root) {
        filters.push(format!("root = \"{}\"", filter_value(&root)));
    }
    if let Some(tag) = nonempty(query.tag) {
        filters.push(format!("tags = \"{}\"", filter_value(&tag)));
    }

    let q = query.q.unwrap_or_default();
    let mut request = json!({
        "q": q,
        "limit": query.limit.unwrap_or(20).clamp(1, 100),
        "attributesToHighlight": ["content", "heading", "body"],
        "highlightPreTag": "<em>",
        "highlightPostTag": "</em>",
        "attributesToCrop": ["content", "body"],
        "cropLength": 40
    });
    if !filters.is_empty() {
        request["filter"] = Value::String(filters.join(" AND "));
    }

    let started = Instant::now();
    let mut response = state
        .meili
        .search(request)
        .await
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, error))?;
    let latency_ms = started.elapsed().as_millis() as u64;
    let hit_count = response
        .get("estimatedTotalHits")
        .and_then(Value::as_u64)
        .or_else(|| {
            response
                .get("hits")
                .and_then(Value::as_array)
                .map(|hits| hits.len() as u64)
        })
        .unwrap_or(0);

    let retained_q = state
        .config
        .retain_search_queries
        .then(|| q.clone())
        .filter(|value| !value.trim().is_empty());
    if let Err(error) = telemetry::record_search(&state.root, latency_ms, hit_count, retained_q) {
        eprintln!("wordkeep-wiki: search telemetry: {error}");
    }
    if let Some(object) = response.as_object_mut() {
        object.insert("clientLatencyMs".to_string(), json!(latency_ms));
    }
    Ok(Json(response))
}

#[derive(Deserialize)]
struct PathQuery {
    path: String,
}

async fn page(State(state): State<AppState>, Query(query): Query<PathQuery>) -> ApiResult {
    let (path, markdown) = read_page(&state, &query.path)?;
    Ok(Json(page_payload(&path, &markdown)))
}

#[derive(Deserialize)]
struct SavePageBody {
    path: String,
    markdown: String,
}

async fn save_page(State(state): State<AppState>, Json(body): Json<SavePageBody>) -> ApiResult {
    if state.config.read_only {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "wiki is read-only in this environment",
        ));
    }
    let (path, absolute) = resolve_page_file(&state, &body.path)?;
    if body.markdown.contains('\0') {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "markdown must not contain NUL bytes",
        ));
    }
    std::fs::write(&absolute, body.markdown.as_bytes()).map_err(|error| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("write {}: {error}", absolute.display()),
        )
    })?;
    Ok(Json(page_payload(&path, &body.markdown)))
}

fn page_payload(path: &str, markdown: &str) -> Value {
    let parsed = parse_markdown(path, markdown);
    json!({
        "path": path,
        "markdown": markdown,
        "html": render_markdown(markdown),
        "outline": outline_sections(&parsed.sections),
        "tags": parsed.tags
    })
}

async fn outline(State(state): State<AppState>, Query(query): Query<PathQuery>) -> ApiResult {
    let (path, markdown) = read_page(&state, &query.path)?;
    let parsed = parse_markdown(&path, &markdown);
    Ok(Json(json!({
        "path": path,
        "sections": outline_sections(&parsed.sections),
        "tags": parsed.tags
    })))
}

fn outline_sections(sections: &[wordkeep_knowledge::Section]) -> Vec<Value> {
    sections
        .iter()
        .filter(|section| section.heading_level > 0)
        .map(|section| {
            json!({
                "heading": section.heading,
                "heading_level": section.heading_level,
                "heading_path": section.heading_path,
                "anchor": section.anchor,
                "html_id": html_anchor_id(&section.anchor),
                "ordinal": section.ordinal,
                "start_line": section.start_line,
                "end_line": section.end_line
            })
        })
        .collect()
}

async fn tree(State(state): State<AppState>) -> ApiResult {
    let documents = collect_docs(&state.root, &state.config)
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error))?;
    let files: Vec<Value> = documents
        .values()
        .map(|document| {
            json!({
                "path": document.relative,
                "root": document.root_label,
                "kind": kind_for_path(&document.relative)
            })
        })
        .collect();
    Ok(Json(json!({"files": files})))
}

async fn backlinks(State(state): State<AppState>, Query(query): Query<PathQuery>) -> ApiResult {
    validate_page_path(&query.path).map_err(|error| api_error(StatusCode::BAD_REQUEST, error))?;
    if state.config.root_label_for(&query.path).is_none() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "path is outside configured document roots",
        ));
    }

    let documents = collect_docs(&state.root, &state.config)
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error))?;
    let mut hits = Vec::new();
    for document in documents.values() {
        let Ok(markdown) = std::fs::read_to_string(&document.absolute) else {
            continue;
        };
        let parsed = parse_markdown(&document.relative, &markdown);
        for section in parsed.sections {
            for link in section.links {
                if !link.is_external
                    && link_points_to(&document.relative, &link.target, &query.path)
                {
                    hits.push(json!({
                        "path": document.relative,
                        "heading": section.heading,
                        "anchor": section.anchor,
                        "raw": link.raw
                    }));
                }
            }
        }
    }
    Ok(Json(json!({"path": query.path, "backlinks": hits})))
}

#[derive(Default, Deserialize)]
struct LimitQuery {
    limit: Option<usize>,
}

async fn recent(State(state): State<AppState>, Query(query): Query<LimitQuery>) -> ApiResult {
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let documents = collect_docs(&state.root, &state.config)
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error))?;
    let mut rows: Vec<(u64, Value)> = Vec::new();
    for document in documents.values() {
        let Ok(metadata) = std::fs::metadata(&document.absolute) else {
            continue;
        };
        let mtime_ns = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs() * 1_000_000_000 + u64::from(duration.subsec_nanos()))
            .unwrap_or(0);
        rows.push((
            mtime_ns,
            json!({
                "path": document.relative,
                "root": document.root_label,
                "kind": kind_for_path(&document.relative),
                "mtime_ns": mtime_ns
            }),
        ));
    }
    rows.sort_by(|left, right| right.0.cmp(&left.0));
    rows.truncate(limit);
    let files: Vec<Value> = rows.into_iter().map(|(_, value)| value).collect();
    Ok(Json(json!({"files": files})))
}

async fn garden_health(State(state): State<AppState>) -> ApiResult {
    garden::analyze(&state.root, &state.config)
        .map(Json)
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error))
}

async fn search_telemetry(State(state): State<AppState>) -> ApiResult {
    Ok(Json(telemetry::summary(&state.root)))
}

#[derive(Deserialize)]
struct ClickBody {
    rank: u32,
}

async fn search_click(State(state): State<AppState>, Json(body): Json<ClickBody>) -> ApiResult {
    let rank = body.rank.max(1);
    telemetry::record_click(&state.root, rank)
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error))?;
    Ok(Json(json!({"ok": true, "rank": rank})))
}

async fn stats() -> ApiResult {
    let path = global_cache_dir().join("wordkeep/savings.json");
    if !path.exists() {
        return Ok(Json(json!({"available": false})));
    }
    let bytes = std::fs::read(&path)
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let data: Value = serde_json::from_slice(&bytes)
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;

    let mut calls = 0u64;
    let mut baseline_tokens = 0u64;
    let mut returned_tokens = 0u64;
    if let Some(tools) = data.get("tools").and_then(Value::as_object) {
        for tool in tools.values() {
            calls = calls.saturating_add(tool.get("calls").and_then(Value::as_u64).unwrap_or(0));
            baseline_tokens = baseline_tokens.saturating_add(
                tool.get("baseline_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            );
            returned_tokens = returned_tokens.saturating_add(
                tool.get("returned_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            );
        }
    }
    Ok(Json(json!({
        "available": true,
        "aggregate": {
            "calls": calls,
            "baseline_tokens": baseline_tokens,
            "returned_tokens": returned_tokens,
            "avoided_tokens": baseline_tokens.saturating_sub(returned_tokens)
        },
        "data": data
    })))
}

async fn dashboard_api() -> ApiResult {
    crate::dashboard::build()
        .map(Json)
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error))
}

fn origin_ok(headers: &HeaderMap) -> bool {
    match headers.get("origin") {
        None => true,
        Some(v) => {
            let s = v.to_str().unwrap_or("");
            s.starts_with("http://127.0.0.1")
                || s.starts_with("http://localhost")
                || s.starts_with("http://[::1]")
        }
    }
}

fn require_runtime(
    state: &AppState,
    headers: &HeaderMap,
    need_token: bool,
) -> Result<(), ApiError> {
    if !state.runtime.enabled() {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "runtime disabled (loopback only)",
        ));
    }
    if !origin_ok(headers) {
        return Err(api_error(StatusCode::FORBIDDEN, "origin not allowed"));
    }
    if need_token {
        let tok = headers
            .get("x-wordkeep-runtime")
            .and_then(|v| v.to_str().ok());
        if !state.runtime.check_token(tok) {
            return Err(api_error(
                StatusCode::UNAUTHORIZED,
                "missing or bad runtime token",
            ));
        }
    }
    Ok(())
}

async fn runtime_get(State(state): State<AppState>, headers: HeaderMap) -> ApiResult {
    require_runtime(&state, &headers, false)?;
    state.runtime.refresh(&state.root);
    Ok(Json(state.runtime.snapshot()))
}

async fn runtime_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Sse<impl tokio_stream::Stream<Item = Result<SseEvent, Infallible>>>, ApiError> {
    require_runtime(&state, &headers, false)?;
    state.runtime.refresh(&state.root);
    let initial = state.runtime.snapshot();
    let updates = state.runtime.subscribe();
    let seq = initial.get("seq").and_then(Value::as_u64).unwrap_or(0);
    let initial_event = SseEvent::default()
        .event("snapshot")
        .id(seq.to_string())
        .retry(Duration::from_secs(2))
        .data(initial.to_string());
    let update_stream = WatchStream::new(updates).skip(1).map(|update| {
        let seq = update.get("seq").and_then(Value::as_u64).unwrap_or(0);
        Ok::<_, Infallible>(
            SseEvent::default()
                .event("delta")
                .id(seq.to_string())
                .data(update.to_string()),
        )
    });
    let stream = tokio_stream::once(Ok::<_, Infallible>(initial_event)).chain(update_stream);
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("runtime-keep-alive"),
    ))
}

async fn runtime_ingest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    require_runtime(&state, &headers, true)?;
    state
        .runtime
        .ingest(body)
        .map(|_| Json(json!({"ok": true})))
        .map_err(|e| api_error(StatusCode::BAD_REQUEST, e))
}

#[derive(Deserialize)]
struct CaptureBody {
    on: bool,
}

async fn runtime_capture(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CaptureBody>,
) -> ApiResult {
    require_runtime(&state, &headers, true)?;
    let status = state
        .runtime
        .set_capturing(&state.root, body.on)
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(json!({"ok": true, "capture": status})))
}

async fn runtime_captures(State(state): State<AppState>, headers: HeaderMap) -> ApiResult {
    require_runtime(&state, &headers, false)?;
    state
        .runtime
        .captures(&state.root)
        .map(Json)
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e))
}

#[derive(Deserialize)]
struct DiffBody {
    base: String,
    current: String,
}

async fn runtime_diff(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<DiffBody>,
) -> ApiResult {
    require_runtime(&state, &headers, true)?;
    state
        .runtime
        .diff_capture(&state.root, &body.base, &body.current)
        .map(Json)
        .map_err(|e| api_error(StatusCode::BAD_REQUEST, e))
}

#[derive(Deserialize)]
struct AttachBody {
    pid: Option<u32>,
    exe: Option<String>,
}

async fn runtime_attach_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AttachBody>,
) -> ApiResult {
    require_runtime(&state, &headers, true)?;
    if !state.runtime.attach_enabled() {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "pass --runtime-attach to enable PID attach",
        ));
    }
    if let Some(pid) = body.pid {
        if let Some(want) = body.exe.as_deref() {
            let actual =
                maps::process_name(pid).map_err(|e| api_error(StatusCode::BAD_REQUEST, e))?;
            if !actual.eq_ignore_ascii_case(want)
                && !actual
                    .to_ascii_lowercase()
                    .contains(&want.to_ascii_lowercase())
            {
                return Err(api_error(
                    StatusCode::BAD_REQUEST,
                    format!("exe mismatch: pid {pid} is {actual}"),
                ));
            }
        }
        state
            .runtime
            .set_attach_pid(Some(pid))
            .map_err(|e| api_error(StatusCode::FORBIDDEN, e))?;
    } else {
        state
            .runtime
            .set_attach_pid(None)
            .map_err(|e| api_error(StatusCode::FORBIDDEN, e))?;
    }
    state.runtime.refresh(&state.root);
    Ok(Json(json!({"ok": true, "pid": state.runtime.attach_pid()})))
}

#[derive(Deserialize)]
struct PeekBody {
    addr: AddressInput,
    len: usize,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AddressInput {
    Number(u64),
    Text(String),
}

impl AddressInput {
    fn parse(&self) -> Result<u64, String> {
        match self {
            Self::Number(value) => Ok(*value),
            Self::Text(value) => {
                let trimmed = value.trim();
                if let Some(hex) = trimmed
                    .strip_prefix("0x")
                    .or_else(|| trimmed.strip_prefix("0X"))
                {
                    u64::from_str_radix(hex, 16).map_err(|e| format!("invalid hex addr: {e}"))
                } else {
                    trimmed
                        .parse()
                        .map_err(|e| format!("invalid decimal addr: {e}"))
                }
            }
        }
    }
}

async fn runtime_peek(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PeekBody>,
) -> ApiResult {
    require_runtime(&state, &headers, true)?;
    if !state.runtime.attach_enabled() {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "peek requires --runtime-attach",
        ));
    }
    if body.len > MAX_PEEK {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            format!("len > {MAX_PEEK}"),
        ));
    }
    let addr = body
        .addr
        .parse()
        .map_err(|e| api_error(StatusCode::BAD_REQUEST, e))?;
    let pid = state.runtime.attach_pid().unwrap_or_else(std::process::id);
    match peek::peek_bytes(pid, addr, body.len) {
        Ok(bytes) => Ok(Json(json!({
            "ok": true,
            "pid": pid,
            "addr": format!("0x{addr:x}"),
            "len": bytes.len(),
            "hex": peek::to_hex(&bytes),
            "ascii": peek::to_ascii(&bytes),
            "quality": "exact"
        }))),
        Err(e) => Err(api_error(StatusCode::BAD_REQUEST, e)),
    }
}

#[derive(Deserialize)]
struct EcsBody {
    symbol: String,
    path: String,
    confirm: Option<bool>,
}

async fn runtime_ecs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<EcsBody>,
) -> ApiResult {
    require_runtime(&state, &headers, true)?;
    let path = ecs_layout::validate_under_root(&state.root, &body.path)
        .map_err(|e| api_error(StatusCode::BAD_REQUEST, e))?;
    let src = std::fs::read_to_string(&path)
        .map_err(|e| api_error(StatusCode::BAD_REQUEST, format!("read: {e}")))?;
    let sug = ecs_layout::suggest(&src, &body.symbol)
        .map_err(|e| api_error(StatusCode::BAD_REQUEST, e))?;
    if body.confirm == Some(true) {
        if !state.runtime.attach_enabled() {
            return Err(api_error(
                StatusCode::FORBIDDEN,
                "ECS apply requires --runtime-attach (write gate)",
            ));
        }
        let next = ecs_layout::apply_src(&src, &body.symbol)
            .map_err(|e| api_error(StatusCode::BAD_REQUEST, e))?;
        std::fs::write(&path, next)
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("write: {e}")))?;
        return Ok(Json(json!({
            "ok": true,
            "applied": true,
            "symbol": sug.struct_name,
            "path": body.path
        })));
    }
    Ok(Json(json!({
        "ok": true,
        "applied": false,
        "changed": sug.changed,
        "symbol": sug.struct_name,
        "path": body.path,
        "fields": sug.fields.iter().map(|f| json!({"ty": f.ty, "name": f.name, "est_size": f.est_size})).collect::<Vec<_>>(),
        "reordered": sug.reordered.iter().map(|f| json!({"ty": f.ty, "name": f.name, "est_size": f.est_size})).collect::<Vec<_>>(),
        "quality": "estimated"
    })))
}

fn read_page(state: &AppState, requested: &str) -> Result<(String, String), ApiError> {
    let (relative, absolute) = resolve_page_file(state, requested)?;
    let markdown = std::fs::read_to_string(&absolute)
        .map_err(|error| api_error(StatusCode::NOT_FOUND, error.to_string()))?;
    Ok((relative, markdown))
}

fn resolve_page_file(state: &AppState, requested: &str) -> Result<(String, PathBuf), ApiError> {
    validate_page_path(requested).map_err(|error| api_error(StatusCode::BAD_REQUEST, error))?;
    if state.config.root_label_for(requested).is_none() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "path is outside configured document roots",
        ));
    }
    if !is_markdown_path(Path::new(requested)) {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "path must name a Markdown document",
        ));
    }

    let canonical_root = state
        .root
        .canonicalize()
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let candidate = state.root.join(requested);
    let canonical = candidate
        .canonicalize()
        .map_err(|_| api_error(StatusCode::NOT_FOUND, "page not found"))?;
    if !canonical.starts_with(&canonical_root) {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "path resolves outside workspace root",
        ));
    }
    if !canonical.is_file() {
        return Err(api_error(StatusCode::BAD_REQUEST, "path must be a file"));
    }
    let relative = canonical
        .strip_prefix(&canonical_root)
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    Ok((relative, canonical))
}

/// Validate the lexical part of a page path before touching the filesystem.
pub fn validate_page_path(requested: &str) -> Result<(), String> {
    if requested.trim().is_empty() {
        return Err("path must not be empty".to_string());
    }
    if requested.contains('\0')
        || requested.contains('\\')
        || requested.as_bytes().get(1) == Some(&b':')
    {
        return Err("path must be a portable relative path".to_string());
    }
    let path = Path::new(requested);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err("path must not be absolute or contain '..'".to_string());
    }
    Ok(())
}

fn render_markdown(markdown: &str) -> String {
    let parsed = parse_markdown("_", markdown);
    let heading_ids: Vec<String> = parsed
        .sections
        .iter()
        .filter(|section| section.heading_level > 0)
        .map(|section| html_anchor_id(&section.anchor))
        .collect();
    let mut heading_index = 0usize;
    let parser = Parser::new_ext(markdown, Options::all()).filter_map(|event| match event {
        Event::Html(_) | Event::InlineHtml(_) => None,
        Event::Start(Tag::Heading {
            level,
            id: _,
            classes,
            attrs,
        }) => {
            let id = heading_ids
                .get(heading_index)
                .cloned()
                .unwrap_or_else(|| format!("heading-{heading_index}"));
            heading_index += 1;
            Some(Event::Start(Tag::Heading {
                level,
                id: Some(id.into()),
                classes,
                attrs,
            }))
        }
        other => Some(other),
    });
    let mut rendered = String::new();
    html::push_html(&mut rendered, parser);
    rendered
}

fn link_points_to(source: &str, raw_target: &str, requested: &str) -> bool {
    let target = raw_target
        .split(['#', '?'])
        .next()
        .unwrap_or_default()
        .trim();
    if target.is_empty() {
        return source == requested;
    }

    let direct = target.trim_start_matches('/');
    if path_variants(direct).iter().any(|path| path == requested) {
        return true;
    }

    let parent = Path::new(source).parent().unwrap_or_else(|| Path::new(""));
    let Some(relative) = normalize_lexical(&parent.join(target)) else {
        return false;
    };
    path_variants(&relative)
        .iter()
        .any(|path| path == requested)
}

fn path_variants(path: &str) -> Vec<String> {
    let normalized = path.trim_start_matches("./").replace('\\', "/");
    let mut variants = vec![normalized.clone()];
    if Path::new(&normalized).extension().is_none() {
        variants.push(format!("{normalized}.md"));
        variants.push(format!("{normalized}.mdc"));
    }
    variants
}

fn normalize_lexical(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => parts.push(value.to_string_lossy().to_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(parts.join("/"))
}

fn nonempty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn filter_value(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn api_error(status: StatusCode, error: impl Into<String>) -> ApiError {
    (status, Json(json!({"error": error.into()})))
}

async fn fallback_page() -> Html<&'static str> {
    Html(include_str!("static_fallback.html"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_path_validation_rejects_escape_forms() {
        assert!(validate_page_path("docs/page.md").is_ok());
        assert!(validate_page_path("../secret.md").is_err());
        assert!(validate_page_path("docs/../../secret.md").is_err());
        assert!(validate_page_path("/etc/passwd").is_err());
        assert!(validate_page_path("C:\\secret.md").is_err());
        assert!(validate_page_path("docs\\page.md").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn page_read_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let base = std::env::temp_dir().join(format!("wordkeep_wiki_path_{}", std::process::id()));
        let root = base.join("root");
        let docs = root.join("docs");
        let outside = base.join("outside.md");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(&outside, "# Secret\n").unwrap();
        symlink(&outside, docs.join("escape.md")).unwrap();

        let config = WikiConfig {
            doc_roots: vec!["docs".to_string()],
            meili_url: "http://127.0.0.1:7700".to_string(),
            meili_key: None,
            index_uid: "wiki_chunks".to_string(),
            bind: "127.0.0.1:8787".to_string(),
            retain_search_queries: false,
            project_name: "Demo".to_string(),
            read_only: false,
        };
        let state = AppState {
            root,
            config,
            meili: MeiliClient::new(
                "http://127.0.0.1:7700".to_string(),
                None,
                "wiki_chunks".to_string(),
            )
            .unwrap(),
            runtime: RuntimeHub::disabled(),
        };
        let error = read_page(&state, "docs/escape.md").unwrap_err();
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn non_loopback_bind_requires_explicit_opt_in() {
        assert!(parse_bind("127.0.0.1:8787", false).is_ok());
        assert!(parse_bind("[::1]:8787", false).is_ok());
        assert!(parse_bind("0.0.0.0:8787", false).is_err());
        assert!(parse_bind("0.0.0.0:8787", true).is_ok());
    }

    #[test]
    fn runtime_token_mismatch() {
        let h = RuntimeHub::new(true, false);
        assert!(!h.check_token(Some("nope")));
        let t = h.token();
        assert!(h.check_token(Some(&t)));
    }

    #[test]
    fn runtime_address_accepts_lossless_hex() {
        assert_eq!(
            AddressInput::Text("0x7fff123456789abc".into()).parse(),
            Ok(0x7fff123456789abc)
        );
        assert_eq!(AddressInput::Number(4096).parse(), Ok(4096));
        assert!(AddressInput::Text("not-an-address".into()).parse().is_err());
    }

    #[test]
    fn raw_html_is_not_passed_through_renderer() {
        let rendered = render_markdown("# Safe\n<script>alert('x')</script>\ntext");
        assert!(!rendered.contains("<script>"), "{rendered}");
        assert!(rendered.contains("text"));
        assert!(
            rendered.contains("id=\"safe\""),
            "expected heading id, got {rendered}"
        );
    }

    #[test]
    fn backlink_targets_resolve_relative_paths() {
        assert!(link_points_to(
            "docs/guide/start.md",
            "../reference.md#part",
            "docs/reference.md"
        ));
        assert!(link_points_to(
            "docs/guide/start.md",
            "docs/reference",
            "docs/reference.md"
        ));
    }
}
