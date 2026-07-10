# Changelog

All notable changes to wordkeep are documented here. The project follows
[Semantic Versioning](https://semver.org/).

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
