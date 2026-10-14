/** Hover / title help copy for wiki Dashboard surfaces. */

export const TOOL_HELP: Record<string, string> = {
  repo_map: "Files-first (or symbols) map of a path tree; pages via continuation when truncated.",
  outline: "Symbol TOC with line numbers for one file or a small files[] batch (pages via continuation).",
  symbol_resolve: "Normalize a messy name and locate it; fuzzy/profile did-you-mean on miss.",
  symbol_refs: "Definitions, calls, and references for a symbol (pages via continuation).",
  call_graph: "One hop of callers and callees.",
  call_path: "Shortest call chain between two symbols.",
  include_graph: "One hop of #include includers/includees.",
  type_layout: "Struct/class fields and non-POD flags.",
  doc_comment: "Leading doc comment plus signature.",
  symbol_context: "Body + callers/callees + layout; pages callers via continuation.",
  batch_context: "Condensed symbol_context for many symbols under one budget.",
  usage_examples: "Call sites with surrounding context.",
  symbol_diff: "How one symbol changed vs a git ref.",
  diff_map: "Symbols changed in a diff plus immediate callers.",
  module_map: "Cross-module call coupling map (pages via continuation).",
  dead_code: "Symbols with zero callers/refs.",
  big_functions: "Largest functions by line span (pages via continuation).",
  undocumented: "Exported symbols missing doc comments.",
  test_map: "Test files that reference a symbol.",
  test_impact: "Changed symbols \u00d7 ranked test files from a git diff.",
  commit_scope: "Dirty-path groups for a reviewable commit (read-only).",
  profile_upsert: "Propose or persist a path_profiles entry in config.",
  knowledge_search: "BM25 search over docs/rules/notes (pages via continuation).",
  knowledge_answer: "Extractive answer + citations from the same BM25 index.",
  knowledge_upsert: "Write or update a markdown knowledge section.",
  semantic_decide: "Score unstructured state + question against typed options (letter logits). Backend is heuristic or Ereshkigal GGUF.",
  trace_summary: "Hottest Tracy zones (or vs a baseline).",
  trace_profile: "Hitch workflow: trace + diff_map + index_stale.",
  perf_triage: "Tracy profile + hotspot context + tests in one bundle.",
  runtime_snapshot: "Latest Runtime Memory Health census (or a capture id).",
  memory_diff: "Signed deltas between two Runtime Health captures.",
  locality_hotspots: "Sampled PMC/ETW/perf address hotspots (or unavailable).",
  integration_hooks: "Curated cross-subsystem wiring (AND then OR match).",
  index_stale: "Whether disk indexes may lag git or miss coverage.",
  index_health: "Path-profile coverage gaps + proactive stale signal.",
  stats: "Estimated context avoided / token displacement telemetry.",
  run_record: "Metadata-only gate/run write (never executes commands).",
  run_history: "Recent runs; flags missing logs/artifacts.",
  artifact_index: "Artifact metadata index (no image grading).",
  session_pressure: "Heuristic context-pressure level; autopilot handoff draft when high.",
  defect_upsert: "Create or update a structured defect.",
  defect_list: "Defect digest by default (counts + top); full list on demand.",
  mas_post: "Append a compact multi-agent session entry.",
  mas_read: "Read MAS entries (filter by recipient/role/round).",
  mas_status: "MAS round bookkeeping and convergence hint.",
  mas_finalize: "Close a MAS session; optional promote + handoff.",
  session_handoff: "Paste-ready next-session prime.",
  izakaya_status: "Who holds live work leases and advisory claims.",
  izakaya_check_in: "Take or resume a lease before the first live edit; claims are advisory.",
  izakaya_update: "Heartbeat, move state, or change claims on an open lease.",
  izakaya_check_out: "Release a lease; optional handoff capsule for the next agent.",
  izakaya_advise: "Read promoted advice (does not assign or check out).",
};

export function toolHelp(name: string): string {
  return TOOL_HELP[name] || `MCP tool “${name}” — see docs/tools.md.`;
}

export const METRIC_HELP: Record<string, string> = {
  calls: "Total MCP tool invocations recorded in savings.json.",
  distilled: "Estimated tokens the agent would have spent reading raw material (baseline).",
  returned: "Tokens actually returned in distilled MCP answers.",
  saved: "Distilled \u2212 returned: estimated context avoided.",
  session: "Saved tokens since this browser tab first opened the Dashboard (session baseline).",
  tracking: "How long cumulative telemetry has been collecting on disk.",
};

