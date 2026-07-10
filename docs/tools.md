# Tool reference

All tools accept optional `token_budget` (approximate max response tokens).
Path-accepting tools use `paths` or `.wordkeep/config.json` `default_paths`.

## Navigation

| Tool | Required args | Returns |
| --- | --- | --- |
| `repo_map` | optional `paths` | Namespaces, types, function signatures per file |
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

## Knowledge and docs

| Tool | Required args | Returns |
| --- | --- | --- |
| `knowledge_search` | `query` | BM25 (optional semantic rerank) over docs/rules |
| `knowledge_upsert` | `path`, `heading`, `body` | Write/update markdown section |

## Performance and workflow

| Tool | Required args | Returns |
| --- | --- | --- |
| `trace_summary` | optional `file`, `dir`, `baseline` | Hottest Tracy zones or diff |
| `trace_profile` | optional trace args | Hitch workflow: trace + diff_map + index_stale |
| `integration_hooks` | optional `query`, `from`, `to` | Curated hooks + optional call_path |
| `index_stale` | optional `ref` | Whether disk indexes may lag git |
| `stats` | optional `reset`, `insights` | Token displacement telemetry |

## Multi-agent (MAS)

| Tool | Purpose |
| --- | --- |
| `mas_post` | Append compact entry to session blackboard |
| `mas_read` | Read entries (filter by recipient, role, round, tag) |
| `mas_status` | Round bookkeeping and convergence hint |
| `mas_finalize` | Close session; optional `promote` to `.wordkeep/notes/` |

See [designs/recursive_mas.md](designs/recursive_mas.md) for the typical
planner/solver/critic loop.

## Languages

`repo_map`, `outline`, `symbol_refs`, and `call_graph` use tree-sitter for
C/C++, GLSL, Rust, Python, C#, TypeScript/TSX/Svelte. Daslang uses a line
scanner unless built with `--features daslang`.

Svelte: only `<script>` blocks are parsed (line numbers preserved).
