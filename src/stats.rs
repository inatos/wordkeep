//! Token-savings telemetry.
//!
//! Every tool reports two numbers per call: `baseline_tokens` - roughly what the
//! agent would have spent reading the raw material itself (source files, docs, a
//! trace CSV) - and `returned_tokens`, the size of the distilled answer
//! wordkeep actually emitted. The difference is the estimated context avoided.
//! Aggregates persist to `savings.json` in the shared cache dir; per-call events
//! append to `{workspace}/events.jsonl`. A `~4 chars/token` heuristic is used
//! throughout, matching the budgeting the other tools already do.
//!
//! Alongside the running totals each tool keeps high-water marks, per-call latency
//! (microseconds), outcome counters, bytes/cache stats, and a capped in-memory
//! event ring for the dashboard. On-disk schema is `version: 5`; v2–v4 files load
//! fine (missing fields default to zero / empty / derive us from ms).

use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::cache;
use crate::workspace;

const FILE: &str = "savings.json";
const EVENTS_FILE: &str = "events.jsonl";

/// Cap on the rolling per-call event log kept in memory for the dashboard.
const EVENT_CAP: usize = 200;

/// Returned tokens at or below this are tagged `low_yield` (one-line empty-ish answers).
const LOW_YIELD_FLOOR: u64 = 25;

/// Debounce: flush aggregates at most this often, or when this many events pend.
const FLUSH_INTERVAL: Duration = Duration::from_secs(2);
const FLUSH_EVENT_THRESHOLD: u64 = 25;

/// Improvement-signal thresholds - only flag genuinely actionable patterns.
const SLOW_MS_FLOOR: u64 = 50;
const TRUNC_RATE_PCT: u64 = 15;
const ERROR_RATE_PCT: u64 = 10;
const ERROR_MIN_CALLS: u64 = 5;
const NET_NEGATIVE_BASELINE_FLOOR: u64 = 500;

/// Dashboard / health: low-yield only when rate and sample size clear these.
#[allow(dead_code)] // used by dashboard + unit tests
pub const LOW_YIELD_RATE_PCT: u64 = 25;
#[allow(dead_code)]
pub const LOW_YIELD_MIN_CALLS: u64 = 10;

thread_local! {
    static PENDING: RefCell<Option<RecordMeta>> = const { RefCell::new(None) };
}

/// Extra fields for [`record_ext`]; [`record`] fills tokens only.
#[derive(Debug, Clone, Default)]
pub struct RecordMeta {
    pub baseline: u64,
    pub returned: u64,
    pub bytes_read: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub reason: Option<String>,
}

/// Classified tool-call outcome (persisted as a snake_case string).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Error,
    Truncated,
    NotFound,
    Invalid,
    Empty,
    LowYield,
    Ok,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Error => "error",
            Outcome::Truncated => "truncated",
            Outcome::NotFound => "not_found",
            Outcome::Invalid => "invalid",
            Outcome::Empty => "empty",
            Outcome::LowYield => "low_yield",
            Outcome::Ok => "ok",
        }
    }
}

#[derive(Default, Clone)]
struct Tool {
    calls: u64,
    baseline_tokens: u64,
    returned_tokens: u64,
    peak_baseline: u64,
    peak_returned: u64,
    peak_saved: u64,
    last_ts: u64,
    total_us: u64,
    peak_us: u64,
    trunc_count: u64,
    error_count: u64,
    low_yield_count: u64,
    bytes_read: u64,
    cache_hits: u64,
    cache_misses: u64,
    invalid_count: u64,
    not_found_count: u64,
}

impl Tool {
    fn observe(
        &mut self,
        baseline: u64,
        returned: u64,
        elapsed_us: u64,
        outcome: Outcome,
        ts: u64,
        meta: &RecordMeta,
    ) {
        self.calls += 1;
        self.baseline_tokens += baseline;
        self.returned_tokens += returned;
        self.peak_baseline = self.peak_baseline.max(baseline);
        self.peak_returned = self.peak_returned.max(returned);
        self.peak_saved = self.peak_saved.max(baseline.saturating_sub(returned));
        self.last_ts = ts;
        self.total_us += elapsed_us;
        self.peak_us = self.peak_us.max(elapsed_us);
        self.bytes_read += meta.bytes_read;
        self.cache_hits += meta.cache_hits;
        self.cache_misses += meta.cache_misses;
        match outcome {
            Outcome::Truncated => self.trunc_count += 1,
            Outcome::Error => self.error_count += 1,
            Outcome::LowYield => self.low_yield_count += 1,
            Outcome::NotFound => self.not_found_count += 1,
            Outcome::Invalid => self.invalid_count += 1,
            Outcome::Empty | Outcome::Ok => {}
        }
    }

    fn total_ms(&self) -> u64 {
        self.total_us / 1000
    }

    fn peak_ms(&self) -> u64 {
        self.peak_us / 1000
    }

    fn avg_ms(&self) -> u64 {
        self.total_ms().checked_div(self.calls).unwrap_or(0)
    }

    fn to_json(&self) -> Value {
        json!({
            "calls": self.calls,
            "baseline_tokens": self.baseline_tokens,
            "returned_tokens": self.returned_tokens,
            "peak_baseline": self.peak_baseline,
            "peak_returned": self.peak_returned,
            "peak_saved": self.peak_saved,
            "last_ts": self.last_ts,
            "total_us": self.total_us,
            "peak_us": self.peak_us,
            "total_ms": self.total_ms(),
            "peak_ms": self.peak_ms(),
            "trunc_count": self.trunc_count,
            "error_count": self.error_count,
            "low_yield_count": self.low_yield_count,
            "bytes_read": self.bytes_read,
            "cache_hits": self.cache_hits,
            "cache_misses": self.cache_misses,
            "invalid_count": self.invalid_count,
            "not_found_count": self.not_found_count,
        })
    }
}

#[derive(Clone)]
pub struct Event {
    pub ts: u64,
    pub tool: String,
    pub baseline: u64,
    pub returned: u64,
    pub elapsed_us: u64,
    pub outcome: String,
    pub reason: Option<String>,
    pub workspace_id: String,
    pub bytes_read: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub wordkeep_version: String,
}

impl Event {
    #[allow(dead_code)] // used by dashboard EventLog mapping
    pub fn elapsed_ms(&self) -> u64 {
        self.elapsed_us / 1000
    }

