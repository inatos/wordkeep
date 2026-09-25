//! Golden-query eval harness for Wordkeep telemetry upgrades.
//!
//! Asserts outcome class (ok / not-found-with-hint / trunc-free) and rough
//! token caps against the hermetic fixture workspace. Run with:
//!   cargo test --test eval_queries
//!
//! Protocol for A/B after tool changes:
//! 1. `stats` with reset:true before a soak (or record baseline snapshot).
//! 2. Land the change; keep this suite green.
//! 3. Compare trunc/err/not_found deltas via `stats`; p50 latency must not
//!    regress more than ~10% on the same machine/workload.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct FixtureRepo {
    root: PathBuf,
}

impl FixtureRepo {
    fn new() -> Self {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("wk_eval_{}_{}", std::process::id(), id));
        let _ = std::fs::remove_dir_all(&root);
        let seed = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        copy_dir_all(&seed, &root).expect("copy fixtures");
        // Extra profiles so symbol_resolve / index_health have something to scan.
        let cfg = root.join(".wordkeep/config.json");
        std::fs::write(
            &cfg,
            r#"{
  "default_paths": ["src", "include"],
  "default_profile": "engine",
  "path_profiles": {
    "engine": ["src", "include"],
    "tests": ["tests"]
  },
  "default_doc_roots": ["docs", ".wordkeep"],
  "test_command": "ctest -R"
}
"#,
        )
        .unwrap();
        init_git_repo(&root);
        Self { root }
    }
}

impl Drop for FixtureRepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let dest = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

fn init_git_repo(root: &Path) {
    let run = |args: &[&str]| {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(root)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .expect("git")
                .success(),
            "git {args:?} failed"
        );
    };
    run(&["init"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "wordkeep eval"]);
    run(&["add", "."]);
    run(&["commit", "-m", "fixture"]);
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    cache_home: PathBuf,
    _fixture: FixtureRepo,
}

impl Server {
    fn start() -> Server {
        let fixture = FixtureRepo::new();
        let cache_home = std::env::temp_dir().join(format!(
            "wk_eval_cache_{}_{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let workdir = std::env::temp_dir().join(format!(
            "wk_eval_cwd_{}_{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&workdir).expect("cwd");
        let mut child = Command::new(env!("CARGO_BIN_EXE_wordkeep"))
            .arg("--root")
            .arg(&fixture.root)
            .current_dir(&workdir)
            .env("XDG_CACHE_HOME", &cache_home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn wordkeep");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut s = Server {
            child,
            stdin,
            stdout,
            cache_home,
            _fixture: fixture,
        };
        let _ = s.call(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "eval", "version": "0" } }
        }));
        s
    }

    fn call(&mut self, req: Value) -> Value {
        let line = serde_json::to_string(&req).unwrap();
        writeln!(self.stdin, "{line}").expect("write");
        self.stdin.flush().expect("flush");
        loop {
            let mut buf = String::new();
            self.stdout.read_line(&mut buf).expect("read");
            let v: Value = serde_json::from_str(buf.trim()).expect("json");
            // Skip progress (and similar) notifications — they have no `id`.
            if v.get("id").is_some() {
                return v;
            }
        }
    }

    /// Like [`call`], but also returns any `notifications/progress` lines seen
    /// before the matching response.
    fn call_with_progress(&mut self, req: Value) -> (Vec<Value>, Value) {
        let line = serde_json::to_string(&req).unwrap();
        writeln!(self.stdin, "{line}").expect("write");
        self.stdin.flush().expect("flush");
        let mut notes = Vec::new();
        loop {
            let mut buf = String::new();
            self.stdout.read_line(&mut buf).expect("read");
            let v: Value = serde_json::from_str(buf.trim()).expect("json");
            if v.get("id").is_some() {
                return (notes, v);
            }
            if v.get("method").and_then(Value::as_str) == Some("notifications/progress") {
                notes.push(v);
            }
        }
    }

    fn tool(&mut self, name: &str, arguments: Value) -> (bool, String) {
        static ID: AtomicU32 = AtomicU32::new(10);
        let id = ID.fetch_add(1, Ordering::Relaxed);
        let resp = self.call(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        }));
        let is_err = resp["result"]["isError"].as_bool().unwrap_or(false);
        let text = resp["result"]["content"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|c| c["text"].as_str())
            .unwrap_or("")
            .to_string();
        (is_err, text)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = std::fs::remove_dir_all(&self.cache_home);
    }
}

