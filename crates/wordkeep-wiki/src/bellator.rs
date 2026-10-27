//! Bellator Admin-Sec studio backend inside `wordkeep-wiki`.
//!
//! Links `bellator-core` and `bellator-runners` directly so Wordkeep (`127.0.0.1:8787`)
//! serves the full Bellator Admin-Sec control plane (`/api/bellator/*`), live `/proc`
//! scanner telemetry, SSE event stream, scheduled user timers, and `.wordkeep/defects.json` sync.

use bellator_core::{
    compare_runs, detect_external_clamscan, explain_finding, is_engine_or_tracy_active, iso_now,
    new_run_id, prune_old_runs, sync_findings_to_wordkeep_defects, unix_now_ms,
    write_wordkeep_telemetry_provider, Confidence, Finding, FindingCounts, JobStatus,
    LiveScanTelemetry, PolicyPack, RunStore, RunnerExecMeta, Severity, Status, StoredRun,
    TriageEntry, TriageState, TriageStore, RUN_RETENTION_DAYS,
};
use bellator_runners::{
    preset_runners, runner_catalog, runner_info, scan_selected_with_telemetry, set_process_paused,
    verify_image_digest, RunnerResult, TelemetryCallback,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

struct BellatorRuntime {
    busy: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    jobs: Arc<Mutex<BTreeMap<String, StoredRun>>>,
    seeded: AtomicBool,
}

fn hub() -> &'static BellatorRuntime {
    static HUB: OnceLock<BellatorRuntime> = OnceLock::new();
    HUB.get_or_init(|| BellatorRuntime {
        busy: Arc::new(AtomicBool::new(false)),
        cancel: Arc::new(AtomicBool::new(false)),
        jobs: Arc::new(Mutex::new(BTreeMap::new())),
        seeded: AtomicBool::new(false),
    })
}

fn policy_dir(root: &Path) -> PathBuf {
    let p = root.join("tools/bellator/policy");
    if p.is_dir() {
        p
    } else {
        root.join("policy")
    }
}

fn runs_dir(root: &Path) -> PathBuf {
    let r = root.join("tools/bellator/runs");
    if root.join("tools/bellator").is_dir() {
        r
    } else {
        root.join("runs")
    }
}

fn open_stores(root: &Path) -> Result<(PolicyPack, RunStore, TriageStore), String> {
    let pdir = policy_dir(root);
    let rdir = runs_dir(root);
    let pack = PolicyPack::load(&pdir).map_err(|e| e.to_string())?;
    let runs = RunStore::open(&rdir).map_err(|e| e.to_string())?;
    let triage = TriageStore::open(&rdir).map_err(|e| e.to_string())?;
    Ok((pack, runs, triage))
}

fn ensure_seeded(root: &Path) {
    let h = hub();
    if h.seeded.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Ok((_pack, runs, _triage)) = open_stores(root) {
        let _ = prune_old_runs(&runs, RUN_RETENTION_DAYS);
        if runs.latest().ok().flatten().is_none() && !h.busy.load(Ordering::SeqCst) {
            let root_c = root.to_path_buf();
            std::thread::spawn(move || {
                let _ = execute_scan_sync(
                    &root_c,
                    "host-sec",
                    preset_runners("quick")
                        .into_iter()
                        .map(|s| s.to_string())
                        .collect(),
                    None,
                );
            });
        } else {
            let live = resolve_live_telemetry(&runs);
            let _ = write_wordkeep_telemetry_provider(root, &runs, live.as_ref());
        }
    }
}

pub fn query_user_timer(unit: &str, label: &str, cadence: &str) -> Value {
    let active = Command::new("systemctl")
        .args(["--user", "is-active", unit])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "active")
        .unwrap_or(false);
    let enabled = Command::new("systemctl")
        .args(["--user", "is-enabled", unit])
        .output()
        .map(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            s == "enabled" || s == "static"
        })
        .unwrap_or(false);
    let mut next_elapses = None;
    let mut last_trigger = None;
    if let Ok(out) = Command::new("systemctl")
        .args([
            "--user",
            "show",
            unit,
            "-p",
            "NextElapseUSecRealtime",
            "-p",
            "LastTriggerUSec",
        ])
        .output()
    {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some(v) = line.strip_prefix("NextElapseUSecRealtime=") {
                if !v.trim().is_empty() && v.trim() != "n/a" {
                    next_elapses = Some(v.trim().to_string());
                }
            } else if let Some(v) = line.strip_prefix("LastTriggerUSec=") {
                if !v.trim().is_empty() && v.trim() != "n/a" {
                    last_trigger = Some(v.trim().to_string());
                }
            }
        }
    }
    json!({
        "unit": unit,
        "label": label,
        "cadence": cadence,
        "active": active,
        "enabled": enabled,
        "next_elapses": next_elapses,
        "last_trigger": last_trigger,
    })
}

