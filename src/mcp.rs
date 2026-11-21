//! Minimal MCP server over stdio.
//!
//! Implements what a tools+resources server needs from the spec: the `initialize`
//! handshake, `tools/list`, `tools/call`, `resources/list`, `resources/read`, and
//! `ping`. Messages are newline-delimited JSON-RPC 2.0 on stdin/stdout (the MCP
//! stdio transport). All logging goes to stderr so it never corrupts the protocol
//! stream on stdout.

use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

thread_local! {
    static STRUCTURED_CONTENT: RefCell<Option<Value>> = const { RefCell::new(None) };
    static CURRENT_TASK_CANCEL: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
}

/// Attach MCP 2025-11 `structuredContent` (JSON object) to the current `tools/call` reply.
pub fn set_structured_content(value: Value) {
    let obj = if value.is_object() {
        value
    } else {
        json!({ "result": value })
    };
    STRUCTURED_CONTENT.with(|slot| *slot.borrow_mut() = Some(obj));
}

pub fn clear_structured_content() {
    STRUCTURED_CONTENT.with(|slot| *slot.borrow_mut() = None);
}

pub fn take_structured_content() -> Option<Value> {
    STRUCTURED_CONTENT.with(|slot| slot.borrow_mut().take())
}

/// Install an async task cancellation flag on the current worker thread.
pub fn install_task_cancel(flag: Option<Arc<AtomicBool>>) {
    CURRENT_TASK_CANCEL.with(|slot| *slot.borrow_mut() = flag);
}

/// True when the current `tools/call` is executing inside an MCP 2025-11 async task.
pub fn is_async_task_context() -> bool {
    CURRENT_TASK_CANCEL.with(|slot| slot.borrow().is_some())
}

/// True when `tasks/cancel` has been requested for the current async task.
pub fn is_current_task_cancelled() -> bool {
    CURRENT_TASK_CANCEL.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
    })
}

/// Long-running tools wired for MCP 2025-11 `execution.taskSupport = "optional"`:
/// Palaestra `task_submit`, Tracy `trace_profile` / `perf_triage`, and SCIP refresh
/// (`index_health` / `index_stale` with `refresh_scip:true` or `mode:"scip_refresh"`).
pub const TASK_OPTIONAL_TOOLS: &[&str] = &[
    "task_submit",
    "trace_profile",
    "perf_triage",
    "index_health",
    "index_stale",
];

pub fn is_task_optional_tool(name: &str) -> bool {
    TASK_OPTIONAL_TOOLS.contains(&name)
}

pub fn tool_execution(name: &str) -> Option<Value> {
    if is_task_optional_tool(name) {
        Some(json!({ "taskSupport": "optional" }))
    } else {
        None
    }
}

const DEFAULT_TASK_TTL_MS: u64 = 300_000;
const DEFAULT_POLL_INTERVAL_MS: u64 = 100;
const TASK_RESULT_WAIT_MAX: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskStatus {
    Working,
    Completed,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Working)
    }
}

#[derive(Clone)]
struct TaskEntry {
    seq: u64,
    task_id: String,
    tool_name: String,
    status: TaskStatus,
    status_message: Option<String>,
    created_at: String,
    last_updated_at: String,
    created_instant: Instant,
    ttl_ms: u64,
    poll_interval_ms: u64,
    cancelled: Arc<AtomicBool>,
    result_payload: Option<Value>,
}

impl TaskEntry {
    fn to_task_json(&self) -> Value {
        let mut obj = json!({
            "taskId": self.task_id,
            "status": self.status.as_str(),
            "createdAt": self.created_at,
            "lastUpdatedAt": self.last_updated_at,
            "ttl": self.ttl_ms,
            "pollInterval": self.poll_interval_ms,
        });
        if let Some(ref msg) = self.status_message {
            obj["statusMessage"] = json!(msg);
        }
        obj
    }
}

struct TaskStore {
    entries: BTreeMap<u64, TaskEntry>,
    by_id: BTreeMap<String, u64>,
}

/// Thread-safe in-process MCP 2025-11 async task table (`tasks/get|result|list|cancel`).
#[derive(Clone)]
pub struct TaskTable {
    next_seq: Arc<AtomicU64>,
    inner: Arc<(Mutex<TaskStore>, Condvar)>,
}

impl Default for TaskTable {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskTable {
    pub fn new() -> Self {
        Self {
            next_seq: Arc::new(AtomicU64::new(1)),
            inner: Arc::new((
                Mutex::new(TaskStore {
                    entries: BTreeMap::new(),
                    by_id: BTreeMap::new(),
                }),
                Condvar::new(),
            )),
        }
    }

    fn reap_expired_locked(store: &mut TaskStore) {
        let expired_seqs: Vec<u64> = store
            .entries
            .iter()
            .filter(|(_, e)| {
                e.status.is_terminal()
                    && e.created_instant.elapsed().as_millis() > e.ttl_ms as u128
            })
            .map(|(seq, _)| *seq)
            .collect();
        for seq in expired_seqs {
            if let Some(removed) = store.entries.remove(&seq) {
                store.by_id.remove(&removed.task_id);
            }
        }
    }

