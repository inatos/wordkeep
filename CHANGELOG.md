# Changelog

All notable changes to wordkeep are documented here. The project follows
[Semantic Versioning](https://semver.org/).

## [Unreleased]

Telemetry upgrade pass (token → accuracy → latency). Surface is **51** tools.

### Added (progressive MCP)

- **Continuation tokens**: when a tool truncates on `token_budget`, it appends
  `continuation: <opaque>` — re-call with that arg to fetch the next page of the
  same query (no re-emit of earlier rows). Wired for `repo_map`, `outline`
  (batch), `knowledge_search`, `module_map`, `big_functions`, `symbol_refs`,
  `symbol_context`. Prefer continuation over blindly raising `token_budget`.
- **`notifications/progress`**: opt-in via `WORDKEEP_MCP_PROGRESS=1`. When the
  client also passes `_meta.progressToken` on `tools/call`, long walks emit MCP
  progress ticks (stdio) before the final result. Default **off** — Cursor Shared
  MCP currently treats unrecognized progress tokens as a transport error and
  marks the server failed.

### Fixed (live-soak follow-up)

- **Fuzzy did-you-mean**: reject short/non-ident leaves; length-ratio + prefix/overlap
  gates so typos no longer suggest `Ve`/`E`/`Ol`.
- **`symbol_resolve` cold path**: DiskMap cache for def names; skip cross-profile
  locate when `paths`/`profile` is explicit (warm miss ~5ms).
- **`index_health` cold path**: `git ls-files` sampling, tracked-only git diff,
  5s TTL cache (cold ~0.4s, warm ~0ms on this tree).
- **Stats baselines**: `session_pressure` / `index_health` use store sizes; on
  `init_registry`, prune removed tools from `savings.json` and clear legacy
  flat-64 false net-negatives.

### Added

- **`symbol_resolve`**: normalize + fuzzy locate + cross-profile did-you-mean.
- **`knowledge_answer`**: extractive synthesis + citations from the shared BM25
  pipeline (no LLM).
- **`batch_context`**: multi-symbol edit context under one budget.
- **`perf_triage`**: `trace_profile` → hotspot `symbol_context` → `test_map` bundle.
- **`test_impact`**: changed symbols × test refs for the narrowest suite.
- **`index_health`**: profile coverage gaps + stale-index signal.
- **`repo_map` progressive**: files-first default (`mode`/`expand`); symbols on demand.
- **`outline` `files[]`**: batch TOC under one budget.
- **`integration_hooks` OR fallback**: AND first; if empty, rank by token hit
  count; vocabulary hint of `##` headings when still empty.
- **`defect_list` digest-first**: default compact summary (`digest:true` /
  `format:"digest"`); pass `digest:false` or `format:"full"` for the detailed list.
- **`session_pressure` autopilot draft** when high/critical (no auto-write).
- **Eval harness**: `cargo test --test eval_queries` (golden outcomes + token caps).
- **`commit_ignore`** config (default `.cache/`, `target/`, `node_modules/`) +
  short-TTL porcelain cache for `commit_scope`.

### Removed

- MCP tools `izakaya_record_decision`, `izakaya_record_outcome`, `izakaya_replay`
  (never called in telemetry; no agent producer path). Offline replay lab remains
  via library + `wordkeep izakaya policy …`.

### Fixed

- **Izakaya stats baseline**: presence tools recorded distilled=`64` while returning
  journal digests — false net-negative. Baseline is now on-disk journal +
  projection size (same pattern as `defect_list` / `run_history`).
- **`mas_read` / `session_handoff` baselines**: use session file size (and for
  handoff, defects + runs stores) so composite digests are not net-negative.
- **Advisory noise**: skip both-stale `base_divergence` pairs (O(n²) historical
  noise that truncated useful check_in/status lines).
- **`memory_diff` / `locality_hotspots` discoverability**: clearer errors listing
  available captures and soft-route hints (wiki Health → Runtime Capture;
  Betwixt `.wordkeep/runtime/latest.json`). Soft-routed from `perf_triage`.
- **Arg repair**: `type_layout` / `test_map` normalize qualified/templated names;
  symbol tools append did-you-mean on not-found.

### Added

- **Izakaya** — local agent presence for linked Git worktrees: append-only
  journal, advisory path/symbol claims, two-phase handoffs, and a read-only
  Dream-RSI-style replay lab (CLI/offline). MCP tools: `izakaya_status`,
  `izakaya_check_in`, `izakaya_update`, `izakaya_check_out`, `izakaya_advise`.
  CLI: `wordkeep izakaya policy list|evaluate|promote|retire`. Promotion only
  activates advice; it does not assign work or mutate Git.

## [0.4.2] - 2026-08-22

### Fixed

- **`classify_outcome`**: for successful tool responses, apply invalid/not_found
  heuristics on the **header line only** so registry and content tools
  (`defect_list`, `session_handoff`, `outline`, `integration_hooks`, …) no
  longer false-positive when embedded text contains phrases like `must be` or
  `(no matching defects)`. Empty `symbol_refs` / `integration_hooks` results
  still classify as `not_found` via explicit suffix-line markers.
- **`run_history`** / **`session_handoff`**: distill baseline uses on-disk store
  size (not a tiny per-row guess) to avoid false net-negative insights.

### Changed

- `stats` improvement signals: add **invalid-prone** line (same thresholds as
  error-prone) so real agent misuse remains visible after the classifier fix.

### Notes

- Historical `invalid_count` in `savings.json` is unchanged; use `stats`
  `{"reset": true}` for a clean slate, or let new calls dominate aggregates.

## [0.4.1] - 2026-08-22

### Fixed

- **`mcp.sh`**: portable rebuild hints (`cargo build` with pinned `CARGO_TARGET_DIR`);
  optional `tools/dev/rebuild_wordkeep.sh` when present in a monorepo layout.
- Avoid stale MCP binaries when a parent shell sets `CARGO_TARGET_DIR` outside
  `tools/wordkeep/target`.

## [0.4.0] - 2026-08-06

Live wiki Dashboard charts, collapsible telemetry/health panels, and wiki UX
polish since 0.3.0.

### Added

- Wiki Dashboard **live charts** (tokens saved / calls bars, outcome donut,
  recent-savings line chart) from the existing 2s `/api/dashboard` poll.
  Zero new npm deps — SVG components under `wiki/src/lib/charts/`.
- Chart interactivity: line chart axes + hover crosshair/tooltip; donut slice
  + legend hover metrics (count, %, description; center follows hover).
- `CHART_HELP` / `OUTCOME_HELP` tooltips on chart cards, bars, and sections.
- Collapsible Dashboard + Health sections with persisted open/closed state
  (`localStorage` key `wordkeep-wiki-section-collapsed`).

### Fixed

- Sticky wiki navbar no longer covered by sidebar filter/combo stacking
  (`aside { isolation: isolate }`, header `z-index: 30`).
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