pub fn all_timers() -> Vec<Value> {
    vec![
        query_user_timer(
            "bellator-quick-scan.timer",
            "Daily Quick ClamAV & Host Audit",
            "Daily 04:15 (+15m jitter, idle cgroup)",
        ),
        query_user_timer(
            "bellator-full-scan.timer",
            "Weekly Full-Nexus & Deep ClamAV",
            "Sun 03:30 (+30m jitter, idle cgroup)",
        ),
    ]
}

pub fn resolve_live_telemetry(runs: &RunStore) -> Option<LiveScanTelemetry> {
    let busy = hub().busy.load(Ordering::SeqCst);
    if let Some(saved) = runs.load_live_telemetry() {
        let fresh = unix_now_ms().saturating_sub(saved.updated_at_ms) < 15_000;
        if busy
            || (fresh
                && matches!(
                    saved.status,
                    JobStatus::Running | JobStatus::PausedForEngine | JobStatus::Queued
                ))
        {
            return Some(saved);
        }
    }
    detect_external_clamscan()
}

pub fn dashboard(root: &Path) -> Value {
    ensure_seeded(root);
    let Ok((pack, runs, triage)) = open_stores(root) else {
        return json!({ "error": "failed to load Bellator policy or run store" });
    };
    let summaries = runs.list_summaries().unwrap_or_default();
    let latest = runs.latest().ok().flatten();
    let (sus, quar, cleared) = triage.counts().unwrap_or((0, 0, 0));
    let catalog = runner_catalog();
    let available = catalog.iter().filter(|r| r.available).count();
    let busy = hub().busy.load(Ordering::SeqCst);
    let live = resolve_live_telemetry(&runs);
    let _ = write_wordkeep_telemetry_provider(root, &runs, live.as_ref());

    let mut severity = BTreeMap::new();
    let mut by_tool = BTreeMap::new();
    if let Some(ref run) = latest {
        for f in &run.findings {
            if f.status != Status::Open {
                continue;
            }
            *severity
                .entry(f.severity.as_str().to_string())
                .or_insert(0usize) += 1;
            *by_tool.entry(f.tool.clone()).or_insert(0usize) += 1;
        }
    }

    let last_green_delta = latest.as_ref().and_then(|r| {
        r.last_green_delta
            .clone()
            .or_else(|| runs.compute_delta_for_run(r).ok().flatten())
    });

    let receipts = runs.load_advisory_receipts();
    let stale_limit = pack.schedule.stale_advisory_db_hours;
    let advisory_stale = receipts.is_stale(stale_limit);
    let timers = all_timers();

    let trend: Vec<_> = summaries
        .iter()
        .rev()
        .take(20)
        .map(|s| {
            json!({
                "id": s.id,
                "ts": s.started_at,
                "findings": s.counts.open,
                "duration_ms": s.duration_ms,
                "gate_ok": s.gate_ok,
                "profile": s.profile,
            })
        })
        .collect();

    let profiles: Vec<_> = pack
        .root
        .profiles
        .iter()
        .map(|(name, p)| {
            json!({
                "id": name,
                "roots": p.roots,
                "default": name == &pack.root.default_profile,
            })
        })
        .collect();

    let expired_suppressions: Vec<_> = pack
        .expired_suppressions()
        .into_iter()
        .cloned()
        .collect();

    json!({
        "gate_ok": latest.as_ref().map(|r| r.gate_ok).unwrap_or(true),
        "gate_reasons": latest.as_ref().map(|r| r.gate_reasons.clone()).unwrap_or_default(),
        "open_high_plus": latest.as_ref().map(|r| r.counts.critical + r.counts.high).unwrap_or(0),
        "open_total": latest.as_ref().map(|r| r.counts.open).unwrap_or(0),
        "suppressed_total": latest.as_ref().map(|r| r.counts.suppressed).unwrap_or(0),
        "scanner_coverage": { "available": available, "total": catalog.len() },
        "triage": { "suspicious": sus, "quarantined": quar, "cleared": cleared },
        "severity": severity,
        "by_tool": by_tool,
        "trend": trend,
        "last_green_delta": last_green_delta,
        "advisory_receipts": receipts,
        "advisory_stale": advisory_stale,
        "stale_advisory_db_hours": stale_limit,
        "timers": timers,
        "schedule": pack.schedule,
        "profiles": profiles,
        "default_profile": pack.root.default_profile,
        "presets": ["quick", "host", "malware", "clamav-full", "dependencies", "secrets", "workflows", "full", "custom"],
        "runners": catalog,
        "live_scan": live,
        "engine_active": is_engine_or_tracy_active(),
        "busy": busy,
        "expired_suppressions": expired_suppressions.len(),
        "recent_runs": summaries.into_iter().take(12).collect::<Vec<_>>(),
        "user_install_cmd": "bash tools/bellator/scripts/install_bellator_timers_user.sh",
        "root_opt_in_cmd": "sudo bash tools/bellator/scripts/install_bellator_root_sweep_opt_in.sh",
    })
}

