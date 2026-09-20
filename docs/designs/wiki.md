# Wordkeep Wiki

Local, dark-themed, search-first browser for Git-backed Markdown. Companion to
the lean `wordkeep` MCP binary — not required for agent use.

## Architecture

- **Source of truth:** Markdown in the workspace (docs, rules, notes, …)
- **Search index:** Meilisearch (`wiki_chunks`) — rebuildable
- **Manifest:** `{XDG_CACHE}/wordkeep/workspaces/<id>/wiki_manifest.json`
- **API / UI:** `wordkeep-wiki serve` on `127.0.0.1:8787`
- **No application DB** in MVP. Later user state (bookmarks, drafts) may use
  Turso/libSQL locally.

## Quick start

```sh
# Meilisearch
docker compose -f docker-compose.wiki.yml up -d

# Index + serve
cargo run -p wordkeep-wiki -- --root /path/to/repo index --full
cargo run -p wordkeep-wiki -- --root /path/to/repo serve --watch

# UI (dev)
cd wiki && bun install && bun run build   # produces wiki/dist
# or: bun run dev  (proxies /api to :8787)
```

Open http://127.0.0.1:8787

## CLI

```
wordkeep-wiki [--root DIR] index [--full]
wordkeep-wiki [--root DIR] watch
wordkeep-wiki [--root DIR] serve [--bind 127.0.0.1:8787] [--watch] [--allow-non-loopback] [--runtime-attach]
wordkeep-wiki [--root DIR] status
```

Cursor folder-open (`tools/wordkeep/wiki.sh`) passes `--runtime-attach` by default
(`WIKI_RUNTIME_ATTACH=0` to opt out).

Env: `WIKI_MEILI_URL`, `WIKI_MEILI_MASTER_KEY`, `WIKI_BIND`, `WIKI_PROJECT_NAME`.

Brand title uses `wiki.project_name` / `project_name` from `.wordkeep/config.json`,
else CMake/`Cargo.toml`/`package.json`, else the workspace folder name.

## Security

- Default bind is loopback only
- Path APIs reject `..`, absolutes, and symlink escape
- Meilisearch master key stays server-side
- Markdown rendered without raw HTML pass-through
- Optional in-browser edit writes Markdown under configured doc roots only
  (`PUT /api/page`); still intended for localhost single-user use

## Features

- Instant search (debounced) with kind / path-root / tag **typeaheads** and
  highlighted snippets
- Collapsible document tree (per-folder + expand/collapse all); width resize
- Recent files as a searchable combobox (open on select)
- Reader with TOC, deep links (`?path=&anchor=`), backlinks, copy path,
  optional autosave edit (`PUT /api/page`, Ctrl/⌘E)
- VS Code-style editor tabs: pinned row + open row; pin/close; Ctrl/⌘W;
  middle-click close; persisted in `localStorage`; `?tab=` URL restore
- Frontmatter tag CRUD in the reader (add / rename / delete → `PUT /api/page`)
- Per-tag colors (picker + hex, `localStorage`) for visual chips
- Knowledge health: Meilisearch/manifest status, broken/orphan links, duplicate
  headings, search telemetry; **Runtime** subview (`?tab=health&view=runtime`)
  live-maps process VA / NUMA, cooperative Betwixt pools, capped peek, and
  confirm-gated ECS field reorder — [runtime_memory_health.md](runtime_memory_health.md).
  Knowledge stays on `/api/health` + `/api/garden`.
- MCP savings dashboard (GUI): overview / live charts / sortable tools table
  (incl. **Inv** for validation/missing-arg calls) / recent activity + health
  as tables / outcome hover shows `reason` when present; via `/api/dashboard`,
  auto-refresh while the tab is open. Every dashboard table header sorts on
  press and flips direction on the next press.
- Izakaya page (`?tab=dashboard&view=izakaya`): live presence donut, journal
  sequence line, event-kind / dirty-path / MCP call and latency bars, plus
  tables for agents, claims, handoffs, journal, notes, and `izakaya_*` calls.
  `GET /api/izakaya` reads the coordination projection and does not write it.
  Both dashboard pages share a `dq` filter (URL `?dq=`): space-separated
  substrings over Izakaya data and over MCP telemetry charts, tables, and
  overview totals. Session savings is not filtered.
- Dashboard charts (SVG, no chart npm deps): tokens-saved + calls bars, outcome
  donut with hover metrics, recent-savings line chart with axes + point hover;
  chart/section tooltips via `CHART_HELP` / `OUTCOME_HELP`
- Collapsible Dashboard sections (Overview / Charts / Recent activity / Health
  signals / Tools) persisted in `localStorage` as
  `wordkeep-wiki-section-collapsed`
- Terminal alternative: `cargo run -p wordkeep --features dashboard -- dashboard`
  (loads embedded `savings.json` events for Recent activity)
- README screenshots: `cd wiki && bun run shots` (wiki must serve on `:8787`)
- Opt-in raw query retention: `WIKI_RETAIN_QUERIES=1` or
  `wiki.retain_search_queries` in `.wordkeep/config.json`

## Deferred

- Turso/libSQL user state (bookmarks/drafts)
- Meilisearch hybrid/vector search
- Azera intelligence (cited librarian, dream jobs) — design inspiration only;
  no runtime coupling
- Runtime Memory Health UI/API is implemented (see
  [runtime_memory_health.md](runtime_memory_health.md)), including SSE deltas,
  captures/diffs, Linux bpftrace attach, Jolt diagnostics, and MCP tools.
  Remaining collectors: Windows PEBS-style data-address samples (ETW profile
  events are instruction IPs) and a fuller GPU inventory.

