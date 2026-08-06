# Changelog

All notable changes to wordkeep are documented here. The project follows
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed

- Wiki sidebar **Tree** left-edge clipping on deep folders (flex `min-width` +
  padding indent overflow). Indent uses a fixed-width spacer + label ellipsis;
  denser row chrome. Regenerated `docs/wiki-*.png` / `docs/dashboard.png`.
- `wiki.sh` restarts when `/api/dashboard` is missing (stale binary) so the GUI
  Dashboard tab does not hit empty JSON parse errors.
- Telemetry: treat `no matching occurrences` / `no test file` / `cannot read`
  replies as **not_found** (was low_yield). `mcp.sh` picks the newest
  release/debug binary and warns when `src/` is newer than the binary.

### Changed

- Wiki Dashboard tab: Recent activity + Health signals render above the Tools
  table.

## [0.3.0] - 2026-08-04

Telemetry truthfulness, `diff_map` hot-path fix, shared Markdown knowledge
crate, and the `wordkeep-wiki` companion (Meilisearch + dark Svelte UI).

### Added

- Telemetry **schema v5**: workspace `events.jsonl`, debounced `savings.json`,
  microsecond latency, typed outcomes (`ok`/`truncated`/`error`/`not_found`/
  `empty`/`low_yield`/`invalid`), cache/I/O fields, `stats` JSON format,
  one-decimal reduction %.
- Shared crate `wordkeep-knowledge`: fence-aware Markdown chunking, frontmatter,
  anchors, links, tags.
- Companion `wordkeep-wiki`: index/watch/serve over Meilisearch; Axum API;
  dark Svelte 5 UI under `wiki/`; `docker-compose.wiki.yml`.
- Wiki **Dashboard** tab + `GET /api/dashboard` (MCP savings overview / tools /
  activity / health) as a GUI alternative to `wordkeep dashboard`.
- Docs: [docs/performance.md](docs/performance.md),
  [docs/designs/wiki.md](docs/designs/wiki.md).
- Example `cold_warm_bench` for cold/warm latency smoke checks.
- Relevance query fixtures under `tests/fixtures/relevance/`.

### Changed

- `diff_map` builds **one** call-graph adjacency per invocation (was N×
  `one_hop` full-tree scans).
- `outline` records stats on unsupported file types.
- Dashboard health: low-yield and net-negative require meaningful rates /
  baseline floors.
- `knowledge_search` chunking uses `wordkeep-knowledge` (fenced `#` ignored).
- Workspace Cargo members: `.`, `crates/wordkeep-knowledge`, `crates/wordkeep-wiki`.
- Validation / missing-arg `Err`s classify as **`invalid`** (not `error`) so
  health “error-prone” tracks real failures.
- `defect_list` distill baseline uses on-disk store size (not a tiny per-row
  guess) to avoid false net-negative.
- Standalone terminal dashboard keeps embedded `savings.json` events (no longer
  drops the ring when `STATS` is unset).
- `savings.json` v5 flush re-embeds the capped events ring (with optional
  `reason`) so the wiki GUI stays in sync without the live MCP process.

### Fixed

- `session_pressure` event window no longer capped at the 200-event ring
  (reads append-only jsonl).
- Terminal dashboard **Recent activity** empty while GUI showed events
  (`read_snapshot` discarded loaded events).
- Non-ok activity outcomes now persist a short `reason` (first line of the
  error / hint) for GUI hover tooltips.
- `--features daslang` binary link: explicit `#[link]` for the vendored
  grammar so the bin (same name as the lib) actually links `tree_sitter_daslang`.

### Notes

- Core MCP binary stays offline by default (no Meilisearch/Axum deps).
- Token figures remain ~4 chars/token **estimated context avoided**, not
  billing.
- Wiki UI/API now includes instant search, TOC/deep links, recent files, link
  garden health, local search telemetry (raw queries remain opt-in), GUI
  dashboard (tools + **Inv** column; activity/health as tables; error hover
  reasons), editor tabs, frontmatter tag CRUD + colors, and searchable
  kind/root/tag/recent comboboxes.
