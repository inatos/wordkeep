//! End-to-end MCP stdio test: spawn the real binary against a hermetic fixture
//! workspace copied into a temporary git repository and exercise every tool.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::AtomicU32;

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct FixtureRepo {
    root: PathBuf,
}

impl FixtureRepo {
    fn new() -> Self {
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("wk_fixture_{}_{}", std::process::id(), id));
        let _ = std::fs::remove_dir_all(&root);
        let seed = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        copy_dir_all(&seed, &root).expect("copy fixtures");
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
            "git {:?} failed",
            args
        );
    };
    run(&["init"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "wordkeep test"]);
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
        Self::start_with_env(&[])
    }

    fn start_with_env(extra_env: &[(&str, &str)]) -> Server {
        let fixture = FixtureRepo::new();
        let cache_home = std::env::temp_dir().join(format!(
            "wk_cache_{}_{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let workdir = std::env::temp_dir().join(format!(
            "wk_cwd_{}_{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&workdir).expect("cwd");
        // Hermetic fixture has no GGUF; pin heuristic so `--features ereshkigal*`
        // does not fail `semantic_decide` on missing `semif.gguf`.
        std::fs::create_dir_all(fixture.root.join(".wordkeep")).expect("semif cfg dir");
        std::fs::write(
            fixture.root.join(".wordkeep/config.json"),
            r#"{"semif":{"enabled":true,"backend":"heuristic","debias":"none"}}"#,
        )
        .expect("semif cfg");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_wordkeep"));
        cmd.arg("--root")
            .arg(&fixture.root)
            .current_dir(&workdir)
            .env("XDG_CACHE_HOME", &cache_home)
            .env("WORDKEEP_EMBED_OFFLINE", "1")
            .env_remove("WORDKEEP_TOOLSET")
            // Host/session SemIf env must not override the fixture's heuristic pin.
            .env_remove("WORDKEEP_SEMIF_BACKEND")
            .env_remove("ERESHKIGAL_BACKEND")
            .env_remove("ERESHKIGAL_GGUF")
            .env_remove("ERESHKIGAL_GGUF_VERIFY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("spawn wordkeep");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Server {
            child,
            stdin,
            stdout,
            cache_home,
            _fixture: fixture,
        }
    }

    fn call(&mut self, req: Value) -> Value {
        let line = serde_json::to_string(&req).unwrap();
        self.stdin.write_all(line.as_bytes()).unwrap();
        self.stdin.write_all(b"\n").unwrap();
        self.stdin.flush().unwrap();
        loop {
            let mut resp = String::new();
            let n = self.stdout.read_line(&mut resp).expect("read response");
            assert!(n > 0, "server closed stdout without responding");
            let v: Value = serde_json::from_str(&resp)
                .unwrap_or_else(|e| panic!("bad response {resp:?}: {e}"));
            if v.get("id").is_some() {
                return v;
            }
        }
    }

    fn tool_text(&mut self, name: &str, args: Value) -> String {
        let resp = self.call(json!({
            "jsonrpc": "2.0", "id": 99, "method": "tools/call",
            "params": { "name": name, "arguments": args }
        }));
        assert_eq!(
            resp["result"]["isError"],
            json!(false),
            "tool {name} errored: {resp}"
        );
        resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no text in {resp}"))
            .to_string()
    }

    fn tool_error(&mut self, name: &str, args: Value) -> String {
        let resp = self.call(json!({
            "jsonrpc": "2.0", "id": 99, "method": "tools/call",
            "params": { "name": name, "arguments": args }
        }));
        assert_eq!(
            resp["result"]["isError"],
            json!(true),
            "tool {name} should error: {resp}"
        );
        resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no text in {resp}"))
            .to_string()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.cache_home);
    }
}

#[test]
fn initialize_handshake() {
    let mut s = Server::start();
    let init = s.call(json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2025-06-18" }
    }));
    assert_eq!(init["result"]["serverInfo"]["name"], "wordkeep");
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
}

#[test]
fn lists_all_tools() {
    let mut s = Server::start();
    let list = s.call(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    let tools = list["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    for n in [
        "repo_map",
        "outline",
        "symbol_refs",
        "symbol_resolve",
        "call_graph",
        "call_path",
        "doc_comment",
        "symbol_context",
        "usage_examples",
        "undocumented",
        "dead_code",
        "big_functions",
        "symbol_diff",
        "module_map",
        "diff_map",
        "include_graph",
        "type_layout",
        "test_map",
        "batch_context",
        "perf_triage",
        "test_impact",
        "knowledge_search",
        "knowledge_answer",
        "knowledge_upsert",
        "semantic_decide",
        "decree",
        "trace_summary",
        "trace_profile",
        "runtime_snapshot",
        "memory_diff",
        "locality_hotspots",
        "integration_hooks",
        "index_stale",
        "index_health",
        "stats",
        "mas_post",
        "mas_read",
        "mas_status",
        "mas_finalize",
        "session_handoff",
        "defect_upsert",
        "defect_list",
        "run_record",
        "tacet",
        "run_history",
        "artifact_index",
        "session_pressure",
        "commit_scope",
        "profile_upsert",
        "izakaya_status",
        "izakaya_check_in",
        "izakaya_update",
        "izakaya_check_out",
        "izakaya_advise",
        "shrift_list",
        "shrift_show",
        "shrift_upsert",
        "shrift_touch",
        "shrift_review",
        "shrift_status",
        "shrift_bookmark",
        "task_submit",
        "task_status",
        "task_fetch",
        "task_cancel",
        "copper_status",
        "mas_timeline",
        "tool_discover",
    ] {
        assert!(names.contains(&n), "missing tool {n}: {names:?}");
    }
    for t in tools {
        let ann = &t["annotations"];
        assert!(
            ann["readOnlyHint"].is_boolean()
                && ann["destructiveHint"].is_boolean()
                && ann["idempotentHint"].is_boolean()
                && ann["openWorldHint"].is_boolean(),
            "tool {} missing MCP 2025-11 annotations: {t}",
            t["name"]
        );
    }
    assert!(
        !names.contains(&"izakaya_record_decision"),
        "replay-lab MCP tools were removed: {names:?}"
    );
    assert!(
        !names.contains(&"izakaya_record_outcome"),
        "replay-lab MCP tools were removed: {names:?}"
    );
    assert!(
        !names.contains(&"izakaya_replay"),
        "replay-lab MCP tools were removed: {names:?}"
    );
    assert_eq!(
        names.len(),
        68,
        "expected 68 tools, got {}: {names:?}",
        names.len()
    );
}

#[test]
fn runtime_tools_read_cooperative_snapshot_without_network() {
    let mut s = Server::start();
    let runtime = s._fixture.root.join(".wordkeep/runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    std::fs::write(
        runtime.join("latest.json"),
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "source": "betwixt",
            "pid": 42,
            "rss_bytes": 4096,
            "quality": {"rss_bytes": "exact"},
            "regions": [{"size": 2048, "kind": "anon", "perm": "rw-", "path": null}]
        }))
        .unwrap(),
    )
    .unwrap();

    let snapshot = s.tool_text("runtime_snapshot", json!({}));
    assert!(snapshot.contains("RSS 4.0 KiB [exact]"), "{snapshot}");
    let locality = s.tool_text("locality_hotspots", json!({}));
    assert!(locality.contains("quality=unavailable"), "{locality}");
}