export const COLUMN_HELP: Record<string, string> = {
  name: "MCP tool name. Hover a row for what the tool does.",
  calls: "Lifetime call count for this tool.",
  avg_ms: "Average latency per call (milliseconds).",
  trunc: "Calls whose answer was truncated to the token budget.",
  err: "Hard failures (IO/panic-class). Validation/missing-arg calls count as Inv instead.",
  inv: "Invalid calls: missing required args, bad modes, or other agent misuse.",
  distill: "Cumulative baseline (distilled) tokens for this tool.",
  return: "Cumulative tokens returned by this tool.",
  saved: "Cumulative tokens avoided (distill \u2212 return). Click to sort.",
  pct: "Reduction percentage: saved / distill.",
  peak: "Largest single-call save watermark for this tool.",
  last: "Time since the most recent call.",
};

export const CHART_HELP: Record<string, string> = {
  section: "Live visualizations from the same /api/dashboard poll as the tables (refreshes every 2s while this tab is open). Collapse to focus on tables.",
  saved: "Per-tool cumulative tokens avoided (distill \u2212 return). Longer bar = more context saved. Top tools shown; remainder folded into \u201cother\u201d. Red bars mean net-negative distill.",
  calls: "Per-tool lifetime MCP call count. Shows which tools dominate agent traffic. Top tools shown; remainder folded into \u201cother\u201d.",
  outcomes: "Share of all recorded calls by outcome: ok (clean), trunc (hit token budget), error (hard failure), invalid (bad args/misuse), low-yield (thin/unhelpful reply). Hover a slice or legend row for count, %, and meaning; center switches to that bucket.",
  spark: "Tokens saved on each recent MCP call (ring buffer, oldest \u2192 newest). Axes show value \u00d7 relative age; hover the line for the value, tool, and age at that point. Meta: n=events, last=newest save, peak=max in the window.",
  overview: "Lifetime MCP call and token totals from ~/.cache/wordkeep/savings.json (same source as the terminal dashboard).",
  activity: "Most recent MCP tool calls (newest first). Outcome chips flag truncated / error / low-yield / invalid replies.",
  signals: "Watermarks and risk signals: peak saves, slow tools, low-yield rate, and net-negative distill.",
  tools: "Per-tool lifetime stats. Click a column header to sort; hover a tool name for what it does.",
};

export const ERESHKIGAL_HELP: Record<string, string> = {
  status: "Last MCP heartbeat: backend, serving mode, GGUF paths. Charts in this section are live SemIf call mix from /api/semif (refreshes every 2s).",
  backend: "Configured scorer: ereshkigal (GGUF letter logits) or heuristic. Missing GGUF on decide is an error, not a silent heuristic score.",
  mode: "Serving path: direct (one option), serial (one-at-a-time), or shared (batched prefix).",
  debias: "Option-order correction. permute remaps letters (i+k)%n and averages; pride is opt-in PriDe; none leaves raw logits.",
  gpu: "n_gpu_layers requested (99 = offload all). Used value may be 0 after CPU fallback if Vulkan/GPU init failed.",
  gguf: "Whether the draft GGUF actually loaded in this MCP process.",
  checkpoint: "Draft GGUF path (and optional 4B verify GGUF for conformal cascade).",
  latency: "Per-call score latency in milliseconds (ring of recent decide/rerank rows). Hover the sparkline for id and ms.",
  mix: "Share of recent rows: GGUF (ereshkigal scorer), heuristic/other, or fallback/error (feature off, missing GGUF, cascade error).",
  cascade: "Conformal cascade source counts: draft (no verify), cascade-draft (0.6B committed), cascade-verify (4B), cascade-skipped (no verify GGUF), error, none.",
  calls: "Recent semantic_decide and knowledge_search semif:true rows (newest first). Hover a column header for the term; click to sort.",
  bakeoff: "1080 Ti same-GGUF: 0.6B 144/144, bartowski 4B 3/3 @ 1e-2, Wordkeep 7-row p50. Published BF16 is a different checkpoint.",
};

