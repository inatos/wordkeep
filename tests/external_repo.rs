//! Hermetic tests for external consumer repositories and root isolation.

use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "wk_ext_{name}_{}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

fn tool_text(
    root: &Path,
    cache_home: &Path,
    cwd: &Path,
    tool: &str,
    args: serde_json::Value,
) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_wordkeep"))
        .arg("--root")
        .arg(root)
        .current_dir(cwd)
        .env("XDG_CACHE_HOME", cache_home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    let req = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": tool, "arguments": args }
    });
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "{}", serde_json::to_string(&req).unwrap()).unwrap();
    writeln!(
        stdin,
        "{}",
        serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": 2, "method": "initialize", "params": {}
        }))
        .unwrap()
    )
    .unwrap();
    stdin.flush().unwrap();
    drop(stdin);
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut last = String::new();
    loop {
        let mut line = String::new();
        if stdout.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if line.contains("\"tools/call\"")
            || line.contains("\"result\"") && line.contains("content")
        {
            last = line;
        }
    }
    let _ = child.wait();
    let v: serde_json::Value = serde_json::from_str(&last).expect("json");
    v["result"]["content"][0]["text"]
        .as_str()
        .expect("text")
        .to_string()
}

fn write_consumer_repo(root: &Path) {
    std::fs::create_dir_all(root.join(".wordkeep")).unwrap();
    std::fs::write(
        root.join(".wordkeep/config.json"),
        r#"{"default_paths":["src"],"test_command":"cargo test"}"#,
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/helper.rs"),
        "pub fn helper() -> i32 { util::value() }\n",
    )
    .unwrap();
    std::fs::write(root.join("src/util.rs"), "pub fn value() -> i32 { 1 }\n").unwrap();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(
        root.join("docs/guide.md"),
        "# Guide\n\nhelper calls util.\n",
    )
    .unwrap();
}

