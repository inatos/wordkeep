//! Token-savings telemetry.
//!
//! Every tool reports two numbers per call: `baseline_tokens` - roughly what the
//! agent would have spent reading the raw material itself (source files, docs, a
//! trace CSV) - and `returned_tokens`, the size of the distilled answer
//! wordkeep actually emitted. The difference is the saving. Totals persist to
//! `savings.json` in the shared cache dir so they accumulate across spawns, and
//! the `stats` tool renders them on demand. A `~4 chars/token` heuristic is used
//! throughout, matching the budgeting the other tools already do.
//!
//! Alongside the running totals each tool keeps high-water marks, per-call latency,
//! outcome counters (truncation, error, low-yield), and a capped rolling event log.
//! The optional `dashboard` reads `savings.json` live. On-disk schema is `version: 3`;
//! v1/v2 files load fine (missing fields default to zero / empty).

use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::cache;

const FILE: &str = "savings.json";

/// Cap on the rolling per-call event log persisted for the dashboard.
const EVENT_CAP: usize = 200;

/// Returned tokens at or below this are tagged `low_yield` (one-line empty-ish answers).
const LOW_YIELD_FLOOR: u64 = 25;

/// Improvement-signal thresholds - only flag genuinely actionable patterns.
const SLOW_MS_FLOOR: u64 = 50;
const TRUNC_RATE_PCT: u64 = 15;
const ERROR_RATE_PCT: u64 = 10;
const ERROR_MIN_CALLS: u64 = 5;
const NET_NEGATIVE_BASELINE_FLOOR: u64 = 500;

thread_local! {
    static PENDING: RefCell<Option<(u64, u64)>> = const { RefCell::new(None) };
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
    total_ms: u64,
    peak_ms: u64,
    trunc_count: u64,
    error_count: u64,
    low_yield_count: u64,
}

impl Tool {
    fn observe(&mut self, baseline: u64, returned: u64, elapsed_ms: u64, outcome: &str, ts: u64) {
        self.calls += 1;
        self.baseline_tokens += baseline;
        self.returned_tokens += returned;
        self.peak_baseline = self.peak_baseline.max(baseline);
        self.peak_returned = self.peak_returned.max(returned);
        self.peak_saved = self.peak_saved.max(baseline.saturating_sub(returned));
        self.last_ts = ts;
        self.total_ms += elapsed_ms;
        self.peak_ms = self.peak_ms.max(elapsed_ms);
        match outcome {
            "truncated" => self.trunc_count += 1,
            "error" => self.error_count += 1,
            "low_yield" => self.low_yield_count += 1,
            _ => {}
        }
    }

    fn avg_ms(&self) -> u64 {
        if self.calls == 0 {
            0
        } else {
            self.total_ms / self.calls
        }
    }
}

#[derive(Clone)]
struct Event {
    ts: u64,
    tool: String,
    baseline: u64,
    returned: u64,
    elapsed_ms: u64,
    outcome: String,
}

struct Store {
    since: u64,
    tools: BTreeMap<String, Tool>,
    events: VecDeque<Event>,
}

static STATS: OnceLock<Mutex<Store>> = OnceLock::new();
static REGISTRY: OnceLock<Vec<&'static str>> = OnceLock::new();

/// Register all MCP tool names (for never-called insights in `stats`).
pub fn init_registry(names: Vec<&'static str>) {
    let _ = REGISTRY.set(names);
}

