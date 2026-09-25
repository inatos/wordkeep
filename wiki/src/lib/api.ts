export type SearchHit = {
  id?: string;
  path: string;
  heading?: string;
  heading_path?: string[];
  anchor?: string;
  kind?: string;
  root?: string;
  tags?: string[];
  start_line?: number;
  body?: string;
  content?: string;
  _formatted?: Record<string, string>;
};

export type OutlineSection = {
  heading: string;
  heading_level: number;
  heading_path?: string[];
  anchor: string;
  html_id: string;
  ordinal?: number;
  start_line?: number;
  end_line?: number;
};

export type PagePayload = {
  path: string;
  markdown?: string;
  html: string;
  outline?: OutlineSection[];
  tags?: string[];
};

export type TreeFile = {
  path: string;
  root?: string;
  kind?: string;
};

export type TreeNode = {
  name: string;
  path?: string;
  kind?: string;
  children?: TreeNode[];
};

export type TreePayload = {
  files?: TreeFile[];
  roots?: TreeNode[];
};

export type HealthPayload = {
  status: string;
  project_name?: string;
  read_only?: boolean;
  meilisearch?: unknown;
  index?: unknown;
  manifest?: { files?: number; chunks?: number };
  search_telemetry?: SearchTelemetry;
  retain_search_queries?: boolean;
  error?: string;
};

export type SearchTelemetry = {
  available?: boolean;
  searches?: number;
  no_results?: number;
  no_result_rate?: number;
  clicks?: number;
  avg_click_rank?: number | null;
  avg_latency_ms?: number;
  recent?: unknown[];
  retain_queries_note?: string;
};

export type GardenPayload = {
  files?: number;
  broken_links?: { source: string; heading?: string; raw?: string; target?: string }[];
  broken_count?: number;
  orphans?: { path: string; kind?: string }[];
  orphan_count?: number;
  duplicate_headings?: { path: string; heading?: string; anchor?: string; html_id?: string }[];
  duplicate_heading_count?: number;
  tags?: Record<string, number>;
};

export type RecentFile = {
  path: string;
  root?: string;
  kind?: string;
  mtime_ns?: number;
};

export type StatsPayload = Record<string, unknown>;

export type DashboardTool = {
  name: string;
  calls: number;
  calls_fmt?: string;
  avg_ms: number;
  trunc_count: number;
  error_count: number;
  invalid_count?: number;
  low_yield_count?: number;
  baseline_tokens: number;
  baseline_fmt?: string;
  returned_tokens: number;
  returned_fmt?: string;
  saved: number;
  saved_fmt?: string;
  reduction_pct: number;
  bar?: string;
  inverted?: boolean;
  peak_saved?: number;
  peak_saved_fmt?: string;
  peak_ms?: number;
  last_ts?: number;
  last_ago?: string;
};

export type DashboardActivity = {
  ts?: number;
  ago: string;
  tool: string;
  elapsed_ms: number;
  baseline: number;
  returned: number;
  saved: number;
  outcome: string;
  reason?: string | null;
  baseline_fmt?: string;
  returned_fmt?: string;
  saved_fmt?: string;
};

export type DashboardSignal = {
  kind: string;
  label: string;
  value: string | number | null;
  detail?: string | null;
};

export type DashboardPayload = {
  available: boolean;
  message?: string;
  terminal_hint?: string;
  overview?: {
    calls: number;
    calls_fmt?: string;
    baseline_tokens: number;
    baseline_fmt?: string;
    returned_tokens: number;
    returned_fmt?: string;
    saved_tokens: number;
    saved_fmt?: string;
    reduction_pct: number;
    since_label?: string;
  };
  tools?: DashboardTool[];
  activity?: DashboardActivity[];
  health?: { signals?: DashboardSignal[] };
};

const API_BASE = `${import.meta.env.BASE_URL}api`;

async function parseJsonResponse<T>(res: Response, url: string): Promise<T> {
  const text = await res.text();
  if (!text) {
    throw new Error(
      res.ok
        ? `Empty response from ${url}`
        : `HTTP ${res.status} empty body from ${url} (rebuild/restart wordkeep-wiki if /api/dashboard is missing)`,
    );
  }
  let data: unknown;
  try {
    data = JSON.parse(text);
  } catch {
    throw new Error(`Non-JSON response from ${url} (HTTP ${res.status})`);
  }
  if (!res.ok) {
    throw new Error((data as { error?: string }).error || `HTTP ${res.status}`);
  }
  return data as T;
}

