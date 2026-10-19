//! Fail-open shell digest for noisy git and test commands.
//!
//! `wordkeep digest -- <cmd...>` runs the command, prints a shorter stdout when
//! the command is on the allowlist, and otherwise prints the original output.
//! The child exit code is preserved. `WORDKEEP_DIGEST=0` skips filtering.
//! File reads (`cat`, `Read`) are never rewritten.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    GitStatus,
    GitDiff,
    GitLog,
    Ctest,
    CargoTest,
    CargoBuild,
}

/// Run `wordkeep digest` argv (tokens after the subcommand).
pub fn cli(args: &[String]) -> i32 {
    let cmd = command_tokens(args);
    if cmd.is_empty() {
        eprintln!("usage: wordkeep digest -- <command> [args...]");
        return 2;
    }
    if digest_disabled() {
        return spawn_passthrough(&cmd);
    }
    let mut child = match Command::new(&cmd[0])
        .args(&cmd[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            eprintln!("wordkeep digest: failed to spawn {}: {e}", cmd[0]);
            return 127;
        }
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }
    let status = match child.wait() {
        Ok(status) => status,
        Err(e) => {
            eprintln!("wordkeep digest: wait failed: {e}");
            let _ = std::io::stdout().write_all(stdout.as_bytes());
            let _ = std::io::stderr().write_all(stderr.as_bytes());
            return 1;
        }
    };
    let filtered = filter_command(&cmd, &stdout, &stderr);
    let _ = std::io::stdout().write_all(filtered.stdout.as_bytes());
    let _ = std::io::stderr().write_all(filtered.stderr.as_bytes());
    status.code().unwrap_or(1)
}

struct Filtered {
    stdout: String,
    stderr: String,
}

fn digest_disabled() -> bool {
    std::env::var("WORDKEEP_DIGEST").is_ok_and(|v| v == "0" || v.eq_ignore_ascii_case("false"))
}

fn command_tokens(args: &[String]) -> Vec<String> {
    let mut args = args;
    if args.first().is_some_and(|a| a == "--") {
        args = &args[1..];
    }
    args.to_vec()
}

fn spawn_passthrough(cmd: &[String]) -> i32 {
    match Command::new(&cmd[0]).args(&cmd[1..]).status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("wordkeep digest: failed to spawn {}: {e}", cmd[0]);
            127
        }
    }
}

fn filter_command(cmd: &[String], stdout: &str, stderr: &str) -> Filtered {
    let Some(kind) = classify(cmd) else {
        return Filtered {
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        };
    };
    let stdout_f = match kind {
        Kind::GitStatus => filter_git_status(stdout).unwrap_or_else(|| stdout.to_string()),
        Kind::GitDiff => shrink_or_keep(stdout, &filter_git_diff(stdout)),
        Kind::GitLog => shrink_or_keep(stdout, &filter_git_log(stdout)),
        Kind::Ctest => shrink_or_keep(stdout, &filter_ctest(stdout)),
        Kind::CargoTest => shrink_or_keep(stdout, &filter_cargo_test(stdout)),
        Kind::CargoBuild => shrink_or_keep(stdout, &filter_cargo_build(stdout)),
    };
    let stderr_f = match kind {
        Kind::CargoBuild | Kind::CargoTest | Kind::Ctest => {
            shrink_or_keep(stderr, &filter_cargo_build(stderr))
        }
        _ => stderr.to_string(),
    };
    Filtered {
        stdout: stdout_f,
        stderr: stderr_f,
    }
}

fn shrink_or_keep(original: &str, filtered: &str) -> String {
    if filtered.trim().is_empty() || filtered.len() > original.len() {
        original.to_string()
    } else {
        filtered.to_string()
    }
}

fn classify(cmd: &[String]) -> Option<Kind> {
    let bin = cmd.first()?.rsplit('/').next()?;
    match bin {
        "git" => match cmd.get(1).map(String::as_str) {
            Some("status") => Some(Kind::GitStatus),
            Some("diff") => Some(Kind::GitDiff),
            Some("log") => Some(Kind::GitLog),
            _ => None,
        },
        "ctest" => Some(Kind::Ctest),
        "cargo" => match cmd.get(1).map(String::as_str) {
            Some("test") => Some(Kind::CargoTest),
            Some("build") | Some("check") => Some(Kind::CargoBuild),
            _ => None,
        },
        _ => None,
    }
}

