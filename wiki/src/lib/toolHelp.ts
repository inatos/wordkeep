/** Short help strings for wiki menuing + MCP telemetry UI. */

export const TOOL_HELP: Record<string, string> = {
  repo_map: 'Namespaces, types, and signatures across a path tree.',
  outline: 'Symbol TOC for one file with line numbers.',
  symbol_refs: 'Definitions, calls, and references for a symbol.',
  call_graph: 'One hop of callers and callees.',
  call_path: 'Shortest call chain between two symbols.',
  include_graph: 'One hop of #include includers/includees.',
  type_layout: 'Struct/class fields and non-POD flags.',
  doc_comment: 'Leading doc comment plus signature.',
  symbol_context: 'Body + callers/callees + layout in one pass.',
  usage_examples: 'Call sites with surrounding context.',
  symbol_diff: 'How one symbol changed vs a git ref.',
  diff_map: 'Symbols changed in a diff plus immediate callers.',
  module_map: 'Cross-module call coupling map.',
  dead_code: 'Symbols with zero callers/refs.',
  big_functions: 'Largest functions by line span.',
  undocumented: 'Exported symbols missing doc comments.',
  test_map: 'Test files that reference a symbol.',
  commit_scope: 'Dirty-path groups for a reviewable commit.',
  knowledge_search: 'BM25 search over docs/rules/notes.',
  knowledge_upsert: 'Write or update a markdown knowledge section.',
  trace_summary: 'Hottest Tracy zones (or vs a baseline).',
  trace_profile: 'Hitch workflow: trace + diff_map + index_stale.',
  integration_hooks: 'Curated cross-subsystem wiring + optional call_path.',
  index_stale: 'Whether disk indexes may lag git or miss coverage.',
  stats: 'Estimated context avoided / token displacement.',
  run_record: 'Metadata-only gate/run write.',
  run_history: 'Recent runs; flags missing logs/artifacts.',
  artifact_index: 'Artifact metadata index (no image grading).',
  session_pressure: 'Heuristic context-pressure level.',
  defect_upsert: 'Create or update a structured defect.',
  defect_list: 'Unresolved defects (eyeball_fail first).',
  mas_post: 'Append a compact multi-agent session entry.',
  mas_read: 'Read MAS entries (filter by recipient/role/round).',
  mas_status: 'MAS round bookkeeping and convergence hint.',
  mas_finalize: 'Close a MAS session; optional promote + handoff.',
  session_handoff: 'Paste-ready next-session prime.',
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