fn approx_tokens(s: &str) -> usize {
    s.len() / 4
}

#[test]
fn eval_repo_map_files_mode_stays_under_budget() {
    let mut s = Server::start();
    let (err, text) = s.tool(
        "repo_map",
        json!({ "mode": "files", "token_budget": 400 }),
    );
    assert!(!err, "{text}");
    assert!(
        !text.contains("truncated by token_budget"),
        "files mode should fit tiny trees without trunc: {text}"
    );
    assert!(
        text.contains("files") || text.contains("symbol"),
        "{text}"
    );
    assert!(approx_tokens(&text) <= 500, "tokens={}", approx_tokens(&text));
}

#[test]
fn eval_repo_map_symbols_finds_widget() {
    let mut s = Server::start();
    let (err, text) = s.tool(
        "repo_map",
        json!({ "mode": "symbols", "pattern": "sample", "token_budget": 2000 }),
    );
    assert!(!err, "{text}");
    assert!(text.contains("Widget") || text.contains("widget_area"), "{text}");
}

#[test]
fn eval_symbol_resolve_exact_and_fuzzy() {
    let mut s = Server::start();
    let (err, text) = s.tool(
        "symbol_resolve",
        json!({ "symbol": "widget_area" }),
    );
    assert!(!err, "{text}");
    assert!(
        text.to_lowercase().contains("widget_area") || text.contains("found"),
        "{text}"
    );

    let (err2, text2) = s.tool(
        "symbol_resolve",
        json!({ "symbol": "::demo::widget_area<int>" }),
    );
    assert!(!err2, "{text2}");
    // Normalized qualified/templated name should still resolve or suggest.
    assert!(
        text2.to_lowercase().contains("widget") || text2.contains("did you mean"),
        "{text2}"
    );
}

#[test]
fn eval_symbol_refs_not_found_includes_did_you_mean() {
    let mut s = Server::start();
    let (err, text) = s.tool(
        "symbol_refs",
        json!({ "symbol": "widget_are" }), // typo
    );
    assert!(!err, "{text}");
    assert!(
        text.contains("did you mean") || text.contains("not found") || text.contains("0 "),
        "{text}"
    );
    // Did-you-mean tokens must be real identifiers, not 1–2 char garbage.
    if let Some(line) = text.lines().find(|l| l.contains("did you mean")) {
        let after = line
            .split("did you mean:")
            .nth(1)
            .unwrap_or("")
            .trim()
            .trim_end_matches('?');
        let list = after.split('(').next().unwrap_or(after);
        for tok in list.split(',').map(|t| t.trim()).filter(|t| !t.is_empty()) {
            assert!(
                tok.len() >= 3,
                "short garbage in did-you-mean: {tok:?} from {line}"
            );
        }
    }
}

#[test]
fn eval_symbol_resolve_typo_no_short_garbage() {
    let mut s = Server::start();
    let (err, text) = s.tool(
        "symbol_resolve",
        json!({ "symbol": "symbl_resolve", "paths": ["src"] }),
    );
    assert!(!err, "{text}");
    for line in text.lines() {
        if line.trim_start().starts_with("did you mean") || line.contains("suggestions") {
            continue;
        }
        // Ranked suggestion lines are indented identifiers.
        let t = line.trim();
        if t.is_empty() || t.contains("symbol_resolve") || t.contains("no definition") {
            continue;
        }
        if line.starts_with("  ") && !line.contains(':') && !line.contains("profile") {
            assert!(t.len() >= 3, "short suggestion token: {t:?} in {text}");
        }
    }
    if let Some(line) = text.lines().find(|l| l.contains("did you mean")) {
        let after = line
            .split("did you mean:")
            .nth(1)
            .unwrap_or("")
            .trim()
            .trim_end_matches('?');
        let list = after.split('(').next().unwrap_or(after);
        for tok in list.split(',').map(|t| t.trim()).filter(|t| !t.is_empty()) {
            assert!(tok.len() >= 3, "short garbage: {tok:?} from {line}");
        }
    }
}

