//! Two Wordkeep processes checking in at once must not drop either agent.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

#[test]
fn two_processes_keep_both_check_ins() {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("wk_iza_proc_root_{}_{id}", std::process::id()));
    let cache = std::env::temp_dir().join(format!("wk_iza_proc_cache_{}_{id}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&cache);
    std::fs::create_dir_all(&root).unwrap();

    let spawn = |agent: &str| {
        Command::new(env!("CARGO_BIN_EXE_wordkeep"))
            .args([
                "--root",
                root.to_str().unwrap(),
                "izakaya",
                "check-in",
                "--agent",
                agent,
                "--task",
                agent,
                "--no-git",
            ])
            .env("XDG_CACHE_HOME", &cache)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn")
    };
    let mut a = spawn("proc-a");
    let mut b = spawn("proc-b");
    let sa = a.wait().unwrap();
    let sb = b.wait().unwrap();
    assert!(sa.success(), "proc-a failed: {sa}");
    assert!(sb.success(), "proc-b failed: {sb}");

    let status = Command::new(env!("CARGO_BIN_EXE_wordkeep"))
        .args([
            "--root",
            root.to_str().unwrap(),
            "izakaya",
            "status",
            "--include-checked-out",
        ])
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&status.stdout);
    assert!(
        status.status.success(),
        "{text} {}",
        String::from_utf8_lossy(&status.stderr)
    );
    assert!(text.contains("proc-a"), "{text}");
    assert!(text.contains("proc-b"), "{text}");

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&cache);
}
