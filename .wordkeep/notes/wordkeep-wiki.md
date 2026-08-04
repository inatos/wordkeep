# Wordkeep Wiki companion (GUI)

Local Meilisearch + Axum + dark Svelte 5 UI (`wordkeep-wiki`). Serve on `127.0.0.1:8787`.

**Release:** 0.3.0 (2026-08-04).

**Start:** `docker compose -f docker-compose.wiki.yml up -d` then `cargo run -p wordkeep-wiki -- --root <repo> index --full` and `serve --watch`. UI build: `cd wiki && bun run build` (or `bun run dev`).

**Telemetry:** GUI Dashboard tab polls `GET /api/dashboard` (same `savings.json` as terminal `cargo run --features dashboard -- dashboard`). Tools table has Trunc / Err / **Inv**; activity + health are tables; non-ok hover shows `reason` when present. Health tab is Meili/garden/search telemetry, not MCP savings dump.

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


# Dashboard shows high Err% on tools that mostly fail with "X is required"

**Symptom:** Dashboard shows high Err% on tools that mostly fail with "X is required" and 0 distill/return.

**Root cause:** finish() always mapped Result::Err to Outcome::Error, so missing-arg agent misuse inflated error_count / error-prone health for test_map, type_layout, knowledge_upsert, outline.

**Fix:** In `finish`/`classify_outcome`, treat validation Err text ("is required", "requires:", …) as Outcome::Invalid (and not_found heuristics as NotFound). GUI shows Inv separately from Err.


# Terminal wordkeep dashboard Recent activity shows "no calls yet" while GUI shows events

**Symptom:** Terminal wordkeep dashboard Recent activity shows "no calls yet" while the wiki GUI Dashboard shows recent MCP events.

**Root cause:** Standalone dashboard discarded in-memory events from savings.json and only fell back to missing workspace events.jsonl; GUI read the embedded array directly.

**Fix:** read_snapshot() must use Store::load().events (not Vec::new()) when STATS is unset; keep embedding the capped events ring (with reason) in savings.json on flush so the wiki GUI can read activity without the live MCP process.