#[test]
fn resources_list_and_read_readme() {
    let mut s = Server::start();
    let init = s.call(json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2025-06-18" }
    }));
    assert!(
        init["result"]["capabilities"]["resources"].is_object(),
        "resources capability missing: {init}"
    );
    let list = s.call(json!({ "jsonrpc": "2.0", "id": 2, "method": "resources/list" }));
    let resources = list["result"]["resources"].as_array().expect("resources");
    assert!(
        resources.iter().any(|r| r["uri"] == "wordkeep://readme"),
        "{list}"
    );
    assert!(
        resources
            .iter()
            .any(|r| r["uri"] == "wordkeep://capabilities"),
        "{list}"
    );
    let read = s.call(json!({
        "jsonrpc": "2.0", "id": 3, "method": "resources/read",
        "params": { "uri": "wordkeep://README" }
    }));
    let text = read["result"]["contents"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("wordkeep") || text.contains("Wordkeep"),
        "{read}"
    );
    let caps = s.call(json!({
        "jsonrpc": "2.0", "id": 4, "method": "resources/read",
        "params": { "uri": "wordkeep://capabilities" }
    }));
    let caps_text = caps["result"]["contents"][0]["text"].as_str().unwrap_or("");
    assert!(caps_text.contains("\"tool_count\": 68"), "{caps}");
    assert!(caps_text.contains("knowledge_upsert"), "{caps}");
    assert!(caps_text.contains("shrift_upsert"), "{caps}");
    assert!(caps_text.contains("\"tacet\""), "{caps}");
}

#[test]
fn tacet_suppresses_write_tools_and_allows_reads() {
    let mut s = Server::start();
    let on = s.tool_text(
        "tacet",
        json!({ "enabled": true, "mode": "session" }),
    );
    assert!(on.contains("tacet: enabled"), "{on}");

    let rr = s.tool_error(
        "run_record",
        json!({ "command": "ctest -R noop", "status": "passed" }),
    );
    assert!(rr.contains("tacet_suppressed"), "{rr}");

    let ku = s.tool_error(
        "knowledge_upsert",
        json!({
            "path": ".wordkeep/notes/tacet-should-not-write.md",
            "heading": "should not write",
            "body": "blocked"
        }),
    );
    assert!(ku.contains("tacet_suppressed"), "{ku}");

    let du = s.tool_error(
        "defect_upsert",
        json!({
            "id": "tacet-should-not-write",
            "summary": "blocked by tacet",
            "status": "open",
            "subsystem": "wordkeep"
        }),
    );
    assert!(du.contains("tacet_suppressed"), "{du}");

    let map = s.tool_text("repo_map", json!({ "paths": ["src"], "token_budget": 400 }));
    assert!(!map.is_empty(), "{map}");
    let ks = s.tool_text(
        "knowledge_search",
        json!({ "query": "wordkeep", "k": 2, "token_budget": 400 }),
    );
    assert!(!ks.is_empty(), "{ks}");

    let off = s.tool_text("tacet", json!({ "enabled": false }));
    assert!(off.contains("tacet: disabled"), "{off}");
    let rr_ok = s.tool_text(
        "run_record",
        json!({ "command": "ctest -R noop", "status": "passed" }),
    );
    assert!(rr_ok.contains("run") || rr_ok.contains("recorded") || !rr_ok.is_empty(), "{rr_ok}");
}

#[test]
fn semantic_decide_scores_typed_options() {
    let mut s = Server::start();
    let text = s.tool_text(
        "semantic_decide",
        json!({
            "id": "route-1",
            "state": "Customer asks to reset a forgotten password and says the reset email never arrived.",
            "question": "Which queue should handle this request?",
            "options": [
                {"id": "account_access", "description": "Account access and authentication support for password reset and email delivery."},
                {"id": "billing", "description": "Billing and payment support for invoices and charges."},
                {"id": "sales", "description": "Sales and product evaluation for new purchases."}
            ]
        }),
    );
    let v: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(v["chosen"], "account_access", "{text}");
    assert_eq!(v["scorer"], "heuristic");
    assert_eq!(v["fallback"], false);
    assert_eq!(
        v["prompt_sha256"].as_str().unwrap_or("").len(),
        64,
        "{text}"
    );
    let opts = v["options"].as_array().expect("options");
    assert_eq!(opts.len(), 3);
    let sum: f64 = opts
        .iter()
        .map(|o| o["probability"].as_f64().unwrap_or(0.0))
        .sum();
    assert!((sum - 1.0).abs() < 1e-6, "softmax sum={sum}");
}

#[test]
fn decree_check_lints_ereshkigal_std() {
    let lib = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../ereshkigal/decrees");
    if !lib.is_dir() {
        return;
    }
    let mut s = Server::start();
    let text = s.tool_text(
        "decree",
        json!({ "method": "check", "lib": lib.display().to_string() }),
    );
    let v: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(v["ok"], true, "{text}");
    assert!(v["decrees"].as_u64().unwrap_or(0) >= 1, "{text}");

    let lint = s.tool_text(
        "decree",
        json!({ "method": "lint", "lib": lib.display().to_string() }),
    );
    let lint_v: Value = serde_json::from_str(&lint).expect("lint json");
    assert_eq!(lint_v["ok"], true, "{lint}");

    let schema = s.tool_text(
        "decree",
        json!({ "method": "schema", "lib": lib.display().to_string() }),
    );
    let schema_v: Value = serde_json::from_str(&schema).expect("schema json");
    assert_eq!(schema_v["ok"], true, "{schema}");
    assert!(schema_v.get("schema").is_some(), "{schema}");
}