pub fn live(root: &Path) -> Value {
    let Ok((_, runs, _)) = open_stores(root) else {
        return json!({ "busy": false, "engine_active": false, "live": null });
    };
    let busy = hub().busy.load(Ordering::SeqCst);
    let live = resolve_live_telemetry(&runs);
    json!({
        "busy": busy,
        "engine_active": is_engine_or_tracy_active(),
        "live": live,
    })
}

pub fn stream_events(root: &Path, mut send: impl FnMut(&str) -> bool) {
    let Ok((_, runs, _)) = open_stores(root) else {
        return;
    };
    for _ in 0..120 {
        let busy = hub().busy.load(Ordering::SeqCst);
        let live = resolve_live_telemetry(&runs);
        let frame = json!({
            "type": "live",
            "busy": busy,
            "engine_active": is_engine_or_tracy_active(),
            "live": live,
            "ts_ms": unix_now_ms(),
        });
        if !send(&frame.to_string()) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1000));
    }
}

pub fn findings(
    root: &Path,
    run_id: Option<&str>,
    severity: Option<&str>,
    tool: Option<&str>,
    status: Option<&str>,
    q: Option<&str>,
) -> Value {
    let Ok((_, runs, triage_store)) = open_stores(root) else {
        return json!({ "findings": [], "run_id": null });
    };
    let run = if let Some(id) = run_id.filter(|s| !s.is_empty()) {
        runs.load(id).ok()
    } else {
        runs.latest().ok().flatten()
    };
    let Some(run) = run else {
        return json!({ "findings": [], "run_id": null });
    };
    let triage = triage_store.list().unwrap_or_default();
    let triage_map: BTreeMap<_, _> = triage
        .iter()
        .cloned()
        .map(|e| (e.finding_id.clone(), e))
        .collect();

    let mut list = run.findings.clone();
    if let Some(sev) = severity.filter(|s| !s.is_empty() && *s != "all") {
        list.retain(|f| f.severity.as_str() == sev);
    }
    if let Some(tl) = tool.filter(|s| !s.is_empty() && *s != "all") {
        list.retain(|f| f.tool == tl);
    }
    if let Some(query) = q.filter(|s| !s.trim().is_empty()) {
        let ql = query.to_ascii_lowercase();
        list.retain(|f| {
            f.title.to_ascii_lowercase().contains(&ql)
                || f.id.to_ascii_lowercase().contains(&ql)
                || f.tool.to_ascii_lowercase().contains(&ql)
                || f.rule_id.to_ascii_lowercase().contains(&ql)
        });
    }

    let mut seen: BTreeMap<String, ()> = BTreeMap::new();
    let mut enriched: Vec<Value> = Vec::new();
    for f in list {
        seen.insert(f.id.clone(), ());
        let t = triage_map.get(&f.id);
        let effective = effective_finding_status(&f, t);
        if let Some(want) = status.filter(|s| !s.is_empty() && *s != "all") {
            if !status_filter_matches(want, &effective) {
                continue;
            }
        }
        enriched.push(json!({
            "finding": f,
            "triage": t,
            "effective_status": effective,
        }));
    }

    let want_open_only = status
        .map(|s| s.eq_ignore_ascii_case("open"))
        .unwrap_or(false);
    if !want_open_only {
        for e in &triage {
            if seen.contains_key(&e.finding_id) {
                continue;
            }
            if let Some(want) = status.filter(|s| !s.is_empty() && *s != "all") {
                if !status_filter_matches(want, e.state.as_str()) {
                    continue;
                }
            } else if e.state == TriageState::Unreviewed {
                continue;
            }
            let sev = e
                .severity
                .as_deref()
                .and_then(Severity::parse)
                .unwrap_or(Severity::Unknown);
            let mut f = Finding::new(
                e.finding_id.clone(),
                e.tool.clone().unwrap_or_else(|| "triage".into()),
                "triage.overlay",
                e.title.clone().unwrap_or_else(|| e.finding_id.clone()),
                sev,
                Confidence::Low,
            );
            f.status = Status::Open;
            f.remediation = Some(
                "Operator triage overlay from a prior run — finding body not in the selected run."
                    .into(),
            );
            enriched.push(json!({
                "finding": f,
                "triage": e,
                "effective_status": e.state.as_str(),
                "from_triage_only": true,
            }));
        }
    }

    json!({
        "run_id": run.id,
        "profile": run.profile,
        "gate_ok": run.gate_ok,
        "counts": run.counts,
        "findings": enriched,
    })
}