fn registry() -> &'static [&'static str] {
    REGISTRY.get().map(|v| v.as_slice()).unwrap_or(&[])
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

impl Store {
    fn empty() -> Store {
        Store {
            since: now_secs(),
            tools: BTreeMap::new(),
            events: VecDeque::new(),
        }
    }

    fn observe(
        &mut self,
        tool: &str,
        baseline: u64,
        returned: u64,
        elapsed_ms: u64,
        outcome: &str,
        ts: u64,
    ) {
        self.tools
            .entry(tool.to_string())
            .or_default()
            .observe(baseline, returned, elapsed_ms, outcome, ts);
        self.events.push_back(Event {
            ts,
            tool: tool.to_string(),
            baseline,
            returned,
            elapsed_ms,
            outcome: outcome.to_string(),
        });
        while self.events.len() > EVENT_CAP {
            self.events.pop_front();
        }
    }

    fn from_value(v: &Value) -> Store {
        let mut s = Store::empty();
        let Value::Object(o) = v else { return s };
        if let Some(t) = o.get("since").and_then(Value::as_u64) {
            s.since = t;
        }
        if let Some(Value::Object(tools)) = o.get("tools") {
            for (name, tv) in tools {
                s.tools.insert(
                    name.clone(),
                    Tool {
                        calls: field(tv, "calls"),
                        baseline_tokens: field(tv, "baseline_tokens"),
                        returned_tokens: field(tv, "returned_tokens"),
                        peak_baseline: field(tv, "peak_baseline"),
                        peak_returned: field(tv, "peak_returned"),
                        peak_saved: field(tv, "peak_saved"),
                        last_ts: field(tv, "last_ts"),
                        total_ms: field(tv, "total_ms"),
                        peak_ms: field(tv, "peak_ms"),
                        trunc_count: field(tv, "trunc_count"),
                        error_count: field(tv, "error_count"),
                        low_yield_count: field(tv, "low_yield_count"),
                    },
                );
            }
        }
        if let Some(Value::Array(evs)) = o.get("events") {
            for ev in evs {
                let tool = ev.get("tool").and_then(Value::as_str).unwrap_or("");
                if tool.is_empty() {
                    continue;
                }
                s.events.push_back(Event {
                    ts: field(ev, "ts"),
                    tool: tool.to_string(),
                    baseline: field(ev, "baseline"),
                    returned: field(ev, "returned"),
                    elapsed_ms: field(ev, "elapsed_ms"),
                    outcome: ev
                        .get("outcome")
                        .and_then(Value::as_str)
                        .unwrap_or("ok")
                        .to_string(),
                });
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
            .map(|(k, t)| {
                (
                    k.clone(),
                    json!({
                        "calls": t.calls,
                        "baseline_tokens": t.baseline_tokens,
                        "returned_tokens": t.returned_tokens,
                        "peak_baseline": t.peak_baseline,
                        "peak_returned": t.peak_returned,
                        "peak_saved": t.peak_saved,
                        "last_ts": t.last_ts,
                        "total_ms": t.total_ms,
                        "peak_ms": t.peak_ms,
                        "trunc_count": t.trunc_count,
                        "error_count": t.error_count,
                        "low_yield_count": t.low_yield_count,
                    }),
                )
            })
            .collect();
        let events: Vec<Value> = self
            .events
            .iter()
            .map(|e| {
                json!({
                    "ts": e.ts,
                    "tool": e.tool,
                    "baseline": e.baseline,
                    "returned": e.returned,
                    "elapsed_ms": e.elapsed_ms,
                    "outcome": e.outcome,
                })
            })
            .collect();
        let doc = json!({
            "version": 3,
            "since": self.since,
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

fn field(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// Stash baseline/returned for the in-flight call; finalized by [`finish`].
pub fn record(_tool: &str, baseline_tokens: u64, returned_tokens: u64) {
    PENDING.with(|p| *p.borrow_mut() = Some((baseline_tokens, returned_tokens)));
}

/// Classify a tool result for telemetry.
pub fn classify_outcome(result: &Result<String, String>, returned_tokens: u64) -> &'static str {
    if result.is_err() {
        return "error";
    }
    if let Ok(text) = result {
        if text.contains("truncated by token_budget") || text.contains("more; raise") {
            return "truncated";
        }
    }
    if returned_tokens <= LOW_YIELD_FLOOR {
        return "low_yield";
    }
    "ok"
}

/// Finalize telemetry for one instrumented tool call.
pub fn finish(tool: &str, elapsed_ms: u64, result: &Result<String, String>) {
    let (baseline, returned) = PENDING.with(|p| p.borrow_mut().take()).unwrap_or((0, 0));
    let outcome = classify_outcome(result, returned);
    let now = now_secs();
    if let Ok(mut s) = cell().lock() {
        s.observe(tool, baseline, returned, elapsed_ms, outcome, now);
        s.save();
    }
    let saved = baseline.saturating_sub(returned);
    eprintln!(
        "[wordkeep] {tool}: ~{returned} tok returned vs ~{baseline} distilled \
         (saved ~{saved}, {}%, {elapsed_ms}ms, {outcome})",
        pct(saved, baseline)
    );
}

/// `stats` tool handler: render cumulative savings + improvement signals.
pub fn report(args: &Value) -> Result<String, String> {
    let reset = args.get("reset").and_then(Value::as_bool).unwrap_or(false);
    let insights = args
        .get("insights")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let mut s = cell()
        .lock()
        .map_err(|_| "stats lock poisoned".to_string())?;
    if reset {
        s.tools.clear();
        s.events.clear();
        s.since = now_secs();
        s.save();
        return Ok("wordkeep savings - counters reset.".to_string());
    }
    let now = now_secs();
    let mut out = render(s.since, now, &s.tools);
    if insights {
        out.push_str(&render_insights(now, &s.tools, registry()));
    }
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
        if self.calls == 0 {
            0
        } else {
            self.total_ms / self.calls
        }
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
    let s = Store::load();
    let tools = s
        .tools
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
            total_ms: t.total_ms,
            peak_ms: t.peak_ms,
            trunc_count: t.trunc_count,
            error_count: t.error_count,
            low_yield_count: t.low_yield_count,
        })
        .collect();
    let events = s
        .events
        .iter()
        .map(|e| EventLog {
            ts: e.ts,
            tool: e.tool.clone(),
            baseline: e.baseline,
            returned: e.returned,
            elapsed_ms: e.elapsed_ms,
            outcome: e.outcome.clone(),
        })
        .collect();
    Snapshot {
        since: s.since,
        now: now_secs(),
        tools,
        events,
    }
}

pub(crate) fn pct(saved: u64, baseline: u64) -> u64 {
    if baseline == 0 {
        0
    } else {
        (saved as f64 / baseline as f64 * 100.0).round() as u64
    }
}

pub(crate) fn commafy(n: u64) -> String {
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

fn render(since: u64, now: u64, tools: &BTreeMap<String, Tool>) -> String {
    if tools.is_empty() {
        return "wordkeep savings - no tool calls recorded yet.".to_string();
    }
    let (mut tc, mut tb, mut tr) = (0u64, 0u64, 0u64);
    let mut rows = String::new();
    for (name, t) in tools {
        let saved = t.baseline_tokens.saturating_sub(t.returned_tokens);
        rows.push_str(&format!(
            "  {:<16} {:>5} {:>8} {:>5} {:>4} {:>11} {:>11} {:>11} {:>4}%\n",
            name,
            t.calls,
            t.avg_ms(),
            t.trunc_count,
            t.error_count,
            commafy(t.baseline_tokens),
            commafy(t.returned_tokens),
            commafy(saved),
            pct(saved, t.baseline_tokens),
        ));
        tc += t.calls;
        tb += t.baseline_tokens;
        tr += t.returned_tokens;
    }
    let tsaved = tb.saturating_sub(tr);
    format!(
        "wordkeep savings - tracking since {}\n\n  {:<16} {:>5} {:>8} {:>5} {:>4} {:>11} {:>11} {:>11} {:>4}\n{}  {:<16} {:>5} {:>8} {:>5} {:>4} {:>11} {:>11} {:>11} {:>4}%\n\n(distilled = est. tokens to read raw material; returned = tokens emitted;\navg ms = mean wall time; trunc/err = outcome counts; ~4 chars/token.)",
        elapsed(since, now),
        "tool", "calls", "avg ms", "trunc", "err", "distilled", "returned", "saved", "red%",
        rows,
        "TOTAL", tc, "-", "-", "-", commafy(tb), commafy(tr), commafy(tsaved), pct(tsaved, tb),
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

    let inverted: Vec<&str> = tools
        .iter()
        .filter(|(_, t)| {
            t.baseline_tokens >= NET_NEGATIVE_BASELINE_FLOOR
                && t.returned_tokens >= t.baseline_tokens
        })
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

    #[test]
    fn pct_is_saturating_ratio() {
        assert_eq!(pct(0, 0), 0);
        assert_eq!(pct(50, 100), 50);
        assert_eq!(pct(97, 100), 97);
        assert_eq!(pct(100, 0), 0);
    }

    #[test]
    fn commafy_groups_thousands() {
        assert_eq!(commafy(0), "0");
        assert_eq!(commafy(42), "42");
        assert_eq!(commafy(1_000), "1,000");
        assert_eq!(commafy(120_500), "120,500");
        assert_eq!(commafy(1_234_567), "1,234,567");
    }

    #[test]
    fn classify_outcome_tags() {
        assert_eq!(classify_outcome(&Err("x".into()), 100), "error");
        assert_eq!(
            classify_outcome(&Ok("… (truncated by token_budget)\n".into()), 500),
            "truncated"
        );
        assert_eq!(
            classify_outcome(&Ok("… (+3 more; raise \"max\")\n".into()), 500),
            "truncated"
        );
        assert_eq!(classify_outcome(&Ok("no hits\n".into()), 10), "low_yield");
        assert_eq!(
            classify_outcome(&Ok("big answer".repeat(100).into()), 500),
            "ok"
        );
    }

    #[test]
    fn observe_tracks_latency_and_outcomes() {
        let mut t = Tool::default();
        t.observe(1_000, 100, 50, "ok", 5);
        t.observe(3_000, 200, 150, "truncated", 9);
        t.observe(500, 10, 20, "error", 12);
        assert_eq!(t.calls, 3);
        assert_eq!(t.total_ms, 220);
        assert_eq!(t.peak_ms, 150);
        assert_eq!(t.trunc_count, 1);
        assert_eq!(t.error_count, 1);
        assert_eq!(t.avg_ms(), 73);
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
                total_ms: 500,
                ..Default::default()
            },
        );
        let out = render_insights(0, &tools, &["repo_map", "outline", "stats"]);
        assert!(out.contains("never called"));
        assert!(out.contains("outline"));
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
                total_ms: 96 * 13,
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
                total_ms: 28 * 60,
                ..Default::default()
            },
        );
        let out = render_insights(0, &tools, &["outline", "repo_map"]);
        assert!(out.contains("error-prone: outline"), "{out}");
        assert!(out.contains("high truncation: repo_map"), "{out}");
        assert!(out.contains("slowest (avg ms): repo_map"), "{out}");
    }

    #[test]
    fn from_value_reads_v3_and_defaults_v2() {
        let v3 = json!({
            "version": 3,
            "since": 100,
            "tools": { "repo_map": {
                "calls": 2, "baseline_tokens": 9_000, "returned_tokens": 300,
                "peak_baseline": 6_000, "peak_returned": 200, "peak_saved": 5_800, "last_ts": 42,
                "total_ms": 120, "peak_ms": 80, "trunc_count": 1, "error_count": 0, "low_yield_count": 0
            }},
            "events": [ { "ts": 41, "tool": "repo_map", "baseline": 3_000, "returned": 100,
                          "elapsed_ms": 40, "outcome": "ok" } ]
        });
        let s = Store::from_value(&v3);
        let t = s.tools.get("repo_map").unwrap();
        assert_eq!(t.total_ms, 120);
        assert_eq!(t.trunc_count, 1);
        assert_eq!(s.events.back().unwrap().outcome, "ok");

        let v2 = json!({
            "version": 2,
            "since": 50,
            "tools": { "outline": { "calls": 1, "baseline_tokens": 800, "returned_tokens": 120 } }
        });
        let s2 = Store::from_value(&v2);
        let o = s2.tools.get("outline").unwrap();
        assert_eq!(o.total_ms, 0);
        assert_eq!(o.trunc_count, 0);
    }
}