    fn to_jsonl(&self) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("ts".into(), json!(self.ts));
        m.insert("tool".into(), json!(self.tool));
        m.insert("baseline".into(), json!(self.baseline));
        m.insert("returned".into(), json!(self.returned));
        m.insert("elapsed_us".into(), json!(self.elapsed_us));
        m.insert("outcome".into(), json!(self.outcome));
        m.insert("workspace_id".into(), json!(self.workspace_id));
        m.insert("bytes_read".into(), json!(self.bytes_read));
        m.insert("cache_hits".into(), json!(self.cache_hits));
        m.insert("cache_misses".into(), json!(self.cache_misses));
        m.insert("wordkeep_version".into(), json!(self.wordkeep_version));
        if let Some(ref r) = self.reason {
            m.insert("reason".into(), json!(r));
        }
        Value::Object(m)
    }
}

struct Store {
    since: u64,
    tools: BTreeMap<String, Tool>,
    events: VecDeque<Event>,
    /// Aggregates dirty since last savings.json write.
    pending_flush: u64,
    last_flush: Instant,
    /// Last workspace id written into savings.json (for the dashboard process).
    saved_workspace_id: String,
}

static STATS: OnceLock<Mutex<Store>> = OnceLock::new();
static REGISTRY: OnceLock<Vec<&'static str>> = OnceLock::new();
static WORKSPACE_ID: OnceLock<String> = OnceLock::new();
static WORKSPACE_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Register all MCP tool names (for never-called insights in `stats`).
/// Also prunes aggregate keys for tools no longer on the surface and clears
/// known false net-negative baselines from the old constant-64 era.
pub fn init_registry(names: Vec<&'static str>) {
    let _ = REGISTRY.set(names);
    if let Ok(mut s) = cell().lock() {
        if s.prune_stale_registry() {
            s.save();
        }
    }
}

/// Bind telemetry events to a workspace (call once from `main`).
pub fn set_workspace_root(root: &Path) {
    let _ = WORKSPACE_ROOT.set(root.to_path_buf());
    let _ = WORKSPACE_ID.set(workspace::workspace_id(root));
}

fn workspace_id_str() -> String {
    WORKSPACE_ID.get().cloned().unwrap_or_default()
}

fn workspace_root() -> Option<&'static Path> {
    WORKSPACE_ROOT.get().map(PathBuf::as_path)
}

fn registry() -> &'static [&'static str] {
    REGISTRY.get().map(|v| v.as_slice()).unwrap_or(&[])
}

/// Tools that historically recorded distilled=`64` per call (false net-negatives).
fn legacy_flat_baseline_tool(name: &str) -> bool {
    matches!(
        name,
        "izakaya_check_in"
            | "izakaya_check_out"
            | "izakaya_status"
            | "izakaya_update"
            | "izakaya_advise"
            | "mas_read"
            | "mas_status"
            | "session_handoff"
            | "session_pressure"
    )
}

fn cell() -> &'static Mutex<Store> {
    STATS.get_or_init(|| Mutex::new(Store::load()))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn wordkeep_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

impl Store {
    fn empty() -> Store {
        Store {
            since: now_secs(),
            tools: BTreeMap::new(),
            events: VecDeque::new(),
            pending_flush: 0,
            last_flush: Instant::now(),
            saved_workspace_id: String::new(),
        }
    }

    fn observe(
        &mut self,
        tool: &str,
        elapsed_us: u64,
        outcome: Outcome,
        ts: u64,
        workspace_id: &str,
        meta: &RecordMeta,
    ) {
        self.tools.entry(tool.to_string()).or_default().observe(
            meta.baseline,
            meta.returned,
            elapsed_us,
            outcome,
            ts,
            meta,
        );
        let ev = Event {
            ts,
            tool: tool.to_string(),
            baseline: meta.baseline,
            returned: meta.returned,
            elapsed_us,
            outcome: outcome.as_str().to_string(),
            reason: meta.reason.clone(),
            workspace_id: workspace_id.to_string(),
            bytes_read: meta.bytes_read,
            cache_hits: meta.cache_hits,
            cache_misses: meta.cache_misses,
            wordkeep_version: wordkeep_version(),
        };
        self.events.push_back(ev.clone());
        while self.events.len() > EVENT_CAP {
            self.events.pop_front();
        }
        append_event_jsonl(&ev);
        self.pending_flush += 1;
    }

    fn needs_flush(&self) -> bool {
        self.pending_flush > 0
            && (self.pending_flush >= FLUSH_EVENT_THRESHOLD
                || self.last_flush.elapsed() >= FLUSH_INTERVAL)
    }

    fn flush_aggregates(&mut self) {
        if self.pending_flush == 0 {
            return;
        }
        self.save();
        self.pending_flush = 0;
        self.last_flush = Instant::now();
    }

    /// Drop tools no longer registered and clear old flat-64 false net-negatives.
    /// Returns true when the store was mutated.
    fn prune_stale_registry(&mut self) -> bool {
        let reg: std::collections::HashSet<&str> = registry().iter().copied().collect();
        let mut dirty = false;
        if !reg.is_empty() {
            let before = self.tools.len();
            self.tools.retain(|name, _| reg.contains(name.as_str()));
            if self.tools.len() != before {
                dirty = true;
            }
        }
        for (name, t) in self.tools.iter_mut() {
            if !legacy_flat_baseline_tool(name) {
                continue;
            }
            if t.calls > 0 && t.baseline_tokens == t.calls.saturating_mul(64) {
                t.baseline_tokens = 0;
                dirty = true;
            }
        }
        dirty
    }

    fn from_value(v: &Value) -> Store {
        let mut s = Store::empty();
        let Value::Object(o) = v else {
            return s;
        };
        if let Some(t) = o.get("since").and_then(Value::as_u64) {
            s.since = t;
        }
        if let Some(wid) = o.get("workspace_id").and_then(Value::as_str) {
            s.saved_workspace_id = wid.to_string();
        }
        if let Some(Value::Object(tools)) = o.get("tools") {
            for (name, tv) in tools {
                s.tools.insert(name.clone(), tool_from_value(tv));
            }
        }
        // v2–v4 may embed a rolling events array; keep it as the in-memory ring.
        if let Some(Value::Array(evs)) = o.get("events") {
            for ev in evs {
                if let Some(parsed) = event_from_value(ev) {
                    s.events.push_back(parsed);
                }
            }
            while s.events.len() > EVENT_CAP {
                s.events.pop_front();
            }
        }
        s
    }

    fn load() -> Store {
        match std::fs::read(cache::dir().join(FILE)) {
            Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
                .map(|v| Store::from_value(&v))
                .unwrap_or_else(|_| Store::empty()),
            Err(_) => Store::empty(),
        }
    }