- Future wiki user-state persistence: Turso/libSQL (not required for MVP).
- No separate vector DB; Meilisearch hybrid/vectors optional later.
- Historical `error_count` aggregates may stay inflated until new calls dominate
  or `savings.json` is reset; only new outcomes use the Invalid split.


## [0.2.0] - 2026-07-23

Session continuity upgrade: durable handoffs, defects, run evidence, path
profiles, MCP resources. **36 tools** (28 → 36). Backward compatible with
0.1.0 stores and configs.

### Added

- Workspace-scoped cache under `wordkeep/workspaces/<root-hash>/` with legacy
  MAS migration on first access.
- Path profiles: `path_profiles`, `default_profile`, `profile_hints`; resolution
  order `paths` → `profile` → keyword hint → `default_paths`.
- `session_handoff` — paste-ready next-session prime from MAS/defects/runs.
- `defect_upsert` / `defect_list` — structured `.wordkeep/defects.json` registry;
  unresolved defects boosted in `knowledge_search` (opt out via `include_defects`).
- `run_record` / `run_history` plus CLI `wordkeep run-record` (metadata only).
- `artifact_index` — metadata over configured `artifact_roots` (no image grading).
- `session_pressure` — heuristic `low|medium|high|critical` pressure proxy.
- `commit_scope` — read-only git dirty-path grouping by `commit_scopes`.
- MCP `resources/list` + `resources/read` for `wordkeep://readme` (alias
  `wordkeep://README`).
- Design note: [docs/designs/session_continuity.md](docs/designs/session_continuity.md).

### Changed

- MAS entries gain `kind`, `commands`, `constraints`, `note_ref`; `kind:handoff`
  uses a larger token cap and spills overflow to `.wordkeep/notes/`.
- `mas_finalize` emits a handoff prompt by default; optional config
  `mas.auto_promote`.
- `knowledge_upsert` validates mode first, reports all missing fields together,
  and allows root `README.md` plus `knowledge_write_roots`.
- `index_stale` is profile/path aware and distinguishes self-refreshable cache
  misses from rebuild-needed cases.
- Stats events carry `workspace_id` (store version 4).
- `stats` / dashboard counts abbreviate at ≥1M / 1B / 1T / 1Q (`3.25M`, `1.23B`).

### Security

- Artifact scans never follow symlinks outside `--root`.
- `commit_scope` never stages or commits.
- Knowledge writes remain allowlisted (README + configured roots only).

## [0.1.0] - 2026-07-10

First public release.

### Added

- Standalone MCP server with 28 token-budgeted tools over stdio JSON-RPC.
- Tree-sitter extraction for C/C++, GLSL, Rust, Python, C#, TypeScript/TSX/Svelte.
- BM25 `knowledge_search` with optional `embeddings` semantic rerank.
- Tracy `trace_summary` / `trace_profile` helpers.
- Recursive MAS blackboard (`mas_post`, `mas_read`, `mas_status`, `mas_finalize`).
- `stats` telemetry (distilled vs returned tokens, latency, outcomes).
- Optional `dashboard` terminal UI for live savings.
- Hermetic integration tests (`mcp_stdio`, `external_repo`).
- GitHub Actions CI (Linux, macOS, Windows) and tag-driven release binaries.
- Documentation set: README, getting started, configuration, tool reference.
- `CONTRIBUTING.md`, `SECURITY.md`, `THIRD_PARTY_NOTICES.md`.
- Example MCP configs and integration-hooks template.

### Security

- Path arguments validated relative to `--root` (reject `..` and absolute paths).
- Platform-native cache directory resolution (XDG, `%LOCALAPPDATA%`, fallback).

### Notes

- `daslang`, `embeddings`, and `dashboard` remain opt-in Cargo features.
- Token savings figures use a ~4 characters per token estimate; treat `stats` as
  comparative telemetry, not billing-grade accounting.