#[test]
fn decree_stats_when_serve_bin_available() {
    let lib = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../ereshkigal/decrees");
    if !lib.is_dir() {
        return;
    }
    let esk = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../ereshkigal");
    let bin = [
        esk.join("target-vulkan/release/ereshkigal"),
        esk.join("target/release/ereshkigal"),
        esk.join("target/debug/ereshkigal"),
    ]
    .into_iter()
    .find(|p| p.is_file());
    let Some(bin) = bin else {
        return;
    };
    // Child inherits env — set before Server::start.
    let prev = std::env::var_os("ERESHKIGAL_BIN");
    std::env::set_var("ERESHKIGAL_BIN", &bin);
    let mut s = Server::start();
    let resp = s.call(json!({
        "jsonrpc": "2.0", "id": 99, "method": "tools/call",
        "params": {
            "name": "decree",
            "arguments": { "method": "stats", "lib": lib.display().to_string() }
        }
    }));
    match prev {
        Some(v) => std::env::set_var("ERESHKIGAL_BIN", v),
        None => std::env::remove_var("ERESHKIGAL_BIN"),
    }
    if resp["result"]["isError"] == json!(true) {
        // Soft-skip when the serve child cannot spawn in this environment.
        eprintln!("decree stats soft-skip: {resp}");
        return;
    }
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let v: Value = serde_json::from_str(&text).expect("json");
    assert!(
        v.get("decrees").is_some() || v.get("error").is_some(),
        "{text}"
    );
    if v.get("error").is_none() {
        assert!(v["decrees"].as_u64().unwrap_or(0) >= 1, "{text}");
    }
}

#[test]
fn continuity_tools_smoke() {
    let mut s = Server::start();
    let _ = s.tool_text(
        "defect_upsert",
        json!({
            "id": "eyeball-cloak",
            "summary": "cloak points up",
            "status": "eyeball_fail",
            "subsystem": "kkbp",
            "acceptance": ["drapes over shoulder"]
        }),
    );
    let listed = s.tool_text("defect_list", json!({}));
    assert!(listed.contains("eyeball-cloak"), "{listed}");
    let _ = s.tool_text(
        "run_record",
        json!({
            "id": "gate-1",
            "command": "validate_character_goldens.sh",
            "status": "passed",
            "tags": ["kkbp"]
        }),
    );
    let hist = s.tool_text("run_history", json!({ "tag": "kkbp" }));
    assert!(hist.contains("gate-1"), "{hist}");
    let pressure = s.tool_text("session_pressure", json!({}));
    assert!(
        pressure.contains("session_pressure") && pressure.contains("level="),
        "{pressure}"
    );
    let _ = s.tool_text(
        "mas_post",
        json!({
            "session": "cont-test",
            "role": "planner",
            "kind": "handoff",
            "summary": "continue fidelity",
            "commands": ["cargo test"],
            "constraints": ["do not edit plan file"]
        }),
    );
    let handoff = s.tool_text("session_handoff", json!({ "session": "cont-test" }));
    assert!(
        handoff.contains("Next-session prime") || handoff.contains("Priority defects"),
        "{handoff}"
    );
    let scope = s.tool_text("commit_scope", json!({}));
    assert!(scope.contains("commit_scope"), "{scope}");
    let arts = s.tool_text("artifact_index", json!({}));
    assert!(arts.contains("artifact_index"), "{arts}");
}

#[test]
fn repo_map_maps_fixture_source() {
    let mut s = Server::start();
    let out = s.tool_text("repo_map", json!({ "paths": ["src"], "mode": "symbols" }));
    assert!(out.contains("sample.cpp"), "{out}");
    assert!(out.contains("namespace demo"), "{out}");
    assert!(out.contains("struct Widget"), "{out}");
    assert!(out.contains("widget_area"), "{out}");
}

#[test]
fn symbol_refs_classifies_def_and_call() {
    let mut s = Server::start();
    let out = s.tool_text(
        "symbol_refs",
        json!({ "symbol": "widget_area", "paths": ["src"] }),
    );
    assert!(out.contains("1 def"), "{out}");
    assert!(out.contains("1 call"), "{out}");
    assert!(out.contains("definitions:"), "{out}");
    assert!(out.contains("calls:"), "{out}");
}

#[test]
fn symbol_refs_def_only_fast_path() {
    let mut s = Server::start();
    let out = s.tool_text(
        "symbol_refs",
        json!({ "symbol": "widget_area", "paths": ["src"], "kind": "def" }),
    );
    assert!(out.contains("definition-only"), "{out}");
    assert!(!out.contains("calls:"), "{out}");
}

#[test]
fn knowledge_search_finds_relevant_doc() {
    let mut s = Server::start();
    let out = s.tool_text(
        "knowledge_search",
        json!({ "query": "physics step", "semantic": false }),
    );
    assert!(out.contains("note.md"), "{out}");
    assert!(out.contains("Physics Pipeline"), "{out}");
}

#[test]
fn trace_summary_single_and_diff() {
    let mut s = Server::start();
    let single = s.tool_text(
        "trace_summary",
        json!({ "file": "cur.csv", "dir": "traces" }),
    );
    assert!(single.contains("PhysicsStep"), "{single}");

    let diff = s.tool_text(
        "trace_summary",
        json!({ "file": "cur.csv", "baseline": "base.csv", "dir": "traces" }),
    );
    assert!(diff.contains("diff"), "{diff}");
    assert!(diff.contains("PhysicsStep"), "{diff}");
    assert!(
        diff.contains("new"),
        "NewZone should be flagged new: {diff}"
    );
}

#[test]
fn call_graph_reports_callers_and_callees() {
    let mut s = Server::start();
    // `run` calls `widget_area` in its body.
    let run = s.tool_text("call_graph", json!({ "symbol": "run", "paths": ["src"] }));
    assert!(run.contains("callees"), "{run}");
    assert!(
        run.contains("widget_area"),
        "run should call widget_area: {run}"
    );

    // `widget_area` is called by `run`.
    let wa = s.tool_text(
        "call_graph",
        json!({ "symbol": "widget_area", "paths": ["src"] }),
    );
    assert!(wa.contains("callers"), "{wa}");
    assert!(
        wa.contains("run"),
        "widget_area should be called by run: {wa}"
    );
    assert!(wa.contains("definitions"), "{wa}");
}

#[test]
fn outline_lists_symbols_with_line_numbers() {
    let mut s = Server::start();
    let out = s.tool_text("outline", json!({ "file": "src/sample.cpp" }));
    assert!(out.contains("struct Widget"), "{out}");
    assert!(out.contains("widget_area"), "{out}");
    assert!(
        out.lines()
            .any(|l| l.contains("struct Widget") && l.chars().any(|c| c.is_ascii_digit())),
        "Widget should include a line number: {out}"
    );
}

#[test]
fn outline_missing_file_is_not_an_error() {
    let mut s = Server::start();
    let resp = s.call(json!({
        "jsonrpc": "2.0", "id": 60, "method": "tools/call",
        "params": {
            "name": "outline",
            "arguments": { "file": "no_such_file.rs" }
        }
    }));
    assert_eq!(resp["result"]["isError"], json!(false), "{resp}");
    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(text.contains("not found"), "{text}");
}