#[test]
fn external_repo_resolves_default_paths() {
    let root = temp("consumer");
    let cache = temp("cache");
    let cwd = temp("cwd");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&cache);
    std::fs::create_dir_all(&cwd).unwrap();
    write_consumer_repo(&root);
    let out = tool_text(&root, &cache, &cwd, "repo_map", json!({}));
    assert!(out.contains("helper"), "{out}");
    assert!(out.contains("util"), "{out}");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&cache);
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn non_git_repo_diff_map_degrades_gracefully() {
    let root = temp("nongit");
    let cache = temp("cache2");
    let cwd = temp("cwd2");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&cache);
    std::fs::create_dir_all(&cwd).unwrap();
    write_consumer_repo(&root);
    let out = tool_text(&root, &cache, &cwd, "diff_map", json!({ "paths": ["src"] }));
    assert!(out.contains("diff_map"), "{out}");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&cache);
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn wordkeep_root_env_selects_target_repo() {
    let root = temp("envroot");
    let cache = temp("cache3");
    let cwd = temp("cwd3");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&cache);
    std::fs::create_dir_all(&cwd).unwrap();
    write_consumer_repo(&root);
    let mut child = Command::new(env!("CARGO_BIN_EXE_wordkeep"))
        .env("WORDKEEP_ROOT", &root)
        .env("XDG_CACHE_HOME", &cache)
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    let req = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "repo_map", "arguments": {} }
    });
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "{}", serde_json::to_string(&req).unwrap()).unwrap();
    stdin.flush().unwrap();
    drop(stdin);
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    assert!(line.contains("helper"), "{line}");
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&cache);
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn profiles_route_paths_and_knowledge_roots() {
    let root = temp("profiles");
    let cache = temp("cache_prof");
    let cwd = temp("cwd_prof");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&cache);
    std::fs::create_dir_all(&cwd).unwrap();
    write_consumer_repo(&root);
    std::fs::create_dir_all(root.join("tools/kkbp")).unwrap();
    std::fs::write(root.join("tools/kkbp/pipe.rs"), "pub fn bake() {}\n").unwrap();
    std::fs::write(
        root.join(".wordkeep/config.json"),
        r#"{
          "default_paths":["src"],
          "path_profiles":{"engine":["src"],"kkbp":["tools/kkbp"]},
          "profile_hints":{"kkbp":["kkbp","bake"]},
          "knowledge_write_roots":["notes"],
          "artifact_roots":["artifacts"]
        }"#,
    )
    .unwrap();
    std::fs::create_dir_all(root.join("notes")).unwrap();
    std::fs::create_dir_all(root.join("artifacts")).unwrap();
    std::fs::write(root.join("artifacts/shot.png"), b"not-a-real-png").unwrap();

    let mapped = tool_text(
        &root,
        &cache,
        &cwd,
        "repo_map",
        json!({ "profile": "kkbp" }),
    );
    assert!(
        mapped.contains("pipe.rs") || mapped.contains("bake"),
        "{mapped}"
    );

    let upsert = tool_text(
        &root,
        &cache,
        &cwd,
        "knowledge_upsert",
        json!({
            "path": "notes/session.md",
            "heading": "Handoff",
            "body": "profile routing works"
        }),
    );
    assert!(upsert.contains("knowledge_upsert"), "{upsert}");
    assert!(root.join("notes/session.md").is_file());

    let arts = tool_text(
        &root,
        &cache,
        &cwd,
        "artifact_index",
        json!({ "refresh": true }),
    );
    assert!(arts.contains("artifact_index"), "{arts}");

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&cache);
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn workspace_mas_isolation_across_roots() {
    let root_a = temp("iso_a");
    let root_b = temp("iso_b");
    let cache = temp("cache_iso");
    let cwd = temp("cwd_iso");
    let _ = std::fs::remove_dir_all(&root_a);
    let _ = std::fs::remove_dir_all(&root_b);
    let _ = std::fs::remove_dir_all(&cache);
    std::fs::create_dir_all(&cwd).unwrap();
    write_consumer_repo(&root_a);
    write_consumer_repo(&root_b);

    let _ = tool_text(
        &root_a,
        &cache,
        &cwd,
        "mas_post",
        json!({
            "session": "shared-name",
            "role": "planner",
            "summary": "only in A"
        }),
    );
    let read_b = tool_text(
        &root_b,
        &cache,
        &cwd,
        "mas_read",
        json!({ "session": "shared-name" }),
    );
    assert!(
        read_b.contains("not found") || read_b.contains("no matching") || read_b.contains("error:"),
        "{read_b}"
    );

    let _ = std::fs::remove_dir_all(&root_a);
    let _ = std::fs::remove_dir_all(&root_b);
    let _ = std::fs::remove_dir_all(&cache);
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn izakaya_presence_is_isolated_across_roots() {
    let root_a = temp("iza_a");
    let root_b = temp("iza_b");
    let cache = temp("iza_cache");
    let cwd = temp("iza_cwd");
    let _ = std::fs::remove_dir_all(&root_a);
    let _ = std::fs::remove_dir_all(&root_b);
    let _ = std::fs::remove_dir_all(&cache);
    std::fs::create_dir_all(&cwd).unwrap();
    write_consumer_repo(&root_a);
    write_consumer_repo(&root_b);

    let checked = tool_text(
        &root_a,
        &cache,
        &cwd,
        "izakaya_check_in",
        json!({
            "agent_id": "alpha",
            "task": "only-a",
            "observe_git": false
        }),
    );
    assert!(checked.contains("alpha"), "{checked}");
    let other = tool_text(&root_b, &cache, &cwd, "izakaya_status", json!({}));
    assert!(!other.contains("only-a"), "{other}");

    let _ = std::fs::remove_dir_all(&root_a);
    let _ = std::fs::remove_dir_all(&root_b);
    let _ = std::fs::remove_dir_all(&cache);
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn commit_scope_degrades_without_git() {
    let root = temp("nongit_scope");
    let cache = temp("cache_scope");
    let cwd = temp("cwd_scope");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&cache);
    std::fs::create_dir_all(&cwd).unwrap();
    write_consumer_repo(&root);
    let out = tool_text(&root, &cache, &cwd, "commit_scope", json!({}));
    assert!(out.contains("commit_scope"), "{out}");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&cache);
    let _ = std::fs::remove_dir_all(&cwd);
}
