/** Short help strings for wiki menuing + MCP telemetry UI. */

export const TOOL_HELP: Record<string, string> = {
  // Navigation / symbols
  repo_map:
    'Files-first (or symbols) map of a path tree; pages via continuation when truncated.',
  outline: 'Symbol TOC with line numbers for one file or a small files[] batch (pages via continuation).',
  symbol_resolve:
    'Normalize a messy name and locate it; fuzzy/profile did-you-mean on miss.',
  symbol_refs: 'Definitions, calls, and references for a symbol (pages via continuation).',
  call_graph: 'One hop of callers and callees.',
  call_path: 'Shortest call chain between two symbols.',
  include_graph: 'One hop of #include includers/includees.',
  type_layout: 'Struct/class fields and non-POD flags.',
  doc_comment: 'Leading doc comment plus signature.',
  symbol_context: 'Body + callers/callees + layout; pages callers via continuation.',
  batch_context: 'Condensed symbol_context for many symbols under one budget.',
  usage_examples: 'Call sites with surrounding context.',

  // Change analysis
  symbol_diff: 'How one symbol changed vs a git ref.',
  diff_map: 'Symbols changed in a diff plus immediate callers.',
  module_map: 'Cross-module call coupling map (pages via continuation).',
  dead_code: 'Symbols with zero callers/refs.',
  big_functions: 'Largest functions by line span (pages via continuation).',
  undocumented: 'Exported symbols missing doc comments.',
  test_map: 'Test files that reference a symbol.',
  test_impact: 'Changed symbols × ranked test files from a git diff.',
  commit_scope: 'Dirty-path groups for a reviewable commit (read-only).',
  profile_upsert: 'Propose or persist a path_profiles entry in config.',

  // Knowledge
  knowledge_search: 'BM25 search over docs/rules/notes (pages via continuation).',
  knowledge_answer: 'Extractive answer + citations from the same BM25 index.',
  knowledge_upsert: 'Write or update a markdown knowledge section.',

  // Perf / runtime
  trace_summary: 'Hottest Tracy zones (or vs a baseline).',
  trace_profile: 'Hitch workflow: trace + diff_map + index_stale.',
  perf_triage: 'Tracy profile + hotspot context + tests in one bundle.',
  runtime_snapshot: 'Latest Runtime Memory Health census (or a capture id).',
  memory_diff: 'Signed deltas between two Runtime Health captures.',
  locality_hotspots: 'Sampled PMC/ETW/perf address hotspots (or unavailable).',
  integration_hooks: 'Curated cross-subsystem wiring (AND then OR match).',
  index_stale: 'Whether disk indexes may lag git or miss coverage.',
  index_health: 'Path-profile coverage gaps + proactive stale signal.',
  stats: 'Estimated context avoided / token displacement telemetry.',

  // Continuity / defects / runs
  run_record: 'Metadata-only gate/run write (never executes commands).',
  run_history: 'Recent runs; flags missing logs/artifacts.',
  artifact_index: 'Artifact metadata index (no image grading).',
  session_pressure:
    'Heuristic context-pressure level; autopilot handoff draft when high.',
  defect_upsert: 'Create or update a structured defect.',
  defect_list: 'Defect digest by default (counts + top); full list on demand.',
  mas_post: 'Append a compact multi-agent session entry.',
  mas_read: 'Read MAS entries (filter by recipient/role/round).',
  mas_status: 'MAS round bookkeeping and convergence hint.',
  mas_finalize: 'Close a MAS session; optional promote + handoff.',
  session_handoff: 'Paste-ready next-session prime.',

  // Izakaya presence
  izakaya_status: 'Who holds live work leases and advisory claims.',
  izakaya_check_in:
    'Take or resume a lease before the first live edit; claims are advisory.',
  izakaya_update: 'Heartbeat, move state, or change claims on an open lease.',
  izakaya_check_out: 'Release a lease; optional handoff capsule for the next agent.',
  izakaya_advise: 'Read promoted advice (does not assign or check out).',
};

export function toolHelp(name: string): string {
  return TOOL_HELP[name] || `MCP tool “${name}” — see docs/tools.md.`;
}

export const METRIC_HELP = {
  calls: 'Total MCP tool invocations recorded in savings.json.',
  distilled:
    'Estimated tokens the agent would have spent reading raw material (baseline).',
  returned: 'Tokens actually returned in distilled MCP answers.',
  saved: 'Distilled − returned: estimated context avoided.',
  session:
    'Saved tokens since this browser tab first opened the Dashboard (session baseline).',
  tracking: 'How long cumulative telemetry has been collecting on disk.',
} as const;

export const COLUMN_HELP = {
  name: 'MCP tool name. Hover a row for what the tool does.',
  calls: 'Lifetime call count for this tool.',
  avg_ms: 'Average latency per call (milliseconds).',
  trunc: 'Calls whose answer was truncated to the token budget.',
  err: 'Hard failures (IO/panic-class). Validation/missing-arg calls count as Inv instead.',
  inv: 'Invalid calls: missing required args, bad modes, or other agent misuse.',
  distill: 'Cumulative baseline (distilled) tokens for this tool.',
  return: 'Cumulative tokens returned by this tool.',
  saved: 'Cumulative tokens avoided (distill − return). Click to sort.',
  pct: 'Reduction percentage: saved / distill.',
  peak: 'Largest single-call save watermark for this tool.',
  last: 'Time since the most recent call.',
} as const;

/** Dashboard chart cards + section chrome. */
export const CHART_HELP = {
  section:
    'Live visualizations from the same /api/dashboard poll as the tables (refreshes every 2s while this tab is open). Collapse to focus on tables.',
  saved:
    'Per-tool cumulative tokens avoided (distill − return). Longer bar = more context saved. Top tools shown; remainder folded into “other”. Red bars mean net-negative distill.',
  calls:
    'Per-tool lifetime MCP call count. Shows which tools dominate agent traffic. Top tools shown; remainder folded into “other”.',
  outcomes:
    'Share of all recorded calls by outcome: ok (clean), trunc (hit token budget), error (hard failure), invalid (bad args/misuse), low-yield (thin/unhelpful reply). Hover a slice or legend row for count, %, and meaning; center switches to that bucket.',
  spark:
    'Tokens saved on each recent MCP call (ring buffer, oldest → newest). Axes show value × relative age; hover the line for the value, tool, and age at that point. Meta: n=events, last=newest save, peak=max in the window.',
  overview:
    'Lifetime MCP call and token totals from ~/.cache/wordkeep/savings.json (same source as the terminal dashboard).',
  activity:
    'Most recent MCP tool calls (newest first). Outcome chips flag truncated / error / low-yield / invalid replies.',
  signals:
    'Watermarks and risk signals: peak saves, slow tools, low-yield rate, and net-negative distill.',
  tools:
    'Per-tool lifetime stats. Click a column header to sort; hover a tool name for what it does.',
} as const;

export const OUTCOME_HELP: Record<string, string> = {
  ok: 'Clean call — answer returned within budget without hard failure.',
  trunc: 'Answer was truncated to fit the token budget.',
  error: 'Hard failure (IO, panic-class, or tool abort).',
  invalid: 'Invalid call: missing required args, bad mode, or other agent misuse.',
  'low-yield': 'Thin or unhelpful reply relative to the distilled baseline.',
  other: 'Remaining tools outside the top-N chart slots, aggregated together.',
};

export const TERMINAL_DASHBOARD_CMD =
  'cargo run -p wordkeep --features dashboard -- dashboard';

export const WIKI_SERVE_CMD =
  'cargo run -p wordkeep-wiki -- --root ../.. serve --watch';