#[test]
fn outline_accepts_path_alias_for_file() {
    let mut s = Server::start();
    let out = s.tool_text("outline", json!({ "path": "src/sample.cpp" }));
    assert!(out.contains("struct Widget"), "{out}");
}

#[test]
fn type_layout_lists_fields() {
    let mut s = Server::start();
    let out = s.tool_text("type_layout", json!({ "type": "Widget", "paths": ["src"] }));
    assert!(out.contains("id"), "{out}");
    assert!(out.contains("value"), "{out}");
    // Both fields are POD scalars.
    assert!(out.contains("POD"), "{out}");
}

#[test]
fn doc_comment_returns_leading_comment_and_signature() {
    let mut s = Server::start();
    let out = s.tool_text(
        "doc_comment",
        json!({ "symbol": "widget_area", "paths": ["src"] }),
    );
    // The fixture documents widget_area with a `///` line above it.
    assert!(out.contains("area of a widget"), "{out}");
    assert!(out.contains("signature:"), "{out}");
    assert!(out.contains("widget_area"), "{out}");
    // The body must NOT be included - only the doc + signature.
    assert!(
        !out.contains("return w.id"),
        "doc_comment should omit the body: {out}"
    );
}

#[test]
fn symbol_context_composes_body_callers_and_callees() {
    let mut s = Server::start();
    let out = s.tool_text(
        "symbol_context",
        json!({ "symbol": "widget_area", "paths": ["src"] }),
    );
    assert!(out.contains("definition:"), "{out}");
    assert!(out.contains("return w.id"), "body should be present: {out}");
    assert!(out.contains("called by"), "{out}");
    assert!(out.contains("run"), "widget_area is called by run: {out}");
}

#[test]
fn symbol_context_includes_layout_for_records() {
    let mut s = Server::start();
    let out = s.tool_text(
        "symbol_context",
        json!({ "symbol": "Widget", "paths": ["src"] }),
    );
    assert!(out.contains("(record)"), "{out}");
    assert!(
        out.contains("layout:"),
        "a record should fold in its layout: {out}"
    );
    assert!(out.contains("value"), "{out}");
}

#[test]
fn big_functions_lists_by_span() {
    let mut s = Server::start();
    let out = s.tool_text("big_functions", json!({ "paths": ["src"] }));
    assert!(out.contains("big_functions"), "{out}");
    assert!(out.contains("widget_area") || out.contains("run"), "{out}");
}

#[test]
fn dead_code_lists_unused_symbols() {
    let mut s = Server::start();
    let out = s.tool_text("dead_code", json!({ "paths": ["src"] }));
    assert!(out.contains("dead_code"), "{out}");
}

#[test]
fn symbol_diff_runs_for_known_symbol() {
    let mut s = Server::start();
    let out = s.tool_text(
        "symbol_diff",
        json!({ "symbol": "widget_area", "paths": ["src"], "ref": "HEAD" }),
    );
    assert!(out.contains("symbol_diff"), "{out}");
    assert!(out.contains("widget_area"), "{out}");
}

#[test]
fn module_map_reports_modules() {
    let mut s = Server::start();
    let out = s.tool_text("module_map", json!({ "paths": ["src"] }));
    assert!(out.contains("module_map"), "{out}");
    assert!(out.contains('←') || out.contains("out /"), "{out}");
}

#[test]
fn diff_map_runs_against_fixture_git_repo() {
    let mut s = Server::start();
    let out = s.tool_text("diff_map", json!({ "paths": ["src"] }));
    assert!(out.contains("diff_map"), "{out}");
}

#[test]
fn call_path_finds_chain_between_functions() {
    let mut s = Server::start();
    // In the fixture, run() calls widget_area(), so a one-hop chain exists.
    let out = s.tool_text(
        "call_path",
        json!({ "from": "run", "to": "widget_area", "paths": ["src"] }),
    );
    assert!(out.contains("call_path"), "{out}");
    assert!(out.contains("hop"), "should report a chain length: {out}");
    assert!(out.contains("run") && out.contains("widget_area"), "{out}");
}

#[test]
fn undocumented_lists_symbols_without_docs() {
    let mut s = Server::start();
    let out = s.tool_text("undocumented", json!({ "paths": ["src"] }));
    assert!(out.contains("undocumented"), "{out}");
    // `run` and the `Widget` struct lack a doc comment; widget_area has one.
    assert!(out.contains("run"), "{out}");
    assert!(out.contains("Widget"), "{out}");
}

#[test]
fn usage_examples_shows_call_site_context() {
    let mut s = Server::start();
    let out = s.tool_text(
        "usage_examples",
        json!({ "symbol": "widget_area", "paths": ["src"] }),
    );
    assert!(out.contains("usage_examples"), "{out}");
    assert!(out.contains("sample.cpp"), "{out}");
    // The call site is shown with surrounding source, including the call itself.
    assert!(out.contains("widget_area(w)"), "{out}");
}

#[test]
fn test_map_finds_referencing_files() {
    let mut s = Server::start();
    let out = s.tool_text("test_map", json!({ "symbol": "widget_area" }));
    assert!(out.contains("test_widget.cpp"), "{out}");
}

#[test]
fn stats_reports_savings_after_a_call() {
    let mut s = Server::start();
    // One tool call to accumulate a saving in this server's hermetic cache.
    let _ = s.tool_text("repo_map", json!({ "paths": ["src"] }));
    let stats = s.tool_text("stats", json!({}));
    // Stats table header evolved from "estimated context avoided" → returned/scanned/counterfactual.
    assert!(
        stats.contains("counterfactual") || stats.contains("estimated context avoided"),
        "{stats}"
    );
    assert!(stats.contains("repo_map"), "{stats}");
    assert!(stats.contains("TOTAL"), "{stats}");
}

#[test]
fn integration_hooks_and_index_stale_run() {
    let mut s = Server::start();
    let hooks = s.tool_text("integration_hooks", json!({ "query": "physics" }));
    assert!(hooks.contains("physics_step"), "{hooks}");

    let stale = s.tool_text("index_stale", json!({ "ref": "HEAD" }));
    assert!(stale.contains("index_stale"), "{stale}");
}

#[test]
fn trace_profile_composes_sections() {
    let mut s = Server::start();
    let out = s.tool_text(
        "trace_profile",
        json!({ "file": "cur.csv", "dir": "traces" }),
    );
    assert!(out.contains("trace_profile"), "{out}");
    assert!(out.contains("Zone ranking"), "{out}");
    assert!(out.contains("diff_map"), "{out}");
    assert!(out.contains("index_stale"), "{out}");
}