fn effective_finding_status(f: &Finding, triage: Option<&TriageEntry>) -> String {
    if let Some(t) = triage {
        if t.state != TriageState::Unreviewed {
            return t.state.as_str().to_string();
        }
    }
    match f.status {
        Status::Open => "open".into(),
        Status::Suppressed => "suppressed".into(),
        Status::Fixed => "fixed".into(),
        Status::Accepted => "accepted".into(),
    }
}

fn status_filter_matches(want: &str, effective: &str) -> bool {
    let w = want.trim().to_ascii_lowercase();
    let e = effective.trim().to_ascii_lowercase();
    if w == "open" {
        return e == "open" || e == "unreviewed";
    }
    w == e
}

pub fn triage_list(root: &Path) -> Value {
    let Ok((_, _, triage)) = open_stores(root) else {
        return json!({ "entries": [], "counts": { "suspicious": 0, "quarantined": 0, "cleared": 0 } });
    };
    let entries = triage.list().unwrap_or_default();
    let (suspicious, quarantined, cleared) = triage.counts().unwrap_or((0, 0, 0));
    json!({
        "entries": entries,
        "counts": { "suspicious": suspicious, "quarantined": quarantined, "cleared": cleared },
        "note": "Logical triage only — Bellator never moves or deletes workspace files."
    })
}

pub fn triage_upsert(root: &Path, body: &Value) -> Result<Value, String> {
    let (_, _, triage) = open_stores(root)?;
    let finding_id = body
        .get("finding_id")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "finding_id required".to_string())?
        .to_string();
    let state_str = body
        .get("state")
        .and_then(Value::as_str)
        .ok_or_else(|| "state required".to_string())?;
    let state =
        TriageState::parse(state_str).ok_or_else(|| format!("invalid triage state: {state_str}"))?;
    let entry = TriageEntry {
        finding_id,
        state,
        owner: body.get("owner").and_then(Value::as_str).map(String::from),
        note: body.get("note").and_then(Value::as_str).map(String::from),
        source_run: body
            .get("source_run")
            .and_then(Value::as_str)
            .map(String::from),
        updated_at: iso_now(),
        title: body.get("title").and_then(Value::as_str).map(String::from),
        severity: body
            .get("severity")
            .and_then(Value::as_str)
            .map(String::from),
        tool: body.get("tool").and_then(Value::as_str).map(String::from),
    };
    let saved = triage.upsert(entry).map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(saved).unwrap_or_default())
}

pub fn runs(root: &Path) -> Value {
    let Ok((_, runs, _)) = open_stores(root) else {
        return json!({ "runs": [], "diff": {} });
    };
    let list = runs.list_summaries().unwrap_or_default();
    let mut diff = json!({});
    if list.len() >= 2 {
        if let (Ok(a), Ok(b)) = (runs.load(&list[1].id), runs.load(&list[0].id)) {
            diff = serde_json::to_value(compare_runs(&a, &b)).unwrap_or_default();
        }
    }
    json!({ "runs": list, "diff": diff })
}