async function getJson<T>(url: string): Promise<T> {
  const res = await fetch(url);
  return parseJsonResponse<T>(res, url);
}

async function sendJson<T>(
  url: string,
  method: 'POST' | 'PUT',
  body: unknown,
  extraHeaders: Record<string, string> = {},
): Promise<T> {
  const res = await fetch(url, {
    method,
    headers: { 'Content-Type': 'application/json', ...extraHeaders },
    body: JSON.stringify(body),
  });
  return parseJsonResponse<T>(res, url);
}

async function postJson<T>(url: string, body: unknown): Promise<T> {
  return sendJson<T>(url, 'POST', body);
}

export function search(
  q: string,
  opts: { kind?: string; root?: string; tag?: string; limit?: number } = {},
) {
  const params = new URLSearchParams({ q });
  if (opts.kind) params.set('kind', opts.kind);
  if (opts.root) params.set('root', opts.root);
  if (opts.tag) params.set('tag', opts.tag);
  if (opts.limit) params.set('limit', String(opts.limit));
  return getJson<{
    hits?: SearchHit[];
    estimatedTotalHits?: number;
    processingTimeMs?: number;
    clientLatencyMs?: number;
  }>(`${API_BASE}/search?${params}`);
}

export function fetchPage(path: string) {
  return getJson<PagePayload>(`${API_BASE}/page?${new URLSearchParams({ path })}`);
}

export function savePage(path: string, markdown: string) {
  return sendJson<PagePayload>(`${API_BASE}/page`, 'PUT', { path, markdown });
}

export function fetchTree() {
  return getJson<TreePayload | TreeNode[]>(`${API_BASE}/tree`);
}

/** Normalize /api/tree payloads into a hierarchical sidebar tree. */
export function normalizeTree(payload: TreePayload | TreeNode[]): TreeNode[] {
  if (Array.isArray(payload)) return payload;
  if (payload.roots?.length) return payload.roots;
  return buildTreeFromFiles(payload.files ?? []);
}

export function buildTreeFromFiles(files: TreeFile[]): TreeNode[] {
  type MutableNode = TreeNode & { children: MutableNode[]; byName: Map<string, MutableNode> };
  const roots: MutableNode[] = [];
  const rootByName = new Map<string, MutableNode>();

  const ensureDir = (parent: Map<string, MutableNode>, list: MutableNode[], name: string) => {
    let node = parent.get(name);
    if (!node) {
      node = { name, children: [], byName: new Map() };
      parent.set(name, node);
      list.push(node);
    }
    return node;
  };

  const sorted = [...files].sort((a, b) => a.path.localeCompare(b.path));
  for (const file of sorted) {
    const parts = file.path.split('/').filter(Boolean);
    if (parts.length === 0) continue;
    let list = roots;
    let map = rootByName;
    for (let i = 0; i < parts.length - 1; i++) {
      const dir = ensureDir(map, list, parts[i]!);
      list = dir.children;
      map = dir.byName;
    }
    const leafName = parts[parts.length - 1]!;
    list.push({
      name: leafName,
      path: file.path,
      kind: file.kind,
      children: [],
      byName: new Map(),
    });
  }

  const strip = (nodes: MutableNode[]): TreeNode[] =>
    nodes.map(({ byName: _byName, children, ...rest }) => ({
      ...rest,
      children: children.length ? strip(children) : undefined,
    }));

  return strip(roots);
}

export function fetchBacklinks(path: string) {
  return getJson<{
    backlinks?: { path: string; heading?: string }[];
    links?: { path: string; heading?: string }[];
  }>(`${API_BASE}/backlinks?${new URLSearchParams({ path })}`);
}

export function fetchRecent(limit = 20) {
  return getJson<{ files?: RecentFile[] }>(`${API_BASE}/recent?${new URLSearchParams({ limit: String(limit) })}`);
}

export function fetchGarden() {
  return getJson<GardenPayload>(`${API_BASE}/garden`);
}

export function fetchHealth() {
  return getJson<HealthPayload>(`${API_BASE}/health`);
}

export function fetchStats() {
  return getJson<StatsPayload>(`${API_BASE}/stats`);
}