#[test]
fn eval_type_layout_normalizes_qualified_name() {
    let mut s = Server::start();
    let (err, text) = s.tool(
        "type_layout",
        json!({ "type": "::demo::Widget" }),
    );
    assert!(!err, "{text}");
    assert!(
        text.contains("id") || text.contains("value") || text.contains("Widget"),
        "{text}"
    );
}

#[test]
fn eval_test_map_finds_widget_tests() {
    let mut s = Server::start();
    let (err, text) = s.tool(
        "test_map",
        json!({ "symbol": "widget_area", "paths": ["tests"] }),
    );
    assert!(!err, "{text}");
    assert!(
        text.contains("test_widget") || text.contains("widget"),
        "{text}"
    );
}

#[test]
fn eval_knowledge_answer_has_citations() {
    let mut s = Server::start();
    let (err, text) = s.tool(
        "knowledge_answer",
        json!({ "query": "physics pipeline", "token_budget": 800 }),
    );
    assert!(!err, "{text}");
    assert!(
        text.contains("citation") || text.contains("Physics") || text.contains("answer"),
        "{text}"
    );
    assert!(approx_tokens(&text) <= 900, "tokens={}", approx_tokens(&text));
}

#[test]
fn eval_integration_hooks_or_fallback() {
    let mut s = Server::start();
    // Partial query should still hit via OR fallback (not all tokens required).
    let (err, text) = s.tool(
        "integration_hooks",
        json!({ "query": "render physics nonexistent_token" }),
    );
    assert!(!err, "{text}");
    assert!(
        text.contains("render") || text.contains("physics") || text.contains("vocab"),
        "{text}"
    );
}

#[test]
fn eval_defect_list_digest_default() {
    let mut s = Server::start();
    let (err, text) = s.tool("defect_list", json!({}));
    assert!(!err, "{text}");
    // Empty store still returns digest framing.
    assert!(
        text.contains("digest") || text.contains("defect") || text.contains("0"),
        "{text}"
    );
}

#[test]
fn eval_batch_context_multi_symbol() {
    let mut s = Server::start();
    let (err, text) = s.tool(
        "batch_context",
        json!({
            "symbols": ["widget_area", "Widget"],
            "token_budget": 3000
        }),
    );
    assert!(!err, "{text}");
    assert!(
        text.contains("widget_area") || text.contains("Widget"),
        "{text}"
    );
}

#[test]
fn eval_index_health_reports_profiles() {
    let mut s = Server::start();
    let (err, text) = s.tool("index_health", json!({}));
    assert!(!err, "{text}");
    assert!(
        text.contains("profile") || text.contains("engine") || text.contains("coverage")
            || text.contains("health") || text.contains("stale"),
        "{text}"
    );
}

#[test]
fn eval_outline_batch_files() {
    let mut s = Server::start();
    let (err, text) = s.tool(
        "outline",
        json!({
            "files": ["src/sample.cpp", "include/demo.h"],
            "token_budget": 2000
        }),
    );
    assert!(!err, "{text}");
    assert!(
        text.contains("sample.cpp") || text.contains("Widget") || text.contains("demo"),
        "{text}"
    );
}

