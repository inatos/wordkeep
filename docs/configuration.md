# Configuration

## `.wordkeep/config.json`

Lives at the **root of the repository you analyze** (the `--root` path).

```json
{
  "default_paths": ["src", "pkg/lib"],
  "test_command": "ctest -R"
}
```

| Key | Purpose |
| --- | --- |
| `default_paths` | Subdirectories searched when a tool omits `paths`. Default: `["src"]`. |
| `test_command` | Prefix for `test_map` filter hints and `knowledge_upsert` pitfall verify lines. |

Template: [`.wordkeep/config.example.json`](../.wordkeep/config.example.json).

## CLI and environment

| Input | Resolution order |
| --- | --- |
| Workspace root | `--root` flag, then `WORDKEEP_ROOT`, then process cwd |

| Variable | Effect |
| --- | --- |
| `WORDKEEP_ROOT` | Default `--root` when the flag is omitted |
| `XDG_CACHE_HOME` | Unix cache base (default `~/.cache`) |
| `LOCALAPPDATA` | Windows cache base |
| `WORDKEEP_TRACY_CSVEXPORT` | Path to Tracy CSV export binary |
| `WORDKEEP_MAS_ENTRY_TOKENS` | Max tokens per MAS blackboard entry |

Caches and telemetry persist under `<cache-base>/wordkeep/` (never inside your
repo). MAS sessions use `<cache-base>/wordkeep/mas/`.

## Path safety

Tool `paths`, `file`, and `roots` arguments must be relative to `--root`. Absolute
paths and `..` components are rejected. Tracy capture files may be absolute paths
when passed explicitly to `trace_summary`.

## Optional Cargo features

| Feature | Adds |
| --- | --- |
| `embeddings` | Semantic rerank for `knowledge_search` (downloads ONNX model on first use) |
| `daslang` | Vendored tree-sitter grammar for Daslang brace dialect |
| `dashboard` | `wordkeep dashboard` terminal UI for live `stats` |

Default build has none of these.

## Knowledge writes

`knowledge_upsert` only writes under:

- `.cursor/rules/`
- `docs/`
- `.wordkeep/notes/`

Modes: `upsert_section` (default), `append_section`, `replace_file`, `pitfall`.

## Integration hooks

`integration_hooks` reads `.wordkeep/integration-hooks.md` in the target repo.
Copy [examples/integration-hooks.example.md](../examples/integration-hooks.example.md)
as a starting point.
