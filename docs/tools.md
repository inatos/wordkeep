# Tool reference

All tools accept optional `token_budget` (approximate max response tokens) where noted.
Path-accepting tools use `paths`, optional `profile`, or `.wordkeep/config.json`
`path_profiles` / `default_paths` (see [configuration.md](configuration.md)).

**Paging:** high-volume tools may append a `continuation:` token when truncated.
Re-call the same tool with `continuation` (and the same query args) to fetch the
next page. Prefer that over raising `token_budget` alone.

**Progress:** opt-in via `WORDKEEP_MCP_PROGRESS=1`. When enabled, hosts that send
`_meta.progressToken` on `tools/call` receive `notifications/progress` ticks
during long walks (status only; the agent still sees one final text result).
Default off — Cursor's Shared MCP client currently fatals on unrecognized
progress tokens and drops the server connection.

**Surface:** 51 tools + MCP resources `wordkeep://readme`, `wordkeep://capabilities`.

## Navigation

| Tool | Required args | Returns |
| --- | --- | --- |
| `repo_map` | optional `paths` / `profile` / `mode` / `expand` / `pattern` / `continuation` | Files-first map (default `mode: auto`) or full signatures (`symbols` / `expand`); pages via `continuation` |
| `outline` | `file` / `path` or `files[]` | Symbol list with line numbers (single or batch; batch pages via `continuation`) |
| `symbol_resolve` | `symbol` | Exact locate + fuzzy/profile did-you-mean |
| `symbol_refs` | `symbol` | Definitions, calls, references (tree-sitter classified) |
| `call_graph` | `symbol` | One hop of callers and callees |
| `call_path` | `from`, `to` | Shortest call chain (depth-bounded BFS) |
| `include_graph` | `header` | One hop of `#include` includers/includees |
| `type_layout` | `type` | Record fields and non-POD flags |
| `doc_comment` | `symbol` | Leading doc comment + signature |
| `symbol_context` | `symbol` | Body + callers/callees + layout (composed) |
| `batch_context` | `symbols[]` | Multi-symbol condensed context under one budget |
| `usage_examples` | `symbol` | Call sites with surrounding context lines |

## Change analysis

| Tool | Required args | Returns |
| --- | --- | --- |
| `symbol_diff` | `symbol`, optional `ref` | How one symbol changed vs a git ref |
| `diff_map` | optional `ref` | Symbols changed in a diff + immediate callers |
| `module_map` | optional `paths`, `focus`, `min_edge` | Cross-module call coupling |
| `dead_code` | optional `paths`, `stale_days` | Symbols with zero callers/refs |
| `big_functions` | optional `paths`, `min_lines` | Largest functions by line span |
| `undocumented` | optional `paths` | Exported symbols lacking doc comments |
| `test_map` | `symbol` | Test files referencing the symbol |
| `test_impact` | optional `ref` | Changed symbols × ranked test files + filter hint |
| `commit_scope` | optional `large_file_bytes` | Read-only dirty-path groups + warnings; skips `commit_ignore` prefixes |
| `profile_upsert` | optional `paths` / `symbol`, `mode` | Propose/apply `path_profiles` + hints + commit_scopes |

## Knowledge and docs

| Tool | Required args | Returns |
| --- | --- | --- |
| `knowledge_search` | `query` | BM25 (+ optional defects boost / semantic rerank) |
| `knowledge_answer` | `query` | Extractive synthesis + citations (shared BM25 pipeline) |
| `knowledge_upsert` | `path` + mode fields | Write/update markdown section; optional `effect_session` for revertible writes |

## Performance and workflow