export const ERESHKIGAL_COLUMN_HELP: Record<string, string> = {
  when: "When the SemIf call was recorded (unix seconds). Newest first until you press a column.",
  kind: "decide = semantic_decide. rerank = knowledge_search semif:true reorders BM25 (does not replace retrieval).",
  id: "Decision id from the request, or the kind when no id was recorded.",
  chosen: "Highest-probability option letter/label after scoring (and permute remap when enabled).",
  scorer: "Which scorer wrote the row: ereshkigal (GGUF) or heuristic.",
  us: "Wall time for that score call in milliseconds.",
  cascade: "draft = 0.6B only; cascade-draft = conformal singleton commit; cascade-verify = 4B pass; cascade-skipped = no verify GGUF; error = score failed; \u2014 = not cascaded.",
  fallback: "Row recorded because GGUF/feature was unavailable. knowledge_search keeps BM25 order.",
  number: "Bakeoff headline (accuracy or BA). Hover the caption for what was actually compared.",
  caption: "What the number measures \u2014 do not mix 0.6B parity, 4B llama.cpp \u0394p, unsloth Q4, and SemIf BF16.",
};

export const OUTCOME_HELP: Record<string, string> = {
  ok: "Clean call \u2014 answer returned within budget without hard failure.",
  trunc: "Answer was truncated to fit the token budget.",
  error: "Hard failure (IO, panic-class, or tool abort).",
  invalid: "Invalid call: missing required args, bad mode, or other agent misuse.",
  "low-yield": "Thin or unhelpful reply relative to the distilled baseline.",
  other: "Remaining tools outside the top-N chart slots, aggregated together.",
};

/** Dashboard → Providers page (config-declared sidecar cards). Keys match card titles (case-insensitive). */
export const PROVIDER_FIELD_HELP: Record<string, string> = {
  peer: "What: peer reachability (up/down) and vantage role. Why: if the peer is down, remote SemIf / Lane-L / task wire will fail open to local fallbacks \u2014 fix cable/serve before chasing app bugs.",
  "rtt ms": "What: round-trip time to the peer in milliseconds (spark of recent samples). Why: rising RTT predicts soak/stream timeouts and sluggish SemIf; empty means health has not measured a ping yet.",
  caps: "What: link capability probe (AF_PACKET / setcap readiness). Why: \u201cmissing\u201d means the process cannot open the fabric wire at all \u2014 no amount of app retry will help until caps/iface are fixed.",
  "infer daemon": "What: local inference daemon for remote SemIf / Lane-L. Why: up = ready to score; stale = sock left behind after a crash (reclaim/restart); n/a = not expected on this vantage.",
  "mtu (maximum transmission unit)": "What: Maximum Transmission Unit \u2014 largest L2 frame the NIC path is configured for (bytes), plus TX backend. Why: sets the hard ceiling on one Copper frame; jumbo (e.g. 9000 B) only helps if both ends and the switch path agree, and stream chunks still cap at 1464 B today.",
  "ack window": "What: how many Copper frames may be in flight before the sender waits for ACKs. Why: larger windows raise throughput on high-RTT links (measured ~21 Mbps @ 16 frames) but also raise loss recovery cost if the peer stalls.",
  "max payload": "What: usable Copper body after Ethernet (14 B) + Copper (22 B) headers = MTU \u2212 36. Why: anything larger must be chunked across multiple L2 frames \u2014 this is the per-frame budget before the stream chunk cap.",
  "chunk cap": "What: BatchChunk copper-payload cap (min(max payload, 1464 B)); detail is data bytes after the 8 B chunk header. Why: even on jumbo MTU we keep this classic cap until a measured safe raise lands \u2014 oversized chunks caused ACK timeouts on live cable.",
  "last soak batch": "What: size of the last soak PASS batch plus Mbps / particles / estimated chunk count. Why: proves the wire is moving real payload (not just ping); empty means no soak has been recorded yet.",
  "soak mbps": "What: sparkline of soak payload throughput in megabits/second. Why: glanceable regression signal after MTU/window/worker changes \u2014 flat/empty until a soak PASS is written.",
  "wire frame": "What: byte split of one L2 Copper frame budget (headers vs chunk data vs jumbo unused). Why: shows where MTU headroom actually goes; jumbo unused means MTU grew but chunk cap still bottlenecks stream size.",
  "payload budget": "What: how the copper-payload is spent (chunk data vs chunk header [/ jumbo unused]). Why: answers \u201chow much application data fits per chunk\u201d without reading copper-frame constants.",
  "courier unread": "What: untaken notify-mailbox messages. Why: backlog means done/fail/health edges you have not triaged \u2014 Courier is notify-only; task_* remains job Source of Truth (SoT).",
  "task lanes": "What: Palaestra LAN job counts by lane:status (L = light smoke/pose/OCR; J = heavy train/mesh). Why: shows whether the fabric is idle, busy, or failing without opening each task receipt.",
  "courier by kind": "What: unread Courier messages grouped by kind (task.done / fail / copper.health / \u2026). Why: triage which notify class is piling up before you dig the mailbox.",
  "semif backends": "What: mix of recent SemIf scorers (remote-fabric / ereshkigal / heuristic). Why: tells you whether decides are riding the peer wire or falling back locally.",
  "recent fabric calls": "What: recent MCP calls biased toward task_*/SemIf/runtime tools. Why: the live breadcrumb of fabric-touching agent work \u2014 outcome icons flag trunc/fail without opening savings.json.",
  "recent courier": "What: newest Courier mailbox rows (kind \u2192 Tool, redacted body \u2192 Detail). Why: the notify timeline beside fabric MCP calls; includes taken messages so history is not lost after triage.",
  status: "Status strip: reachability, daemon, wire envelope (MTU/window/payload), Courier unread, and RTT/soak sparks \u2014 the \u201cis the fabric usable?\u201d glance.",
  mix: "Mix charts: wire/payload budget, task lanes, Courier kinds, SemIf backends \u2014 where capacity and traffic are going.",
};