#[test]
fn mas_blackboard_post_read_status_finalize() {
    let mut s = Server::start();
    let session = "stdio-mas-test";

    let post = s.tool_text(
        "mas_post",
        json!({
            "session": session,
            "role": "planner",
            "summary": "Refactor widget_area for clarity",
            "handoff_to": "solver",
            "claims": ["run is the only caller"],
            "anchors": ["src/sample.cpp:8"],
        }),
    );
    assert!(post.contains("mas_post"), "{post}");
    assert!(post.contains("entry id=1"), "{post}");

    let read = s.tool_text(
        "mas_read",
        json!({ "session": session, "recipient": "solver" }),
    );
    assert!(read.contains("Refactor widget_area"), "{read}");
    assert!(read.contains("handoff_to: solver"), "{read}");

    let status = s.tool_text("mas_status", json!({ "session": session }));
    assert!(status.contains("round: 1/"), "{status}");
    assert!(status.contains("planner: 1"), "{status}");

    let fin = s.tool_text(
        "mas_finalize",
        json!({ "session": session, "result": "Keep widget_area; add unit test." }),
    );
    assert!(fin.contains("marked final"), "{fin}");

    // Finalized sessions reject further posts.
    let resp = s.call(json!({
        "jsonrpc": "2.0", "id": 50, "method": "tools/call",
        "params": {
            "name": "mas_post",
            "arguments": {
                "session": session,
                "role": "planner",
                "summary": "too late"
            }
        }
    }));
    assert_eq!(resp["result"]["isError"], json!(true), "{resp}");
}

#[test]
fn unknown_tool_is_jsonrpc_error() {
    let mut s = Server::start();
    let resp = s.call(json!({
        "jsonrpc": "2.0", "id": 5, "method": "tools/call",
        "params": { "name": "nope", "arguments": {} }
    }));
    assert_eq!(resp["error"]["code"], json!(-32602), "{resp}");
}

#[test]
fn include_graph_reports_header_edges() {
    let mut s = Server::start();
    let out = s.tool_text(
        "include_graph",
        json!({ "header": "demo.h", "paths": ["src", "include"] }),
    );
    assert!(out.contains("include_graph"), "{out}");
    assert!(out.contains("demo.h"), "{out}");
}

#[test]
fn knowledge_upsert_writes_searchable_note() {
    let mut s = Server::start();
    let up = s.tool_text(
        "knowledge_upsert",
        json!({
            "path": ".wordkeep/notes/release-note.md",
            "heading": "Release note",
            "body": "Hermetic upsert round-trip for public release tests."
        }),
    );
    assert!(up.contains("knowledge_upsert"), "{up}");
    let search = s.tool_text(
        "knowledge_search",
        json!({
            "query": "hermetic upsert",
            "roots": [".wordkeep/notes"],
            "semantic": false
        }),
    );
    assert!(search.contains("release-note.md"), "{search}");
}

#[test]
fn stats_reset_zeros_counters() {
    let mut s = Server::start();
    let _ = s.tool_text("repo_map", json!({ "paths": ["src"] }));
    let before = s.tool_text("stats", json!({}));
    assert!(before.contains("repo_map"), "{before}");
    let reset = s.tool_text("stats", json!({ "reset": true }));
    assert!(reset.contains("reset"), "{reset}");
    let after = s.tool_text("stats", json!({}));
    assert!(after.contains("TOTAL"), "{after}");
}

#[test]
fn mas_finalize_can_promote_to_notes() {
    let mut s = Server::start();
    let session = "stdio-mas-promote";
    let _ = s.tool_text(
        "mas_post",
        json!({
            "session": session,
            "role": "planner",
            "summary": "ship v0.1.0",
            "handoff_to": "solver"
        }),
    );
    let fin = s.tool_text(
        "mas_finalize",
        json!({
            "session": session,
            "result": "Release checklist complete.",
            "promote": true,
            "note_path": ".wordkeep/notes/mas-result.md"
        }),
    );
    assert!(fin.contains("marked final"), "{fin}");
    let search = s.tool_text(
        "knowledge_search",
        json!({
            "query": "release checklist",
            "roots": [".wordkeep/notes"],
            "semantic": false
        }),
    );
    assert!(search.contains("mas-result.md"), "{search}");
}

#[test]
fn knowledge_upsert_effect_session_journaled_and_recovered() {
    let mut s = Server::start();
    let session = "stdio-effect-recover";

    let _ = s.tool_text(
        "mas_post",
        json!({
            "session": session,
            "role": "planner",
            "summary": "try revertible note",
            "handoff_to": "solver"
        }),
    );

    let up = s.tool_text(
        "knowledge_upsert",
        json!({
            "path": ".wordkeep/notes/effect-poc.md",
            "heading": "Effect PoC",
            "body": "journaled content should disappear on recover.",
            "effect_session": session
        }),
    );
    assert!(up.contains("knowledge_upsert"), "{up}");

    let status = s.tool_text("mas_status", json!({ "session": session }));
    assert!(status.contains("pending_effect_writes: 1"), "{status}");

    let fin = s.tool_text(
        "mas_finalize",
        json!({
            "session": session,
            "result": "abandon experiment",
            "effects": "recover"
        }),
    );
    assert!(fin.contains("reverted 1"), "{fin}");
    assert!(fin.contains("not finalized"), "{fin}");

    let search = s.tool_text(
        "knowledge_search",
        json!({
            "query": "journaled content should disappear",
            "roots": [".wordkeep/notes"],
            "semantic": false
        }),
    );
    assert!(
        !search.contains("effect-poc.md"),
        "note should be gone after recover: {search}"
    );
}

#[test]
fn mas_finalize_requires_effects_when_pending() {
    let mut s = Server::start();
    let session = "stdio-effect-required";

    let _ = s.tool_text(
        "mas_post",
        json!({
            "session": session,
            "role": "planner",
            "summary": "pending write test"
        }),
    );
    let _ = s.tool_text(
        "knowledge_upsert",
        json!({
            "path": ".wordkeep/notes/effect-required.md",
            "heading": "Pending",
            "body": "needs effects arg",
            "effect_session": session
        }),
    );

    let resp = s.call(json!({
        "jsonrpc": "2.0", "id": 60, "method": "tools/call",
        "params": {
            "name": "mas_finalize",
            "arguments": {
                "session": session,
                "result": "done"
            }
        }
    }));
    assert_eq!(resp["result"]["isError"], json!(true), "{resp}");
    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(text.contains("pending effect writes"), "{text}");
}

#[test]
fn paths_escape_root_is_rejected() {
    let mut s = Server::start();
    let err = s.tool_error("repo_map", json!({ "paths": ["../outside"] }));
    assert!(err.contains("..") || err.contains("relative"), "{err}");
}

