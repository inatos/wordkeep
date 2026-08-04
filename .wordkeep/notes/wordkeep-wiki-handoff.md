# Session handoff — wordkeep 0.3.0

**Date:** 2026-08-04
**Repo:** `tools/wordkeep` (submodule `https://github.com/inatos/wordkeep.git`)
**Release:** `v0.3.0` (main package + `wordkeep-wiki` / `wordkeep-knowledge`)

## Result

Shipped and polished **0.3.0**: wiki companion + telemetry truthfulness + GUI dashboard parity with the terminal TUI.

### This session (post-wiki polish)

- Terminal dashboard Recent activity: `read_snapshot` no longer discards embedded `savings.json` events.
- Activity `reason` on non-ok outcomes; GUI hover shows `outcome: message`.
- `savings.json` v5 flush re-embeds the capped events ring for GUI consumers.
- Validation `Err`s classify as **`invalid`** (not `error`); Tools table **Inv** column.
- `defect_list` distill baseline = on-disk store size (fixes false net-negative).
- Activity + Health rendered as `dash-table`s (static headers).

### Earlier 0.3.0 surface

- `GET /api/dashboard` + Dashboard tab; editor tabs; tag CRUD/colors; combobox filters
- README gallery + `bun run shots`; Meilisearch wiki serve

## Key paths

- Design: `docs/designs/wiki.md`, `CHANGELOG.md`
- Telemetry: `src/stats.rs`, `src/defects.rs`, `src/dashboard.rs`
- Wiki API/UI: `crates/wordkeep-wiki/src/dashboard.rs`, `wiki/src/{App.svelte,lib/*}`

## Ops

```sh
cd tools/wordkeep
docker compose -f docker-compose.wiki.yml up -d
cargo run -p wordkeep-wiki -- --root ../.. serve --watch
# UI http://127.0.0.1:8787
cd wiki && bun run build   # after UI edits
cargo run --features dashboard -- dashboard
```

Reload the wordkeep MCP after pulling so new outcome classification + reasons apply.
Historical `error_count` in `~/.cache/wordkeep/savings.json` stays inflated until new calls dominate or the file is reset.

## Deferred

Turso bookmarks/drafts; Meilisearch hybrid/vectors; Azera librarian/dream jobs;
server-side tag color persistence (localStorage today).

## Next

- Confirm GitHub Actions CI green on `main` and Release job on tag `v0.3.0`
- Bump parent `betwixt_engine` submodule pointer after wordkeep push
