//! One-shot: task_submit against the real workspace (for telemetry smoke).
use std::env;
use std::path::PathBuf;

fn main() {
    let root = env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("../.."));
    let job = env::args().nth(2).unwrap_or_else(|| "identity-tmlp".into());
    let wait = env::args().nth(3).map(|s| s != "0").unwrap_or(true);
    let out = wordkeep::task_lan_submit_for_test(&root, &job, wait);
    match out {
        Ok(s) => {
            println!("{s}");
        }
        Err(e) => {
            eprintln!("ERR: {e}");
            std::process::exit(1);
        }
    }
}