pub fn run_detail(root: &Path, id: &str) -> Result<Value, String> {
    if let Some(job) = hub().jobs.lock().unwrap().get(id).cloned() {
        return Ok(serde_json::to_value(job).unwrap_or_default());
    }
    let (_, runs, _) = open_stores(root)?;
    let r = runs.load(id).map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(r).unwrap_or_default())
}

pub fn compare(root: &Path, base: &str, head: &str) -> Result<Value, String> {
    let (_, runs, _) = open_stores(root)?;
    let b = runs.load(base).map_err(|e| e.to_string())?;
    let h = runs.load(head).map_err(|e| e.to_string())?;
    let cmp = compare_runs(&b, &h);
    Ok(json!({
        "compare": cmp,
        "base": b.summary(),
        "head": h.summary(),
    }))
}

pub fn start_scan(root: &Path, body: &Value) -> Result<Value, String> {
    let h = hub();
    if h.busy.swap(true, Ordering::SeqCst) {
        return Err("scan already running".into());
    }
    h.cancel.store(false, Ordering::SeqCst);

    let profile = body
        .get("profile")
        .and_then(Value::as_str)
        .unwrap_or("full-nexus")
        .to_string();
    let preset = body.get("preset").and_then(Value::as_str).map(String::from);
    let image = body.get("image").and_then(Value::as_str).map(String::from);
    let explicit_runners: Option<Vec<String>> = body
        .get("runners")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .filter(|v: &Vec<String>| !v.is_empty());

    let runners: Vec<String> = if let Some(list) = explicit_runners {
        list
    } else if let Some(ref p) = preset {
        preset_runners(p).into_iter().map(String::from).collect()
    } else if profile == "clamav-quick" {
        preset_runners("malware")
            .into_iter()
            .map(String::from)
            .collect()
    } else if profile == "clamav-full" {
        preset_runners("clamav-full")
            .into_iter()
            .map(String::from)
            .collect()
    } else if profile == "host-sec" {
        preset_runners("host")
            .into_iter()
            .map(String::from)
            .collect()
    } else {
        preset_runners("quick")
            .into_iter()
            .map(String::from)
            .collect()
    };

    if let Some(ref img) = image {
        if runners.iter().any(|r| r == "trivy") {
            if let Err(e) = verify_image_digest(img) {
                h.busy.store(false, Ordering::SeqCst);
                return Err(e);
            }
        }
    }

    let run_id = new_run_id();
    let root_c = root.to_path_buf();
    let rid = run_id.clone();
    let prof = profile.clone();
    std::thread::spawn(move || {
        let _ = execute_scan_job(&root_c, &rid, &prof, &runners, image.as_deref());
        hub().busy.store(false, Ordering::SeqCst);
    });

    Ok(json!({
        "ok": true,
        "run_id": run_id,
        "status": "queued",
        "profile": profile,
    }))
}

fn execute_scan_sync(
    root: &Path,
    profile: &str,
    runners: Vec<String>,
    image: Option<&str>,
) -> Result<StoredRun, String> {
    let rid = new_run_id();
    execute_scan_job(root, &rid, profile, &runners, image)
}

