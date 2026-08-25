# Runtime Memory Health

WinDirStat-like live view of process virtual memory, allocator and pool occupancy,
Flecs table slack, and sampled locality — hosted in Wordkeep Wiki **Health** as a
**Runtime** subview. Tracy remains the deep timing / call-stack forensic tool.
Wordkeep correlates heap, ECS, pool, and OS signals by frame and timestamp.

This document is the product and protocol design. Implementation lives in
`wordkeep-wiki` (`/api/runtime*`, Health Runtime subview), a loopback attach
helper, and a debug/profile Betwixt census probe.

Related: [wiki.md](wiki.md) (Health tab, loopback Axum, Dashboard poll conventions).
Engine constraints: [docs/designs/_design.md](../../../../docs/designs/_design.md)
(memory strategy), [betwixt-perf.mdc](../../../../.cursor/rules/betwixt-perf.mdc),
[betwixt-optimization.mdc](../../../../.cursor/rules/betwixt-optimization.mdc).

## Implementation status (2026-08-19)

Implemented:

- Real SSE snapshot/delta events with 2 s polling fallback; map payloads only
  travel when dirty.
- Bounded four-minute census ring, ≥25 ms hitch markers, 16 rotated captures
  capped at 1,800 samples, token-free persistence, and saved-vs-live diffs.
- Linux + Windows VA census, RSS/peak RSS, lossless-hex capped peek, executable
  identity confirmation, and NUMA topology. Linux EDAC DIMMs are emitted once;
  Windows DIMMs are unavailable because `GetNuma*` does not expose slots.
- Address-ordered **scanline** projection with exact gap endpoints and log₂-span
  display area. Gaps are never labeled fragmentation.
- Fixed-table, opt-in Jolt **and Flecs** diagnostic allocation tracking for
  Debug/Tracy (`BETWIXT_DIAGNOSTIC_ALLOC=1`) and the `memory-diagnostic` preset.
  Named streams (`Jolt`, `JoltAligned`, `Flecs`) plus a 16-event ring appear in
  the cooperative census; Tracy `TracyAllocN` mirrors those names.
- Explicit attach helper: Linux bpftrace heap, Linux `perf mem` data-address
  samples, Windows real-time ETW heap + SampledProfile (instruction addresses,
  labeled as such). `--input` / `WORDKEEP_ETW_INPUT` replays tracerpt/xperf/perf
  text on any host. The wiki never elevates.
- GPU census from `OpenGLBackend::EndFrame`: particle SSBO, dynamic mesh slots,
  estimated portal RT bytes, optional NVX/ATI driver VRAM (**estimated**).
- Scenario budgets: `.wordkeep/runtime/budget.json` or `WORDKEEP_RUNTIME_BUDGET`
  (see `tools/wordkeep/testdata/runtime-budget.json`). Hub evaluates limits on
  each refresh; MCP `runtime_snapshot` prints violations. Missing actuals are
  **unavailable**, not a silent pass.
- MCP `runtime_snapshot`, `memory_diff`, and `locality_hotspots`, reading local
  merged/capture data without network access.
- Shared wiki/MCP cache-root behavior and conservative ECS apply that refuses
  declarations it cannot preserve losslessly.

Still open:

- Windows PEBS-style *data-address* samples (ETW SampledProfile is instruction
  IPs only; Linux `perf mem` remains the data-address path).
- Full GPU resource inventory (textures, GI probes, mesh VBO bytes) beyond the
  EndFrame rollup.

---

## Problem

Betwixt is data-oriented: Flecs columns, Jolt temp arenas, and engine pools are
meant to stay contiguous. Failures show up as allocator holes, unused table
capacity, tiny scattered allocations, or hot systems walking cold data. Tracy
already times zones and (if hooked) can plot allocations, but it does not:

- Show a living address-space treemap of *what occupies which VA ranges*
- Separate **virtual gaps**, **heap fragmentation**, **table slack**, **page
  residency**, and **cache locality**
- Overlay Flecs tables, named pools, and OS mappings in one inspector
- Attach to an uninstrumented process for a coarse envelope

