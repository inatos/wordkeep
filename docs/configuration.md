# Configuration

## `.wordkeep/config.json`

Lives at the **root of the repository you analyze** (the `--root` path).

```json
{
  "default_paths": ["src", "pkg/lib"],
  "default_profile": "engine",
  "path_profiles": {
    "engine": ["src"],
    "kkbp": ["tools/kkbp"]
  },
  "profile_hints": {
    "kkbp": ["kkbp", "ghilli", "bkkbp"]
  },
  "default_doc_roots": [".cursor/rules", "docs", ".github", "README.md"],
  "artifact_roots": ["tools/kkbp/artifacts"],
  "knowledge_write_roots": [".wordkeep/notes"],
  "commit_scopes": {
    "engine": ["src/", "docs/"],
    "kkbp": ["tools/kkbp/"]
  },
  "large_file_bytes": 5242880,
  "mas": {
    "auto_promote": false,
    "handoff_tokens": 1600
  },
  "test_command": "ctest -R"
}
```

| Key | Purpose |
| --- | --- |
| `default_paths` | Subdirectories searched when a tool omits `paths` / profile. Default: `["src"]`. |
| `path_profiles` | Named path sets selectable via tool arg `profile`. |
| `default_profile` | Profile used when `paths` and `profile` are omitted (before falling back to `default_paths`). |
| `profile_hints` | Keyword → profile map for deterministic inference from a query/symbol hint. |
| `default_doc_roots` | Roots for `knowledge_search` when `roots` is omitted. |
| `artifact_roots` | Roots scanned by `artifact_index` (evidence only; often gitignored). |
| `knowledge_write_roots` | Extra allowlisted write prefixes for `knowledge_upsert` (plus built-ins + root `README.md`). |
| `commit_scopes` | Named path-prefix groups for `commit_scope`. |
| `large_file_bytes` | Large-file warning threshold for `commit_scope` (default 5 MiB). |
| `mas.auto_promote` | Default `promote` for `mas_finalize` / `session_handoff` when omitted. |
| `mas.handoff_tokens` | Token cap for `kind: "handoff"` MAS entries (default 1600). |
| `test_command` | Prefix for `test_map` filter hints and `knowledge_upsert` pitfall verify lines. |

**Path resolution order:** explicit `paths` → explicit `profile` → keyword-inferred
profile → `default_profile` → `default_paths` → `["src"]`.

Template: [`.wordkeep/config.example.json`](../.wordkeep/config.example.json).

## CLI and environment

| Input | Resolution order |
| --- | --- |
| Workspace root | `--root` flag, then `WORDKEEP_ROOT`, then process cwd |

| Subcommand | Purpose |
| --- | --- |
| (default) | Speak MCP over stdio |
| `run-record` | Record gate/run metadata without executing commands |
| `dashboard` | Live savings UI (`--features dashboard`) |

| Variable | Effect |
| --- | --- |
| `WORDKEEP_ROOT` | Default `--root` when the flag is omitted |
| `XDG_CACHE_HOME` | Unix cache base (default `~/.cache`) |
| `LOCALAPPDATA` | Windows cache base |
| `WORDKEEP_TRACY_CSVEXPORT` | Path to Tracy CSV export binary |
| `WORDKEEP_MAS_ENTRY_TOKENS` | Max tokens per normal MAS blackboard entry |

Caches and telemetry persist under `<cache-base>/wordkeep/`. Workspace-scoped
MAS/runs/artifacts live under `<cache-base>/wordkeep/workspaces/<root-hash>/`.
Legacy `<cache-base>/wordkeep/mas/` sessions migrate on first access.

## Path safety

Tool `paths`, `file`, and `roots` arguments must be relative to `--root`. Absolute
paths and `..` components are rejected. Tracy capture files may be absolute paths
when passed explicitly to `trace_summary`. Artifact indexing never follows
symlinks outside the workspace.

## Optional Cargo features

| Feature | Adds |
| --- | --- |
| `embeddings` | Semantic rerank for `knowledge_search` (downloads ONNX model on first use) |
| `daslang` | Vendored tree-sitter grammar for Daslang brace dialect |
| `dashboard` | `wordkeep dashboard` terminal UI for live `stats` |

Default build has none of these.

## Knowledge writes

`knowledge_upsert` only writes:

- `.cursor/rules/`
- `docs/`
- `.wordkeep/notes/`
- root `README.md` (not arbitrary `**/README.md`)
- any extra `knowledge_write_roots`

Modes: `upsert_section` (default), `append_section`, `replace_file`, `pitfall`.
Mode is validated before fields; missing mode-specific fields are reported together.

## Integration hooks

`integration_hooks` reads `.wordkeep/integration-hooks.md` in the target repo.
Copy [examples/integration-hooks.example.md](../examples/integration-hooks.example.md)
as a starting point.

## Session continuity

See [designs/session_continuity.md](designs/session_continuity.md) for store
ownership, handoff lifecycle, and pressure semantics.
