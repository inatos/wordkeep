# Izakaya

Local coordination for coding agents that already have live work in a repository,
plus an offline replay lab inspired by Dream-RSI. Izakaya does not replace
Recursive-MAS. MAS remains the task/round blackboard. Izakaya owns presence,
leases, advisory claims, checkpoints, directed notes, and handoff acknowledgment.

## Where state lives

One machine. Linked Git worktrees share a coordination group derived from
`git rev-parse --git-common-dir`. Non-git roots fall back to the Wordkeep
workspace id, so separate trees do not see each other.

```
$XDG_CACHE_HOME/wordkeep/coordination/<id>/izakaya/
  izakaya.lock
  events.ndjson
  projection.json
```

`events.ndjson` is the source of truth. `projection.json` is rebuilt when its
sequence does not match the journal. A partial last line is dropped on load.
The journal stores references (paths, symbols, run ids, MAS slugs, checkpoint
names), not source bodies or patches.

## Lifecycle

Persisted states: `checked_in`, `live_code`, `suspended`, `checked_out`.

`stale` is derived from lease expiry. Expiry does not check an agent out, clear
its task, or transfer work. A matching lease can heartbeat back. A new check-in
needs `force: true` or an expired lease before it replaces an active incarnation.

Claims are advisory. Path-prefix overlap, symbol overlap, dirty-path drift, and
base-OID divergence are reported. Nothing in Izakaya blocks an edit.

Suspend refuses to proceed when dirty paths are present and no checkpoint
reference was given. Checkout releases claims. It is idempotent if the agent is
already checked out.

## Handoffs

Long prose still goes through `mas_post` with `kind: "handoff"`. Checkout then
stores a capsule: recipient, summary, `mas_session`, `mas_entry_id`, `note_ref`,
run ids, artifacts, checkpoint, anchors, commands, constraints.

Izakaya never calls `mas_finalize`. The recipient acknowledges by checking in
with `handoff_id`. Unaccepted offers stay `offered`. They become `orphaned`
only after the offering agent is checked out, the recipient is missing or also
checked out, and `izakaya.orphan_secs` has elapsed.

## Cooperative protocol

1. `izakaya_status` before mutating.
2. `izakaya_check_in` immediately before the first live edit.
3. `izakaya_update` when scope or state changes. Use `live_code` while editing.
4. Suspend only with a checkpoint when the tree is dirty.
5. Post the long handoff to MAS first, then `izakaya_check_out` with that reference.

## Replay lab (CLI / offline)

Decision/outcome recording and historical supported replay are **not** MCP tools
(agents never produced journal entries; zero telemetry calls). The offline lab
remains for tests and CLI policy work:

- `presence::record_decision` / `record_outcome` (library + unit tests)
- `wordkeep izakaya policy evaluate|promote|retire`

Replay is **historical supported replay**, not a counterfactual simulator:

- The incumbent policy reproduces recorded selections.
- A candidate may stop early or take a recorded subset.
- An action that was legal but not recorded is `out_of_support` and gets no reward.
- Active and suspended episodes are censored, not scored as failures.
- Reports include support coverage, a chronological holdout, a task holdout when
  two or more tasks exist, and Pareto axes. There is no single winner.
- `beta` is recorded and is not swept.

Policies are declarative JSON (`selection`, `max_batch`, `prefer`). They are not
arbitrary code. Project profiles live in `.wordkeep/izakaya/profiles/*.json` and
may `extends` the built-in `coordination` profile. Profiles cannot declare
`exec`, `shell`, `command`, or `cmd`.

`wordkeep izakaya policy promote` is the only promotion path. It refuses a spec
that misses the holdout, drops a correctness/integration gate, or leaves
support. Promotion activates read-only advice. `izakaya_advise` cannot assign
work, suspend, check out, or run Git.

## Tools

| Tool | Writes |
| --- | --- |
| `izakaya_status` | no |
| `izakaya_check_in` | journal |
| `izakaya_update` | journal |
| `izakaya_check_out` | journal |
| `izakaya_advise` | no |

## Dashboard

The wiki Dashboard has an Izakaya page (`?tab=dashboard&view=izakaya`).
`GET /api/izakaya` reads `projection.json` and the journal tail for the
workspace coordination group. It does not lock or rebuild the projection.
Polls every 2s while that page is open. Charts cover presence, journal
sequence, event kinds, dirty paths, and `izakaya_*` call counts and latency.
Tables cover agents, claims, handoffs, the journal tail, notes, and recent
MCP calls. Each table uses the same collapsible section as Telemetry
(open/closed state is stored in `wordkeep-wiki-section-collapsed`). Call counts
come from savings telemetry, still scoped to `izakaya_*`. Column headers sort
that table ascending or descending; press again to flip. A `dq` query on the dashboard (`?tab=dashboard&dq=`) filters
both pages: Izakaya charts and tables (agents, claims, journal, notes,
handoffs, and those calls) and the Telemetry page (overview totals, charts,
activity, health signals, and the tools table). Tokens are space-separated
substrings. Session savings stays the unfiltered delta since the page loaded.