fn execute_scan_job(
    root: &Path,
    run_id: &str,
    profile: &str,
    runners: &[String],
    image: Option<&str>,
) -> Result<StoredRun, String> {
    let (pack, runs, _) = open_stores(root)?;
    let roots = pack
        .resolve_profile_roots(root, profile)
        .map_err(|e| e.to_string())?;
    let started = Instant::now();
    let started_at = iso_now();

    let stub = StoredRun {
        id: run_id.to_string(),
        status: JobStatus::Running,
        profile: profile.to_string(),
        roots: roots.iter().map(|p| p.display().to_string()).collect(),
        runners: runners.to_vec(),
        started_at: started_at.clone(),
        finished_at: None,
        duration_ms: 0,
        gate_ok: true,
        gate_reasons: vec![],
        findings: vec![],
        counts: FindingCounts::from_findings(&[]),
        runner_meta: runners
            .iter()
            .map(|id| {
                let info = runner_info(id);
                RunnerExecMeta {
                    id: id.clone(),
                    label: info.label.to_string(),
                    status: JobStatus::Queued,
                    finding_count: 0,
                    duration_ms: 0,
                    error: None,
                    available: info.available,
                }
            })
            .collect(),
        error: None,
        progress_pct: Some(1),
        current_runner: None,
        last_green_delta: None,
    };
    hub().jobs.lock().unwrap().insert(run_id.to_string(), stub.clone());
    let _ = runs.save(&stub);

    let total = runners.len().max(1);
    let mut all_findings: Vec<Finding> = Vec::new();
    let mut meta: Vec<RunnerExecMeta> = Vec::new();

    for (i, id) in runners.iter().enumerate() {
        if hub().cancel.load(Ordering::SeqCst) {
            let (gate_ok, gate_reasons) = pack.evaluate_gate(&all_findings);
            let mut run = StoredRun {
                id: run_id.to_string(),
                status: JobStatus::Cancelled,
                profile: profile.to_string(),
                roots: roots.iter().map(|p| p.display().to_string()).collect(),
                runners: runners.to_vec(),
                started_at,
                finished_at: Some(iso_now()),
                duration_ms: started.elapsed().as_millis() as u64,
                gate_ok,
                gate_reasons,
                counts: FindingCounts::from_findings(&all_findings),
                findings: all_findings,
                runner_meta: meta,
                error: Some("cancelled".into()),
                progress_pct: Some(100),
                current_runner: None,
                last_green_delta: None,
            };
            run.last_green_delta = runs.compute_delta_for_run(&run).ok().flatten();
            hub().jobs.lock().unwrap().insert(run_id.to_string(), run.clone());
            let _ = runs.save(&run);
            return Ok(run);
        }

        let base_pct = ((i * 100) / total) as u8;
        let init_live = LiveScanTelemetry {
            run_id: run_id.to_string(),
            pid: std::process::id(),
            child_pid: None,
            status: JobStatus::Running,
            profile: profile.to_string(),
            current_runner: Some(id.clone()),
            current_file: roots.first().map(|p| p.display().to_string()),
            files_scanned: i as u64,
            bytes_read: 0,
            read_mbps: 0.0,
            rss_bytes: 0,
            cpu_pct: 0.0,
            elapsed_secs: started.elapsed().as_secs(),
            progress_pct: base_pct,
            engine_guard_paused: false,
            is_external_root_sweep: false,
            log_path: None,
            recent_log_lines: vec![format!("Running {id} on profile {profile}...")],
            updated_at_ms: unix_now_ms(),
        };
        let _ = runs.save_live_telemetry(&init_live);

        let runs_cb = runs.clone();
        let rid_cb = run_id.to_string();
        let prof_cb = profile.to_string();
        let cb: TelemetryCallback = Arc::new(move |sample: &LiveScanTelemetry| {
            let mut l = sample.clone();
            l.run_id = rid_cb.clone();
            l.profile = prof_cb.clone();
            l.progress_pct = base_pct.saturating_add(5).min(98);
            let _ = runs_cb.save_live_telemetry(&l);
        });

        let results = scan_selected_with_telemetry(
            root,
            &roots,
            &[id.as_str()],
            &pack.ci.forbidden_path_markers,
            image,
            pack.schedule.pause_on_engine_active,
            Some(cb),
        );
        let (_, res) = results.into_iter().next().unwrap_or((
            id.clone(),
            RunnerResult {
                findings: vec![],
                available: false,
                error: Some("no result".into()),
                duration_ms: 0,
            },
        ));
        let info = runner_info(id);
        let status = if !res.available {
            JobStatus::Completed
        } else if res.error.is_some() && res.findings.is_empty() {
            JobStatus::Failed
        } else {
            JobStatus::Completed
        };

        if res.available && matches!(id.as_str(), "osv-scanner" | "cargo-audit" | "clamav-quick" | "clamav-full") {
            let _ = runs.record_advisory_refresh(id, res.error.is_none(), info.version.clone());
        }

        meta.push(RunnerExecMeta {
            id: id.clone(),
            label: info.label.to_string(),
            status,
            finding_count: res.findings.len(),
            duration_ms: res.duration_ms,
            error: res.error.clone(),
            available: res.available,
        });
        all_findings.extend(res.findings);
    }

    let mut seen = std::collections::BTreeSet::new();
    all_findings.retain(|f| seen.insert(f.id.clone()));
    pack.apply_suppressions(&mut all_findings);
    let (gate_ok, gate_reasons) = pack.evaluate_gate(&all_findings);
    let mut run = StoredRun {
        id: run_id.to_string(),
        status: JobStatus::Completed,
        profile: profile.to_string(),
        roots: roots.iter().map(|p| p.display().to_string()).collect(),
        runners: runners.to_vec(),
        started_at,
        finished_at: Some(iso_now()),
        duration_ms: started.elapsed().as_millis() as u64,
        gate_ok,
        gate_reasons,
        counts: FindingCounts::from_findings(&all_findings),
        findings: all_findings,
        runner_meta: meta,
        error: None,
        progress_pct: Some(100),
        current_runner: None,
        last_green_delta: None,
    };
    run.last_green_delta = runs.compute_delta_for_run(&run).ok().flatten();
    hub().jobs.lock().unwrap().insert(run_id.to_string(), run.clone());
    let _ = runs.save(&run);

    let done_live = LiveScanTelemetry {
        run_id: run_id.to_string(),
        pid: std::process::id(),
        child_pid: None,
        status: JobStatus::Completed,
        profile: profile.to_string(),
        current_runner: None,
        current_file: None,
        files_scanned: runners.len() as u64,
        bytes_read: 0,
        read_mbps: 0.0,
        rss_bytes: 0,
        cpu_pct: 0.0,
        elapsed_secs: started.elapsed().as_secs(),
        progress_pct: 100,
        engine_guard_paused: false,
        is_external_root_sweep: false,
        log_path: None,
        recent_log_lines: vec![format!(
            "Completed {} ({}) in {} ms — gate_ok={} open={}",
            run_id, profile, run.duration_ms, run.gate_ok, run.counts.open
        )],
        updated_at_ms: unix_now_ms(),
    };
    let _ = runs.save_live_telemetry(&done_live);
    let _ = write_wordkeep_telemetry_provider(root, &runs, None);
    Ok(run)
}