Wordkeep Health today is **Knowledge health** (Meilisearch, garden, search
telemetry). Dashboard `health.signals` are MCP token-savings watermarks. Neither
is a process-memory monitor. Runtime diagnostics must not share the garden scan
path or the `savings.json` schema.

---

## Goals

1. Identify fragmentation, locality, leak/lifetime, and pool-saturation issues
   while the process runs.
2. Support **cooperative Betwixt** (preferred, high-fidelity) and
   **arbitrary-process attach** (Linux and Windows, best-effort).
3. Keep native allocators by default; add targeted hooks; optional diagnostic
   allocator in profile builds.
4. Elevate only a dedicated attach helper after explicit confirmation — never
   the wiki server.
5. Label every number **exact**, **estimated**, **sampled**, **delayed**, or
   **unavailable**, with capability / confidence / coverage-start / overhead
   badges.
6. Map **physical RAM** (NUMA nodes, DIMM/controller slots when the OS exposes
   them) alongside the process VA treemap.
7. Inspect **capped raw bytes** for a selected VA range in the inspector
   (debugger-style), never unbounded dumps.
8. From the Runtime UI, **propose and optionally apply** ECS/POD layout rewrites
   (field packing, slack shrink notes) after an explicit confirm — measured
   census first, no silent live-world mutation.

## Non-goals

- Shipping / production telemetry
- Replacing Tracy
- Putting telemetry into rollback snapshots, CRC, or `DeterminismConfig`
- Unbounded process dumps, kernel memory, or other users' processes without
  PID + executable confirmation
- Blind whole-repo ECS “optimization passes” (`betwixt-optimization.mdc`);
  rewrites are per-symbol, confirm-gated, and must preserve rollback
  determinism (field reorder of POD is CRC-safe; adding/removing fields is not
  auto-applied)

---

## Semantics (do not conflate)

| Signal | What it is | What it is not |
| --- | --- | --- |
| **Virtual-address layout** | Contiguous VA ranges (maps / `VirtualQueryEx`): image, heap, stacks, mappings | Allocator-internal free lists |
| **Allocator fragmentation** | Free holes *inside* a heap/arena that cannot satisfy a request of typical size | Unmapped VA between mappings |
| **Flecs table slack** | `ecs_table_size` − `ecs_table_count`; unused column bytes (`bytes_table_components_unused`) | Heap fragmentation |
| **Residency** | Pages in the working set (`smaps` RSS / `QueryWorkingSetEx`) | “This allocation is hot” |
| **Sampled cache locality** | Hardware PMC / ETW samples attributed to addresses (cache-miss, false-share hints) | Inferred from rectangle packing |
| **Physical RAM / NUMA** | OS topology: nodes, distances, CPU lists, DIMM/slot labels when sysfs/WMI expose them | Process VA layout; “this byte lives on DIMM 2” requires page→PFN mapping |
| **Raw inspect** | Capped hex/ASCII of a confirmed VA range the process already maps | Full-process dump; secrets exfil; kernel memory |

**Rule:** never label a VA gap as “fragmentation.” Never infer cache locality from
the treemap packing. Inspector copy must state *why* a region is flagged and the
metric quality of each field.

Coverage start: OS attach that begins mid-run cannot reconstruct allocations that
predate the session. Those ranges are quality **unavailable** (unknown coverage),
not a fabricated complete heap.

Quality tags used everywhere below:

| Tag | Meaning |
| --- | --- |
| **exact** | Counter or range from the owner (kernel, Flecs, pool head, arena top) |
| **estimated** | Derived (column size × count, texture W×H×bpp, remainder wait) |
| **sampled** | Hardware or tracer samples; not a complete set |
| **delayed** | True value, lagged (GPU timestamp ring) |
| **unavailable** | Capability off, privilege missing, not wired, or before coverage start |

---

## UX: Health as an umbrella

Keep one Health tab (`?tab=health`). Add a subview switch:

| Subview | URL | Data |
| --- | --- | --- |
| **Runtime** | `?tab=health&view=runtime` (default when a live session exists) | `/api/runtime*` |
| **Knowledge** | `?tab=health&view=knowledge` (today’s page) | existing `GET /api/health` + `GET /api/garden` |

