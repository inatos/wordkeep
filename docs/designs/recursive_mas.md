# Recursive MAS in wordkeep

Inspired by [RecursiveMAS](https://recursivemas.github.io/) and the research narrative
around cross-agent state transfer, but implemented for **today's MCP subagent workflows**
rather than latent-vector plumbing between models.

## What RecursiveMAS proposes

The paper/video vision is **cross-agent latent state transfer**: agents pass hidden-state
tensors instead of decoding thoughts to English and re-encoding them, cutting token cost
and preserving "brain state" across roles (planner, solver, critic).

That requires model/runtime support for tensor handoff. MCP hosts (Cursor subagents, etc.)
speak JSON tool calls and text payloads - not arbitrary latent buffers.

## What wordkeep implements instead

A **session-scoped blackboard** under
`$XDG_CACHE_HOME/wordkeep/workspaces/<root-hash>/mas/<session>.json`
(legacy global `wordkeep/mas/` files migrate on first access):

| Tool | Role |
| --- | --- |
| `mas_post` | Append a compact entry (summary, claims, decisions, open questions, anchors, `commands`, `constraints`, `kind`, `handoff_to`, tags, `approved`) |
| `mas_read` | Read entries token-budgeted, newest-first; filter by `recipient`, `role`, `round`, `tag`, `since_id` |
| `mas_status` | Round bookkeeping, per-role counts, convergence hint; `advance_round: true` closes a loop |
| `mas_finalize` | Mark session done, store consolidated `result`; optional `promote`; emits paste-ready handoff prompt by default; **`effects: commit\|recover`** when `effect_session` writes pending |
| `session_handoff` | Build the same handoff template without requiring a fresh finalize (or finalize+promote in one call) |

Design constraints (see `src/mas.rs`):

- **Token caps** - per-entry default ~400 tokens (`WORDKEEP_MAS_ENTRY_TOKENS`); `kind: "handoff"` uses ~1600 (`mas.handoff_tokens`); overflow spills full text to `.wordkeep/notes/` and keeps a bounded blackboard summary/`note_ref`.
- **Revertible writes** - optional `effect_session` on `knowledge_upsert`; `mas_finalize` requires `effects` when pending (`src/effect_journal.rs`).
- **Disk-backed** - each MCP subagent may spawn a fresh process; sessions reload from disk so handoffs still work.
- **Workspace-scoped** - sessions from different `--root` trees never collide.
- **Atomic writes** - temp file + rename; session ids validated as slugs (no `/`, no `..`).
- **Honest telemetry** - blackboard I/O goes through the same `stats` distilled/returned accounting as other tools.

This is the MCP-feasible analog: structured, bounded state instead of re-pasting prior
agent output into chat. Cross-session continuity (defects, runs, pressure) is documented
in [session_continuity.md](session_continuity.md).

## Typical loop

Documented step-by-step in the [README MAS section](../../README.md#recursive-mas-multi-agent-blackboard):

1. `mas_status` (open session, default `max_rounds: 3`)
2. Planner: gather with `repo_map` / `symbol_context`, then `mas_post` → `handoff_to: "solver"`
3. Solver: `mas_read recipient=solver`, implement, `mas_post` → `handoff_to: "critic"`
4. Critic: `mas_read recipient=critic`, review, `mas_post` with `approved`
5. Orchestrator: `mas_status`; if rounds remain, `advance_round: true` and repeat
6. `mas_finalize` (+ optional `promote: true`; when journaled writes exist, `effects: "commit"|"recover"`)

### Revertible `knowledge_upsert` — operator workflow

Use when an agent session may **discard** note writes (paper PoC: temporal composability).

**1. Open a MAS session**

```json
{"session": "feature-x", "role": "planner", "summary": "…"}
```

(`mas_post` or implicit via first `mas_status` / `effect_session` write.)

**2. Journal note writes**

```json
{
  "path": ".wordkeep/notes/feature-x.md",
  "heading": "Draft",
  "body": "…",
  "effect_session": "feature-x"
}
```

Repeat as needed. Each write is recorded LIFO in
`$XDG_CACHE_HOME/wordkeep/workspaces/<id>/mas/effects/feature-x.json`.

**3. Check pending state**

```json
{"session": "feature-x"}
```

(`mas_status` → `pending_effect_writes: N` when a journal is open.)

**4. Close effects before finalize**

| Decision | When | Result |
| --- | --- | --- |
| `"effects": "commit"` | Keep journaled writes | Journal dropped; session can finalize / promote |
| `"effects": "recover"` | Undo agent notes | LIFO revert; session **stays open** (not finalized) |

```json
{"session": "feature-x", "result": "…", "effects": "recover"}
```

**5. Finalize + optional promote** (after commit or when no pending writes)

```json
{"session": "feature-x", "result": "…", "promote": true, "effects": "commit"}
```

**Conflict policy:** recover refuses if on-disk content ≠ journaled post-write snapshot (external edit or concurrent writer).

**Spatial coeffects:** declared in `wordkeep://capabilities`. Write tools notify dependent caches (`knowledge-chunks-cache`, MAS session cache). `mas_read` evicts session cache before load for cross-process freshness.

**Capability manifest:** `resources/read` → `wordkeep://capabilities`.

Compare token leverage before/after with `stats`.

## When to promote vs keep ephemeral

- **Blackboard** - scratch state for one multi-agent task; lives in cache, gitignored.
- **`.wordkeep/notes/`** - durable conclusions worth indexing; `mas_finalize promote: true`
  or `knowledge_upsert` for structured pitfalls.

## References

- [RecursiveMAS site](https://recursivemas.github.io/)
- [RecursiveMAS repository](https://github.com/RecursiveMAS/RecursiveMAS)
- Implementation: `src/mas.rs`, MCP registration in `src/main.rs`
