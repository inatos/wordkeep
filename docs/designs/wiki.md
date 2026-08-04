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
wordkeep-wiki [--root DIR] serve [--bind 127.0.0.1:8787] [--watch] [--allow-non-loopback]
wordkeep-wiki [--root DIR] status
```

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
  headings, search telemetry (latency, no-result rate, click rank)
- MCP savings dashboard (GUI): overview / sortable tools table / recent activity /
  health signals via `/api/dashboard`, auto-refresh while the tab is open
- Terminal alternative: `cargo run -p wordkeep --features dashboard -- dashboard`
- README screenshots: `cd wiki && bun run shots` (wiki must serve on `:8787`)
- Opt-in raw query retention: `WIKI_RETAIN_QUERIES=1` or
  `wiki.retain_search_queries` in `.wordkeep/config.json`

## Deferred

- Turso/libSQL user state (bookmarks/drafts)
- Meilisearch hybrid/vector search
- Azera intelligence (cited librarian, dream jobs) — design inspiration only;
  no runtime coupling

