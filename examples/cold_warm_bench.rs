//! Cold vs warm wall-time smoke harness for Wordkeep hot tools.
//!
//! Run:
//! ```sh
//! cargo run --release --example cold_warm_bench -- --root .
//! ```
//!
//! This is intentionally a binary example (not criterion) so it works without
//! extra deps and prints numbers suitable for docs/performance.md.

use std::env;
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let mut root = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--root" {
            if let Some(p) = args.next() {
                root = PathBuf::from(p);
            }
        }
    }
    eprintln!("cold_warm_bench root={}", root.display());

    let paths = serde_json::json!({ "paths": ["src"] });
    for (name, run) in [
        (
            "repo_map",
            Box::new(|| wordkeep::repo_map::build(&root, &paths))
                as Box<dyn Fn() -> Result<String, String>>,
        ),
        (
            "knowledge_search",
            Box::new(|| {
                wordkeep::knowledge::search(
                    &root,
                    &serde_json::json!({ "query": "token savings telemetry", "k": 5 }),
                )
            }) as Box<dyn Fn() -> Result<String, String>>,
        ),
    ] {
        // cold
        let t0 = Instant::now();
        let cold = run();
        let cold_ms = t0.elapsed().as_secs_f64() * 1000.0;
        // warm
        let t1 = Instant::now();
        let warm = run();
        let warm_ms = t1.elapsed().as_secs_f64() * 1000.0;
        let cold_ok = cold.is_ok();
        let warm_ok = warm.is_ok();
        println!("{name:16} cold={cold_ms:8.2}ms ok={cold_ok}  warm={warm_ms:8.2}ms ok={warm_ok}");
    }
}