fn filter_git_status(text: &str) -> Option<String> {
    let mut branch = String::new();
    let mut groups: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    let mut recognized = false;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            branch = rest.split("...").next().unwrap_or(rest).trim().to_string();
            recognized = true;
            continue;
        }
        if let Some(rest) = line.strip_prefix("On branch ") {
            branch = rest.trim().to_string();
            recognized = true;
            continue;
        }
        if line.len() >= 4 && line.as_bytes().get(2) == Some(&b' ') && !line.starts_with('\t') {
            let status = line[..2].trim();
            let path = line[3..].trim();
            if path.is_empty() {
                continue;
            }
            let key = match status {
                "??" => "untracked",
                "M" | "MM" => "modified",
                "A" | "AM" => "added",
                "D" => "deleted",
                "R" | "RM" => "renamed",
                _ if status.contains('M') => "modified",
                _ if status.contains('A') => "added",
                _ if status.contains('D') => "deleted",
                _ => "other",
            };
            groups.entry(key).or_default().push(path.to_string());
            recognized = true;
            continue;
        }
        let trimmed = line.trim();
        for (prefix, key) in [
            ("modified:", "modified"),
            ("new file:", "added"),
            ("deleted:", "deleted"),
            ("renamed:", "renamed"),
        ] {
            if let Some(path) = trimmed.strip_prefix(prefix) {
                groups.entry(key).or_default().push(path.trim().to_string());
                recognized = true;
            }
        }
    }
    if !recognized {
        return None;
    }
    let mut out = String::from("git status\n");
    if !branch.is_empty() {
        out.push_str(&format!("branch {branch}\n"));
    }
    for (key, paths) in &groups {
        if *key == "untracked" {
            let mut dirs: BTreeMap<String, usize> = BTreeMap::new();
            for path in paths {
                let bucket = if path.ends_with('/') {
                    path.trim_end_matches('/').to_string()
                } else {
                    match path.rfind('/') {
                        Some(i) if i > 0 => path[..i].to_string(),
                        _ => ".".to_string(),
                    }
                };
                *dirs.entry(bucket).or_default() += 1;
            }
            for (dir, n) in dirs {
                out.push_str(&format!("untracked {dir}/ ({n})\n"));
            }
        } else {
            out.push_str(&format!("{key} ({}):\n", paths.len()));
            for path in paths.iter().take(40) {
                out.push_str(&format!("  {path}\n"));
            }
            if paths.len() > 40 {
                out.push_str(&format!("  … (+{} more)\n", paths.len() - 40));
            }
        }
    }
    Some(out)
}

