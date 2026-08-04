# Wordkeep performance & telemetry

Figures use a **~4 characters per token** heuristic. Treat them as
**estimated context avoided** (counterfactual raw-material size vs tool
output), not billing-grade agent-token savings, wall-clock task speedups,
or proof that an agent would have read every scanned byte.

## Snapshot (baseline, 2026-08-03)

Captured from the live `stats` store before the telemetry v5 / hot-path work:

| Metric | Value |
| --- | --- |
| Tracking window | ~54 days |
| Tool calls | ~8,767 |
| Distilled (est.) | ~10.48B tokens |
| Returned (est.) | ~5.84M tokens |
| Estimated context avoided | ~10.47B (~99.94%, ~1,800:1) |

### Handler latency (avg ms)

| Tool | Calls | Avg ms | Notes |
| --- | ---: | ---: | --- |
| `outline` | 1,388 | 7 | Fast path; unsupported-type bug omitted `stats::record` |
| `symbol_refs` | 1,092 | 39 | |
| `repo_map` | 495 | 49 | High truncation under default budget |
| `symbol_context` | 3,325 | 62–63 | Busiest tool; largest distilled denominator |
| `knowledge_search` | 817 | 140 | Cold embed spikes when embeddings feature on |
| `diff_map` | 36 | **1,073** | Peak 10.3s — rebuilds whole-tree callers per symbol |
| `commit_scope` | 5 | 2,038 | Low sample; measure before optimizing |
| `trace_profile` | 7 | 430 | |

### Outcome rates (selected)

| Tool | Trunc | Err | Issue |
| --- | ---: | ---: | --- |
| `repo_map` | 144 / 495 | 0 | Budget truncation under wide trees |
| `test_map` | 1 | 106 / 357 | Many typed `Err` for missing/invalid args |
| `outline` | 12 | 104 | Mix of not-found / unsupported |
| `knowledge_upsert` | 0 | 74 / 322 | Validation failures |
| `type_layout` | 1 | 73 / 198 | Not-found vs true errors |
| `big_functions` | 3 / 3 | 0 | Always truncated at default budget |

## Methodology caveats

1. **Baseline is counterfactual.** Tools often attribute full-tree file sizes
   even on cache hits. Repeated queries inflate cumulative “distilled.”
2. **`symbol_context` ranks as the top saver** largely because it scans the
   most source bytes per call, not because it has the best marginal ROI.
3. **Net-negative tools** (`defect_list`, `mas_read`, …) are usually tiny
   metadata responses with tiny baselines — not regressions.
4. **Sub-ms latency** was previously floored to `0` via `as_millis()`.
5. **`session_pressure`** previously saw only the last 200 in-memory events.

## Reproducible bench

```sh
cargo bench --bench cold_warm
# or:
cargo run --release --example cold_warm_bench -- --root .
```

Reports cold vs warm wall time for `repo_map`, `symbol_context`,
`knowledge_search`, and `diff_map` against the local tree (or a fixture).

## After changes

Update this document with before/after p50/p95 for `diff_map` and telemetry
save overhead once the single-adjacency and debounced persistence land.
Record cache hit rates from `stats` JSON once schema v5 is live.

## After (2026-08-03 implementation)

- Telemetry **v5** shipped: append-only workspace `events.jsonl`, debounced
  aggregate saves, `elapsed_us`, typed outcomes, JSON `stats` format.
- `diff_map` now builds **one** `call_graph::adjacency` per call and answers
  caller lookups via `Adjacency::callers_of` (eliminates N full-tree
  `one_hop` rebuilds). Re-run `cold_warm_bench` / wide-diff smoke on your
  tree to capture new p50/p95.
- Fence-aware Markdown chunking lives in `wordkeep-knowledge`; BM25 and wiki
  share the same section model.
- Wiki companion: `wordkeep-wiki` + Meilisearch + Svelte UI (`wiki/`).
