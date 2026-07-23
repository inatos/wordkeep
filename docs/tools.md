# Tool reference

All tools accept optional `token_budget` (approximate max response tokens) where noted.
Path-accepting tools use `paths`, optional `profile`, or `.wordkeep/config.json`
`path_profiles` / `default_paths` (see [configuration.md](configuration.md)).

**Surface:** 36 tools + MCP resource `wordkeep://readme`.

## Navigation

| Tool | Required args | Returns |
| --- | --- | --- |
| `repo_map` | optional `paths` / `profile` | Namespaces, types, function signatures per file |
| `outline` | `file` or `path` | Single-file symbol list with line numbers |
| `symbol_refs` | `symbol` | Definitions, calls, references (tree-sitter classified) |
| `call_graph` | `symbol` | One hop of callers and callees |
| `call_path` | `from`, `to` | Shortest call chain (depth-bounded BFS) |
| `include_graph` | `header` | One hop of `#include` includers/includees |
| `type_layout` | `type` | Record fields and non-POD flags |
| `doc_comment` | `symbol` | Leading doc comment + signature |
| `symbol_context` | `symbol` | Body + callers/callees + layout (composed) |
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
| `commit_scope` | optional `large_file_bytes` | Read-only dirty-path groups + warnings |

## Knowledge and docs

| Tool | Required args | Returns |
| --- | --- | --- |
| `knowledge_search` | `query` | BM25 (+ optional defects boost / semantic rerank) |
| `knowledge_upsert` | `path` + mode fields | Write/update markdown section |

## Performance and workflow

| Tool | Required args | Returns |
| --- | --- | --- |
| `trace_summary` | optional `file`, `dir`, `baseline` | Hottest Tracy zones or diff |
| `trace_profile` | optional trace args | Hitch workflow: trace + diff_map + index_stale |
| `integration_hooks` | optional `query`, `from`, `to` | Curated hooks + optional call_path |
| `index_stale` | optional `ref`, `paths` / `profile` | Whether disk indexes may lag git / miss coverage |
| `stats` | optional `reset`, `insights` | Token displacement telemetry (counts ≥1M → `3.25M` / `1.23B` / …) |
| `run_record` | `command` | Metadata-only gate/run write (also CLI `run-record`) |
| `run_history` | optional filters | Recent runs; flags missing logs/artifacts |
| `artifact_index` | optional `roots` / `query` | Artifact metadata index (no image grading) |
| `session_pressure` | optional `session` | Heuristic context-pressure level |
| `defect_upsert` | `summary` | Structured defect create/update |
| `defect_list` | optional filters | Unresolved defects (`eyeball_fail` first) |

## Multi-agent (MAS)

| Tool | Purpose |
| --- | --- |
| `mas_post` | Append compact entry (`kind`, `commands`, `constraints`, handoff spill) |
| `mas_read` | Read entries (filter by recipient, role, round, tag) |
| `mas_status` | Round bookkeeping and convergence hint |
| `mas_finalize` | Close session; optional promote + handoff prompt |
| `session_handoff` | Paste-ready next-session prime (works on finalized sessions) |

See [designs/recursive_mas.md](designs/recursive_mas.md) and
[designs/session_continuity.md](designs/session_continuity.md).

## MCP resources

| URI | Content |
| --- | --- |
| `wordkeep://readme` | Packaged README (alias: `wordkeep://README`) |

## Languages

`repo_map`, `outline`, `symbol_refs`, and `call_graph` use tree-sitter for
C/C++, GLSL, Rust, Python, C#, TypeScript/TSX/Svelte. Daslang uses a line
scanner unless built with `--features daslang`.

Svelte: only `<script>` blocks are parsed (line numbers preserved).