export function fetchDashboard() {
  return getJson<DashboardPayload>(`${API_BASE}/dashboard`);
}

export type IzakayaClaim = { path: string; symbols?: string[]; intent?: string };

export type IzakayaAgent = {
  agent_id: string;
  state: string;
  stale?: boolean;
  role?: string;
  task?: string;
  summary?: string;
  branch?: string;
  head?: string;
  worktree?: string;
  dirty_count?: number;
  claims?: IzakayaClaim[];
  blockers?: string[];
  checkpoint?: string | null;
  checkout_reason?: string | null;
  mas_session?: string | null;
  revision?: number;
  checked_in_at?: number;
  last_seen_at?: number;
  expires_at?: number;
};

export type IzakayaHandoff = {
  id: string;
  from: string;
  to: string;
  status: string;
  derived_status?: string;
  summary?: string;
  mas_session?: string | null;
  checkpoint?: string | null;
  created_at?: number;
  accepted_by?: string | null;
};

export type IzakayaMessage = {
  seq: number;
  from: string;
  to: string;
  body: string;
  acked?: boolean;
};

export type IzakayaEvent = {
  seq: number;
  kind: string;
  agent_id: string;
  ts: number;
  result?: string;
};

export type IzakayaFinding = { kind: string; detail: string };

export type IzakayaPayload = {
  available: boolean;
  empty?: boolean;
  message?: string;
  coordination_id?: string;
  seq?: number;
  journal_seq?: number;
  updated_at?: number;
  now?: number;
  active_policy?: string | null;
  malformed?: number;
  projection_behind?: boolean;
  counts?: {
    live_code: number;
    checked_in: number;
    suspended: number;
    checked_out: number;
    stale: number;
    handoffs_open: number;
  };
  agents?: IzakayaAgent[];
  handoffs?: IzakayaHandoff[];
  messages?: IzakayaMessage[];
  events?: IzakayaEvent[];
  findings?: IzakayaFinding[];
};

export function fetchIzakaya() {
  return getJson<IzakayaPayload>(`${API_BASE}/izakaya`);
}

export type RuntimeRegion = {
  start?: string;
  end?: string;
  start_u64?: number;
  end_u64?: number;
  size?: number;
  kind?: string;
  path?: string | null;
  perm?: string;
  rss_bytes?: number;
};

export type RuntimeSnapshot = {
  available?: boolean;
  message?: string;
  token?: string;
  schema_version?: number;
  seq?: number;
  dropped?: number;
  source?: string;
  os?: string;
  pid?: number;
  name?: string;
  rss_bytes?: number | null;
  peak_rss_bytes?: number | null;
  committed_va_bytes?: number;
  quality?: Record<string, string>;
  capabilities?: Record<string, boolean>;
  attach_enabled?: boolean;
  cooperative?: boolean;
  capturing?: boolean;
  capture_id?: string;
  capture_truncated?: boolean;
  regions?: RuntimeRegion[];
  address_map?: {
    projection?: string;
    area_scale?: string;
    source_segments?: number;
    segments?: RuntimeRegion[];
  };
  kinds?: Record<string, number>;
  numa?: {
    quality?: string;
    dimm_quality?: string;
    detail?: string;
    dimms?: { label?: string; size_hint?: number; memory_controller?: string }[];
    nodes?: {
      id: number;
      cpulist?: string;
      distance?: string;
      mem_total_bytes?: number | null;
      mem_free_bytes?: number | null;
      dimms?: unknown[];
    }[];
  };
  pools?: { name: string; in_use: number; cap: number; quality?: string }[];
  flecs?: Record<string, number>;
  jolt?: Record<string, number>;
  frame?: Record<string, number | object>;
  hints?: { kind?: string; label?: string; detail?: string }[];
  timeline?: {
    seq?: number;
    ts_ns?: number;
    rss_bytes?: number | null;
    committed_va_bytes?: number | null;
    flecs_unused_bytes?: number | null;
    jolt_temp_used?: number | null;
    presented_ms?: number | null;
    hitch?: boolean;
  }[];
  lifetime?: {
    quality?: string;
    live_allocations?: number;
    live_bytes?: number;
    oldest_ms?: number;
    age_buckets?: { label: string; bytes: number; count: number }[];
  };
  locality?: {
    quality?: string;
    source?: string;
    coverage_start_ns?: number;
    samples?: number;
    dropped?: number;
    page_size?: number;
    hotspots?: {
      addr?: string;
      end?: string;
      samples?: number;
      weight?: number;
      misses?: number;
      thread?: string;
      symbol?: string;
      data_source?: string;
    }[];
  };
  gpu?: {
    quality?: string;
    particles?: number;
    particle_cap?: number;
    particle_ssbo_bytes?: number;
    dynamic_meshes_live?: number;
    dynamic_meshes_free?: number;
    mesh_slots_remaining?: number;
    portal_rt_bytes?: number;
    driver_dedicated_kb?: number;
    driver_available_kb?: number;
    driver_quality?: string;
  };
  budget?: {
    name?: string;
    ok?: boolean;
    checked?: number;
    unavailable?: number;
    quality?: string;
    violations?: {
      metric?: string;
      limit?: number | null;
      actual?: number | null;
      delta?: number | null;
      quality?: string;
    }[];
  };
  alloc_streams?: {
    name?: string;
    live_bytes?: number;
    allocs?: number;
    frees?: number;
  }[];
};