    fn save(&self) {
        let tools: serde_json::Map<String, Value> = self
            .tools
            .iter()
            .map(|(k, t)| (k.clone(), t.to_json()))
            .collect();
        let wid = if !workspace_id_str().is_empty() {
            workspace_id_str()
        } else {
            self.saved_workspace_id.clone()
        };
        // Keep a capped events ring in savings.json so the wiki GUI (and
        // standalone dashboard) can show recent activity + outcome reasons
        // without needing the live MCP process or workspace jsonl path.
        let events: Vec<Value> = self.events.iter().map(Event::to_jsonl).collect();
        let doc = json!({
            "version": 5,
            "since": self.since,
            "workspace_id": wid,
            "tools": Value::Object(tools),
            "events": events,
        });
        let dir = cache::dir();
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(FILE);
        let tmp = path.with_extension("tmp");
        if let Ok(bytes) = serde_json::to_vec(&doc) {
            if std::fs::write(&tmp, &bytes).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }
}

fn tool_from_value(tv: &Value) -> Tool {
    let total_us = match field(tv, "total_us") {
        0 => field(tv, "total_ms").saturating_mul(1000),
        u => u,
    };
    let peak_us = match field(tv, "peak_us") {
        0 => field(tv, "peak_ms").saturating_mul(1000),
        u => u,
    };
    Tool {
        calls: field(tv, "calls"),
        baseline_tokens: field(tv, "baseline_tokens"),
        returned_tokens: field(tv, "returned_tokens"),
        peak_baseline: field(tv, "peak_baseline"),
        peak_returned: field(tv, "peak_returned"),
        peak_saved: field(tv, "peak_saved"),
        last_ts: field(tv, "last_ts"),
        total_us,
        peak_us,
        trunc_count: field(tv, "trunc_count"),
        error_count: field(tv, "error_count"),
        low_yield_count: field(tv, "low_yield_count"),
        bytes_read: field(tv, "bytes_read"),
        cache_hits: field(tv, "cache_hits"),
        cache_misses: field(tv, "cache_misses"),
        invalid_count: field(tv, "invalid_count"),
        not_found_count: field(tv, "not_found_count"),
    }
}

fn event_from_value(ev: &Value) -> Option<Event> {
    let tool = ev.get("tool").and_then(Value::as_str).unwrap_or("");
    if tool.is_empty() {
        return None;
    }
    let elapsed_us = match field(ev, "elapsed_us") {
        0 => field(ev, "elapsed_ms").saturating_mul(1000),
        u => u,
    };
    Some(Event {
        ts: field(ev, "ts"),
        tool: tool.to_string(),
        baseline: field(ev, "baseline"),
        returned: field(ev, "returned"),
        elapsed_us,
        outcome: ev
            .get("outcome")
            .and_then(Value::as_str)
            .unwrap_or("ok")
            .to_string(),
        reason: ev.get("reason").and_then(Value::as_str).map(str::to_string),
        workspace_id: ev
            .get("workspace_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        bytes_read: field(ev, "bytes_read"),
        cache_hits: field(ev, "cache_hits"),
        cache_misses: field(ev, "cache_misses"),
        wordkeep_version: ev
            .get("wordkeep_version")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

fn field(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn append_event_jsonl(ev: &Event) {
    let Some(root) = workspace_root() else {
        return;
    };
    let Ok(dir) = workspace::ensure_workspace_dir(root) else {
        return;
    };
    let path = dir.join(EVENTS_FILE);
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    if let Ok(line) = serde_json::to_string(&ev.to_jsonl()) {
        let _ = writeln!(f, "{line}");
    }
}

fn read_events_jsonl(root: &Path) -> Vec<Event> {
    let path = workspace::workspace_dir(root).join(EVENTS_FILE);
    let Ok(file) = std::fs::File::open(&path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { continue };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(ev) = event_from_value(&v) {
            out.push(ev);
        }
    }
    out
}

/// Stash baseline/returned for the in-flight call; finalized by [`finish`].
pub fn record(_tool: &str, baseline_tokens: u64, returned_tokens: u64) {
    record_ext(
        _tool,
        RecordMeta {
            baseline: baseline_tokens,
            returned: returned_tokens,
            ..Default::default()
        },
    );
}

/// Stash extended call metadata for the in-flight call; finalized by [`finish`].
pub fn record_ext(_tool: &str, meta: RecordMeta) {
    PENDING.with(|p| *p.borrow_mut() = Some(meta));
}

fn looks_like_not_found(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("not found")
        || t.contains("no definition")
        || t.contains("unsupported")
        || t.contains("no such")
        || t.contains("cannot find")
        || t.contains("cannot read")
        || t.contains("no top-level")
        || t.contains("unknown symbol")
        || t.contains("no matches")
        || t.contains("no matching")
        || t.contains("no test file")
}

fn looks_like_invalid(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("is required")
        || t.contains("requires:")
        || t.contains("requires ")
        || t.contains("invalid ")
        || t.contains("unknown mode")
        || t.contains("must be")
        || t.contains("must provide")
        || t.contains("must end with")
        || t.contains("expected ")
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or(text)
}

/// Empty-result suffix lines that should count as not_found (body only — not headers).
fn empty_result_suffix_is_not_found(text: &str) -> bool {
    text.lines().skip(1).any(|line| {
        let t = line.trim().to_ascii_lowercase();
        t.starts_with("(no matching occurrences)") || t.starts_with("(no matching hooks")
    })
}

/// Classify a tool result for telemetry.
pub fn classify_outcome(result: &Result<String, String>, returned_tokens: u64) -> Outcome {
    // Validation / missing-arg Errs are agent misuse, not tool failures — keep
    // them off the error-prone health signal so real panics/IO stand out.
    if let Err(e) = result {
        if looks_like_invalid(e) {
            return Outcome::Invalid;
        }
        if looks_like_not_found(e) {
            return Outcome::NotFound;
        }
        return Outcome::Error;
    }
    let Ok(text) = result else {
        return Outcome::Error;
    };
    if text.contains("truncated by token_budget") || text.contains("more; raise") {
        return Outcome::Truncated;
    }
    // Ok responses: classify invalid/not_found from the header line only so
    // embedded docs/defects/source do not false-positive (defect_list, outline, …).
    let header = first_line(text);
    if looks_like_not_found(header) {
        return Outcome::NotFound;
    }
    if looks_like_invalid(header) {
        return Outcome::Invalid;
    }
    if empty_result_suffix_is_not_found(text) {
        return Outcome::NotFound;
    }
    if returned_tokens == 0 {
        return Outcome::Empty;
    }
    if returned_tokens <= LOW_YIELD_FLOOR {
        return Outcome::LowYield;
    }
    Outcome::Ok
}

/// Finalize telemetry for one instrumented tool call.
///
/// `elapsed_us` is wall time in **microseconds** (see `main::instrument`).
pub fn finish(tool: &str, elapsed_us: u64, result: &Result<String, String>) {
    let mut meta = PENDING.with(|p| p.borrow_mut().take()).unwrap_or_default();
    let outcome = classify_outcome(result, meta.returned);
    if meta.reason.is_none() {
        meta.reason = outcome_reason(result, outcome);
    }
    let now = now_secs();
    let ws = workspace_id_str();
    if let Ok(mut s) = cell().lock() {
        s.observe(tool, elapsed_us, outcome, now, &ws, &meta);
        if s.needs_flush() {
            s.flush_aggregates();
        }
    }
    let saved = meta.baseline.saturating_sub(meta.returned);
    let elapsed_ms = elapsed_us / 1000;
    eprintln!(
        "[wordkeep] {tool}: ~{} tok returned vs ~{} distilled \
         (avoided ~{saved}, {}%, {elapsed_ms}ms, {})",
        meta.returned,
        meta.baseline,
        pct_precise(saved, meta.baseline),
        outcome.as_str(),
    );
}

/// Short first-line detail for non-ok outcomes (error message, not-found hint, etc.).
fn outcome_reason(result: &Result<String, String>, outcome: Outcome) -> Option<String> {
    if matches!(outcome, Outcome::Ok) {
        return None;
    }
    let raw = match result {
        Err(e) => e.as_str(),
        Ok(t) => t.as_str(),
    };
    let line = raw.lines().next().unwrap_or(raw).trim();
    if line.is_empty() {
        return None;
    }
    const MAX: usize = 240;
    let mut chars = line.chars();
    let truncated: String = chars.by_ref().take(MAX).collect();
    if chars.next().is_some() {
        Some(format!("{truncated}…"))
    } else {
        Some(truncated)
    }
}

/// Events for a workspace since `since_ts` (exclusive lower bound when > 0).
///
/// Reads the append-only `{workspace}/events.jsonl` so callers see history beyond
/// the in-memory ring of 200 (fixes `session_pressure` under long sessions).
pub fn events_since(root: &Path, since_ts: u64) -> Vec<Event> {
    let wid = workspace::workspace_id(root);
    read_events_jsonl(root)
        .into_iter()
        .filter(|e| e.ts > since_ts && (e.workspace_id.is_empty() || e.workspace_id == wid))
        .collect()
}

/// `stats` tool handler: render cumulative savings + improvement signals.
pub fn report(args: &Value) -> Result<String, String> {
    let reset = args.get("reset").and_then(Value::as_bool).unwrap_or(false);
    let insights = args
        .get("insights")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let format = args.get("format").and_then(Value::as_str).unwrap_or("text");
    let workspace_filter = args
        .get("workspace")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let id = workspace_id_str();
            if id.is_empty() {
                None
            } else {
                Some(id)
            }
        });

    let mut s = cell()
        .lock()
        .map_err(|_| "stats lock poisoned".to_string())?;
    if reset {
        s.tools.clear();
        s.events.clear();
        s.since = now_secs();
        s.pending_flush = 0;
        s.save();
        s.last_flush = Instant::now();
        return Ok("wordkeep estimated context avoided - counters reset.".to_string());
    }
    // Ensure a fresh read of aggregates for long-lived servers.
    if s.needs_flush() {
        s.flush_aggregates();
    }
    let now = now_secs();
    let out = if format == "json" {
        render_json(
            s.since,
            now,
            &s.tools,
            &s.events,
            workspace_filter.as_deref(),
        )
    } else {
        let mut text = render(s.since, now, &s.tools);
        if insights {
            text.push_str(&render_insights(now, &s.tools, registry()));
        }
        text
    };
    // Avoid holding the lock while recording (record only sets TLS).
    drop(s);
    record("stats", 0, (out.len() / 4) as u64);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Dashboard snapshot API
// ---------------------------------------------------------------------------

#[cfg(feature = "dashboard")]
pub struct ToolStat {
    pub name: String,
    pub calls: u64,
    pub baseline_tokens: u64,
    pub returned_tokens: u64,
    pub peak_baseline: u64,
    pub peak_returned: u64,
    pub peak_saved: u64,
    pub last_ts: u64,
    pub total_ms: u64,
    pub peak_ms: u64,
    pub trunc_count: u64,
    pub error_count: u64,
    pub low_yield_count: u64,
}

#[cfg(feature = "dashboard")]
impl ToolStat {
    pub fn avg_ms(&self) -> u64 {
        self.total_ms.checked_div(self.calls).unwrap_or(0)
    }
}

#[cfg(feature = "dashboard")]
pub struct EventLog {
    pub ts: u64,
    pub tool: String,
    pub baseline: u64,
    pub returned: u64,
    pub elapsed_ms: u64,
    pub outcome: String,
}

#[cfg(feature = "dashboard")]
pub struct Snapshot {
    pub since: u64,
    pub now: u64,
    pub tools: Vec<ToolStat>,
    pub events: Vec<EventLog>,
}

#[cfg(feature = "dashboard")]
pub fn read_snapshot() -> Snapshot {
    // Prefer the live in-process store when the MCP server owns it; otherwise
    // reload from disk (dashboard subcommand): v2–v4 embed a rolling `events`
    // array in savings.json, and v5 keeps recent rows in workspace events.jsonl.
    let (since, tools, mut events, saved_wid) = if let Some(cell) = STATS.get() {
        if let Ok(s) = cell.lock() {
            (
                s.since,
                s.tools.clone(),
                s.events.iter().cloned().collect::<Vec<_>>(),
                s.saved_workspace_id.clone(),
            )
        } else {
            let s = Store::load();
            (
                s.since,
                s.tools,
                s.events.into_iter().collect(),
                s.saved_workspace_id,
            )
        }
    } else {
        let s = Store::load();
        (
            s.since,
            s.tools,
            s.events.into_iter().collect(),
            s.saved_workspace_id,
        )
    };

    if events.is_empty() {
        let wid = if !workspace_id_str().is_empty() {
            workspace_id_str()
        } else {
            saved_wid
        };
        if !wid.is_empty() {
            let path = cache::dir().join("workspaces").join(&wid).join(EVENTS_FILE);
            if let Ok(file) = std::fs::File::open(&path) {
                let mut loaded = Vec::new();
                for line in BufReader::new(file).lines() {
                    let Ok(line) = line else { continue };
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    if let Ok(v) = serde_json::from_str::<Value>(line) {
                        if let Some(ev) = event_from_value(&v) {
                            loaded.push(ev);
                        }
                    }
                }
                let start = loaded.len().saturating_sub(EVENT_CAP);
                events = loaded.split_off(start);
            }
        }
    }

    let tools: Vec<ToolStat> = tools
        .iter()
        .map(|(name, t)| ToolStat {
            name: name.clone(),
            calls: t.calls,
            baseline_tokens: t.baseline_tokens,
            returned_tokens: t.returned_tokens,
            peak_baseline: t.peak_baseline,
            peak_returned: t.peak_returned,
            peak_saved: t.peak_saved,
            last_ts: t.last_ts,
            total_ms: t.total_ms(),
            peak_ms: t.peak_ms(),
            trunc_count: t.trunc_count,
            error_count: t.error_count,
            low_yield_count: t.low_yield_count,
        })
        .collect();
    let events: Vec<EventLog> = events
        .iter()
        .map(|e| EventLog {
            ts: e.ts,
            tool: e.tool.clone(),
            baseline: e.baseline,
            returned: e.returned,
            elapsed_ms: e.elapsed_ms(),
            outcome: e.outcome.clone(),
        })
        .collect();
    Snapshot {
        since,
        now: now_secs(),
        tools,
        events,
    }
}

/// Whether a tool should appear on the dashboard low-yield health line.
#[allow(dead_code)] // dashboard + unit tests
pub fn is_notable_low_yield(calls: u64, low_yield_count: u64) -> bool {
    calls >= LOW_YIELD_MIN_CALLS && low_yield_count * 100 / calls >= LOW_YIELD_RATE_PCT
}

/// Whether a tool should be flagged net-negative (insights + dashboard).
pub fn is_net_negative(baseline_tokens: u64, returned_tokens: u64) -> bool {
    baseline_tokens >= NET_NEGATIVE_BASELINE_FLOOR && returned_tokens >= baseline_tokens
}

#[allow(dead_code)] // dashboard + unit tests
pub(crate) fn pct(saved: u64, baseline: u64) -> u64 {
    if baseline == 0 {
        0
    } else {
        (saved as f64 / baseline as f64 * 100.0).round() as u64
    }
}

/// Reduction % for display: exact `100` when fully avoided; otherwise one decimal
/// so values like 99.94% render as `99.9` instead of rounding up to `100`.
pub(crate) fn pct_precise(saved: u64, baseline: u64) -> String {
    if baseline == 0 {
        return "0".to_string();
    }
    if saved >= baseline {
        return "100".to_string();
    }
    let p = saved as f64 / baseline as f64 * 100.0;
    let rounded = format!("{p:.1}");
    if rounded == "100.0" {
        "99.9".to_string()
    } else {
        rounded
    }
}

/// Format a count for `stats` / dashboard display.
///
/// Below one million: grouped digits (`120,500`). At/above 1M / 1B / 1T / 1Q:
/// compact suffix form with up to two decimals (`3.25M`, `1.23B`).
pub(crate) fn commafy(n: u64) -> String {
    const MILLION: u64 = 1_000_000;
    const BILLION: u64 = 1_000_000_000;
    const TRILLION: u64 = 1_000_000_000_000;
    const QUADRILLION: u64 = 1_000_000_000_000_000;
    if n >= MILLION {
        let (div, suffix) = if n >= QUADRILLION {
            (QUADRILLION as f64, "Q")
        } else if n >= TRILLION {
            (TRILLION as f64, "T")
        } else if n >= BILLION {
            (BILLION as f64, "B")
        } else {
            (MILLION as f64, "M")
        };
        let value = n as f64 / div;
        let raw = format!("{value:.2}");
        let trimmed = raw.trim_end_matches('0').trim_end_matches('.');
        return format!("{trimmed}{suffix}");
    }
    let s = n.to_string();
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i != 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

pub(crate) fn elapsed(since: u64, now: u64) -> String {
    let secs = now.saturating_sub(since);
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let mins = (secs % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h ago")
    } else if hours > 0 {
        format!("{hours}h {mins}m ago")
    } else {
        format!("{mins}m ago")
    }
}

fn percentile_us(samples: &mut [u64], p: f64) -> Option<u64> {
    if samples.is_empty() {
        return None;
    }
    samples.sort_unstable();
    let idx = ((p / 100.0) * (samples.len() as f64 - 1.0)).round() as usize;
    Some(samples[idx.min(samples.len() - 1)])
}

fn render_json(
    since: u64,
    now: u64,
    tools: &BTreeMap<String, Tool>,
    events: &VecDeque<Event>,
    workspace_filter: Option<&str>,
) -> String {
    let tools_json: serde_json::Map<String, Value> = tools
        .iter()
        .map(|(k, t)| (k.clone(), t.to_json()))
        .collect();
    let mut samples: Vec<u64> = events
        .iter()
        .filter(|e| match workspace_filter {
            Some(w) => e.workspace_id.is_empty() || e.workspace_id == w,
            None => true,
        })
        .map(|e| e.elapsed_us)
        .collect();
    let p50 = percentile_us(&mut samples.clone(), 50.0);
    let p95 = percentile_us(&mut samples.clone(), 95.0);
    let p99 = percentile_us(&mut samples, 99.0);
    let (mut tb, mut tr) = (0u64, 0u64);
    for t in tools.values() {
        tb += t.baseline_tokens;
        tr += t.returned_tokens;
    }
    let avoided = tb.saturating_sub(tr);
    let doc = json!({
        "version": 5,
        "label": "estimated context avoided",
        "since": since,
        "now": now,
        "workspace": workspace_filter,
        "baseline_tokens": tb,
        "returned_tokens": tr,
        "avoided_tokens": avoided,
        "reduction_pct": pct_precise(avoided, tb),
        "tools": Value::Object(tools_json),
        "percentiles": {
            "elapsed_us": {
                "p50": p50,
                "p95": p95,
                "p99": p99,
            }
        },
    });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".into())
}

fn render(since: u64, now: u64, tools: &BTreeMap<String, Tool>) -> String {
    if tools.is_empty() {
        return "wordkeep estimated context avoided - no tool calls recorded yet.".to_string();
    }
    let (mut tc, mut tb, mut tr) = (0u64, 0u64, 0u64);
    let mut rows = String::new();
    for (name, t) in tools {
        let saved = t.baseline_tokens.saturating_sub(t.returned_tokens);
        rows.push_str(&format!(
            "  {:<16} {:>5} {:>8} {:>5} {:>4} {:>11} {:>11} {:>11} {:>5}%\n",
            name,
            t.calls,
            t.avg_ms(),
            t.trunc_count,
            t.error_count,
            commafy(t.baseline_tokens),
            commafy(t.returned_tokens),
            commafy(saved),
            pct_precise(saved, t.baseline_tokens),
        ));
        tc += t.calls;
        tb += t.baseline_tokens;
        tr += t.returned_tokens;
    }
    let tsaved = tb.saturating_sub(tr);
    format!(
        "wordkeep estimated context avoided - tracking since {}\n\n  {:<16} {:>5} {:>8} {:>5} {:>4} {:>11} {:>11} {:>11} {:>5}\n{}  {:<16} {:>5} {:>8} {:>5} {:>4} {:>11} {:>11} {:>11} {:>5}%\n\n(distilled = est. tokens to read raw material; returned = tokens emitted;\navoided = estimated context avoided; avg ms = mean wall time; trunc/err = outcome counts; ~4 chars/token.)",
        elapsed(since, now),
        "tool", "calls", "avg ms", "trunc", "err", "distilled", "returned", "avoided", "red%",
        rows,
        "TOTAL", tc, "-", "-", "-", commafy(tb), commafy(tr), commafy(tsaved), pct_precise(tsaved, tb),
    )
}

fn render_insights(now: u64, tools: &BTreeMap<String, Tool>, registry: &[&'static str]) -> String {
    if tools.is_empty() {
        return String::new();
    }
    let mut signals = String::new();

    let mut by_avg: Vec<(&str, u64)> = tools
        .iter()
        .filter(|(_, t)| t.calls > 0 && t.avg_ms() >= SLOW_MS_FLOOR)
        .map(|(n, t)| (n.as_str(), t.avg_ms()))
        .collect();
    by_avg.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    if let Some(top) = by_avg.first() {
        signals.push_str(&format!("slowest (avg ms): {} ({}ms avg)\n", top.0, top.1));
        for (name, ms) in by_avg.iter().skip(1).take(2) {
            signals.push_str(&format!("  also slow: {name} ({ms}ms avg)\n"));
        }
    }

    let mut by_trunc: Vec<(&str, u64, u64)> = tools
        .iter()
        .filter(|(_, t)| {
            t.calls > 0 && t.trunc_count > 0 && t.trunc_count * 100 / t.calls >= TRUNC_RATE_PCT
        })
        .map(|(n, t)| (n.as_str(), t.trunc_count, t.calls))
        .collect();
    by_trunc.sort_by(|a, b| {
        let ra = a.1 * 100 / a.2;
        let rb = b.1 * 100 / b.2;
        rb.cmp(&ra).then_with(|| a.0.cmp(b.0))
    });
    if let Some((name, trunc, calls)) = by_trunc.first() {
        signals.push_str(&format!(
            "high truncation: {name} ({trunc}/{calls} calls - raise token_budget or max)\n"
        ));
    }

    let err_tools: Vec<&str> = tools
        .iter()
        .filter(|(_, t)| {
            t.calls >= ERROR_MIN_CALLS && t.error_count * 100 / t.calls >= ERROR_RATE_PCT
        })
        .map(|(n, _)| n.as_str())
        .collect();
    if !err_tools.is_empty() {
        signals.push_str(&format!("error-prone: {}\n", err_tools.join(", ")));
    }

    let invalid_tools: Vec<&str> = tools
        .iter()
        .filter(|(_, t)| {
            t.calls >= ERROR_MIN_CALLS && t.invalid_count * 100 / t.calls >= ERROR_RATE_PCT
        })
        .map(|(n, _)| n.as_str())
        .collect();
    if !invalid_tools.is_empty() {
        signals.push_str(&format!("invalid-prone: {}\n", invalid_tools.join(", ")));
    }

    let not_found_tools: Vec<&str> = tools
        .iter()
        .filter(|(_, t)| {
            t.calls >= ERROR_MIN_CALLS && t.not_found_count * 100 / t.calls >= ERROR_RATE_PCT
        })
        .map(|(n, _)| n.as_str())
        .collect();
    if !not_found_tools.is_empty() {
        signals.push_str(&format!(
            "not-found-prone: {}\n",
            not_found_tools.join(", ")
        ));
    }

    let inverted: Vec<&str> = tools
        .iter()
        .filter(|(_, t)| is_net_negative(t.baseline_tokens, t.returned_tokens))
        .map(|(n, _)| n.as_str())
        .collect();
    if !inverted.is_empty() {
        signals.push_str(&format!(
            "net-negative (returned ≥ distilled): {}\n",
            inverted.join(", ")
        ));
    }

    let never: Vec<&str> = registry
        .iter()
        .copied()
        .filter(|name| !tools.contains_key(*name))
        .collect();
    if !never.is_empty() {
        signals.push_str(&format!("never called: {}\n", never.join(", ")));
    }

    let _ = now;
    if signals.is_empty() {
        return String::new();
    }
    format!("\n--- improvement signals ---\n{signals}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn pct_is_saturating_ratio() {
        assert_eq!(pct(0, 0), 0);
        assert_eq!(pct(50, 100), 50);
        assert_eq!(pct(97, 100), 97);
        assert_eq!(pct(100, 0), 0);
    }

    #[test]
    fn pct_precise_keeps_one_decimal_near_100() {
        assert_eq!(pct_precise(0, 0), "0");
        assert_eq!(pct_precise(100, 100), "100");
        assert_eq!(pct_precise(50, 100), "50.0");
        // 99.94% must not round to 100.
        assert_eq!(pct_precise(9_994, 10_000), "99.9");
        assert_eq!(pct_precise(9_995, 10_000), "99.9"); // {:.1} → 100.0 clamped
    }

    #[test]
    fn commafy_groups_thousands_and_abbreviates_large() {
        assert_eq!(commafy(0), "0");
        assert_eq!(commafy(42), "42");
        assert_eq!(commafy(1_000), "1,000");
        assert_eq!(commafy(120_500), "120,500");
        assert_eq!(commafy(999_999), "999,999");
        assert_eq!(commafy(1_000_000), "1M");
        assert_eq!(commafy(3_250_000), "3.25M");
        assert_eq!(commafy(3_200_000), "3.2M");
        assert_eq!(commafy(1_234_567_890), "1.23B");
        assert_eq!(commafy(3_250_000_000_000), "3.25T");
        assert_eq!(commafy(1_500_000_000_000_000), "1.5Q");
    }

    #[test]
    fn classify_outcome_tags() {
        assert_eq!(classify_outcome(&Err("x".into()), 100), Outcome::Error);
        assert_eq!(
            classify_outcome(&Err("symbol is required".into()), 0),
            Outcome::Invalid
        );
        assert_eq!(
            classify_outcome(&Err("mode pitfall requires: symptom, fix".into()), 0),
            Outcome::Invalid
        );
        assert_eq!(
            classify_outcome(&Ok("… (truncated by token_budget)\n".into()), 500),
            Outcome::Truncated
        );
        assert_eq!(
            classify_outcome(&Ok("… (+3 more; raise \"max\")\n".into()), 500),
            Outcome::Truncated
        );
        assert_eq!(
            classify_outcome(&Ok("outline - a.txt: unsupported file type".into()), 10),
            Outcome::NotFound
        );
        assert_eq!(
            classify_outcome(&Ok("symbol X: not found".into()), 40),
            Outcome::NotFound
        );
        assert_eq!(
            classify_outcome(
                &Ok(
                    "symbol_refs - \"X\": 0 def, 0 call, 0 ref\n(no matching occurrences)\n".into()
                ),
                23
            ),
            Outcome::NotFound
        );
        assert_eq!(
            classify_outcome(&Ok("file is required".into()), 5),
            Outcome::Invalid
        );
        assert_eq!(classify_outcome(&Ok("".into()), 0), Outcome::Empty);
        assert_eq!(
            classify_outcome(&Ok("no hits\n".into()), 10),
            Outcome::LowYield
        );
        assert_eq!(
            classify_outcome(&Ok("big answer".repeat(100)), 500),
            Outcome::Ok
        );
        assert_eq!(
            classify_outcome(
                &Ok(
                    "defect_list - 1 defect(s)\n\n[open] x — 1\n  summary: foo must be bar\n"
                        .into()
                ),
                200
            ),
            Outcome::Ok
        );
        assert_eq!(
            classify_outcome(
                &Ok("defect_list - 0 defect(s)\n\n(no matching defects)\n".into()),
                30
            ),
            Outcome::Ok
        );
        assert_eq!(
            classify_outcome(
                &Ok("session_handoff - session s status=open\n\nAcceptance: must be true\n".into()),
                500
            ),
            Outcome::Ok
        );
        assert_eq!(
            classify_outcome(
                &Ok(
                    "integration_hooks - query \"water\"\n\n(no matching hooks - try broader)\n"
                        .into()
                ),
                40
            ),
            Outcome::NotFound
        );
    }

    #[test]
    fn outcome_reason_uses_first_line_and_caps() {
        assert_eq!(
            outcome_reason(&Err("boom\nmore".into()), Outcome::Error).as_deref(),
            Some("boom")
        );
        assert_eq!(outcome_reason(&Ok("fine".into()), Outcome::Ok), None);
        assert_eq!(
            outcome_reason(&Ok("symbol X: not found\nextra".into()), Outcome::NotFound).as_deref(),
            Some("symbol X: not found")
        );
        let long = "x".repeat(300);
        let clipped = outcome_reason(&Err(long), Outcome::Error).unwrap();
        assert!(clipped.ends_with('…'));
        assert_eq!(clipped.chars().count(), 241);
    }

    #[test]
    fn observe_tracks_latency_and_outcomes() {
        let mut t = Tool::default();
        let meta = RecordMeta::default();
        t.observe(1_000, 100, 50_000, Outcome::Ok, 5, &meta);
        t.observe(3_000, 200, 150_000, Outcome::Truncated, 9, &meta);
        t.observe(500, 10, 20_000, Outcome::Error, 12, &meta);
        t.observe(0, 5, 1_000, Outcome::NotFound, 13, &meta);
        assert_eq!(t.calls, 4);
        assert_eq!(t.total_us, 221_000);
        assert_eq!(t.peak_us, 150_000);
        assert_eq!(t.total_ms(), 221);
        assert_eq!(t.peak_ms(), 150);
        assert_eq!(t.trunc_count, 1);
        assert_eq!(t.error_count, 1);
        assert_eq!(t.not_found_count, 1);
        assert_eq!(t.invalid_count, 0);
        assert_eq!(t.avg_ms(), 55);
    }

    #[test]
    fn render_includes_tools_and_total() {
        let mut tools = BTreeMap::new();
        tools.insert(
            "repo_map".to_string(),
            Tool {
                calls: 3,
                baseline_tokens: 12_000,
                returned_tokens: 600,
                ..Default::default()
            },
        );
        let out = render(1_000, 1_000 + 3_700, &tools);
        assert!(out.contains("repo_map"));
        assert!(out.contains("TOTAL"));
        assert!(out.contains("avg ms"));
        assert!(out.contains("estimated context avoided"));
    }

    #[test]
    fn render_insights_lists_never_called() {
        let mut tools = BTreeMap::new();
        tools.insert(
            "repo_map".to_string(),
            Tool {
                calls: 5,
                baseline_tokens: 10_000,
                returned_tokens: 500,
                total_us: 500_000,
                ..Default::default()
            },
        );
        let out = render_insights(0, &tools, &["repo_map", "outline", "stats"]);
        assert!(out.contains("never called"));
        assert!(out.contains("outline"));
        assert!(!out.contains("low_yield"), "{out}");
    }

    #[test]
    fn render_insights_skips_benign_slow_and_net_negative() {
        let mut tools = BTreeMap::new();
        tools.insert(
            "symbol_context".to_string(),
            Tool {
                calls: 96,
                baseline_tokens: 1_000,
                returned_tokens: 500,
                total_us: 96 * 13_000,
                error_count: 1,
                ..Default::default()
            },
        );
        tools.insert(
            "include_graph".to_string(),
            Tool {
                calls: 2,
                baseline_tokens: 49,
                returned_tokens: 106,
                ..Default::default()
            },
        );
        let out = render_insights(0, &tools, &["symbol_context", "include_graph"]);
        assert!(!out.contains("slowest"), "{out}");
        assert!(!out.contains("error-prone"), "{out}");
        assert!(!out.contains("net-negative"), "{out}");
    }

    #[test]
    fn render_insights_flags_actionable_patterns() {
        let mut tools = BTreeMap::new();
        tools.insert(
            "outline".to_string(),
            Tool {
                calls: 43,
                baseline_tokens: 10_000,
                returned_tokens: 500,
                error_count: 8,
                ..Default::default()
            },
        );
        tools.insert(
            "repo_map".to_string(),
            Tool {
                calls: 28,
                baseline_tokens: 50_000,
                returned_tokens: 5_000,
                trunc_count: 5,
                total_us: 28 * 60_000,
                ..Default::default()
            },
        );
        let out = render_insights(0, &tools, &["outline", "repo_map"]);
        assert!(out.contains("error-prone: outline"), "{out}");
        assert!(out.contains("high truncation: repo_map"), "{out}");
        assert!(out.contains("slowest (avg ms): repo_map"), "{out}");
    }

    #[test]
    fn from_value_loads_v4_and_v5() {
        let v4 = json!({
            "version": 4,
            "since": 100,
            "tools": { "repo_map": {
                "calls": 2, "baseline_tokens": 9_000, "returned_tokens": 300,
                "peak_baseline": 6_000, "peak_returned": 200, "peak_saved": 5_800, "last_ts": 42,
                "total_ms": 120, "peak_ms": 80, "trunc_count": 1, "error_count": 0, "low_yield_count": 0
            }},
            "events": [ { "ts": 41, "tool": "repo_map", "baseline": 3_000, "returned": 100,
                          "elapsed_ms": 40, "outcome": "ok" } ]
        });
        let s = Store::from_value(&v4);
        let t = s.tools.get("repo_map").unwrap();
        assert_eq!(t.total_us, 120_000);
        assert_eq!(t.peak_us, 80_000);
        assert_eq!(t.trunc_count, 1);
        assert_eq!(s.events.back().unwrap().elapsed_us, 40_000);
        assert_eq!(s.events.back().unwrap().outcome, "ok");

        let v5 = json!({
            "version": 5,
            "since": 200,
            "workspace_id": "abcd",
            "tools": { "outline": {
                "calls": 3, "baseline_tokens": 800, "returned_tokens": 120,
                "total_us": 9_000, "peak_us": 4_000,
                "bytes_read": 4096, "cache_hits": 2, "cache_misses": 1, "invalid_count": 1
            }}
        });
        let s5 = Store::from_value(&v5);
        let o = s5.tools.get("outline").unwrap();
        assert_eq!(o.total_us, 9_000);
        assert_eq!(o.bytes_read, 4096);
        assert_eq!(o.invalid_count, 1);
        assert_eq!(s5.saved_workspace_id, "abcd");

        let v2 = json!({
            "version": 2,
            "since": 50,
            "tools": { "outline": { "calls": 1, "baseline_tokens": 800, "returned_tokens": 120 } }
        });
        let s2 = Store::from_value(&v2);
        let o = s2.tools.get("outline").unwrap();
        assert_eq!(o.total_us, 0);
        assert_eq!(o.trunc_count, 0);
    }

    #[test]
    fn prune_stale_registry_drops_removed_and_clears_flat64() {
        let _ = REGISTRY.set(vec!["outline", "repo_map", "izakaya_status"]);
        let mut s = Store::empty();
        s.tools.insert(
            "outline".into(),
            Tool {
                calls: 10,
                baseline_tokens: 5_000,
                returned_tokens: 100,
                ..Default::default()
            },
        );
        s.tools.insert(
            "izakaya_record_decision".into(),
            Tool {
                calls: 1,
                baseline_tokens: 64,
                returned_tokens: 20,
                ..Default::default()
            },
        );
        s.tools.insert(
            "izakaya_status".into(),
            Tool {
                calls: 5,
                baseline_tokens: 320, // 5 * 64
                returned_tokens: 900,
                ..Default::default()
            },
        );
        assert!(s.prune_stale_registry());
        assert!(!s.tools.contains_key("izakaya_record_decision"));
        assert!(s.tools.contains_key("outline"));
        assert_eq!(s.tools.get("izakaya_status").unwrap().baseline_tokens, 0);
    }

    #[test]
    fn events_since_reads_jsonl_beyond_ring() {
        let _guard = cache::test_env_lock();
        let pid = std::process::id();
        let cache_home = std::env::temp_dir().join(format!("wk_stats_cache_{pid}"));
        let root = std::env::temp_dir().join(format!("wk_stats_root_{pid}"));
        let _ = std::fs::remove_dir_all(&cache_home);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("XDG_CACHE_HOME", &cache_home);

        let dir = workspace::ensure_workspace_dir(&root).unwrap();
        let path = dir.join(EVENTS_FILE);
        let wid = workspace::workspace_id(&root);
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 1..=250u64 {
            let line = json!({
                "ts": i,
                "tool": "outline",
                "baseline": 10,
                "returned": 2,
                "elapsed_us": 1000,
                "outcome": "ok",
                "workspace_id": wid,
                "bytes_read": 0,
                "cache_hits": 0,
                "cache_misses": 0,
                "wordkeep_version": "0.2.0",
            });
            writeln!(f, "{line}").unwrap();
        }
        drop(f);

        let evs = events_since(&root, 0);
        assert!(
            evs.len() > 200,
            "expected >200 from jsonl, got {}",
            evs.len()
        );
        assert_eq!(evs.len(), 250);
        let filtered = events_since(&root, 200);
        assert_eq!(filtered.len(), 50);

        let _ = std::fs::remove_dir_all(&cache_home);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn finish_debounce_does_not_fail() {
        let _guard = cache::test_env_lock();
        let pid = std::process::id();
        let cache_home = std::env::temp_dir().join(format!("wk_stats_fin_{pid}"));
        let root = std::env::temp_dir().join(format!("wk_stats_fin_root_{pid}"));
        let _ = std::fs::remove_dir_all(&cache_home);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("XDG_CACHE_HOME", &cache_home);
        // Best-effort: workspace root may already be set in-process; events still ok.
        let _ = WORKSPACE_ROOT.set(root.clone());
        let _ = WORKSPACE_ID.set(workspace::workspace_id(&root));

        for i in 0..5 {
            record("stats", 100 + i, 10);
            finish("stats", 1_500, &Ok("ok enough text here".into()));
        }
        // No panic / lock poison is success.
        let _ = std::fs::remove_dir_all(&cache_home);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn low_yield_and_net_negative_thresholds() {
        assert!(!is_notable_low_yield(5, 5));
        assert!(!is_notable_low_yield(10, 2)); // 20%
        assert!(is_notable_low_yield(10, 3)); // 30%
        assert!(!is_net_negative(100, 400));
        assert!(is_net_negative(500, 500));
        assert!(is_net_negative(1_000, 2_000));
    }
}
