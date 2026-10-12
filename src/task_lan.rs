//! Palaestra LAN task delegation — Wordkeep MCP wrappers around
//! `tools/palaestra/remote/run_job.sh` with an Izakaya GPU lease (`gpu-3080ti`).
//!
//! Folds `progress.json` (GPU vitals + phase/pct) into the task record for
//! Courier telemetry. Terminal transitions fire `notify-send` + Courier
//! `task.done` / `task.fail`. Courier remains notify-only; `task_*` is SoT.

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::copper_health;
use crate::izakaya::presence;
use crate::mcp;
use crate::stats;

const GPU_AGENT: &str = "gpu-3080ti";
const GPU_CLAIM_PATH: &str = "tools/palaestra/remote";
/// Display-only: heartbeat older than this → `stalled` hint (never auto-fail).
const PROGRESS_STALE_DEFAULT_SEC: u64 = 45;

static ACTIVE: Mutex<Option<ActiveTask>> = Mutex::new(None);

struct ActiveTask {
    task_id: String,
    child: Child,
    last_progress_phase: Option<String>,
}

fn tasks_dir(root: &Path) -> PathBuf {
    root.join(".cache/palaestra/tasks")
}

fn remote_dir(root: &Path, job: &str) -> PathBuf {
    root.join(".cache/palaestra/remote").join(job)
}

fn run_job_sh(root: &Path) -> PathBuf {
    root.join("tools/palaestra/remote/run_job.sh")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn optional_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn required_str(args: &Value, key: &str) -> Result<String, String> {
    optional_str(args, key).ok_or_else(|| format!("{key} is required"))
}

fn read_task(root: &Path, task_id: &str) -> Result<Value, String> {
    let path = tasks_dir(root).join(format!("{task_id}.json"));
    let raw = fs::read_to_string(&path).map_err(|e| format!("task {task_id}: {e}"))?;
    serde_json::from_str(&raw).map_err(|e| format!("task {task_id} json: {e}"))
}

fn write_task(root: &Path, task_id: &str, value: &Value) -> Result<(), String> {
    let dir = tasks_dir(root);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{task_id}.json"));
    let body = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    fs::write(&path, body).map_err(|e| e.to_string())
}

/// Mirror Wordkeep task into `.cache/arte/jobs/<task_id>.json` (receipt only; task_* is SoT).
fn mirror_craft_receipt(root: &Path, rec: &Value) {
    let Some(task_id) = rec.get("task_id").and_then(Value::as_str) else {
        return;
    };
    let job = rec.get("job").and_then(Value::as_str).unwrap_or("unknown");
    let status = rec.get("status").and_then(Value::as_str).unwrap_or("unknown");
    let transport = rec
        .get("transport")
        .and_then(Value::as_str)
        .unwrap_or("none");
    let created = rec
        .get("started_at")
        .and_then(Value::as_u64)
        .unwrap_or_else(now_secs);
    let dir = root.join(".cache/arte/jobs");
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let receipt = json!({
        "id": task_id,
        "kind": job,
        "channel": "lan",
        "status": status,
        "args": { "job": job },
        "receipt": {
            "artifacts_dir": rec.get("artifacts_dir").cloned().unwrap_or(Value::Null),
            "exit_code": rec.get("exit_code").cloned().unwrap_or(Value::Null),
            "log_tail": rec.get("log_tail").cloned().unwrap_or(Value::Null),
            "backend": "palaestra-task",
            "files": rec.get("files").cloned().unwrap_or(Value::Null),
        },
        "created_unix": created,
        "palaestra_task_id": task_id,
        "transport": transport,
        "provenance": { "source": "wordkeep task_*", "sot": "task_*" },
    });
    let _ = fs::write(
        dir.join(format!("{task_id}.json")),
        serde_json::to_string_pretty(&receipt).unwrap_or_else(|_| "{}".into()),
    );
}

/// Lane classification for agent docs / receipts (SSH still executes heavy bodies).
fn job_lane(job: &str) -> &'static str {
    match job {
        // Live / micro workers — Copper wire + optional Copper worker.
        "identity-tmlp" | "cuda-smoke" | "arte-art-pose" | "deepseek-ocr" => "L",
        // Overnight / heavy — task_* SoT + SSH executor (Copper notify only).
        // deepseek-ocr-provision is Lane J (venv + HF download); Courier announce-only.
        "train-stub" | "hunyuan-img2mesh" | "lift-stub" | "deepseek-ocr-provision" => "J",
        _ => "J",
    }
}