pub fn pause_scan(root: &Path, pause: bool) -> Value {
    let Ok((_, runs, _)) = open_stores(root) else {
        return json!({ "ok": false, "paused": pause });
    };
    let mut toggled = false;
    if let Some(mut live) = runs.load_live_telemetry() {
        if let Some(cpid) = live.child_pid {
            if !live.is_external_root_sweep && set_process_paused(cpid, pause) {
                toggled = true;
                live.status = if pause {
                    JobStatus::PausedForEngine
                } else {
                    JobStatus::Running
                };
                live.engine_guard_paused = pause;
                live.updated_at_ms = unix_now_ms();
                let _ = runs.save_live_telemetry(&live);
            }
        }
    }
    json!({ "ok": toggled, "paused": pause })
}

pub fn cancel_scan(root: &Path, id: Option<&str>) -> Value {
    hub().cancel.store(true, Ordering::SeqCst);
    if let Ok((_, runs, _)) = open_stores(root) {
        if let Some(live) = runs.load_live_telemetry() {
            if let Some(cpid) = live.child_pid {
                if !live.is_external_root_sweep {
                    let _ = Command::new("kill")
                        .args(["-TERM", &cpid.to_string()])
                        .status();
                }
            }
        }
    }
    json!({ "ok": true, "run_id": id })
}

pub fn schedule_get(root: &Path) -> Value {
    let Ok((pack, runs, _)) = open_stores(root) else {
        return json!({ "error": "failed to load schedule policy" });
    };
    let receipts = runs.load_advisory_receipts();
    let stale_limit = pack.schedule.stale_advisory_db_hours;
    json!({
        "schedule": pack.schedule,
        "timers": all_timers(),
        "advisory_receipts": receipts,
        "advisory_stale": receipts.is_stale(stale_limit),
        "user_install_cmd": "bash tools/bellator/scripts/install_bellator_timers_user.sh",
        "root_opt_in_cmd": "sudo bash tools/bellator/scripts/install_bellator_root_sweep_opt_in.sh",
    })
}

pub fn schedule_toggle(root: &Path, body: &Value) -> Result<Value, String> {
    let (pack, _, _) = open_stores(root)?;
    let mut sched = pack.schedule.clone();
    if let Some(v) = body.get("enabled").and_then(Value::as_bool) {
        sched.enabled = v;
    }
    if let Some(v) = body.get("daily_quick_clamav").and_then(Value::as_bool) {
        sched.daily_quick_clamav = v;
    }
    if let Some(v) = body.get("weekly_full_clamav").and_then(Value::as_bool) {
        sched.weekly_full_clamav = v;
    }
    if let Some(v) = body.get("daily_host_audit").and_then(Value::as_bool) {
        sched.daily_host_audit = v;
    }
    if let Some(v) = body.get("daily_advisory_refresh").and_then(Value::as_bool) {
        sched.daily_advisory_refresh = v;
    }
    if let Some(v) = body.get("pause_on_engine_active").and_then(Value::as_bool) {
        sched.pause_on_engine_active = v;
    }
    pack.save_schedule(&sched).map_err(|e| e.to_string())?;
    Ok(json!({
        "ok": true,
        "schedule": sched,
        "timers": all_timers(),
    }))
}

