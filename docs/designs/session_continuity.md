# Session continuity

Wordkeep 0.2 adds durable, workspace-scoped state so multi-session agent work
survives chat resets without pasting megabytes of transcript.

## Store ownership

| Store | Location | Owner tools |
| --- | --- | --- |
| MAS sessions | `$XDG_CACHE_HOME/wordkeep/workspaces/<id>/mas/` | `mas_*`, `session_handoff` |
| Run history | `…/workspaces/<id>/runs.json` | `run_record`, `run_history`, CLI `run-record` |
| Artifact index cache | `…/workspaces/<id>/artifacts.json` | `artifact_index` |
| Last handoff marker | `…/workspaces/<id>/last_handoff.json` | `session_handoff`, `mas_finalize`, `session_pressure` |
| Defects | `<root>/.wordkeep/defects.json` | `defect_upsert`, `defect_list` |
| Knowledge notes | `<root>/.wordkeep/notes/` (and allowlisted roots) | `knowledge_upsert`, MAS spill/promote |
| Stats | `$XDG_CACHE_HOME/wordkeep/savings.json` | all instrumented tools |

`<id>` is a stable hash of the canonical `--root` path. Legacy global
`wordkeep/mas/<session>.json` files are copied into the workspace cache on first
access (not deleted).

## Lifecycle

1. Agents record progress with `mas_post` (use `kind: "handoff"` for long state).
2. Gate scripts optionally call `wordkeep run-record` (never blocks on Wordkeep).
3. Eyeball/gate blockers go into `defect_upsert` (`eyeball_fail` ranks first).
4. Before context pressure climbs, call `session_handoff` (or `mas_finalize`) to
   get a paste-ready next-session prime and reset the pressure window.
5. Next session: paste the prime, then `defect_list` → `run_history` → continue.

## Path profile precedence

`paths` (explicit) → `profile` → `profile_hints` keyword inference →
`default_profile` → `default_paths` → `["src"]`.

## Security boundaries

- All tool paths stay relative to `--root` (no `..`, no absolutes).
- `artifact_index` does not follow outbound symlinks and does not grade images.
- `commit_scope` is read-only git status parsing.
- `run_record` never executes the recorded command.
- `session_pressure` is a heuristic; host turn count / context-window usage is
  unavailable and must not be inferred as exact.
