# wordkeep

Local MCP server that gives coding agents structured, token-budgeted context
instead of full-file dumps. Point it at any repository with `--root` and call
tools over stdio (JSON-RPC).

Works with Cursor, VS Code Copilot, Claude Desktop, Zed, and other MCP clients.

## Install

### Prebuilt binary (recommended)

Download the archive for your platform from
[GitHub Releases](https://github.com/inatos/wordkeep/releases). Verify the
checksum in `SHA256SUMS.txt`, unpack, and put `wordkeep` on your `PATH`.

### Build from source

```sh
git clone https://github.com/inatos/wordkeep.git
cd wordkeep
cargo build --release
```

Binary: `target/release/wordkeep`

Requires Rust 1.74+ and a C toolchain only if you enable the optional `daslang`
feature.

## Quick start

1. Build or download `wordkeep`.
2. Add an MCP server entry that runs the binary with `--root` set to your
   workspace.
3. Reload the editor so the client respawns the server.
4. Ask your agent to call `repo_map` or `symbol_refs` before opening whole
   files.

Example (Cursor): copy [examples/mcp.cursor.json](examples/mcp.cursor.json) to
`.cursor/mcp.json` and replace `/absolute/path/to/wordkeep`.

Example (VS Code Copilot): copy [examples/mcp.vscode.json](examples/mcp.vscode.json)
to `.vscode/mcp.json`.

Optional launcher script for this repo: [mcp.sh](mcp.sh) runs
`target/release/wordkeep` with a pinned `CARGO_TARGET_DIR`.

## Configuration

Copy [`.wordkeep/config.example.json`](.wordkeep/config.example.json) to your
**target repository** as `.wordkeep/config.json`:

```json
{
  "default_paths": ["src", "pkg/lib"],
  "test_command": "ctest -R"
}
```

- `default_paths`: searched when a tool omits `paths` (fallback: `["src"]`).
- `test_command`: prefix printed by `test_map` filter hints and pitfall verify
  lines.

Environment variables:

| Variable | Purpose |
| --- | --- |
| `WORDKEEP_ROOT` | Fallback workspace root when `--root` is omitted |
| `XDG_CACHE_HOME` / `%LOCALAPPDATA%` | Platform cache base; wordkeep uses `<base>/wordkeep/` |
| `WORDKEEP_TRACY_CSVEXPORT` | Override `tracy-csvexport` binary for `.tracy` captures |
| `WORDKEEP_MAS_ENTRY_TOKENS` | Per-entry cap for MAS blackboard posts (default ~400) |

## What you get

28 MCP tools, including:

| Tool | Use when you need |
| --- | --- |
| `repo_map` | Structure of a source tree |
| `outline` | Symbols and line numbers in one file |
| `symbol_refs` | Where a symbol is defined, called, referenced |
| `call_graph` / `call_path` | Caller/callee blast radius or shortest chain |
| `symbol_context` | Body + one hop of graph + layout in one call |
| `knowledge_search` | Relevant docs/rules for a question |
| `test_map` | Narrowest tests after a change |
| `stats` | Measured token displacement per tool |

Full catalog: [docs/tools.md](docs/tools.md). Design notes:
[docs/onboarding.md](docs/onboarding.md).

Languages (via tree-sitter): C/C++, GLSL, Rust, Python, C#, TypeScript/TSX/Svelte.
Daslang (`.das`) uses a lightweight scanner by default; exact parsing is
opt-in (`--features daslang`).

## Optional features

```sh
cargo build --release --features embeddings   # semantic rerank for knowledge_search
cargo build --release --features daslang      # vendored Daslang grammar
cargo run --features dashboard -- dashboard   # live stats terminal UI
```

Default build is offline and deterministic (BM25 only, no ONNX).

## Tests

```sh
cargo test
cargo test --test mcp_stdio      # full MCP protocol harness
cargo test --test external_repo  # isolated consumer repo
```

Integration tests copy fixtures into a temporary git repository so tools never
accidentally read the parent monorepo's `.git`.

## Manual smoke test

From the wordkeep repo root, after `cargo build --release`:

```sh
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"repo_map","arguments":{"paths":["src"]}}}' \
  | ./target/release/wordkeep --root .
```

See [docs/getting-started.md](docs/getting-started.md) for a longer walkthrough.

## Telemetry

Every tool records **distilled** (estimated tokens to read raw material) vs
**returned** (what it actually emitted). Call `stats` to inspect savings.
Figures use a ~4 characters per token heuristic.

In a 29-day trial on a large private polyglot codebase (627 calls), wordkeep
estimated **240M distilled vs 315K returned** (~99.9% reduction). Methodology and
caveats: [blog.md](blog.md#measured-results-anonymized-july-2026).

## Docs map

- [docs/getting-started.md](docs/getting-started.md) - first run and MCP wiring
- [docs/configuration.md](docs/configuration.md) - config file, env vars, cache
- [docs/tools.md](docs/tools.md) - tool reference
- [docs/onboarding.md](docs/onboarding.md) - MCP primer and design rationale
- [docs/designs/recursive_mas.md](docs/designs/recursive_mas.md) - MAS blackboard
- [blog.md](blog.md) - why the project exists
- [CHANGELOG.md](CHANGELOG.md) - release notes
- [CONTRIBUTING.md](CONTRIBUTING.md) / [SECURITY.md](SECURITY.md)

## License

MIT. See [LICENSE](LICENSE) and [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