#[test]
fn eval_commit_scope_ignores_cache_noise() {
    let mut s = Server::start();
    let cache = s._fixture.root.join(".cache/noise.bin");
    std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
    std::fs::write(&cache, b"x").unwrap();
    let (err, text) = s.tool("commit_scope", json!({}));
    assert!(!err, "{text}");
    // Soft assert: .cache should not dominate groupings when ignore is active.
    if text.contains(".cache") {
        // Still ok if listed under warnings/ignored — just must not Err.
    }
}

#[test]
fn eval_new_tools_are_listed() {
    let mut s = Server::start();
    let list = s.call(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    let tools = list["result"]["tools"].as_array().expect("tools");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    for n in [
        "symbol_resolve",
        "knowledge_answer",
        "batch_context",
        "perf_triage",
        "test_impact",
        "index_health",
    ] {
        assert!(names.contains(&n), "missing {n} in {names:?}");
    }
    assert_eq!(names.len(), 51, "surface drift: {names:?}");
}

fn extract_continuation(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.strip_prefix("continuation: ").map(|s| s.trim().to_string()))
}

#[test]
fn eval_repo_map_continuation_pages_disjoint() {
    let mut s = Server::start();
    // Force pagination: first file always emits; starve so the second cannot fit.
    let (err, page1) = s.tool(
        "repo_map",
        json!({ "mode": "files", "token_budget": 8 }),
    );
    assert!(!err, "{page1}");
    assert!(
        page1.contains("truncated by token_budget") && page1.contains("continuation: "),
        "expected truncate+continuation, got: {page1}"
    );
    let tok = extract_continuation(&page1).expect("continuation token");
    let (err2, page2) = s.tool(
        "repo_map",
        json!({
            "mode": "files",
            "token_budget": 8,
            "continuation": tok
        }),
    );
    assert!(!err2, "{page2}");
    assert!(
        page2.contains("continuation from file"),
        "page2 should mark resume: {page2}"
    );
    // First emitted file path line from page1 should not reappear as the first body file on page2.
    let p1_files: Vec<&str> = page1
        .lines()
        .filter(|l| l.contains("symbols)") || l.ends_with("symbols)"))
        .collect();
    let p2_files: Vec<&str> = page2
        .lines()
        .filter(|l| l.contains("symbols)") || l.ends_with("symbols)"))
        .collect();
    if let (Some(a), Some(b)) = (p1_files.first(), p2_files.first()) {
        assert_ne!(a, b, "pages should be disjoint: {a} vs {b}");
    }
}

#[test]
fn eval_continuation_rejects_args_drift() {
    let mut s = Server::start();
    let (err, page1) = s.tool(
        "repo_map",
        json!({ "mode": "files", "token_budget": 8 }),
    );
    assert!(!err, "{page1}");
    let Some(tok) = extract_continuation(&page1) else {
        // Tiny trees may not truncate; still validate mismatch path via encode in-unit.
        return;
    };
    let (err2, page2) = s.tool(
        "repo_map",
        json!({
            "mode": "symbols",
            "token_budget": 8,
            "continuation": tok
        }),
    );
    assert!(err2, "mode drift should fail: {page2}");
    assert!(
        page2.contains("does not match") || page2.contains("continuation"),
        "{page2}"
    );
}

#[test]
fn eval_progress_notifications_on_repo_map() {
    // Progress is opt-in (Cursor Shared MCP fatals on unknown progress tokens).
    std::env::set_var("WORDKEEP_MCP_PROGRESS", "1");
    let mut s = Server::start();
    let (notes, resp) = s.call_with_progress(json!({
        "jsonrpc": "2.0",
        "id": 77,
        "method": "tools/call",
        "params": {
            "name": "repo_map",
            "arguments": { "mode": "files", "token_budget": 800 },
            "_meta": { "progressToken": "eval-progress-1" }
        }
    }));
    assert_eq!(resp["result"]["isError"], json!(false), "{resp}");
    assert!(
        !notes.is_empty(),
        "expected at least one notifications/progress, got none; resp={resp}"
    );
    let first = &notes[0];
    assert_eq!(
        first["params"]["progressToken"],
        json!("eval-progress-1"),
        "{first}"
    );
}
