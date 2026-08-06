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

async function sendJson<T>(url: string, method: 'POST' | 'PUT', body: unknown): Promise<T> {
  const res = await fetch(url, {
    method,
    headers: { 'Content-Type': 'application/json' },
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
  }>(`/api/search?${params}`);
}

export function fetchPage(path: string) {
  return getJson<PagePayload>(`/api/page?${new URLSearchParams({ path })}`);
}

export function savePage(path: string, markdown: string) {
  return sendJson<PagePayload>('/api/page', 'PUT', { path, markdown });
}

export function fetchTree() {
  return getJson<TreePayload | TreeNode[]>(`/api/tree`);
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
  }>(`/api/backlinks?${new URLSearchParams({ path })}`);
}

export function fetchRecent(limit = 20) {
  return getJson<{ files?: RecentFile[] }>(`/api/recent?${new URLSearchParams({ limit: String(limit) })}`);
}

export function fetchGarden() {
  return getJson<GardenPayload>('/api/garden');
}

export function fetchHealth() {
  return getJson<HealthPayload>('/api/health');
}

export function fetchStats() {
  return getJson<StatsPayload>('/api/stats');
}

export function fetchDashboard() {
  return getJson<DashboardPayload>('/api/dashboard');
}

export function fetchSearchTelemetry() {
  return getJson<SearchTelemetry>('/api/search-telemetry');
}

export function recordClick(rank: number) {
  return postJson<{ ok: boolean }>(`/api/search-telemetry/click`, { rank });
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
