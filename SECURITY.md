# Security policy

## Supported versions

Security fixes land on `main` and are included in the next tagged release.

## Reporting a vulnerability

Please report security issues privately:

1. Open a [GitHub security advisory](https://github.com/inatos/wordkeep/security/advisories/new) if you have access, or
2. Email the maintainer through GitHub profile contact options.

Do not file public issues for undisclosed vulnerabilities.

## Scope notes

wordkeep runs locally as an MCP stdio subprocess. It reads files under the `--root` workspace you configure and writes only to:

- validated paths under `.wordkeep/`, `docs/`, `.cursor/rules/`, root `README.md`, and configured `knowledge_write_roots` via `knowledge_upsert`
- `.wordkeep/defects.json` via `defect_upsert`
- cache files under your platform cache directory (`$XDG_CACHE_HOME/wordkeep/` or `%LOCALAPPDATA%/wordkeep/`), including workspace-scoped MAS/run/artifact stores and the Izakaya coordination journal (`coordination/<id>/izakaya/`)

It does **not**:

- execute commands recorded by `run_record` / `wordkeep run-record`
- stage or commit via `commit_scope`
- follow artifact symlinks outside `--root`
- grade image quality from `artifact_index`
- store source bodies or patches in the Izakaya journal (paths, symbols, and references only)
- let a promoted Izakaya policy assign work, suspend agents, check them out, or run Git

Treat `--root` like any local code-execution tool: point it only at repositories you trust.

MCP resource `wordkeep://readme` (alias `wordkeep://README`) serves the packaged README text only.

## Wiki companion (`wordkeep-wiki`)

The optional wiki server binds **loopback only** by default (`127.0.0.1`).
Non-loopback binds require `--allow-non-loopback`. Path APIs reject `..`,
absolute paths, and symlink escape outside `--root`. Meilisearch admin keys
stay server-side; do not expose Meilisearch or the wiki API on untrusted
networks without additional auth.
