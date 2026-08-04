# Session handoff — wiki GUI polish

**Date:** 2026-08-04
**Repo:** `tools/wordkeep` (submodule `https://github.com/inatos/wordkeep.git`)

## Result

Shipped wiki GUI telemetry dashboard + UX polish as alternative to terminal `wordkeep dashboard`:

- `GET /api/dashboard` + Dashboard tab (overview, sortable tools, activity, health, copyable CLI hints)
- URL `?tab=` persistence; editor tabs (pinned row + open); tree collapse/expand-all
- Kind/root/tag/recent searchable comboboxes; tag CRUD + color picker/hex
- README gallery + `bun run shots` capture script; refreshed `docs/dashboard.png`

## Key paths

- Design: `tools/wordkeep/docs/designs/wiki.md`
- API: `crates/wordkeep-wiki/src/{web,dashboard}.rs`
- UI: `wiki/src/{App.svelte,app.css,lib/*}`
- Shots: `wiki/scripts/capture-readme-shots.ts` → `docs/wiki-*.png`, `docs/dashboard.png`

## Ops

```sh
cd tools/wordkeep
docker compose -f docker-compose.wiki.yml up -d
cargo run -p wordkeep-wiki -- --root ../.. serve --watch
# UI http://127.0.0.1:8787
cd wiki && bun run build   # after UI edits
cd wiki && bun run shots   # README screenshots (serve must be up)
```

Avoid `pkill -f wordkeep-wiki` (can match the launching shell). Free port 8787 by PID.

## Deferred

Turso bookmarks/drafts; Meilisearch hybrid/vectors; Azera librarian/dream jobs.

## Next

- Publish/tag wordkeep 0.3.0 if not already on origin
- Bump parent `betwixt_engine` submodule pointer after wordkeep push
- Optional: persist tag colors server-side later (currently localStorage only)