Preserve current Knowledge behavior: on-demand `refreshHealth()`, collapsible
ids `health-search` / `health-garden` / …, no 2s poll of `garden::analyze`.

Runtime uses Dashboard-style live updates (poll snapshot + SSE tile/aggregate
deltas while the subview is visible). Do not put runtime sampling on the garden
timer.

Navbar chip stays Knowledge/Meili status unless Runtime has an active session
with warnings — then a second chip or tooltip, not a silent overwrite.

### Layout (Runtime subview)

Inspired by disk treemaps, adapted to **address-preserving** layout (Hilbert or
row-major VA → 2D), not squarified size-only packing (that hides holes).

1. **Overview cards** — RSS, committed VA, heap used/capacity, pool saturation,
   Flecs unused column bytes, alloc/free rate, page faults. Each card: value +
   quality badge.
2. **Memory map** — WebGL2 (Canvas2D fallback). Color by owner/type; brightness
   by sampled hotness; overlays: residency, lifetime, confidence. Zoom, filter,
   selection. Dense small blocks read as fragmentation *texture* only when the
   overlay is allocator-aware.
3. **Left: region tree** — name, size, %, occupancy bar (WinDirStat list analogue).
4. **Right: legend + insights** — type/owner donut, hints (“table slack > 40% on
   `Hitbox`”, “large files” analogue: “large mappings / leak candidates”).
5. **Bottom** — collection tier, elapsed, events/s, drop count, Cancel / lower
   tier. Timeline scrubber for the bounded ring.
6. **Inspector** — selected range: owner, size, quality per field, “why flagged”,
   link to Tracy capture (frame + timestamp + optional address), **hex peek**
   (capped), NUMA/node if known.
7. **Physical map** — second canvas: NUMA nodes as regions; DIMM/slot children
   when topology is available; process RSS attributed per node (**estimated**
   unless page→node is sampled).
8. **ECS layout** — slack table + packing suggestions; **Apply rewrite** confirm
   dialog (symbol, file, before/after field order, sizeof).

Existing SVG charts (`BarChart`, `DonutChart`, `Sparkline`) for rates and
histograms. VA map + NUMA map are WebGL2/Canvas.

Optional later: system × component read/write **locality matrix** (sampled or
from cooperative query working-set hints).

---

## Architecture

```
Betwixt debug/profile probe  ──binary ingest──►  wordkeep-wiki RuntimeHub
Privileged attach helper     ──same protocol──►         │
                                                        │  GET /api/runtime
                                                        │  GET /api/runtime/stream (SSE)
                                                        │  POST /api/runtime/capture
                                                        │  POST /api/runtime/peek
                                                        │  POST /api/runtime/ecs-optimize
                                                        ▼
                              Health → Runtime subview (Svelte)
```

Tracy GUI stays independent. The probe may mirror alloc/free to `TracyAlloc` /
`TracyFree` when `TRACY_ENABLE`. Wordkeep does not parse Tracy’s wire protocol
in v1; correlation is shared frame index + monotonic timestamp (+ optional
capture path).

### Components

| Piece | Role |
| --- | --- |
| **Engine probe** | Debug/profile builds only. Presentation-side sampler (skip `RollbackConfig.Resimulating`). Emits census + optional alloc events. |
| **Attach helper** | Separate binary. Linux `/proc` + opt-in eBPF/perf; Windows `VirtualQueryEx` / working set + opt-in ETW. May prompt elevation; wiki never elevates. |
| **Ingest protocol** | Versioned length-prefixed binary on loopback (or jsonl file fallback). Sequence numbers; drop counters. |
| **RuntimeHub** | In-process in `wordkeep-wiki`: latest snapshot, ring buffer, `tokio::sync::watch` / broadcast. |
| **Capture store** | `{cache}/wordkeep/workspaces/<id>/runtime/` — jsonl + atomic `index.json`. Use MCP `cache::dir()` semantics (`LOCALAPPDATA` on Windows), not wiki `global_cache_dir()` mismatch. |
| **HTTP** | Snapshot, stream, capture start/stop, attach control. |