fn courier_post(
    root: &Path,
    kind: &str,
    body: &str,
    task_id: &str,
    transport: &str,
) {
    let dir = root.join(".cache/palaestra/courier");
    let _ = fs::create_dir_all(&dir);
    let id = format!("{}-{}", now_secs(), kind.replace('.', "-"));
    let doc = json!({
        "id": id,
        "from": "wordkeep-task",
        "to": "desktop",
        "kind": kind,
        "body": body,
        "paths": [],
        "created_unix": now_secs(),
        "transport": transport,
        "taken": false,
        "palaestra_task_id": task_id,
        "sot_note": "Courier mailbox notify only; Wordkeep task_* remains SoT",
    });
    let _ = fs::write(
        dir.join(format!("{id}.json")),
        serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".into()),
    );
}

fn courier_notify_submit(root: &Path, task_id: &str, job: &str, transport: &str) {
    if transport != "copper" && transport != "aeron" {
        return;
    }
    courier_post(
        root,
        "task.submit",
        &format!("task_submit {task_id} job={job} transport={transport}"),
        task_id,
        transport,
    );
}

/// Desktop notification on terminal transitions only (done/failed/cancelled).
fn desktop_notify(title: &str, body: &str) {
    let _ = Command::new("notify-send")
        .args(["--app-name=BetwixtCourier", "--urgency=normal", title, body])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn read_progress_json(root: &Path, job: &str) -> Option<Value> {
    let path = remote_dir(root, job).join("progress.json");
    let raw = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn progress_is_stale(progress: &Value) -> bool {
    let Some(hb) = progress.get("heartbeat_at").and_then(Value::as_u64) else {
        return false;
    };
    let stale_after = progress
        .get("stale_after_sec")
        .and_then(Value::as_u64)
        .unwrap_or(PROGRESS_STALE_DEFAULT_SEC);
    now_secs().saturating_sub(hb) > stale_after
}

/// Fold progress.json into the task record. Returns current phase (if any).
/// Stale heartbeats set `stalled: true` as a display hint — never change status.
fn fold_progress(root: &Path, rec: &mut Value) -> Option<String> {
    let job = rec.get("job").and_then(Value::as_str)?.to_string();
    let progress = read_progress_json(root, &job)?;
    let phase = progress
        .get("phase")
        .and_then(Value::as_str)
        .map(str::to_string);
    let stale = progress_is_stale(&progress);
    if let Some(obj) = rec.as_object_mut() {
        obj.insert("progress".into(), progress);
        obj.insert("stalled".into(), json!(stale));
        // Display hint only — never flip status based on stalled heartbeat.
        if stale {
            obj.insert(
                "stalled_note".into(),
                json!("heartbeat stale (display hint; task_* status unchanged)"),
            );
        } else {
            obj.remove("stalled_note");
        }
    }
    phase
}

fn notify_terminal(root: &Path, rec: &Value, status: &str) {
    let task_id = rec
        .get("task_id")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let job = rec.get("job").and_then(Value::as_str).unwrap_or("?");
    let transport = rec
        .get("transport")
        .and_then(Value::as_str)
        .unwrap_or("none");
    let (kind, title) = match status {
        "done" => ("task.done", "Courier task done"),
        "failed" => ("task.fail", "Courier task failed"),
        "cancelled" => ("task.fail", "Courier task cancelled"),
        _ => return,
    };
    let body = format!("{status}: {task_id} job={job}");
    courier_post(root, kind, &body, task_id, transport);
    desktop_notify(title, &body);
}

fn notify_phase_change(root: &Path, rec: &Value, phase: &str) {
    let task_id = rec
        .get("task_id")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let job = rec.get("job").and_then(Value::as_str).unwrap_or("?");
    let transport = rec
        .get("transport")
        .and_then(Value::as_str)
        .unwrap_or("none");
    // Phase-change only (throttled by caller comparing last phase).
    courier_post(
        root,
        "task.progress",
        &format!("phase={phase} task={task_id} job={job}"),
        task_id,
        transport,
    );
}

fn select_transport(root: &Path) -> String {
    // Prefer health client (respects cooldown / caps-missing) before transport.sh.
    if copper_health::prefer_copper(root) {
        return "copper".into();
    }
    if copper_health::prefer_aeron(root) {
        return "aeron".into();
    }
    let script = root.join("tools/palaestra/remote/transport.sh");
    if !script.is_file() {
        return "none".into();
    }
    // Blip retry: short backoff then re-probe selector once.
    for attempt in 0u32..3 {
        match Command::new("bash").arg(&script).current_dir(root).output() {
            Ok(out) if out.status.success() => {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                match s.as_str() {
                    "copper" | "aeron" => return s,
                    "none" if attempt + 1 < 3 => {
                        copper_health::backoff_sleep(attempt);
                        continue;
                    }
                    "none" => return "none".into(),
                    _ => return "none".into(),
                }
            }
            _ if attempt + 1 < 3 => {
                copper_health::backoff_sleep(attempt);
            }
            _ => return "none".into(),
        }
    }
    "none".into()
}

fn acquire_gpu_lease(root: &Path) -> Result<(String, u64), String> {
    let args = json!({
        "agent_id": GPU_AGENT,
        "role": "gpu-worker",
        "task": "palaestra-remote",
        "summary": "gpu:3080ti LAN job via task_*",
        "claims": [{
            "path": GPU_CLAIM_PATH,
            "intent": "gpu:3080ti",
            "symbols": ["run_job.sh"]
        }],
        "ttl_secs": 3600,
        "observe_git": false,
        "format": "json",
        "force": false,
    });
    let v = presence::check_in(root, &args).map_err(|e| {
        format!("GPU lease busy or check_in failed ({GPU_AGENT}): {e}")
    })?;
    let lease_id = v
        .get("lease_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "check_in missing lease_id".to_string())?
        .to_string();
    let revision = v.get("revision").and_then(Value::as_u64).unwrap_or(1);
    Ok((lease_id, revision))
}

fn release_gpu_lease(root: &Path, lease_id: &str) {
    let args = json!({
        "agent_id": GPU_AGENT,
        "lease_id": lease_id,
        "reason": "completed",
        "format": "json",
    });
    let _ = presence::check_out(root, &args);
}

fn tail_log(path: &Path, max_lines: usize) -> String {
    match fs::read_to_string(path) {
        Ok(s) => {
            let lines: Vec<&str> = s.lines().collect();
            let start = lines.len().saturating_sub(max_lines);
            lines[start..].join("\n")
        }
        Err(_) => String::new(),
    }
}

fn poll_active(root: &Path) {
    let mut guard = match ACTIVE.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let Some(active) = guard.as_mut() else {
        return;
    };
    match active.child.try_wait() {
        Ok(Some(status)) => {
            let task_id = active.task_id.clone();
            let code = status.code().unwrap_or(-1);
            if let Ok(mut rec) = read_task(root, &task_id) {
                let job = rec
                    .get("job")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let lease = rec
                    .get("lease_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let terminal = if status.success() { "done" } else { "failed" };
                if let Some(obj) = rec.as_object_mut() {
                    obj.insert("status".into(), json!(terminal));
                    obj.insert("exit_code".into(), json!(code));
                    obj.insert("finished_at".into(), json!(now_secs()));
                    obj.insert(
                        "log_tail".into(),
                        json!(tail_log(&remote_dir(root, &job).join("run.log"), 40)),
                    );
                }
                let _ = fold_progress(root, &mut rec);
                let _ = write_task(root, &task_id, &rec);
                mirror_craft_receipt(root, &rec);
                notify_terminal(root, &rec, terminal);
                if !lease.is_empty() {
                    release_gpu_lease(root, &lease);
                }
            }
            *guard = None;
        }
        Ok(None) => {
            // Still running — fold live progress; Courier on phase change only.
            let task_id = active.task_id.clone();
            if let Ok(mut rec) = read_task(root, &task_id) {
                let job = rec
                    .get("job")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if !job.is_empty() {
                    if let Some(obj) = rec.as_object_mut() {
                        obj.insert(
                            "log_tail".into(),
                            json!(tail_log(&remote_dir(root, &job).join("run.log"), 40)),
                        );
                    }
                }
                if let Some(phase) = fold_progress(root, &mut rec) {
                    let changed = active.last_progress_phase.as_deref() != Some(phase.as_str());
                    if changed {
                        // Skip terminal phases here — handled on child exit.
                        if phase != "done" && phase != "failed" && phase != "cancelled" {
                            notify_phase_change(root, &rec, &phase);
                        }
                        active.last_progress_phase = Some(phase);
                    }
                }
                let _ = write_task(root, &task_id, &rec);
                mirror_craft_receipt(root, &rec);
            }
        }
        Err(_) => {
            *guard = None;
        }
    }
}

/// Submit a Palaestra remote job (sync wait by default).
pub fn submit(root: &Path, args: &Value) -> Result<String, String> {
    poll_active(root);
    {
        let guard = ACTIVE.lock().map_err(|e| e.to_string())?;
        if guard.is_some() {
            return Err("another LAN task is still running; wait or task_cancel".into());
        }
    }

    let job = required_str(args, "job")?;
    let script = run_job_sh(root)
        .parent()
        .unwrap()
        .join("jobs")
        .join(format!("{job}.sh"));
    if !script.is_file() {
        return Err(format!("unknown job script: {}", script.display()));
    }

    let wait = args
        .get("wait")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    let (lease_id, revision) = acquire_gpu_lease(root)?;
    let task_id = format!("{}-{}", now_secs(), job);
    let out_dir = remote_dir(root, &job);
    let _ = fs::create_dir_all(&out_dir);
    let transport = select_transport(root);
    let lane = job_lane(&job);

    let mut rec = json!({
        "task_id": task_id,
        "job": job,
        "lane": lane,
        "status": "running",
        "lease_id": lease_id,
        "lease_revision": revision,
        "gpu_agent": GPU_AGENT,
        "transport": transport,
        "started_at": now_secs(),
        "pid": Value::Null,
        "exit_code": Value::Null,
        "artifacts_dir": out_dir.display().to_string(),
        "log_tail": "",
        "sot_note": "task_* remains SoT; Copper/Aeron are submit/notify wire only",
    });
    write_task(root, &task_id, &rec)?;
    mirror_craft_receipt(root, &rec);
    courier_notify_submit(root, &task_id, &job, &transport);

    let sh = run_job_sh(root);
    let mut cmd = Command::new("bash");
    cmd.arg(&sh)
        .arg("--job")
        .arg(&job)
        .current_dir(root)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if transport == "none" {
        // Local stubs when Copper/Aeron are down (SSH stays emergency, not default).
        cmd.env("PALAESTRA_FORCE_LOCAL", "1");
    }
    let mut child = cmd.spawn().map_err(|e| {
        release_gpu_lease(root, &lease_id);
        format!("spawn run_job.sh: {e}")
    })?;

    let pid = child.id();
    if let Some(obj) = rec.as_object_mut() {
        obj.insert("pid".into(), json!(pid));
    }
    write_task(root, &task_id, &rec)?;

    if wait {
        let status = child.wait().map_err(|e| e.to_string())?;
        let code = status.code().unwrap_or(-1);
        let terminal = if status.success() { "done" } else { "failed" };
        if let Some(obj) = rec.as_object_mut() {
            obj.insert("status".into(), json!(terminal));
            obj.insert("exit_code".into(), json!(code));
            obj.insert("finished_at".into(), json!(now_secs()));
            obj.insert(
                "log_tail".into(),
                json!(tail_log(&out_dir.join("run.log"), 40)),
            );
        }
        let _ = fold_progress(root, &mut rec);
        write_task(root, &task_id, &rec)?;
        mirror_craft_receipt(root, &rec);
        notify_terminal(root, &rec, terminal);
        release_gpu_lease(root, &lease_id);
        return Ok(serde_json::to_string_pretty(&rec).unwrap_or_else(|_| "{}".into()));
    }

    let mut guard = ACTIVE.lock().map_err(|e| e.to_string())?;
    *guard = Some(ActiveTask {
        task_id: task_id.clone(),
        child,
        last_progress_phase: None,
    });
    mirror_craft_receipt(root, &rec);
    Ok(serde_json::to_string_pretty(&rec).unwrap_or_else(|_| "{}".into()))
}

pub fn status(root: &Path, args: &Value) -> Result<String, String> {
    poll_active(root);
    let task_id = required_str(args, "task_id")?;
    let mut rec = read_task(root, &task_id)?;
    if rec.get("status").and_then(Value::as_str) == Some("running") {
        let job = rec
            .get("job")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if !job.is_empty() {
            if let Some(obj) = rec.as_object_mut() {
                obj.insert(
                    "log_tail".into(),
                    json!(tail_log(&remote_dir(root, &job).join("run.log"), 40)),
                );
            }
        }
    }
    let _ = fold_progress(root, &mut rec);
    let _ = write_task(root, &task_id, &rec);
    mirror_craft_receipt(root, &rec);
    Ok(serde_json::to_string_pretty(&rec).unwrap_or_else(|_| "{}".into()))
}

pub fn fetch(root: &Path, args: &Value) -> Result<String, String> {
    poll_active(root);
    let task_id_opt = optional_str(args, "task_id");
    let job = if let Some(j) = optional_str(args, "job") {
        j
    } else {
        let task_id = required_str(args, "task_id")?;
        let rec = read_task(root, &task_id)?;
        rec.get("job")
            .and_then(Value::as_str)
            .ok_or_else(|| "task missing job".to_string())?
            .to_string()
    };
    let dir = remote_dir(root, &job);
    if !dir.is_dir() {
        return Err(format!("no artifacts at {}", dir.display()));
    }
    let mut files = Vec::new();
    if let Ok(rd) = fs::read_dir(&dir) {
        for e in rd.flatten() {
            files.push(e.file_name().to_string_lossy().into_owned());
        }
    }
    files.sort();
    if let Some(task_id) = task_id_opt {
        if let Ok(mut rec) = read_task(root, &task_id) {
            if let Some(obj) = rec.as_object_mut() {
                obj.insert("files".into(), json!(files.clone()));
                obj.insert(
                    "artifacts_dir".into(),
                    json!(dir.display().to_string()),
                );
            }
            mirror_craft_receipt(root, &rec);
        }
    }
    Ok(serde_json::to_string_pretty(&json!({
        "job": job,
        "artifacts_dir": dir.display().to_string(),
        "files": files,
    }))
    .unwrap_or_else(|_| "{}".into()))
}

pub fn cancel(root: &Path, args: &Value) -> Result<String, String> {
    let task_id = required_str(args, "task_id")?;
    let mut killed = false;
    {
        let mut guard = ACTIVE.lock().map_err(|e| e.to_string())?;
        if let Some(active) = guard.as_mut() {
            if active.task_id == task_id {
                let _ = active.child.kill();
                let _ = active.child.wait();
                killed = true;
                *guard = None;
            }
        }
    }
    let mut rec = read_task(root, &task_id)?;
    if !killed {
        if let Some(pid) = rec.get("pid").and_then(Value::as_u64) {
            let _ = Command::new("kill").arg(pid.to_string()).status();
            killed = true;
        }
    }
    let lease = rec
        .get("lease_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if let Some(obj) = rec.as_object_mut() {
        obj.insert("status".into(), json!("cancelled"));
        obj.insert("finished_at".into(), json!(now_secs()));
        obj.insert("killed".into(), json!(killed));
    }
    let _ = fold_progress(root, &mut rec);
    write_task(root, &task_id, &rec)?;
    notify_terminal(root, &rec, "cancelled");
    if !lease.is_empty() {
        release_gpu_lease(root, &lease);
    }
    Ok(serde_json::to_string_pretty(&rec).unwrap_or_else(|_| "{}".into()))
}

pub fn mcp_tools(root: PathBuf) -> Vec<mcp::Tool> {
      vec![
          tool(
              &root,
              "task_submit",
              "Submit a Palaestra LAN job via tools/palaestra/remote/run_job.sh. Acquires Izakaya GPU lease (gpu-3080ti). Uses Copper/Aeron wire when healthy for submit/notify; task_* remains SoT; SSH still executes heavy bodies. Args: job (required), wait (bool, default true).",
              json!({"type":"object","properties":{
                  "job":{"type":"string","description":"Job name under tools/palaestra/remote/jobs/<job>.sh (Lane L: identity-tmlp/cuda-smoke/arte-art-pose/deepseek-ocr; Lane J: train-stub/hunyuan-img2mesh/lift-stub)"},
                  "wait":{"type":"boolean","description":"Wait for completion (default true). false spawns background."}
              },"required":["job"]}),
          ),
          tool(
              &root,
              "task_status",
              "Status + log tail for a Palaestra LAN task_id from task_submit.",
              json!({"type":"object","properties":{
                  "task_id":{"type":"string"}
              },"required":["task_id"]}),
          ),
          tool(
              &root,
              "task_fetch",
              "List artifacts under .cache/palaestra/remote/<job>/ for a task_id or job name.",
              json!({"type":"object","properties":{
                  "task_id":{"type":"string"},
                  "job":{"type":"string"}
              }}),
          ),
          tool(
              &root,
              "task_cancel",
              "Best-effort cancel: kill recorded PID / active child and release GPU lease.",
              json!({"type":"object","properties":{
                  "task_id":{"type":"string"}
              },"required":["task_id"]}),
          ),
          tool(
              &root,
              "copper_status",
              "Copper peer health glance (.cache/palaestra/copper_health.json). Not job SoT — use before assuming local-only SemIf/Studio/task wire. Args: refresh (bool, default true).",
              json!({"type":"object","properties":{
                  "refresh":{"type":"boolean","description":"Refresh via copper_health.sh if stale (default true)."}
              }}),
          ),
      ]
  }

fn tool(root: &Path, name: &'static str, description: &'static str, schema: Value) -> mcp::Tool {
    let root = root.to_path_buf();
    mcp::Tool {
        name,
        description,
        input_schema: schema,
        handler: Box::new(move |args| {
            let out = dispatch(name, &root, args)?;
            stats::record(name, 64, (out.len() / 4) as u64);
            Ok(out)
        }),
    }
}

fn copper_status(root: &Path, args: &Value) -> Result<String, String> {
    let refresh = args
        .get("refresh")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let h = if refresh {
        copper_health::refresh(root, false)
    } else {
        copper_health::read_cached(root)
    };
    let cooldown = copper_health::in_cooldown(root);
    let (prefer, hint) = match &h {
        Some(health) if !cooldown && health.peer_up() && health.can_probe() => {
            (true, "copper")
        }
        Some(_) if !cooldown && copper_health::prefer_aeron(root) => (false, "aeron"),
        _ => (false, "none"),
    };
    let mut out = match h {
        Some(ref health) => copper_health::to_json(health),
        None => json!({
            "ok": false,
            "peer": "unknown",
            "reason": "copper_health.json missing — run tools/copper/scripts/copper_health.sh",
            "sot_note": "Copper health is not job SoT; Wordkeep task_* remains SoT",
        }),
    };
    if let Some(obj) = out.as_object_mut() {
        obj.insert("cooldown".into(), json!(cooldown));
        obj.insert("prefer_copper".into(), json!(prefer));
        obj.insert("transport_hint".into(), json!(hint));
    }
    Ok(serde_json::to_string_pretty(&out).unwrap_or_else(|_| "{}".into()))
}

fn dispatch(name: &str, root: &Path, args: &Value) -> Result<String, String> {
    match name {
        "task_submit" => submit(root, args),
        "task_status" => status(root, args),
        "task_fetch" => fetch(root, args),
        "task_cancel" => cancel(root, args),
        "copper_status" => copper_status(root, args),
        other => Err(format!("unknown task tool {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static LOCK: Mutex<()> = Mutex::new(());

    fn isolated(name: &str) -> (std::sync::MutexGuard<'static, ()>, PathBuf) {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let cache = std::env::temp_dir().join(format!("wk_task_lan_cache_{name}_{}", std::process::id()));
        let root = std::env::temp_dir().join(format!("wk_task_lan_root_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&cache);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("tools/palaestra/remote/jobs")).unwrap();
        fs::create_dir_all(root.join(".wordkeep")).unwrap();
        std::env::set_var("XDG_CACHE_HOME", &cache);
        std::env::set_var("WORDKEEP_IZAKAYA_NOW", "2000");
        // Minimal run_job.sh that writes an artifact + progress.json for identity-tmlp.
        let run = root.join("tools/palaestra/remote/run_job.sh");
        fs::write(
            &run,
            r#"#!/usr/bin/env bash
set -euo pipefail
JOB=""
while [[ $# -gt 0 ]]; do case "$1" in --job) JOB="$2"; shift 2;; *) shift;; esac; done
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
OUT="$ROOT/.cache/palaestra/remote/$JOB"
mkdir -p "$OUT" "$ROOT/.cache/palaestra/jobs/$JOB"
echo ok > "$OUT/run.log"
echo fixture > "$OUT/identity.tmlp"
echo fixture > "$ROOT/.cache/palaestra/jobs/$JOB/identity.tmlp"
NOW=$(date +%s)
cat > "$OUT/progress.json" <<EOF
{
  "phase": "done",
  "pct": 100,
  "eta_sec": 0,
  "job": "$JOB",
  "heartbeat_at": $NOW,
  "stale_after_sec": 45,
  "gpu": {"util": 12.0, "temp": 45.0, "vram_used_mib": 100, "vram_total_mib": 12288, "power_w": 40, "fan": 30, "available": true}
}
EOF
cp -f "$OUT/progress.json" "$ROOT/.cache/palaestra/jobs/$JOB/progress.json"
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&run).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&run, perms).unwrap();
        }
        fs::write(
            root.join("tools/palaestra/remote/jobs/identity-tmlp.sh"),
            "#!/usr/bin/env bash\necho identity\n",
        )
        .unwrap();
        (_g, root)
    }

    #[test]
    fn submit_identity_local_wait() {
        let (_g, root) = isolated("submit");
        // Izakaya journal needs a real-ish root; presence::check_in writes under .wordkeep
        let out = submit(&root, &json!({"job": "identity-tmlp", "wait": true})).expect("submit");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["status"], "done");
        assert_eq!(v["exit_code"], 0);
        let task_id = v["task_id"].as_str().unwrap();
        let craft_receipt = root.join(".cache/arte/jobs").join(format!("{task_id}.json"));
        assert!(
            craft_receipt.is_file(),
            "missing craft receipt {}",
            craft_receipt.display()
        );
        let receipt: Value =
            serde_json::from_str(&fs::read_to_string(&craft_receipt).unwrap()).unwrap();
        assert_eq!(receipt["channel"], "lan");
        assert_eq!(receipt["palaestra_task_id"], task_id);
        let fetch = fetch(&root, &json!({"job": "identity-tmlp", "task_id": task_id})).unwrap();
        assert!(fetch.contains("identity.tmlp"));
        // progress folded + terminal Courier notify
        assert_eq!(v["progress"]["phase"], "done");
        assert_eq!(v["progress"]["pct"], 100.0);
        assert_eq!(v["stalled"], false);
        let courier_dir = root.join(".cache/palaestra/courier");
        let terminal: Vec<_> = fs::read_dir(&courier_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("task-done"))
            .collect();
        assert!(
            !terminal.is_empty(),
            "expected task.done Courier post under {}",
            courier_dir.display()
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fold_progress_marks_stale_without_flipping_status() {
        let (_g, root) = isolated("stale");
        let job = "identity-tmlp";
        let out = remote_dir(&root, job);
        fs::create_dir_all(&out).unwrap();
        let old = now_secs().saturating_sub(120);
        fs::write(
            out.join("progress.json"),
            serde_json::to_string_pretty(&json!({
                "phase": "train",
                "pct": 40,
                "job": job,
                "heartbeat_at": old,
                "stale_after_sec": 45,
                "gpu": {"util": 90.0, "available": true}
            }))
            .unwrap(),
        )
        .unwrap();
        let mut rec = json!({
            "task_id": "t-stale",
            "job": job,
            "status": "running",
        });
        let phase = fold_progress(&root, &mut rec);
        assert_eq!(phase.as_deref(), Some("train"));
        assert_eq!(rec["status"], "running"); // never auto-fail
        assert_eq!(rec["stalled"], true);
        assert!(rec.get("stalled_note").is_some());
        assert_eq!(rec["progress"]["pct"], 40.0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn notify_terminal_posts_task_fail() {
        let (_g, root) = isolated("fail-notify");
        let rec = json!({
            "task_id": "t-fail",
            "job": "identity-tmlp",
            "transport": "none",
            "status": "failed",
        });
        notify_terminal(&root, &rec, "failed");
        let courier_dir = root.join(".cache/palaestra/courier");
        let posts: Vec<_> = fs::read_dir(&courier_dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        assert!(!posts.is_empty());
        let body = fs::read_to_string(&posts[0]).unwrap();
        assert!(body.contains("task.fail") || body.contains("\"kind\": \"task.fail\""));
        let _ = fs::remove_dir_all(&root);
    }
}