    pub fn create_task(
        &self,
        tool_name: &str,
        ttl_ms: Option<u64>,
    ) -> (Value, String, Arc<AtomicBool>) {
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let task_id = format!("wk-task-{seq}");
        let ts = iso8601_now();
        let ttl = ttl_ms.unwrap_or(DEFAULT_TASK_TTL_MS).clamp(1_000, 86_400_000);
        let cancelled = Arc::new(AtomicBool::new(false));
        let entry = TaskEntry {
            seq,
            task_id: task_id.clone(),
            tool_name: tool_name.to_string(),
            status: TaskStatus::Working,
            status_message: Some(format!("running {tool_name}")),
            created_at: ts.clone(),
            last_updated_at: ts,
            created_instant: Instant::now(),
            ttl_ms: ttl,
            poll_interval_ms: DEFAULT_POLL_INTERVAL_MS,
            cancelled: cancelled.clone(),
            result_payload: None,
        };
        let task_json = entry.to_task_json();
        let (lock, _) = &*self.inner;
        if let Ok(mut guard) = lock.lock() {
            Self::reap_expired_locked(&mut guard);
            guard.by_id.insert(task_id.clone(), entry.seq);
            guard.entries.insert(entry.seq, entry);
        }
        (task_json, task_id, cancelled)
    }

    pub fn complete_ok(&self, task_id: &str, text: String, structured: Option<Value>) {
        let (lock, cvar) = &*self.inner;
        if let Ok(mut guard) = lock.lock() {
            if let Some(&seq) = guard.by_id.get(task_id) {
                if let Some(entry) = guard.entries.get_mut(&seq) {
                    if entry.status == TaskStatus::Working {
                        let mut payload = json!({
                            "content": [{ "type": "text", "text": text }],
                            "isError": false,
                            "_meta": {
                                "io.modelcontextprotocol/related-task": { "taskId": task_id }
                            }
                        });
                        if let Some(sc) = structured {
                            payload["structuredContent"] = sc;
                        }
                        entry.status = TaskStatus::Completed;
                        entry.status_message = Some("completed".into());
                        entry.last_updated_at = iso8601_now();
                        entry.result_payload = Some(payload);
                        cvar.notify_all();
                    }
                }
            }
        }
    }

    pub fn complete_err(&self, task_id: &str, err: String) {
        let (lock, cvar) = &*self.inner;
        if let Ok(mut guard) = lock.lock() {
            if let Some(&seq) = guard.by_id.get(task_id) {
                if let Some(entry) = guard.entries.get_mut(&seq) {
                    if entry.status == TaskStatus::Working {
                        let payload = json!({
                            "content": [{ "type": "text", "text": format!("error: {err}") }],
                            "isError": true,
                            "_meta": {
                                "io.modelcontextprotocol/related-task": { "taskId": task_id }
                            }
                        });
                        entry.status = TaskStatus::Failed;
                        entry.status_message = Some(err);
                        entry.last_updated_at = iso8601_now();
                        entry.result_payload = Some(payload);
                        cvar.notify_all();
                    }
                }
            }
        }
    }

    pub fn get_task(&self, task_id: &str) -> Result<Value, String> {
        let (lock, _) = &*self.inner;
        let mut guard = lock.lock().map_err(|e| e.to_string())?;
        Self::reap_expired_locked(&mut guard);
        let Some(&seq) = guard.by_id.get(task_id) else {
            return Err(format!("unknown task: {task_id}"));
        };
        let Some(entry) = guard.entries.get(&seq) else {
            return Err(format!("unknown task: {task_id}"));
        };
        Ok(entry.to_task_json())
    }

    pub fn list_tasks(&self, cursor: Option<&str>, limit: usize) -> Result<Value, String> {
        let min_seq: u64 = match cursor.map(str::trim).filter(|s| !s.is_empty()) {
            Some(raw) => raw
                .parse::<u64>()
                .map_err(|_| format!("invalid cursor: {raw}"))?,
            None => 0,
        };
        let cap = limit.clamp(1, 200);
        let (lock, _) = &*self.inner;
        let mut guard = lock.lock().map_err(|e| e.to_string())?;
        Self::reap_expired_locked(&mut guard);

        let mut tasks = Vec::new();
        let mut next_cursor: Option<String> = None;
        for (&seq, entry) in guard.entries.range((min_seq + 1)..) {
            if tasks.len() >= cap {
                next_cursor = Some(seq.saturating_sub(1).to_string());
                break;
            }
            tasks.push(entry.to_task_json());
        }
        let mut out = json!({ "tasks": tasks });
        if let Some(nc) = next_cursor {
            out["nextCursor"] = json!(nc);
        }
        Ok(out)
    }