Wiki crate stays independent of the MCP binary. Share cache-path helpers by copy
or a tiny cache crate — no dependency cycle.

Cursor `wiki.sh` treats missing `/api/runtime` or `attach_enabled: false` as
unhealthy (same class as a missing `/api/dashboard`) so folder-open restarts a
stale serve. Generic probes that only care “is the wiki up” can still use `GET /`
+ `/api/dashboard`. Opt out with `WIKI_RUNTIME_ATTACH=0`.

### Collection tiers

| Tier | Cadence (target) | Overhead budget | Contents |
| --- | --- | --- | --- |
| **Census** | 2 Hz (UI poll 2s is fine; probe may be faster) | ≤ 0.3 ms CPU in probe on typical gym; maps walk amortized | RSS, VA regions (top-N + aggregates), pools, Flecs/Jolt summaries |
| **Sampled locality** | 10–20 Hz sample stream, UI tiles ~10 Hz | ≤ 2% CPU when enabled; degrade on drops | PMC/ETW or perf address samples → heat overlay |
| **Full alloc trace** | Event stream, batched | Explicit opt-in; drop rather than stall sim | Alloc/free/realloc with size, ptr, named pool, optional stacks |

Always emit `seq`, `dropped`, `tier`, `coverage_start_ns`. If drops exceed a
threshold, UI forces a lower tier and shows a warning.

Engine hot path: no heap allocation in the census fill; skip during rollback
resim; never write the rollback ring.

---

## Cooperative Betwixt sources

Ground in APIs that already exist. Quality tags in **bold**.

### Process envelope

| Metric | Quality | Source |
| --- | --- | --- |
| RSS / peak RSS | **exact** (kernel) | Linux `/proc/self/status`; Windows process counters |
| Committed VA | **exact** | maps / `VirtualQueryEx` aggregation |
| Swap | **exact** or **unavailable** | smaps / equivalent |
| NUMA node count / distances | **exact** when sysfs/WMI present | Linux `node*/distance`, `cpulist`; Windows `GetNuma*` |
| DIMM/slot labels | **exact** or **unavailable** | Linux DMI/sysfs `edac`/`dimm*`; Windows SMBIOS — often **unavailable** in VMs |
| RSS per NUMA node | **estimated** (smaps rollup) or **sampled** (page→node) | not DIMM-accurate without PFN |

### Flecs

| Metric | Quality | Source |
| --- | --- | --- |
| Table count, empty tables | **exact** | `ecs_get_world_info`, `ecs_tables_memory_get` |
| Table occupancy vs capacity | **exact** | `ecs_table_count` / `ecs_table_size` |
| Column unused bytes | **exact** | `ecs_component_memory_get.bytes_table_components_unused` |
| Per-table bytes | **exact** | `ecs_table_memory_get` |
| Table size histogram | **exact** | `ecs_table_histogram_get` |
| Allocator unused (Flecs internal) | **exact** | `ecs_allocator_memory_get` |
| World total | **exact** | `ecs_memory_get` |
| Column base addresses (map overlay) | **exact** when probe walks tables | `ecs_table_get_column` |
| Query working-set | **estimated** | cached query match counts × column sizes; not cache-line truth |
| `ecs_os_api` malloc count | **exact** when hooks installed; else **unavailable** | `ecs_os_set_api` wrappers |

Do not 60 Hz `world.each()` for telemetry (`betwixt-perf.mdc`).

### Jolt

| Metric | Quality | Source |
| --- | --- | --- |
| Temp arena size / usage | **exact** | `TempAllocatorImpl::GetSize` / `GetUsage` (16 MiB + 2×2 MiB CV) |
| Body / pair caps vs live | **exact** vs **estimated** | `PhysicsSystem::GetNumBodies` **exact**; ECS `JoltBody` walk is a different count — document mismatch |
| Default allocator bytes | **exact** when Jolt allocate/free hooked; else **unavailable** | replace `RegisterDefaultAllocator` in profile builds only |

