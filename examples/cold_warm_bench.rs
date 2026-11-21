//! Cold vs warm wall-time smoke harness for Wordkeep hot tools.
//!
//! Run:
//! ```sh
//! cargo run --release --example cold_warm_bench -- --root .
//! ```
//!
//! Prints cold + warm p50/p95 over several iterations, SQLite store size, and
//! cumulative `stats` cache_hits — suitable for docs/designs/wordkeep_v05.md.

use std::env;
use std::path::PathBuf;
use std::time::Instant;

fn percentile(sorted_ms: &[f64], p: f64) -> f64 {
    if sorted_ms.is_empty() {
        return 0.0;
    }
    let n = sorted_ms.len();
    let idx = ((p * (n as f64 - 1.0)).round() as usize).min(n - 1);
    sorted_ms[idx]
}

fn time_iters<F>(iters: usize, mut run: F) -> (f64, f64, f64, bool)
where
    F: FnMut() -> Result<String, String>,
{
    let mut samples = Vec::with_capacity(iters);
    let mut ok = true;
    for _ in 0..iters {
        let t0 = Instant::now();
        ok = run().is_ok() && ok;
        samples.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p50 = percentile(&samples, 0.50);
    let p95 = percentile(&samples, 0.95);
    let mean = samples.iter().sum::<f64>() / samples.len().max(1) as f64;
    (p50, p95, mean, ok)
}

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
    wordkeep::stats::set_workspace_root(&root);
    wordkeep::stats::init_registry(vec![
        "repo_map",
        "knowledge_search",
        "symbol_resolve",
        "symbol_context",
        "batch_context",
    ]);

    let paths = serde_json::json!({ "paths": ["src"] });
    let ks = serde_json::json!({ "query": "token savings telemetry", "k": 5 });
    let resolve = serde_json::json!({ "symbol": "knowledge_search" });
    let ctx = serde_json::json!({ "symbol": "retrieve_with_embedder" });
    let batch = serde_json::json!({
        "symbols": ["retrieve_with_embedder", "decide_one", "doc_chunks_for_roots"]
    });

    let benches: [(&str, Box<dyn Fn() -> Result<String, String>>); 5] = [
        (
            "repo_map",
            Box::new(|| wordkeep::repo_map::build(&root, &paths)),
        ),
        (
            "knowledge_search",
            Box::new(|| wordkeep::knowledge::search(&root, &ks)),
        ),
        (
            "symbol_resolve",
            Box::new(|| wordkeep::symbol_resolve::resolve(&root, &resolve)),
        ),
        (
            "symbol_context",
            Box::new(|| wordkeep::symbol_context::build(&root, &ctx)),
        ),
        (
            "batch_context",
            Box::new(|| wordkeep::triage::batch_context(&root, &batch)),
        ),
    ];

    const WARM_ITERS: usize = 11;
    for (name, run) in benches {
        // one cold sample
        let t0 = Instant::now();
        let cold = run();
        let cold_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let cold_ok = cold.is_ok();
        // warm p50/p95
        let (p50, p95, mean, warm_ok) = time_iters(WARM_ITERS, || run());
        println!(
            "{name:16} cold={cold_ms:8.2}ms ok={cold_ok}  warm_p50={p50:8.2}ms warm_p95={p95:8.2}ms warm_mean={mean:8.2}ms ok={warm_ok}"
        );
    }

    let db = wordkeep::store::db_path(&root);
    let store_mb = std::fs::metadata(&db)
        .map(|m| m.len() as f64 / (1024.0 * 1024.0))
        .unwrap_or(0.0);
    println!("store_sqlite     path={} size_mb={store_mb:.2}", db.display());

    let stats_out = wordkeep::stats::report(&serde_json::json!({ "format": "json" }))
        .unwrap_or_default();
    let cache_hits = serde_json::from_str::<serde_json::Value>(&stats_out)
        .ok()
        .and_then(|v| {
            // Prefer knowledge_search tool row, else totals.
            v.get("tools")
                .and_then(|t| t.get("knowledge_search"))
                .and_then(|ks| ks.get("cache_hits"))
                .and_then(|x| x.as_u64())
                .or_else(|| v.pointer("/totals/cache_hits").and_then(|x| x.as_u64()))
                .or_else(|| v.get("cache_hits").and_then(|x| x.as_u64()))
        })
        .unwrap_or(0);
    println!("stats_cache_hits {cache_hits}");
    if store_mb >= 25.0 {
        eprintln!("WARN: store size {store_mb:.2} MB exceeds 25 MB gate (embeddings + symbols)");
    }
    if cache_hits == 0 {
        eprintln!("WARN: stats cache_hits is zero after warm loops");
    }
}
