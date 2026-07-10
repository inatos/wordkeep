# Getting started

## 1. Install wordkeep

Pick one:

- **Release binary:** [GitHub Releases](https://github.com/inatos/wordkeep/releases)
- **Source:** `git clone` + `cargo build --release` (Rust 1.74+)

## 2. Wire your editor

wordkeep speaks MCP over stdio. Your client launches it as a subprocess and
passes `--root <workspace>`.

**Cursor** (`.cursor/mcp.json`):

```json
{
  "mcpServers": {
    "wordkeep": {
      "command": "/absolute/path/to/wordkeep",
      "args": ["--root", "${workspaceFolder}"]
    }
  }
}
```

**VS Code Copilot** (`.vscode/mcp.json`):

```json
{
  "servers": {
    "wordkeep": {
      "type": "stdio",
      "command": "/absolute/path/to/wordkeep",
      "args": ["--root", "${workspaceFolder}"]
    }
  }
}
```

Templates: [examples/mcp.cursor.json](../examples/mcp.cursor.json),
[examples/mcp.vscode.json](../examples/mcp.vscode.json).

Reload the editor after editing MCP config so the server respawns.

## 3. Configure the target repo

In the **project you are coding in** (not necessarily the wordkeep repo), copy
[`.wordkeep/config.example.json`](../.wordkeep/config.example.json) to
`.wordkeep/config.json` and set `default_paths` to where your source lives.

Optional: add `.wordkeep/integration-hooks.md` for curated cross-module wiring
(see [examples/integration-hooks.example.md](../examples/integration-hooks.example.md)).

## 4. First tool calls

Ask your agent to try:

1. `repo_map` with `{}` (uses `default_paths`)
2. `outline` with `{ "file": "src/main.rs" }` (or your entry file)
3. `symbol_refs` with `{ "symbol": "YourFunction" }`
4. `stats` to see token displacement

## 5. Smoke test from a shell

```sh
cargo build --release   # if building from source
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  | wordkeep --root /path/to/your/repo
```

You should see `wordkeep` in `serverInfo` and 28 tools in `tools/list`.

## Next steps

- [configuration.md](configuration.md) for env vars and cache layout
- [tools.md](tools.md) for per-tool arguments
- [onboarding.md](onboarding.md) for how wordkeep differs from grep-and-read