Hooks must be thread-safe when `JobSystemThreadPool` is on. Off by default for
`[Determinism]` / `[Rollback]` / `[Net]`.

### Engine pools (occupancy **exact**)

`EntityPool::InUse`, `VfxEntityPool::InUse`, `NetIdPool::ActiveCount`,
`RagdollArticulatedActiveCount`, `SpatialHash::GetEntryCount`,
`GpuParticleBuffer::Count`, `AssetRegistry` slot remaining / live dynamic meshes.
Caps are compile-time **exact**.

### Frame / GPU

| Metric | Quality | Source |
| --- | --- | --- |
| Presented / CPU work / present wait | **exact** wall clocks; present wait **estimated** as remainder | `FrameBudget` |
| GPU pass ms | **delayed** (~2 frames) | `OpenGLBackend` timestamp queries |
| `GiStats` pass ms | **delayed**; `VoxelUploadMs` is **exact** CPU | `cmp_gi` / cascades |
| `ForwardPlusStats.CullMs` | **unavailable** until wired (today always 0) | do not display as real |
| GPU resource bytes | **estimated** | texture W×H×bpp, mesh VBO estimate, GI `dim³`, portal RTs |
| Driver VRAM | **unavailable** unless a vendor query is added later | not v1 |

### Tracy

| Metric | Quality | Source |
| --- | --- | --- |
| Zone times | Tracy GUI; Wordkeep **unavailable** live | existing client |
| Alloc/free mirror | **exact** when probe calls `TracyAllocN` / `TracyFreeN` | `profiler.h` today has plots/zones only — extend in implementation |

---

## Attach (Linux and Windows)

Wiki never opens other processes. The **helper** does, after UI confirmation
(PID, executable path, user). Elevation prompt is helper-only.

### Linux

| Signal | Quality | Mechanism |
| --- | --- | --- |
| VA regions | **exact** | `/proc/<pid>/maps` |
| RSS per mapping | **exact** or **estimated** | `smaps` / `smaps_rollup` (costly; sample) |
| libc malloc/free | **sampled** or **exact** stream | opt-in eBPF uprobes; **unavailable** without privilege / Yama |
| Cache-miss addresses | **sampled** | `perf_event_open` `PERF_SAMPLE_ADDR`; **unavailable** without PMC access |
| Pre-attach heap | **unavailable** | unknown coverage |
| Raw peek | **exact** bytes, **capped** | `process_vm_readv` / `/proc/<pid>/mem` after confirm; max 4096 B/request |

Prefer maps + rollup without ptrace. Peek requires `--runtime-attach` (or cooperative
self-peek in the Betwixt probe, which reads its own address space and sends
hex — still capped).

### Windows

| Signal | Quality | Mechanism |
| --- | --- | --- |
| VA regions | **exact** | `VirtualQueryEx` |
| Working set / page attributes | **exact** per queried page | `QueryWorkingSetEx` (batched, not all pages every tick) |
| Heap alloc/free | **sampled** / stream | ETW heap events (`ETW_HEAP_EVENT_*`); **unavailable** until session starts |
| PMC locality | **sampled** | ETW/PMC; **unavailable** without privilege |
| Protected processes | **unavailable** | fail closed, badge it |
| Raw peek | **exact** bytes, **capped** | `ReadProcessMemory` after confirm; max 4096 B/request |

`OpenProcess` needs query rights; PPL/antivirus fail closed.

### Capability badges

Every session header lists: OS, cooperative vs attach, tiers enabled, elevation
used (yes/no), coverage start, estimated overhead, drop rate. Metrics that the
current capability set cannot produce show **unavailable**, not 0.

---

## Allocators

**Default:** native (Flecs OS API, Jolt default, CRT/glibc). Profile builds add
counting wrappers around Flecs `ecs_os_api` and Jolt `Allocate`/`Free`, plus
named engine pool events. Optional **diagnostic allocator** (debug/profile CLI)
records per-block size/owner for true fragmentation maps; not for FPS gates or
rollback tests.

Do not hook global `operator new` in shipping/rollback builds (RmlUi, daScript,
GL driver noise and timing).

---

## Protocol sketch