#[test]
fn progress_token_emits_notification() {
    // Progress is opt-in (Cursor Shared MCP fatals on unknown progress tokens).
    std::env::set_var("WORDKEEP_MCP_PROGRESS", "1");
    let mut s = Server::start();
    let _ = s.call(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "t", "version": "0" } }
    }));
    let line = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": 42,
        "method": "tools/call",
        "params": {
            "name": "repo_map",
            "arguments": { "mode": "files", "token_budget": 800 },
            "_meta": { "progressToken": "stdio-progress" }
        }
    }))
    .unwrap();
    s.stdin.write_all(line.as_bytes()).unwrap();
    s.stdin.write_all(b"\n").unwrap();
    s.stdin.flush().unwrap();
    let mut saw_progress = false;
    let mut saw_result = false;
    for _ in 0..32 {
        let mut resp = String::new();
        let n = s.stdout.read_line(&mut resp).expect("read");
        assert!(n > 0);
        let v: Value = serde_json::from_str(&resp).expect("json");
        if v.get("method").and_then(Value::as_str) == Some("notifications/progress") {
            assert_eq!(v["params"]["progressToken"], json!("stdio-progress"));
            saw_progress = true;
        }
        if v.get("id") == Some(&json!(42)) {
            saw_result = true;
            break;
        }
    }
    assert!(
        saw_progress,
        "expected notifications/progress before result"
    );
    assert!(saw_result, "expected tools/call result");
}

#[test]
fn repo_map_uses_config_default_paths() {
    let mut s = Server::start();
    let out = s.tool_text("repo_map", json!({}));
    assert!(out.contains("sample.cpp"), "{out}");
}

#[test]
fn izakaya_presence_round_trip() {
    let mut s = Server::start();
    let inn = s.tool_text(
        "izakaya_check_in",
        json!({
            "agent_id": "alpha",
            "task": "smoke",
            "observe_git": false,
            "claims": [{"path": "src/sample.cpp", "symbols": ["run"]}]
        }),
    );
    assert!(inn.contains("lease_id:"), "{inn}");
    let board = s.tool_text("izakaya_status", json!({}));
    assert!(board.contains("alpha"), "{board}");
    let advice = s.tool_text("izakaya_advise", json!({}));
    assert!(
        advice.contains("inert") || advice.contains("no promoted"),
        "{advice}"
    );
}

#[test]
fn mcp_2025_11_annotations_and_structured_content() {
    let mut s = Server::start();
    let list = s.call(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }));
    let tools = list["result"]["tools"].as_array().unwrap();
    let by_name = |n: &str| tools.iter().find(|t| t["name"] == n).unwrap();

    assert_eq!(by_name("repo_map")["annotations"]["readOnlyHint"], true);
    assert_eq!(by_name("repo_map")["annotations"]["openWorldHint"], false);
    assert_eq!(by_name("run_record")["annotations"]["readOnlyHint"], false);
    assert_eq!(by_name("run_record")["annotations"]["destructiveHint"], false);
    assert_eq!(by_name("task_cancel")["annotations"]["destructiveHint"], true);
    assert_eq!(by_name("task_cancel")["annotations"]["openWorldHint"], true);
    assert_eq!(by_name("copper_status")["annotations"]["readOnlyHint"], true);
    assert_eq!(by_name("copper_status")["annotations"]["openWorldHint"], true);

    // 1. stats
    let _ = s.tool_text("repo_map", json!({}));
    let stats_resp = s.call(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": { "name": "stats", "arguments": {} }
    }));
    assert!(stats_resp["result"]["content"][0]["text"].is_string());
    assert_eq!(stats_resp["result"]["structuredContent"]["version"], 5, "{stats_resp}");
    assert!(stats_resp["result"]["structuredContent"]["tools"].is_object());

    // 2. semantic_decide
    let sd_resp = s.call(json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {
            "name": "semantic_decide",
            "arguments": {
                "state": "cache hit ratio 95%",
                "question": "keep warm store?",
                "options": [
                    { "id": "keep", "description": "retain warm cache hit store" },
                    { "id": "drop", "description": "discard cold files" }
                ]
            }
        }
    }));
    assert!(sd_resp["result"]["structuredContent"]["chosen"].is_string(), "{sd_resp}");
    assert!(sd_resp["result"]["structuredContent"]["options"].is_array(), "{sd_resp}");

    // 3. decree
    let decree_resp = s.call(json!({
        "jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": {
            "name": "decree",
            "arguments": {
                "method": "feedback",
                "prompt_sha256": "abc123",
                "chosen": "keep",
                "outcome": "accept"
            }
        }
    }));
    assert_eq!(decree_resp["result"]["structuredContent"]["ok"], true, "{decree_resp}");

    // 4. izakaya_status
    let iz_resp = s.call(json!({
        "jsonrpc": "2.0", "id": 5, "method": "tools/call",
        "params": { "name": "izakaya_status", "arguments": {} }
    }));
    assert_eq!(iz_resp["result"]["structuredContent"]["read_only"], true, "{iz_resp}");
    assert!(iz_resp["result"]["structuredContent"]["agents"].is_array(), "{iz_resp}");

    // 5. copper_status
    let cop_resp = s.call(json!({
        "jsonrpc": "2.0", "id": 6, "method": "tools/call",
        "params": { "name": "copper_status", "arguments": { "refresh": false } }
    }));
    assert!(cop_resp["result"]["structuredContent"]["transport_hint"].is_string(), "{cop_resp}");

    // 6. index_health
    let ih_resp = s.call(json!({
        "jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": { "name": "index_health", "arguments": {} }
    }));
    assert_eq!(ih_resp["result"]["structuredContent"]["ref"], "HEAD", "{ih_resp}");
    assert!(ih_resp["result"]["structuredContent"]["union_coverage_roots"].is_array(), "{ih_resp}");
}