export type RuntimeDelta = {
  schema_version?: number;
  seq?: number;
  changed?: Partial<RuntimeSnapshot> & { map_dirty?: boolean };
};

export type RuntimeCapture = {
  id: string;
  bytes?: number;
  samples?: number | null;
  started_ns?: number | null;
  ended_ns?: number | null;
  truncated?: boolean;
  capturing?: boolean;
};

export type RuntimeCaptures = {
  captures: RuntimeCapture[];
  max_captures?: number;
  max_samples?: number;
};

export type RuntimeDiffMetric = {
  base: number | null;
  current: number | null;
  delta: number | null;
  quality?: string;
};

export type RuntimeDiff = {
  base: string;
  current: string;
  quality?: string;
  metrics?: Record<string, RuntimeDiffMetric>;
  pools?: { name: string; base: number; current: number; delta: number }[];
  kinds?: { name: string; base: number; current: number; delta: number }[];
};

let runtimeToken = '';

function runtimeHeaders(): Record<string, string> {
  return runtimeToken ? { 'X-Wordkeep-Runtime': runtimeToken } : {};
}

export async function fetchRuntime() {
  const res = await fetch(`${API_BASE}/runtime`, { headers: runtimeHeaders() });
  const data = await parseJsonResponse<RuntimeSnapshot>(res, `${API_BASE}/runtime`);
  if (data.token) runtimeToken = data.token;
  return data;
}

export function openRuntimeStream(
  onSnapshot: (snapshot: RuntimeSnapshot) => void,
  onDelta: (delta: RuntimeDelta) => void,
  onOpen: () => void,
  onError: () => void,
) {
  const source = new EventSource(`${API_BASE}/runtime/stream`);
  source.addEventListener('snapshot', (event) => {
    try {
      const snapshot = JSON.parse((event as MessageEvent<string>).data) as RuntimeSnapshot;
      if (snapshot.token) runtimeToken = snapshot.token;
      onSnapshot(snapshot);
    } catch {
      onError();
    }
  });
  source.addEventListener('delta', (event) => {
    try {
      onDelta(JSON.parse((event as MessageEvent<string>).data) as RuntimeDelta);
    } catch {
      onError();
    }
  });
  source.onopen = onOpen;
  source.onerror = onError;
  return () => source.close();
}

export function postRuntime<T>(url: string, body: unknown) {
  return sendJson<T>(url, 'POST', body, runtimeHeaders());
}

export function fetchRuntimeCaptures() {
  return getJson<RuntimeCaptures>(`${API_BASE}/runtime/captures`);
}

export function fetchSearchTelemetry() {
  return getJson<SearchTelemetry>(`${API_BASE}/search-telemetry`);
}

export function recordClick(rank: number) {
  return postJson<{ ok: boolean }>(`${API_BASE}/search-telemetry/click`, { rank });
}

export function snippetOf(hit: SearchHit): string {
  const formatted = hit._formatted?.content || hit._formatted?.body;
  if (formatted) return formatted;
  const raw = hit.content || hit.body || '';
  return raw.length > 280 ? `${raw.slice(0, 280)}…` : raw;
}

export function htmlIdForAnchor(anchor?: string): string {
  return (anchor || '').replace(/#/g, '-');
}
