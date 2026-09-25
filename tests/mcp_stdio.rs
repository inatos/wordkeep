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
            let v: Value =
                serde_json::from_str(&resp).unwrap_or_else(|e| panic!("bad response {resp:?}: {e}"));
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
    ] {
        assert!(names.contains(&n), "missing tool {n}: {names:?}");
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
        51,
        "expected 51 tools, got {}: {names:?}",
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
    assert!(caps_text.contains("\"tool_count\": 51"), "{caps}");
    assert!(caps_text.contains("knowledge_upsert"), "{caps}");
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
    let out = s.tool_text(
        "repo_map",
        json!({ "paths": ["src"], "mode": "symbols" }),
    );
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
    assert!(stats.contains("estimated context avoided"), "{stats}");
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
    assert!(saw_progress, "expected notifications/progress before result");
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