    /// Cancel a `working` task. Returns `(updated_task_json, tool_name)`.
    pub fn cancel_task(&self, task_id: &str) -> Result<(Value, String), String> {
        let (lock, cvar) = &*self.inner;
        let mut guard = lock.lock().map_err(|e| e.to_string())?;
        Self::reap_expired_locked(&mut guard);
        let Some(&seq) = guard.by_id.get(task_id) else {
            return Err(format!("unknown task: {task_id}"));
        };
        let Some(entry) = guard.entries.get_mut(&seq) else {
            return Err(format!("unknown task: {task_id}"));
        };
        if entry.status.is_terminal() {
            return Err(format!(
                "cannot cancel task {task_id} in terminal status {}",
                entry.status.as_str()
            ));
        }
        entry.cancelled.store(true, Ordering::Relaxed);
        entry.status = TaskStatus::Cancelled;
        entry.status_message = Some("cancelled by client".into());
        entry.last_updated_at = iso8601_now();
        let tool_name = entry.tool_name.clone();
        let json_out = entry.to_task_json();
        cvar.notify_all();
        Ok((json_out, tool_name))
    }

    pub fn await_result(&self, task_id: &str) -> Result<Value, String> {
        let (lock, cvar) = &*self.inner;
        let deadline = Instant::now() + TASK_RESULT_WAIT_MAX;
        let mut guard = lock.lock().map_err(|e| e.to_string())?;
        loop {
            let Some(&seq) = guard.by_id.get(task_id) else {
                return Err(format!("unknown task: {task_id}"));
            };
            let Some(entry) = guard.entries.get(&seq) else {
                return Err(format!("unknown task: {task_id}"));
            };
            match entry.status {
                TaskStatus::Completed | TaskStatus::Failed => {
                    return entry
                        .result_payload
                        .clone()
                        .ok_or_else(|| format!("task {task_id} missing result payload"));
                }
                TaskStatus::Cancelled => {
                    return Err(format!("task cancelled: {task_id}"));
                }
                TaskStatus::Working => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Err(format!("timed out waiting for task {task_id}"));
                    }
                    let wait_for = deadline.saturating_duration_since(now);
                    let (next_guard, _) = cvar
                        .wait_timeout(guard, wait_for)
                        .map_err(|e| e.to_string())?;
                    guard = next_guard;
                }
            }
        }
    }
}

fn iso8601_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let hh = rem / 3600;
    let mm = (rem % 3600) / 60;
    let ss = rem % 60;
    // Howard Hinnant civil-from-days algorithm.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// A tool handler maps a JSON `arguments` object to a text result (or an error string).
pub type Handler = Box<dyn Fn(&Value) -> Result<String, String> + Send + Sync>;

pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub handler: Handler,
}

pub struct Resource {
    pub uri: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub mime_type: &'static str,
    pub text: &'static str,
}

pub struct ResourceTemplate {
    pub uri_template: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub mime_type: &'static str,
}

pub struct Server {
    pub tools: Vec<Tool>,
    pub hidden_tools: Vec<Tool>,
    pub resources: Vec<Resource>,
    pub resource_templates: Vec<ResourceTemplate>,
    pub server_name: String,
    pub server_version: String,
    /// Workspace root for `/tacet` write gating and dynamic resource templates.
    pub workspace_root: PathBuf,
}