#[test]
fn mcp_2025_11_resource_templates_and_core_toolset() {
    let mut s = Server::start_with_env(&[("WORDKEEP_TOOLSET", "core")]);
    let list = s.call(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }));
    let tools = list["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!((20..=24).contains(&names.len()), "core toolset size={}: {names:?}", names.len());
    assert!(names.contains(&"repo_map"));
    assert!(names.contains(&"tool_discover"));
    assert!(!names.contains(&"shrift_list"));

    // Discover extended tool and invoke it directly.
    let disc = s.call(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": { "name": "tool_discover", "arguments": { "query": "shrift" } }
    }));
    assert!(disc["result"]["content"][0]["text"].as_str().unwrap().contains("shrift_list"), "{disc}");
    assert!(disc["result"]["structuredContent"]["matched"].as_u64().unwrap() >= 1);

    let shrift_out = s.tool_text("shrift_list", json!({}));
    assert!(shrift_out.contains("shrift"), "{shrift_out}");

    // Resource templates list + read for symbol and defect.
    let tmpl = s.call(json!({ "jsonrpc": "2.0", "id": 3, "method": "resources/templates/list" }));
    let templates = tmpl["result"]["resourceTemplates"].as_array().unwrap();
    assert!(templates.iter().any(|t| t["uriTemplate"] == "wordkeep://symbol/{name}"), "{tmpl}");
    assert!(templates.iter().any(|t| t["uriTemplate"] == "wordkeep://defect/{id}"), "{tmpl}");

    let sym_read = s.call(json!({
        "jsonrpc": "2.0", "id": 4, "method": "resources/read",
        "params": { "uri": "wordkeep://symbol/Widget" }
    }));
    let sym_text = sym_read["result"]["contents"][0]["text"].as_str().unwrap_or("");
    assert!(sym_text.contains("struct Widget"), "{sym_read}");

    s.tool_text("defect_upsert", json!({ "id": "tmpl-def-1", "summary": "template read smoke" }));
    let def_read = s.call(json!({
        "jsonrpc": "2.0", "id": 5, "method": "resources/read",
        "params": { "uri": "wordkeep://defect/tmpl-def-1" }
    }));
    let def_text = def_read["result"]["contents"][0]["text"].as_str().unwrap_or("");
    assert!(def_text.contains("template read smoke"), "{def_read}");
}

#[test]
fn ergonomics_aliases_markdown_outline_and_telemetry() {
    let mut s = Server::start();
    std::fs::write(
        s._fixture.root.join("docs_guide.mdc"),
        "---\ndescription: Guide rule\n---\n\n# Intro\n\nhello\n\n## Usage\n\nworld\n",
    )
    .unwrap();
    let md_out = s.tool_text("outline", json!({ "file": "docs_guide.mdc" }));
    assert!(md_out.contains("frontmatter:") && md_out.contains("Guide rule"), "{md_out}");
    assert!(md_out.contains("# Intro") && md_out.contains("## Usage"), "{md_out}");

    let tl_sym = s.tool_text("type_layout", json!({ "symbol": "Widget" }));
    assert!(tl_sym.contains("Widget") && tl_sym.contains("id") && tl_sym.contains("POD"), "{tl_sym}");
    let tl_name = s.tool_text("type_layout", json!({ "name": "Widget" }));
    assert!(tl_name.contains("Widget") && tl_name.contains("value") && tl_name.contains("POD"), "{tl_name}");

    let tm_name = s.tool_text("test_map", json!({ "name": "widget_area" }));
    assert!(tm_name.contains("test_widget.cpp"), "{tm_name}");

    let rr_pass = s.tool_text("run_record", json!({ "command": "ctest", "status": "pass" }));
    assert!(rr_pass.contains("status=passed"), "{rr_pass}");
    let rr_fail = s.tool_text("run_record", json!({ "command": "ctest", "status": "fail" }));
    assert!(rr_fail.contains("status=failed"), "{rr_fail}");

    s.tool_text("izakaya_check_in", json!({ "agent_id": "erg-agent", "observe_git": false }));
    let iz_upd = s.tool_text("izakaya_update", json!({ "agent_id": "erg-agent", "state": "live_code", "observe_git": false }));
    assert!(iz_upd.contains("live_code"), "{iz_upd}");
    let iz_done = s.tool_text("izakaya_update", json!({ "agent_id": "erg-agent", "state": "done", "observe_git": false }));
    assert!(iz_done.contains("checked out") || iz_done.contains("checked_out"), "{iz_done}");

    let handoff_default = s.tool_text("session_handoff", json!({}));
    assert!(handoff_default.contains("session_handoff"), "{handoff_default}");

    // Warm store cache hits + compact mutation low_yield exemption check in stats.
    s.tool_text("repo_map", json!({}));
    s.tool_text("repo_map", json!({}));
    let stats_resp = s.call(json!({
        "jsonrpc": "2.0", "id": 99, "method": "tools/call",
        "params": { "name": "stats", "arguments": {} }
    }));
    let sc = &stats_resp["result"]["structuredContent"]["tools"];
    assert!(sc["repo_map"]["cache_hits"].as_u64().unwrap_or(0) >= 1, "{sc}");
    assert!(sc["repo_map"]["bytes_read"].as_u64().unwrap_or(0) >= 1, "{sc}");
    assert_eq!(sc["run_record"]["low_yield_count"].as_u64().unwrap_or(0), 0, "{sc}");
}