export const PROVIDER_CHROME_HELP = {
  live: "Sidecar is fresh \u2014 values are from the last health/writer refresh. Trust these for go/no-go.",
  stale: "Sidecar older than stale_secs \u2014 showing last-known values. Why it matters: peer may have flipped since; refresh copper_health / writer before acting.",
  unavailable: "Sidecar missing or unreadable. Why: Providers has nothing to project until telemetry_providers.sidecar exists and the Copper writer runs.",
  age: "Age of the sidecar sample. Wiki polls /api/dashboard every 2s while this page is open.",
  provider: "Config-declared opaque sidecar (JSON Pointers only). Wordkeep stays fabric-agnostic; Copper writes the redacted file out-of-tree.",
} as const;

export const PROVIDER_COLUMN_HELP = {
  when: "How long ago this row happened (live from event ts, not the sidecar bake stamp). Newest at the top \u2014 use it to see if the fabric is live right now or stuck on old noise.",
  tool: "MCP tool name or Courier kind. Hover for tool help when available; this is the \u201cwhat ran / what notified\u201d.",
  outcome: "Result icon: check = ok; warn = trunc/empty/progress/health blip; x = error/fail. Why: scan for red before reading Detail.",
  detail: "Reason string or redacted Courier body \u2014 the so-what behind the outcome (error text, notify payload, etc.).",
} as const;

/** Resolve help for a provider card title (or kind fallback). */
export function providerFieldHelp(title: string, kind?: string): string {
  const key = title.trim().toLowerCase();
  if (PROVIDER_FIELD_HELP[key]) return PROVIDER_FIELD_HELP[key];
  // Prefer so-what stubs over “Status field X from the sidecar” noise.
  if (kind === "metric") {
    return `What: ${title}. Why: live scalar from the provider sidecar — hover sibling fields for fabric-specific so-what.`;
  }
  if (kind === "sparkline") {
    return `What: ${title} over time (oldest → newest). Why: trend beats a single sample when judging regressions.`;
  }
  if (kind === "bar") {
    return `What: ${title} breakdown. Why: compares shares/counts at a glance.`;
  }
  if (kind === "donut") {
    return `What: ${title} share mix. Why: shows which category dominates recent traffic.`;
  }
  if (kind === "activity") {
    return `What: ${title} log. Why: recent events for this provider — scan outcomes before opening raw caches.`;
  }
  return `What: ${title}.`;
}

export const TERMINAL_DASHBOARD_CMD =
  'cargo run -p wordkeep --features dashboard -- dashboard';

export const WIKI_SERVE_CMD =
  'cargo run -p wordkeep-wiki -- --root ../.. serve --watch';

export type KindFilterOption = { value: string; label: string; tip: string };

export const KIND_FILTER_OPTIONS: KindFilterOption[] = [
  { value: "", label: "All kinds", tip: "Do not filter by kind \u2014 search docs, rules, notes, and lore together." },
  { value: "doc", label: "Doc", tip: "Design / reference Markdown (typically under docs/)." },
  { value: "rule", label: "Rule", tip: "Cursor / agent rule files (.cursor/rules, directives)." },
  { value: "note", label: "Note", tip: "Working notes and knowledge entries (.wordkeep/notes, \u2026)." },
  { value: "lore", label: "Lore", tip: "Story / worldbuilding lore Markdown." },
];

export function kindFilterOption(value: string): KindFilterOption {
  return KIND_FILTER_OPTIONS.find((o) => o.value === value) || KIND_FILTER_OPTIONS[0]!;
}