impl Server {
    pub fn run(&self) -> io::Result<()> {
        let tasks = TaskTable::new();
        std::thread::scope(|scope| -> io::Result<()> {
            let stdin = io::stdin();
            let mut reader = stdin.lock();
            let mut line = String::new();

            loop {
                line.clear();
                if reader.read_line(&mut line)? == 0 {
                    break; // EOF: client closed the pipe.
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                let req: Value = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("[wordkeep] parse error: {e}");
                        continue;
                    }
                };

                let method = req.get("method").and_then(Value::as_str).unwrap_or("");
                let params = req.get("params").cloned().unwrap_or(Value::Null);

                // Notifications carry no `id` and get no response.
                // Cursor caches tools across reconnects. After it finishes
                // `notifications/initialized` it registers list-changed handlers, so
                // a short delay lets that registration land before we announce.
                if req.get("id").is_none() {
                    if method == "notifications/initialized" {
                        std::thread::sleep(std::time::Duration::from_millis(200));
                        let note = json!({
                            "jsonrpc": "2.0",
                            "method": "notifications/tools/list_changed"
                        });
                        write_stdout_line(&note)?;
                    }
                    continue;
                }

                let id = req.get("id").cloned().unwrap_or(Value::Null);

                // Do not hold the stdout lock across tools/call: progress ticks
                // flush notifications mid-handler.
                let response = self.dispatch_request(scope, &tasks, method, &params, id);

                write_stdout_line(&response)?;
            }
            Ok(())
        })
    }

    pub fn dispatch_request<'scope>(
        &'scope self,
        scope: &'scope std::thread::Scope<'scope, '_>,
        tasks: &TaskTable,
        method: &str,
        params: &Value,
        id: Value,
    ) -> Value {
        match method {
            "initialize" => self.handle_initialize(params, id),
            "tools/list" => self.handle_list(id),
            "tools/call" => self.handle_call_scoped(Some((scope, tasks)), params, id),
            "tasks/get" => self.handle_tasks_get(tasks, params, id),
            "tasks/result" => self.handle_tasks_result(tasks, params, id),
            "tasks/list" => self.handle_tasks_list(tasks, params, id),
            "tasks/cancel" => self.handle_tasks_cancel(tasks, params, id),
            "resources/list" => self.handle_resources_list(id),
            "resources/templates/list" => self.handle_resource_templates_list(id),
            "resources/read" => self.handle_resources_read(params, id),
            "ping" => ok_result(id, json!({})),
            other => err_response(id, -32601, &format!("method not found: {other}")),
        }
    }

    fn handle_initialize(&self, params: &Value, id: Value) -> Value {
        // Echo the client's protocol version for max compatibility.
        let pv = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or("2025-06-18");
        ok_result(
            id,
            json!({
                "protocolVersion": pv,
                "capabilities": {
                    "tools": { "listChanged": true },
                    "resources": { "listChanged": false, "subscribe": false },
                    "tasks": {
                        "list": {},
                        "cancel": {},
                        "requests": {
                            "tools": { "call": {} }
                        }
                    }
                },
                "serverInfo": {
                    "name": self.server_name.clone(),
                    "version": self.server_version.clone()
                }
            }),
        )
    }

    fn handle_list(&self, id: Value) -> Value {
        let tools: Vec<Value> = self
            .tools
            .iter()
            .map(|t| {
                let mut entry = json!({
                    "name": t.name,
                    "description": t.description,
                    "inputSchema": t.input_schema.clone(),
                    "annotations": crate::coeffects::tool_annotations(t.name),
                });
                if let Some(exec) = tool_execution(t.name) {
                    entry["execution"] = exec;
                }
                entry
            })
            .collect();
        ok_result(id, json!({ "tools": tools }))
    }

    #[allow(dead_code)]
    fn handle_call(&self, params: &Value, id: Value) -> Value {
        self.handle_call_scoped(None, params, id)
    }

    fn handle_call_scoped<'scope>(
        &'scope self,
        async_ctx: Option<(&'scope std::thread::Scope<'scope, '_>, &TaskTable)>,
        params: &Value,
        id: Value,
    ) -> Value {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));

        let found = self
            .tools
            .iter()
            .chain(self.hidden_tools.iter())
            .find(|t| t.name == name);
        let Some(tool) = found else {
            return err_response(id, -32602, &format!("unknown tool: {name}"));
        };

        // Task-augmented tools/call (MCP 2025-11 `params.task`).
        if let Some(task_param) = params.get("task").filter(|v| !v.is_null()) {
            let Some(task_obj) = task_param.as_object() else {
                return err_response(id, -32602, "params.task must be a JSON object");
            };
            if !is_task_optional_tool(tool.name) {
                return err_response(
                    id,
                    -32602,
                    &format!("task execution not supported for tool: {}", tool.name),
                );
            }
            let Some((scope, tasks)) = async_ctx else {
                return err_response(id, -32601, "async task runtime unavailable");
            };
            if !self.workspace_root.as_os_str().is_empty() {
                if let Err(e) = crate::tacet::gate_write(&self.workspace_root, name) {
                    return ok_result(
                        id,
                        json!({
                            "content": [{ "type": "text", "text": format!("error: {e}") }],
                            "isError": true
                        }),
                    );
                }
            }
            let ttl_ms = task_obj.get("ttl").and_then(Value::as_u64);
            let (task_json, task_id, cancel_flag) = tasks.create_task(tool.name, ttl_ms);
            let tasks_clone = tasks.clone();
            let args_owned = args;
            let progress_tok = crate::progress::token_from_call_params(params);
            scope.spawn(move || {
                crate::progress::install(progress_tok);
                install_task_cancel(Some(cancel_flag));
                clear_structured_content();
                let res = (tool.handler)(&args_owned);
                match res {
                    Ok(text) => {
                        let sc = take_structured_content();
                        tasks_clone.complete_ok(&task_id, text, sc);
                    }
                    Err(err) => {
                        clear_structured_content();
                        tasks_clone.complete_err(&task_id, err);
                    }
                }
                install_task_cancel(None);
                crate::progress::clear();
            });
            return ok_result(id, json!({ "task": task_json }));
        }

        // Synchronous tools/call.
        crate::progress::install(crate::progress::token_from_call_params(params));
        clear_structured_content();
        let gated = if !self.workspace_root.as_os_str().is_empty() {
            crate::tacet::gate_write(&self.workspace_root, name)
        } else {
            Ok(())
        };
        let response = match gated.and_then(|_| (tool.handler)(&args)) {
            // Tool-level failures are reported via `isError`, not a JSON-RPC error.
            Ok(text) => {
                let mut payload = json!({
                    "content": [{ "type": "text", "text": text }],
                    "isError": false,
                });
                if let Some(sc) = take_structured_content() {
                    payload["structuredContent"] = sc;
                }
                ok_result(id, payload)
            }
            Err(e) => {
                clear_structured_content();
                ok_result(
                    id,
                    json!({ "content": [{ "type": "text", "text": format!("error: {e}") }], "isError": true }),
                )
            }
        };
        crate::progress::clear();
        response
    }

    fn extract_task_id(params: &Value) -> Result<String, String> {
        params
            .get("taskId")
            .or_else(|| params.get("task_id"))
            .or_else(|| params.get("id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .ok_or_else(|| "missing required string 'taskId'".to_string())
    }

    fn handle_tasks_get(&self, tasks: &TaskTable, params: &Value, id: Value) -> Value {
        match Self::extract_task_id(params).and_then(|tid| tasks.get_task(&tid)) {
            Ok(task) => ok_result(id, task),
            Err(msg) => err_response(id, -32602, &msg),
        }
    }

    fn handle_tasks_result(&self, tasks: &TaskTable, params: &Value, id: Value) -> Value {
        match Self::extract_task_id(params).and_then(|tid| tasks.await_result(&tid)) {
            Ok(payload) => ok_result(id, payload),
            Err(msg) => err_response(id, -32602, &msg),
        }
    }

    fn handle_tasks_list(&self, tasks: &TaskTable, params: &Value, id: Value) -> Value {
        let cursor = params.get("cursor").and_then(Value::as_str);
        let limit = params
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(50) as usize;
        match tasks.list_tasks(cursor, limit) {
            Ok(list) => ok_result(id, list),
            Err(msg) => err_response(id, -32602, &msg),
        }
    }

    fn handle_tasks_cancel(&self, tasks: &TaskTable, params: &Value, id: Value) -> Value {
        match Self::extract_task_id(params).and_then(|tid| tasks.cancel_task(&tid)) {
            Ok((task, tool_name)) => {
                if tool_name == "task_submit" && !self.workspace_root.as_os_str().is_empty() {
                    crate::task_lan::cancel_active(&self.workspace_root);
                }
                ok_result(id, task)
            }
            Err(msg) => err_response(id, -32602, &msg),
        }
    }

    fn handle_resources_list(&self, id: Value) -> Value {
        let resources: Vec<Value> = self
            .resources
            .iter()
            .map(|r| {
                json!({
                    "uri": r.uri,
                    "name": r.name,
                    "description": r.description,
                    "mimeType": r.mime_type,
                })
            })
            .collect();
        ok_result(id, json!({ "resources": resources }))
    }

    fn handle_resource_templates_list(&self, id: Value) -> Value {
        let templates: Vec<Value> = self
            .resource_templates
            .iter()
            .map(|t| {
                json!({
                    "uriTemplate": t.uri_template,
                    "name": t.name,
                    "description": t.description,
                    "mimeType": t.mime_type,
                })
            })
            .collect();
        ok_result(id, json!({ "resourceTemplates": templates }))
    }

    fn handle_resources_read(&self, params: &Value, id: Value) -> Value {
        let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
        let canonical = normalize_resource_uri(uri);
        if let Some(r) = self
            .resources
            .iter()
            .find(|r| r.uri == canonical || r.uri == uri)
        {
            return ok_result(
                id,
                json!({
                    "contents": [{
                        "uri": r.uri,
                        "mimeType": r.mime_type,
                        "text": r.text,
                    }]
                }),
            );
        }

        if let Some(raw_sym) = uri.strip_prefix("wordkeep://symbol/") {
            let sym = percent_decode(raw_sym);
            let sym = sym.trim();
            if sym.is_empty() || self.workspace_root.as_os_str().is_empty() {
                return err_response(id, -32002, &format!("resource not found: {uri}"));
            }
            match crate::symbol_context::build(&self.workspace_root, &json!({ "symbol": sym })) {
                Ok(text) if !text.contains(": no definition found under") => {
                    return ok_result(
                        id,
                        json!({
                            "contents": [{
                                "uri": uri,
                                "mimeType": "text/plain",
                                "text": text,
                            }]
                        }),
                    );
                }
                _ => return err_response(id, -32002, &format!("symbol not found: {sym}")),
            }
        }

        if let Some(raw_id) = uri.strip_prefix("wordkeep://defect/") {
            let defect_id = percent_decode(raw_id);
            let defect_id = defect_id.trim();
            if defect_id.is_empty() || self.workspace_root.as_os_str().is_empty() {
                return err_response(id, -32002, &format!("resource not found: {uri}"));
            }
            if let Some(doc) = crate::defects::get_json(&self.workspace_root, defect_id) {
                let text = serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".into());
                return ok_result(
                    id,
                    json!({
                        "contents": [{
                            "uri": uri,
                            "mimeType": "application/json",
                            "text": text,
                        }]
                    }),
                );
            }
            return err_response(id, -32002, &format!("defect not found: {defect_id}"));
        }

        err_response(id, -32002, &format!("resource not found: {uri}"))
    }
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Accept `wordkeep://README` as an alias of the canonical `wordkeep://readme`.
pub fn normalize_resource_uri(uri: &str) -> &str {
    if uri.eq_ignore_ascii_case("wordkeep://readme") || uri == "wordkeep://README" {
        "wordkeep://readme"
    } else {
        uri
    }
}

fn ok_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn err_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn write_stdout_line(msg: &Value) -> io::Result<()> {
    // Share the lock with progress::tick so notification and result lines
    // never interleave mid-JSON.
    let _guard = crate::progress::stdout_lock();
    let mut out = io::stdout().lock();
    let s = serde_json::to_string(msg).unwrap_or_else(|_| "{}".to_string());
    out.write_all(s.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

pub const CORE_TOOLS: &[&str] = &[
    "repo_map",
    "outline",
    "symbol_resolve",
    "symbol_refs",
    "symbol_context",
    "batch_context",
    "call_graph",
    "type_layout",
    "diff_map",
    "test_map",
    "knowledge_search",
    "knowledge_upsert",
    "semantic_decide",
    "decree",
    "defect_list",
    "defect_upsert",
    "run_record",
    "commit_scope",
    "izakaya_status",
    "izakaya_check_in",
    "izakaya_update",
    "tool_discover",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toolset {
    Full,
    Core,
}

impl Toolset {
    pub fn from_env() -> Self {
        match std::env::var("WORDKEEP_TOOLSET") {
            Ok(v) => Self::parse(&v),
            Err(_) => Self::Full,
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "core" => Self::Core,
            "" | "full" => Self::Full,
            other => {
                eprintln!("[wordkeep] unknown WORDKEEP_TOOLSET={other:?}; defaulting to full");
                Self::Full
            }
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Core => "core",
        }
    }
}

pub fn is_core_tool(name: &str) -> bool {
    CORE_TOOLS.contains(&name)
}

pub fn partition_tools(all_tools: Vec<Tool>, toolset: Toolset) -> (Vec<Tool>, Vec<Tool>) {
    match toolset {
        Toolset::Full => (all_tools, Vec::new()),
        Toolset::Core => all_tools.into_iter().partition(|t| is_core_tool(t.name)),
    }
}

pub fn default_resource_templates() -> Vec<ResourceTemplate> {
    vec![
        ResourceTemplate {
            uri_template: "wordkeep://symbol/{name}",
            name: "Wordkeep symbol context",
            description: "Definition body, signature, doc comment, one-hop call graph, and record layout for a workspace symbol.",
            mime_type: "text/plain",
        },
        ResourceTemplate {
            uri_template: "wordkeep://defect/{id}",
            name: "Wordkeep defect record",
            description: "Structured defect JSON by ID from .wordkeep/defects.json.",
            mime_type: "application/json",
        },
    ]
}

#[derive(Clone)]
struct CatalogItem {
    name: &'static str,
    description: &'static str,
    input_schema: Value,
    is_core: bool,
}

pub fn build_tool_discover(existing_tools: &[Tool], active_toolset: Toolset) -> Tool {
    let discover_schema = json!({
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "description": "Case-insensitive keyword search across tool names, descriptions, and argument names."
            },
            "name": {
                "type": "string",
                "description": "Exact tool name to inspect full inputSchema and annotations. Alias: tool."
            },
            "tool": {
                "type": "string",
                "description": "Alias for name."
            },
            "tier": {
                "type": "string",
                "enum": ["all", "core", "extended"],
                "description": "Filter by toolset tier. Default \"all\"."
            },
            "include_schema": {
                "type": "boolean",
                "description": "Include full JSON inputSchema for matches (defaults to true for exact name or <=3 matches)."
            },
            "max": {
                "type": "integer",
                "description": "Maximum matching tools to list. Default 25."
            }
        }
    });
    let discover_desc = "Progressive discovery over Wordkeep's full MCP tool catalog. \
                         Search by keyword or inspect a tool's inputSchema and MCP 2025-11 annotations \
                         when WORDKEEP_TOOLSET=core trims tools/list. Every discovered tool remains \
                         directly callable via tools/call.";

    let mut catalog: Vec<CatalogItem> = existing_tools
        .iter()
        .map(|t| CatalogItem {
            name: t.name,
            description: t.description,
            input_schema: t.input_schema.clone(),
            is_core: is_core_tool(t.name),
        })
        .collect();
    catalog.push(CatalogItem {
        name: "tool_discover",
        description: discover_desc,
        input_schema: discover_schema.clone(),
        is_core: true,
    });
    let catalog_bytes: u64 = catalog
        .iter()
        .map(|c| (c.name.len() + c.description.len() + c.input_schema.to_string().len()) as u64)
        .sum();

    Tool {
        name: "tool_discover",
        description: discover_desc,
        input_schema: discover_schema,
        handler: Box::new(move |args| {
            let exact = args
                .get("name")
                .or_else(|| args.get("tool"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let query = args
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            let tier = args
                .get("tier")
                .and_then(Value::as_str)
                .unwrap_or("all")
                .trim()
                .to_ascii_lowercase();
            let max = args.get("max").and_then(Value::as_u64).unwrap_or(25).max(1) as usize;
            let terms: Vec<&str> = query.split_whitespace().filter(|t| !t.is_empty()).collect();

            let matched: Vec<&CatalogItem> = catalog
                .iter()
                .filter(|item| {
                    if let Some(target) = exact {
                        return item.name == target;
                    }
                    match tier.as_str() {
                        "core" if !item.is_core => return false,
                        "extended" if item.is_core => return false,
                        _ => {}
                    }
                    if terms.is_empty() {
                        return true;
                    }
                    let prop_names = item
                        .input_schema
                        .get("properties")
                        .and_then(Value::as_object)
                        .map(|m| m.keys().cloned().collect::<Vec<_>>().join(" "))
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    let hay = format!(
                        "{} {} {prop_names}",
                        item.name.to_ascii_lowercase(),
                        item.description.to_ascii_lowercase()
                    );
                    terms.iter().all(|term| hay.contains(term))
                })
                .collect();

            let include_schema = args
                .get("include_schema")
                .and_then(Value::as_bool)
                .unwrap_or_else(|| exact.is_some() || (!matched.is_empty() && matched.len() <= 3));

            let shown_slice = &matched[..matched.len().min(max)];
            let mut out = format!(
                "tool_discover - {} match(es) of {} total tools [active_toolset={}]\n",
                matched.len(),
                catalog.len(),
                active_toolset.as_str()
            );
            let mut json_rows = Vec::with_capacity(shown_slice.len());
            for item in shown_slice {
                let tier_label = if item.is_core { "core" } else { "extended" };
                let ann = crate::coeffects::tool_annotations(item.name);
                let ro = if ann["readOnlyHint"] == true {
                    "read-only"
                } else {
                    "write"
                };
                let props: Vec<String> = item
                    .input_schema
                    .get("properties")
                    .and_then(Value::as_object)
                    .map(|m| m.keys().cloned().collect())
                    .unwrap_or_default();
                let short_desc: String = item.description.chars().take(140).collect();
                out.push_str(&format!(
                    "\n- {} [{tier_label}, {ro}] args=[{}]\n  {}{}\n",
                    item.name,
                    props.join(", "),
                    short_desc,
                    if item.description.chars().count() > 140 { "…" } else { "" }
                ));
                if include_schema {
                    out.push_str(&format!("  inputSchema: {}\n", item.input_schema));
                }
                let mut row = json!({
                    "name": item.name,
                    "tier": tier_label,
                    "description": item.description,
                    "annotations": ann,
                    "args": props,
                });
                if let Some(exec) = tool_execution(item.name) {
                    row["execution"] = exec;
                }
                if include_schema {
                    row["inputSchema"] = item.input_schema.clone();
                }
                json_rows.push(row);
            }
            if matched.len() > shown_slice.len() {
                out.push_str(&format!(
                    "\n… (+{} more; narrow query or raise max)\n",
                    matched.len() - shown_slice.len()
                ));
            }
            if matched.is_empty() {
                out.push_str("\n(no matching tools)\n");
            }

            set_structured_content(json!({
                "active_toolset": active_toolset.as_str(),
                "total_tools": catalog.len(),
                "matched": matched.len(),
                "tools": json_rows,
            }));
            crate::stats::record("tool_discover", catalog_bytes / 4, (out.len() / 4) as u64);
            Ok(out)
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readme_alias_normalizes() {
        assert_eq!(
            normalize_resource_uri("wordkeep://README"),
            "wordkeep://readme"
        );
        assert_eq!(
            normalize_resource_uri("wordkeep://readme"),
            "wordkeep://readme"
        );
        assert_eq!(
            normalize_resource_uri("wordkeep://other"),
            "wordkeep://other"
        );
    }

    #[test]
    fn toolset_partition_and_discover() {
        assert_eq!(Toolset::parse("core"), Toolset::Core);
        assert_eq!(Toolset::parse("full"), Toolset::Full);
        assert_eq!(Toolset::parse(""), Toolset::Full);

        let dummy_tools = vec![
            Tool {
                name: "repo_map",
                description: "Map source tree",
                input_schema: json!({"type": "object", "properties": {"paths": {"type": "array"}}}),
                handler: Box::new(|_| Ok("mapped".into())),
            },
            Tool {
                name: "shrift_list",
                description: "List shrift ideas",
                input_schema: json!({"type": "object", "properties": {"tag": {"type": "string"}}}),
                handler: Box::new(|_| Ok("ideas".into())),
            },
        ];
        let mut all = dummy_tools;
        let discover = build_tool_discover(&all, Toolset::Core);
        all.push(discover);

        let (core, hidden) = partition_tools(all, Toolset::Core);
        assert!(core.iter().any(|t| t.name == "repo_map"));
        assert!(core.iter().any(|t| t.name == "tool_discover"));
        assert!(!core.iter().any(|t| t.name == "shrift_list"));
        assert!(hidden.iter().any(|t| t.name == "shrift_list"));

        let srv = Server {
            tools: core,
            hidden_tools: hidden,
            resources: Vec::new(),
            resource_templates: default_resource_templates(),
            server_name: "wordkeep".into(),
            server_version: "0.5.0".into(),
            workspace_root: PathBuf::new(),
        };

        let listed = srv.handle_list(json!(1));
        let arr = listed["result"]["tools"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["annotations"]["readOnlyHint"], true);

        // Hidden extended tool is discoverable AND still callable via tools/call!
        let disc = srv.handle_call(
            &json!({"name": "tool_discover", "arguments": {"query": "shrift"}}),
            json!(2),
        );
        assert!(disc["result"]["content"][0]["text"].as_str().unwrap().contains("shrift_list"));
        assert_eq!(disc["result"]["structuredContent"]["matched"], 1);

        let called_hidden = srv.handle_call(
            &json!({"name": "shrift_list", "arguments": {}}),
            json!(3),
        );
        assert_eq!(called_hidden["result"]["isError"], false);
        assert_eq!(called_hidden["result"]["content"][0]["text"], "ideas");

        let tmpl = srv.handle_resource_templates_list(json!(4));
        let t_arr = tmpl["result"]["resourceTemplates"].as_array().unwrap();
        assert!(t_arr.iter().any(|t| t["uriTemplate"] == "wordkeep://symbol/{name}"));
        assert!(t_arr.iter().any(|t| t["uriTemplate"] == "wordkeep://defect/{id}"));
    }

    #[test]
    fn async_tasks_table_complete_list_and_cancel() {
        let started = Arc::new((Mutex::new(false), Condvar::new()));
        let started_worker = started.clone();
        let srv = Server {
            tools: vec![
                Tool {
                    name: "trace_profile",
                    description: "Slow trace",
                    input_schema: json!({"type": "object"}),
                    handler: Box::new(move |args| {
                        if args.get("hang").and_then(Value::as_bool) == Some(true) {
                            let (lk, cv) = &*started_worker;
                            *lk.lock().unwrap() = true;
                            cv.notify_all();
                            for _ in 0..400 {
                                if is_current_task_cancelled() {
                                    return Err("cancelled".into());
                                }
                                std::thread::sleep(Duration::from_millis(5));
                            }
                        }
                        set_structured_content(json!({"ok": true}));
                        Ok("profiled".into())
                    }),
                },
                Tool {
                    name: "repo_map",
                    description: "Sync only",
                    input_schema: json!({"type": "object"}),
                    handler: Box::new(|_| Ok("mapped".into())),
                },
            ],
            hidden_tools: Vec::new(),
            resources: Vec::new(),
            resource_templates: Vec::new(),
            server_name: "wordkeep".into(),
            server_version: "0.5.0".into(),
            workspace_root: PathBuf::new(),
        };

        let listed = srv.handle_list(json!(1));
        let tools = listed["result"]["tools"].as_array().unwrap();
        assert_eq!(tools[0]["execution"]["taskSupport"], "optional");
        assert!(tools[1].get("execution").is_none());

        let tasks = TaskTable::new();
        std::thread::scope(|scope| {
            // Unsupported tool rejects task augmentation.
            let bad = srv.dispatch_request(
                scope,
                &tasks,
                "tools/call",
                &json!({"name": "repo_map", "arguments": {}, "task": {}}),
                json!(2),
            );
            assert_eq!(bad["error"]["code"], -32602);

            // Fast completion via tasks/result + structuredContent preserved.
            let created = srv.dispatch_request(
                scope,
                &tasks,
                "tools/call",
                &json!({"name": "trace_profile", "arguments": {}, "task": {"ttl": 60000}}),
                json!(3),
            );
            let tid1 = created["result"]["task"]["taskId"]
                .as_str()
                .unwrap()
                .to_string();
            let res1 = srv.dispatch_request(
                scope,
                &tasks,
                "tasks/result",
                &json!({"taskId": tid1}),
                json!(4),
            );
            assert_eq!(res1["result"]["isError"], false);
            assert_eq!(res1["result"]["content"][0]["text"], "profiled");
            assert_eq!(res1["result"]["structuredContent"]["ok"], true);
            assert_eq!(
                res1["result"]["_meta"]["io.modelcontextprotocol/related-task"]["taskId"],
                tid1
            );

            let got1 = srv.dispatch_request(
                scope,
                &tasks,
                "tasks/get",
                &json!({"taskId": tid1}),
                json!(5),
            );
            assert_eq!(got1["result"]["status"], "completed");

            // Cooperative cancellation via tasks/cancel.
            let created2 = srv.dispatch_request(
                scope,
                &tasks,
                "tools/call",
                &json!({"name": "trace_profile", "arguments": {"hang": true}, "task": {}}),
                json!(6),
            );
            let tid2 = created2["result"]["task"]["taskId"]
                .as_str()
                .unwrap()
                .to_string();
            {
                let (lk, cv) = &*started;
                let mut g = lk.lock().unwrap();
                while !*g {
                    g = cv.wait(g).unwrap();
                }
            }
            let cancelled = srv.dispatch_request(
                scope,
                &tasks,
                "tasks/cancel",
                &json!({"taskId": tid2}),
                json!(7),
            );
            assert_eq!(cancelled["result"]["status"], "cancelled");

            // Cancelling an already-terminal task returns -32602.
            let cancel_again = srv.dispatch_request(
                scope,
                &tasks,
                "tasks/cancel",
                &json!({"taskId": tid2}),
                json!(8),
            );
            assert_eq!(cancel_again["error"]["code"], -32602);

            // tasks/list enumerates both tasks.
            let list_resp =
                srv.dispatch_request(scope, &tasks, "tasks/list", &json!({}), json!(9));
            let all_tasks = list_resp["result"]["tasks"].as_array().unwrap();
            assert_eq!(all_tasks.len(), 2);
        });
    }
}
