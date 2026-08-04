# Wordkeep Wiki companion (GUI)

Local Meilisearch + Axum + dark Svelte 5 UI (`wordkeep-wiki`). Serve on `127.0.0.1:8787`.

**Start:** `docker compose -f docker-compose.wiki.yml up -d` then `cargo run -p wordkeep-wiki -- --root <repo> index --full` and `serve --watch`. UI build: `cd wiki && bun run build` (or `bun run dev`).

**Telemetry:** GUI Dashboard tab polls `GET /api/dashboard` (same `savings.json` as terminal `cargo run --features dashboard -- dashboard`). Health tab is Meili/garden/search telemetry, not MCP savings dump.

**UI:** searchable kind/root/tag/recent comboboxes; collapsible tree; VS Code-style pinned+open tabs (`localStorage` + `?tab=`/`?path=`); frontmatter tag CRUD via `PUT /api/page`; tag colors in `localStorage`; rounded scrollbars.

**Shots:** with serve up, `cd wiki && bun run shots` refreshes `docs/dashboard.png` + `docs/wiki-*.png` for README.

**Deferred:** Turso bookmarks/drafts, Meili hybrid/vectors, Azera librarian.


# Wiki /api/tree empty-state

**Symptom:** Wiki sidebar Tree shows empty even after a successful index.

**Root cause:** `GET /api/tree` returns `{files:[...]}` (flat list). Older UI expected `roots` or a bare array.

**Fix:** Normalize in `wiki/src/lib/api.ts` via `normalizeTree` / `buildTreeFromFiles`. Empty copy should say no markdown under configured doc roots.



# Wiki page path must be under doc roots

**Symptom:** `Failed to load page` / tag update `page not found` for paths like `docs/designs/wiki.md` when the wiki `--root` is the engine monorepo.

**Root cause:** Page APIs only resolve Markdown under configured wiki doc roots (engine `docs/`, `.cursor/rules`, etc.). Files that live only under `tools/wordkeep/docs/` are outside those roots when serving with `--root` at the engine.

**Fix:** Open paths from the Tree/Recent lists (e.g. `.cursor/rules/*.mdc`, engine `docs/...`). For README shots use tree clicks, not hardcoded submodule-only paths.