Length-prefixed little-endian messages. `schema_version` in handshake; unknown
fields ignored; breaking bumps require hub reject with a clear UI error.

Snapshot (census) JSON for HTTP (poll-friendly, truncated):

```json
{
  "schema_version": 1,
  "ts_ns": 0,
  "seq": 0,
  "dropped": 0,
  "tier": "census",
  "source": "betwixt",
  "os": "linux",
  "pid": 0,
  "name": "betwixt",
  "coverage_start_ns": 0,
  "capabilities": { "cooperative": true, "alloc_hooks": false, "pmc": false },
  "quality": { "rss_bytes": "exact", "heap_used_bytes": "unavailable" },
  "rss_bytes": 0,
  "peak_rss_bytes": 0,
  "committed_va_bytes": 0,
  "numa": [],
  "pools": [],
  "regions": [],
  "hints": [],
  "ecs_suggestions": []
}
```

`regions[]` is top-N by size plus an `other` bucket. Full region list lives in
captures, not the 2s poll.

SSE: aggregate counters + dirty map tiles (not full snapshots every frame).

Peek responses are a separate POST body: `{ "addr", "len", "hex", "ascii" }`
with `len <= 4096`. They are **not** stored in captures by default.

---

## HTTP and security

Existing wiki: unauthenticated loopback, no CORS layer. Runtime is a new
privilege surface.

| Rule | Detail |
| --- | --- |
| Bind | Runtime routes loopback-only. If `--allow-non-loopback`, **disable** runtime/attach unless real auth exists (v1: disable). |
| Token | Per-serve session token; browser sends it; ingest from probe/helper requires the same or a separate ingest token. |
| Origin | Reject cross-origin `fetch` to `/api/runtime*`. |
| Attach | Off by default (`--runtime-attach`). PID allowlist. Confirm executable. Helper elevation only. |
| Bytes | Aggregates and addresses in snapshots. Peek is extra-gated, 4 KiB cap, not jsonl-logged. |
| Store | Cache dir, not git, not Meilisearch. Rotate jsonl. |
| ECS apply | Confirm body includes `symbol` + `path` + `confirm: true`. Only POD field
  reorder (large→small) or documented `static_assert(sizeof)`. No live Flecs
  world mutation. |

Path APIs unchanged (`validate_page_path`). Knowledge `GET /api/health` unchanged.

Suggested routes (implementation later):

- `GET /api/runtime` — latest snapshot
- `GET /api/runtime/stream` — SSE
- `POST /api/runtime/capture` — start/stop
- `POST /api/runtime/attach` — helper control (flag-gated)
- `POST /api/runtime/ingest` — loopback probe fallback if not using the binary socket
- `POST /api/runtime/peek` — capped hex inspect
- `POST /api/runtime/ecs-optimize` — suggest; `confirm` applies header reorder

---

## Phasing

### MVP

- Health **Runtime** / **Knowledge** subviews; Knowledge behavior preserved
- Census: RSS, VA region map (Linux + Windows attach **or** self maps from probe)
- Cooperative Flecs/Jolt/pool summaries when Betwixt probe is present
- 2s poll + bounded timeline ring
- Snapshot capture / diff (two captures, region + pool deltas)
- Capability badges and quality labels
- NUMA/DIMM topology map (best-effort; DIMM often **unavailable**)
- Capped inspector peek
- ECS packing suggestions; confirm-gated header rewrite

### Next

- Flecs/Jolt/engine alloc hooks mirrored to Tracy
- ETW / eBPF alloc streams (attach)
- Optional diagnostic allocator
- Leak / lifetime views (live set, age histogram)
- Pre/post-hitch ring (keep N seconds around a FrameBudget spike)

### Advanced

- Cache-miss address overlay; false-sharing hints (adjacent cache lines, two
  threads) — always **sampled**
- GPU resource census (**estimated**)
- Baseline regression budgets vs a saved capture
- Deterministic scenario comparisons (same `--test-area` / soak script)
- MCP tools: `runtime_snapshot`, `memory_diff`, `locality_hotspots` (token-budgeted
  text, not the WebGL map)

### ECS layout rewrite (from UI)