pub fn schedule_install_user(root: &Path) -> Result<Value, String> {
    let script = root.join("tools/bellator/scripts/install_bellator_timers_user.sh");
    let out = Command::new("bash")
        .arg(&script)
        .output()
        .map_err(|e| format!("run installer: {e}"))?;
    Ok(json!({
        "ok": out.status.success(),
        "stdout": String::from_utf8_lossy(&out.stdout),
        "stderr": String::from_utf8_lossy(&out.stderr),
        "timers": all_timers(),
    }))
}

pub fn policy_get(root: &Path) -> Value {
    let Ok((pack, _, _)) = open_stores(root) else {
        return json!({ "error": "failed to load policy" });
    };
    let expired: Vec<_> = pack
        .expired_suppressions()
        .into_iter()
        .cloned()
        .collect();
    json!({
        "root": pack.root,
        "ci": pack.ci,
        "host": pack.host,
        "schedule": pack.schedule,
        "suppressions": pack.suppressions,
        "expired": expired,
        "dir": pack.dir,
        "draft_path": pack.dir.join("suppressions.draft.toml").display().to_string(),
    })
}

pub fn suppressions_draft(root: &Path, body: &Value) -> Result<Value, String> {
    let (pack, _, _) = open_stores(root)?;
    let reason = body
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let owner = body
        .get("owner")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let expires = body
        .get("expires")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let finding_id = body
        .get("finding_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let rule_id = body
        .get("rule_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());

    if reason.is_empty() || owner.is_empty() || expires.is_empty() {
        return Err("reason, owner, and expires are required".into());
    }
    if finding_id.is_none() && rule_id.is_none() {
        return Err("finding_id or rule_id is required".into());
    }
    let path = pack.dir.join("suppressions.draft.toml");
    let mut block = String::from("\n[[suppress]]\n");
    if let Some(id) = finding_id {
        block.push_str(&format!("finding_id = \"{}\"\n", id.replace('"', "")));
    }
    if let Some(rule) = rule_id {
        block.push_str(&format!("rule_id = \"{}\"\n", rule.replace('"', "")));
    }
    block.push_str(&format!(
        "reason = \"{}\"\nowner = \"{}\"\nexpires = \"{}\"\n",
        reason.replace('"', ""),
        owner.replace('"', ""),
        expires.replace('"', "")
    ));
    use std::fs::OpenOptions;
    use std::io::Write;
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    f.write_all(block.as_bytes()).map_err(|e| e.to_string())?;
    Ok(json!({
        "ok": true,
        "draft_path": path.display().to_string(),
        "snippet": block,
        "note": "Draft appended — commit tools/bellator/policy/suppressions.toml after review."
    }))
}

pub fn explain(root: &Path, query: &str) -> Value {
    let store = open_stores(root).ok().map(|(_, r, _)| r);
    let card = explain_finding(query, store.as_ref());
    serde_json::to_value(card).unwrap_or_default()
}

pub fn sync_defects(root: &Path, run_id: Option<&str>) -> Result<Value, String> {
    let (_, runs, _) = open_stores(root)?;
    let run = if let Some(id) = run_id.filter(|s| !s.is_empty()) {
        runs.load(id).map_err(|e| e.to_string())?
    } else {
        match runs.latest().map_err(|e| e.to_string())? {
            Some(r) => r,
            None => return Ok(json!({ "ok": true, "upserted": 0, "note": "No runs available" })),
        }
    };
    let n = sync_findings_to_wordkeep_defects(root, &run.findings).map_err(|e| e.to_string())?;
    Ok(json!({
        "ok": true,
        "run_id": run.id,
        "upserted": n,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bellator_explain_and_schedule_load_in_wordkeep() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
        if root.join("tools/bellator/policy/bellator.toml").is_file() {
            let sched = schedule_get(&root);
            assert!(sched.get("schedule").is_some());
            let exp = explain(&root, "docker.socket.mount");
            assert_eq!(
                exp.get("rule_id").and_then(Value::as_str),
                Some("docker.socket.mount")
            );
        }
    }
}