#[test]
fn mcp_2025_11_async_tasks_stdio_trace_profile_and_scip_refresh() {
    let mut s = Server::start();
    let init = s.call(json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2025-11-25" }
    }));
    assert!(init["result"]["capabilities"]["tasks"]["list"].is_object(), "{init}");
    assert!(init["result"]["capabilities"]["tasks"]["cancel"].is_object(), "{init}");

    let list = s.call(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    let tools = list["result"]["tools"].as_array().unwrap();
    let exec_of = |n: &str| {
        tools
            .iter()
            .find(|t| t["name"] == n)
            .and_then(|t| t.get("execution"))
            .and_then(|e| e.get("taskSupport"))
            .and_then(Value::as_str)
    };
    assert_eq!(exec_of("task_submit"), Some("optional"));
    assert_eq!(exec_of("trace_profile"), Some("optional"));
    assert_eq!(exec_of("index_health"), Some("optional"));
    assert_eq!(exec_of("index_stale"), Some("optional"));
    assert_eq!(exec_of("repo_map"), None);

    // 1. Async trace_profile via tools/call + params.task -> tasks/result.
    let tp_create = s.call(json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {
            "name": "trace_profile",
            "arguments": {},
            "task": { "ttl": 60000 }
        }
    }));
    let tp_tid = tp_create["result"]["task"]["taskId"]
        .as_str()
        .expect("taskId")
        .to_string();
    let tp_res = s.call(json!({
        "jsonrpc": "2.0", "id": 4, "method": "tasks/result",
        "params": { "taskId": tp_tid }
    }));
    assert_eq!(tp_res["result"]["isError"], false, "{tp_res}");
    assert!(
        tp_res["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("trace_profile"),
        "{tp_res}"
    );

    // 2. Async SCIP refresh via index_health mode=scip_refresh + JSON SCIP fixture.
    std::fs::write(
        s._fixture.root.join("index.scip"),
        r#"{"metadata":{"tool_info":{"name":"rust-analyzer","version":"0.1"}},"documents":[],"edges":[{"file":"src/sample.cpp","src_name":"widget_area","dst_name":"helper","line":4,"kind":"call","site_text":"helper()"}]}"#,
    )
    .unwrap();
    let scip_create = s.call(json!({
        "jsonrpc": "2.0", "id": 5, "method": "tools/call",
        "params": {
            "name": "index_health",
            "arguments": { "mode": "scip_refresh" },
            "task": {}
        }
    }));
    let scip_tid = scip_create["result"]["task"]["taskId"]
        .as_str()
        .expect("scip taskId")
        .to_string();
    let scip_res = s.call(json!({
        "jsonrpc": "2.0", "id": 6, "method": "tasks/result",
        "params": { "taskId": scip_tid }
    }));
    assert_eq!(scip_res["result"]["isError"], false, "{scip_res}");
    assert_eq!(scip_res["result"]["structuredContent"]["refreshed"], true, "{scip_res}");
    assert!(
        scip_res["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("scip_refresh"),
        "{scip_res}"
    );

    // 3. tasks/get + tasks/list verify completed state.
    let get_resp = s.call(json!({
        "jsonrpc": "2.0", "id": 7, "method": "tasks/get",
        "params": { "taskId": scip_tid }
    }));
    assert_eq!(get_resp["result"]["status"], "completed", "{get_resp}");

    let list_resp = s.call(json!({
        "jsonrpc": "2.0", "id": 8, "method": "tasks/list",
        "params": {}
    }));
    assert!(
        list_resp["result"]["tasks"].as_array().unwrap().len() >= 2,
        "{list_resp}"
    );

    // 4. Async task_submit (stubbed local job) + tasks/cancel on a slow stub job.
    let jobs_dir = s._fixture.root.join("tools/palaestra/remote/jobs");
    std::fs::create_dir_all(&jobs_dir).unwrap();
    let run_sh = s._fixture.root.join("tools/palaestra/remote/run_job.sh");
    std::fs::write(
        &run_sh,
        "#!/usr/bin/env bash\nset -euo pipefail\nJOB=\"\"\nwhile [[ $# -gt 0 ]]; do case \"$1\" in --job) JOB=\"$2\"; shift 2;; *) shift;; esac; done\nROOT=\"$(cd \"$(dirname \"$0\")/../../..\" && pwd)\"\nmkdir -p \"$ROOT/.cache/palaestra/remote/$JOB\"\necho ok > \"$ROOT/.cache/palaestra/remote/$JOB/run.log\"\nif [[ \"$JOB\" == \"slow-stub\" ]]; then sleep 5; fi\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&run_sh).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&run_sh, perms).unwrap();
    }
    std::fs::write(jobs_dir.join("fast-stub.sh"), "#!/usr/bin/env bash\n").unwrap();
    std::fs::write(jobs_dir.join("slow-stub.sh"), "#!/usr/bin/env bash\n").unwrap();

    let sub_fast = s.call(json!({
        "jsonrpc": "2.0", "id": 9, "method": "tools/call",
        "params": {
            "name": "task_submit",
            "arguments": { "job": "fast-stub", "wait": true },
            "task": {}
        }
    }));
    let fast_tid = sub_fast["result"]["task"]["taskId"].as_str().unwrap().to_string();
    let fast_res = s.call(json!({
        "jsonrpc": "2.0", "id": 10, "method": "tasks/result",
        "params": { "taskId": fast_tid }
    }));
    assert_eq!(fast_res["result"]["isError"], false, "{fast_res}");
    assert_eq!(fast_res["result"]["structuredContent"]["status"], "done", "{fast_res}");

    let sub_slow = s.call(json!({
        "jsonrpc": "2.0", "id": 11, "method": "tools/call",
        "params": {
            "name": "task_submit",
            "arguments": { "job": "slow-stub", "wait": true },
            "task": {}
        }
    }));
    let slow_tid = sub_slow["result"]["task"]["taskId"].as_str().unwrap().to_string();
    std::thread::sleep(std::time::Duration::from_millis(60));
    let cancel_resp = s.call(json!({
        "jsonrpc": "2.0", "id": 12, "method": "tasks/cancel",
        "params": { "taskId": slow_tid }
    }));
    assert_eq!(cancel_resp["result"]["status"], "cancelled", "{cancel_resp}");
}

#[test]
fn staleness_links_stdio_surfaces_stale_anchor_in_search_shrift_defects_and_health() {
    let mut s = Server::start();
    // Create note, shrift, and defect referencing `widget_area` / `src/sample.cpp`.
    s.tool_text(
        "knowledge_upsert",
        json!({
            "path": ".wordkeep/notes/widget-note.md",
            "heading": "Widget Area Contract",
            "body": "Geometry helper calls `widget_area` in `src/sample.cpp`."
        }),
    );
    s.tool_text(
        "shrift_upsert",
        json!({
            "title": "SIMD widget area",
            "body": "Vectorize `widget_area` for batch queries."
        }),
    );
    s.tool_text(
        "defect_upsert",
        json!({
            "id": "widget-neg-area",
            "subsystem": "math",
            "summary": "Guard `widget_area` against negative extents",
            "anchors": ["src/sample.cpp:1"]
        }),
    );

    // Index observation links while `widget_area` matches original fixture.
    let initial_kb = s.tool_text(
        "knowledge_search",
        json!({ "query": "Widget Area Contract", "roots": [".wordkeep/notes"] }),
    );
    assert!(!initial_kb.contains("[stale-anchor"), "{initial_kb}");

    // Mutate `widget_area` implementation in `src/sample.cpp`.
    let sample_cpp = s._fixture.root.join("src/sample.cpp");
    let orig = std::fs::read_to_string(&sample_cpp).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(15));
    let mutated = orig.replace("return w.id;", "return w.id * 2;");
    assert_ne!(orig, mutated, "fixture sample.cpp must contain `return w.id;`");
    std::fs::write(&sample_cpp, mutated).unwrap();

    let kb = s.tool_text(
        "knowledge_search",
        json!({ "query": "Widget Area Contract", "roots": [".wordkeep/notes"] }),
    );
    assert!(kb.contains("[stale-anchor"), "expected [stale-anchor] in knowledge_search:\n{kb}");

    let sr = s.tool_text("shrift_review", json!({}));
    assert!(sr.contains("[stale-anchor"), "expected [stale-anchor] in shrift_review:\n{sr}");

    let dl = s.tool_text("defect_list", json!({}));
    assert!(dl.contains("[stale-anchor"), "expected [stale-anchor] in defect_list:\n{dl}");

    let ih = s.tool_text("index_health", json!({}));
    assert!(ih.contains("[stale-anchor]"), "expected [stale-anchor] in index_health:\n{ih}");
}

