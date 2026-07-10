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

- validated paths under `.wordkeep/`, `docs/`, and `.cursor/rules/` via `knowledge_upsert`
- cache files under your platform cache directory (`$XDG_CACHE_HOME/wordkeep/` or `%LOCALAPPDATA%/wordkeep/`)

Treat `--root` like any local code-execution tool: point it only at repositories you trust.