fn filter_git_diff(text: &str) -> String {
    let mut out = String::new();
    let mut changes = 0usize;
    for line in text.lines() {
        if line.starts_with("diff --git ") || line.starts_with("diff --cc ") {
            changes = 0;
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if line.starts_with("@@")
            || line.starts_with("+++")
            || line.starts_with("---")
            || line.starts_with("index ")
        {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let changed = (line.starts_with('+') || line.starts_with('-')) && !line.starts_with("+++");
        if changed {
            changes += 1;
            if changes <= 12 {
                out.push_str(line);
                out.push('\n');
            } else if changes == 13 {
                out.push_str("…\n");
            }
        }
    }
    out
}

fn filter_git_log(text: &str) -> String {
    let mut out = String::new();
    let mut pending: Option<String> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("commit ") {
            if rest.len() >= 7 && rest.as_bytes()[..7].iter().all(|b| b.is_ascii_hexdigit()) {
                pending = Some(rest.chars().take(7).collect());
            }
            continue;
        }
        if line.starts_with("Author:") || line.starts_with("Date:") || line.starts_with("Merge:") {
            continue;
        }
        if pending.is_some() {
            let subject = line.trim();
            // Default `git log` puts a blank line between the headers and the subject.
            if subject.is_empty() {
                continue;
            }
            let hash = pending.take().unwrap_or_default();
            out.push_str(&format!("{hash} {subject}\n"));
            continue;
        }
        if line.len() > 8
            && line.as_bytes().get(7) == Some(&b' ')
            && line.as_bytes()[..7].iter().all(|b| b.is_ascii_hexdigit())
        {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn filter_ctest(text: &str) -> String {
    let mut out = String::new();
    let mut passed = 0u32;
    let mut failed = 0u32;
    for line in text.lines() {
        if line.contains("***Failed") || line.contains("***Timeout") || line.contains("***Not Run")
        {
            failed += 1;
            out.push_str(line);
            out.push('\n');
        } else if line.contains("Passed") {
            passed += 1;
        } else if line.contains("tests passed") || line.contains("% tests passed") {
            out.push_str(line);
            out.push('\n');
        }
    }
    if passed == 0 && failed == 0 {
        return String::new();
    }
    format!("{passed} passed, {failed} failed\n{out}")
}

fn filter_cargo_test(text: &str) -> String {
    let mut out = String::new();
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut keep_next = false;
    for line in text.lines() {
        if line.contains("FAILED") || line.starts_with("failures:") || line.starts_with("---- ") {
            failed += 1;
            keep_next = true;
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if line.starts_with("test result:") || line.starts_with("error:") {
            out.push_str(line);
            out.push('\n');
            keep_next = false;
            continue;
        }
        if keep_next && (line.starts_with("    ") || line.starts_with('\t')) {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if line.contains(" ... ok") {
            passed += 1;
            keep_next = false;
        } else {
            keep_next = false;
        }
    }
    if passed == 0 && failed == 0 && out.is_empty() {
        return String::new();
    }
    format!("{passed} passed, {failed} failed lines\n{out}")
}

fn filter_cargo_build(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("error")
            || line.contains("warning:")
            || line.starts_with("Finished")
            || line.contains("could not compile")
        {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_command_is_not_classified() {
        let cmd = vec!["cat".into(), "src/main.rs".into()];
        assert!(classify(&cmd).is_none());
        let filtered = filter_command(&cmd, "hello\n", "err\n");
        assert_eq!(filtered.stdout, "hello\n");
        assert_eq!(filtered.stderr, "err\n");
    }

    #[test]
    fn git_status_groups_and_collapses_untracked() {
        let raw = "\
## main...origin/main
 M src/a.rs
?? screenshots/a.png
?? screenshots/b.png
";
        let out = filter_git_status(raw).unwrap();
        assert!(out.contains("branch main"), "{out}");
        assert!(out.contains("src/a.rs"), "{out}");
        assert!(out.contains("untracked screenshots/ (2)"), "{out}");
        assert!(!out.contains("a.png"), "{out}");
    }

    #[test]
    fn ctest_keeps_failures_and_counts_passes() {
        let raw = "\
1/3 Test #1: alpha ....................   Passed    0.01 sec
2/3 Test #2: beta .....................***Failed    0.02 sec
3/3 Test #3: gamma ....................   Passed    0.01 sec
67% tests passed, 1 tests failed out of 3
";
        let out = filter_ctest(raw);
        assert!(out.contains("2 passed, 1 failed"), "{out}");
        assert!(out.contains("***Failed"), "{out}");
        assert!(!out.contains("alpha"), "{out}");
    }

    #[test]
    fn cargo_test_drops_passing_lines() {
        let raw = "\
test alpha ... ok
test beta ... FAILED
test gamma ... ok
test result: FAILED. 1 passed; 1 failed
";
        let out = filter_cargo_test(raw);
        assert!(out.contains("beta"), "{out}");
        assert!(out.contains("test result:"), "{out}");
        assert!(!out.contains("alpha"), "{out}");
    }

    #[test]
    fn git_log_keeps_hash_and_subject() {
        let raw = "\
commit 5e7c579f581219b7634377b1e0ca6013cfb7980f
Author: ada <ada@example.com>
Date:   Fri Oct 16 11:50:01 2026 -0400

    Stop the mist curtain.

    Longer body that should not be copied.

";
        let out = filter_git_log(raw);
        assert_eq!(out, "5e7c579 Stop the mist curtain.\n");
        assert!(!out.contains("Author:"), "{out}");
        assert!(!out.contains("Longer body"), "{out}");
    }

    #[test]
    fn unrecognized_status_passes_through() {
        let cmd = vec!["git".into(), "status".into()];
        let filtered = filter_command(&cmd, "not a status\n", "");
        assert_eq!(filtered.stdout, "not a status\n");
    }
}