| Tool | Required args | Returns |
| --- | --- | --- |
| `trace_summary` | optional `file`, `dir`, `baseline` | Hottest Tracy zones or diff |
| `trace_profile` | optional trace args | Hitch workflow: trace + diff_map + index_stale |
| `perf_triage` | optional trace args | Profile + hotspot context + tests (+ runtime soft-route) |
| `runtime_snapshot` | optional `capture`, `top` | Token-budgeted live/captured Runtime Health census |
| `memory_diff` | `base`; optional `current` | Signed memory, pool, and mapping-kind deltas |
| `locality_hotspots` | optional `capture`, `top`, `min_samples` | Sampled address hotspots, or explicit unavailable |
| `integration_hooks` | optional `query`, `from`, `to` | Curated hooks (AND then OR fallback) + optional call_path |
| `index_stale` | optional `ref`, `paths` / `profile` | Whether disk indexes may lag git / miss coverage |
| `index_health` | optional | Profile coverage gaps + proactive stale signal |
| `stats` | optional `reset`, `insights`, `format` (`text`\|`json`), `workspace` | Estimated context avoided (v5: µs latency, typed outcomes, jsonl events) |
| `run_record` | `command` | Metadata-only gate/run write (also CLI `run-record`) |
| `run_history` | optional filters | Recent runs; flags missing logs/artifacts |
| `artifact_index` | optional `roots` / `query` | Artifact metadata index (no image grading) |
| `session_pressure` | optional `session` | Heuristic context-pressure (+ autopilot draft when high) |
| `defect_upsert` | `summary` | Structured defect create/update |
| `defect_list` | optional filters | Digest by default (`digest:false` / `format:"full"` for detail) |

## Multi-agent (MAS)

| Tool | Purpose |
| --- | --- |
| `mas_post` | Append compact entry (`kind`, `commands`, `constraints`, handoff spill) |
| `mas_read` | Read entries (filter by recipient, role, round, tag) |
| `mas_status` | Round bookkeeping and convergence hint |
| `mas_finalize` | Close session; optional promote + handoff prompt; `effects: commit\|recover` when journaled writes pending |
| `session_handoff` | Paste-ready next-session prime (works on finalized sessions) |

See [designs/recursive_mas.md](designs/recursive_mas.md) and
[designs/session_continuity.md](designs/session_continuity.md).

### Revertible writes (`effect_session`)

Opt-in temporal composability for agent notes (FIG-2026-005 PoC):

1. Open a MAS session (`mas_post` / `mas_status`).
2. Call `knowledge_upsert` with `"effect_session": "<slug>"` — each write is journaled under the workspace cache.
3. On close, pass `"effects": "commit"` (keep writes) or `"effects": "recover"` (LIFO revert) to `mas_finalize` when `mas_status` reports `pending_effect_writes`.
4. `effects: recover` leaves the session **open** (not finalized, no promote).

Conflict policy: recover refuses if on-disk content no longer matches the journaled post-write snapshot (external edit or concurrent writer).

**Spatial coeffects:** write tools notify dependent caches per `wordkeep://capabilities` (`coeffects.rs`). `mas_read` evicts session cache before load.

## Izakaya (presence)

Live agents coordinate through Izakaya, not by overloading MAS rounds.
Check in before the first edit, update when scope changes, and check out on
handoff. Long handoff prose still uses `mas_post`; checkout only stores the
reference. See [designs/izakaya.md](designs/izakaya.md).

| Tool | Purpose |
| --- | --- |
| `izakaya_status` | Read-only board: agents, stale leases, advisory overlaps, handoffs |
| `izakaya_check_in` | Take or resume a lease before live edits |
| `izakaya_update` | Heartbeat, `live_code` / `suspended`, notes |
| `izakaya_check_out` | Release claims; optional handoff capsule |
| `izakaya_advise` | Read-only recommendations from a promoted policy |

Offline Dream-RSI replay (decision/outcome journal + policy evaluate) stays on
the CLI (`wordkeep izakaya policy …`); those MCP tools were removed after
telemetry showed zero agent producers.

## MCP resources

| URI | Content |
| --- | --- |
| `wordkeep://readme` | Packaged README (alias: `wordkeep://README`) |
| `wordkeep://capabilities` | Static effect/coeffect manifest for write tools (FIG-2026-005 Phase 2) |

## Languages

`repo_map`, `outline`, `symbol_refs`, and `call_graph` use tree-sitter for
C/C++, GLSL, Rust, Python, C#, TypeScript/TSX/Svelte. Daslang uses a line
scanner unless built with `--features daslang`.

Svelte: only `<script>` blocks are parsed (line numbers preserved).