1. Census lists tables/components with size, count, slack (**exact**).
2. Static `type_layout` (or a header parse) flags interior padding (**exact**
   declaration order; sizeof is **estimated** unless compiled).
3. Suggestion: reorder members large→small; keep trailing bools; do not split
   hot/cold automatically (that needs a new component — out of one-click apply).
4. Apply: rewrite the named struct in-tree, add/keep `static_assert(sizeof(T)==N)`
   when N is known. User confirms in the Health UI. Rollback-safe if it is a
   pure field reorder of POD already in snapshots.

---

## Tracy correlation

Wordkeep is the map and the ECS/pool overlay. Tracy is zones, call stacks, and
(once hooked) alloc plots. Shared `frame_index` + `ts_ns`. Inspector “Open in
Tracy” is a documented workflow (capture path / timestamp), not an embedded
Tracy server in v1.

---

## Verification

| Area | Checks |
| --- | --- |
| Protocol | Schema compat; hub rejects unknown major version |
| Security | Flag-off attach → 403; bad token → 401; non-loopback → runtime disabled; peek oversize → 400; peek without attach/cooperative → 403 |
| Peek | Fixture process; cap 4096; unmapped addr → error, not zeros pretending to be data |
| ECS apply | Dry-run diff; refuse non-POD / missing struct; sizeof assert preserved |
| Parsers | Linux maps/smaps fixtures; Windows VMA list fixtures; no live ptrace in CI |
| Store | Jsonl round-trip, ring cap, rotation, atomic index |
| UI | Region truncation; map LOD; drop-tier warning; Knowledge garden not on a timer |
| Engine | Census fill allocates nothing; skipped on resim; rollback CRC unchanged when hooks off |
| Labels | Tests that a metric with missing capability serializes `unavailable`, not `0` |

### Acceptance budgets

| Budget | Target |
| --- | --- |
| Census probe CPU | ≤ 0.3 ms typical; never in resim |
| UI update | Snapshot ≤ 2s poll; SSE aggregates ≤ 10 Hz; map tiles capped |
| Snapshot JSON | Fit a 2s poll without jank (top-N regions, activity-like cap) |
| Capture | Ring + rotate; warn before disk flood |
| Badges | If a signal is missing, UI says **unavailable** |

Engine tests when implemented: `[FrameBudget]`, pool tags, `[Debug][Profiler]`;
allocator-hook experiments **not** default-on for `[Determinism]` / `[Rollback]` /
`[Net]`. Wiki: `cargo test -p wordkeep-wiki`; `cd wiki && bun run check`; shots
include `?tab=health&view=runtime` without breaking `wiki-health.png` Knowledge
shots.

---

## Implementation touchpoints

- Engine: presentation sampler near `FrameBudget::RecordPresented` /
  `OpenGLBackend::EndFrame`; Flecs stats getters; Jolt `GetUsage`; pool `InUse()`
- `src/core/profiler.h` — optional `TracyAlloc` wrappers
- Wiki: `crates/wordkeep-wiki/src/runtime/`, routes in `web.rs`, types in
  `wiki/src/lib/api.ts`, Health subviews in `App.svelte` / `RuntimeHealth.svelte`
- Helper: new binary next to wiki, not inlined into Axum (v1 may census
  `/proc/<pid>` inside wiki when `--runtime-attach` and loopback)
- Peek: Linux `/proc/pid/mem` or `process_vm_readv`; Windows `ReadProcessMemory`
- ECS apply: header rewrite via field reorder helper
- MCP (advanced): new tools, not overloads of `trace_summary`

---

## Open follow-ups

- Cache root: resolved through the shared `wordkeep-knowledge` helper; Windows
  wiki and MCP both honor `LOCALAPPDATA`.
- Empty Health state: Runtime remains the default subview and shows explicit
  unavailable/activation guidance; Knowledge remains one click away.
- Projection: **scanline** chosen and fixture-tested. Endpoints/order are exact;
  display area is log₂(span), explicitly labeled.
- Remaining collector work is tracked in the implementation-status section,
  chiefly Windows ETW and hardware address samples.
