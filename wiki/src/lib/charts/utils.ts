export type BarDatum = {
  label: string;
  value: number;
  color?: string;
  /** Hover tip for the whole row (tool description + metric). */
  title?: string;
};

export type DonutDatum = {
  label: string;
  value: number;
  color: string;
  /** Hover tip for slice + legend row. */
  title?: string;
};

export type SparkPoint = {
  ts: number;
  value: number;
  /** Optional series label (e.g. MCP tool name) for hover tips. */
  label?: string;
};

export function formatAgoShort(ts: number, nowSecs = Math.floor(Date.now() / 1000)): string {
  const secs = Math.max(0, nowSecs - ts);
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h`;
  return `${Math.floor(secs / 86400)}d`;
}

export type PlotPoint = { x: number; y: number; value: number; ts: number; label?: string };

/** Map series into a plot box with axis margins. */
export function plotSeries(
  points: SparkPoint[],
  width: number,
  height: number,
  margin: { left: number; right: number; top: number; bottom: number },
): { plotted: PlotPoint[]; min: number; max: number; polyline: string } {
  const ordered = [...points]
    .filter((p) => Number.isFinite(p.ts) && Number.isFinite(p.value))
    .sort((a, b) => a.ts - b.ts);
  if (ordered.length === 0) {
    return { plotted: [], min: 0, max: 0, polyline: '' };
  }
  const values = ordered.map((p) => p.value);
  const min = Math.min(...values);
  const max = Math.max(...values);
  const span = max - min || 1;
  const innerW = Math.max(1, width - margin.left - margin.right);
  const innerH = Math.max(1, height - margin.top - margin.bottom);
  const plotted: PlotPoint[] = ordered.map((p, i) => {
    const x =
      margin.left +
      (ordered.length === 1 ? innerW / 2 : (i / (ordered.length - 1)) * innerW);
    const y = margin.top + innerH - ((p.value - min) / span) * innerH;
    return { x, y, value: p.value, ts: p.ts, label: p.label };
  });
  return {
    plotted,
    min,
    max,
    polyline: plotted.map((p) => `${p.x.toFixed(2)},${p.y.toFixed(2)}`).join(' '),
  };
}

export function maxValue(items: { value: number }[]): number {
  let m = 0;
  for (const item of items) {
    if (item.value > m) m = item.value;
  }
  return m;
}

/** Keep top `limit` bars; fold the rest into a trailing "other" slice. */
export function topBars(items: BarDatum[], limit = 8): BarDatum[] {
  const sorted = [...items]
    .filter((item) => item.value > 0)
    .sort((a, b) => b.value - a.value || a.label.localeCompare(b.label));
  if (sorted.length <= limit) return sorted;
  const head = sorted.slice(0, limit - 1);
  const rest = sorted.slice(limit - 1);
  const other = rest.reduce((sum, item) => sum + item.value, 0);
  return [
    ...head,
    {
      label: 'other',
      value: other,
      color: 'var(--muted)',
      title: `other — ${rest.length} tools outside the top ${limit - 1} by this metric (combined ${other})`,
    },
  ];
}

export function formatCompact(n: number): string {
  if (!Number.isFinite(n)) return '0';
  const abs = Math.abs(n);
  if (abs >= 1_000_000_000_000) return `${(n / 1_000_000_000_000).toFixed(2)}T`;
  if (abs >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(2)}B`;
  if (abs >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M`;
  if (abs >= 10_000) return `${Math.round(n / 1000)}k`;
  return String(Math.round(n));
}

/** Build SVG donut/pie arc path (outer ring with optional inner hole). */
export function donutSlicePath(
  cx: number,
  cy: number,
  outerR: number,
  innerR: number,
  startAngle: number,
  endAngle: number,
): string {
  const large = endAngle - startAngle > Math.PI ? 1 : 0;
  const ox1 = cx + outerR * Math.cos(startAngle);
  const oy1 = cy + outerR * Math.sin(startAngle);
  const ox2 = cx + outerR * Math.cos(endAngle);
  const oy2 = cy + outerR * Math.sin(endAngle);
  if (innerR <= 0) {
    return [
      `M ${cx} ${cy}`,
      `L ${ox1} ${oy1}`,
      `A ${outerR} ${outerR} 0 ${large} 1 ${ox2} ${oy2}`,
      'Z',
    ].join(' ');
  }
  const ix1 = cx + innerR * Math.cos(endAngle);
  const iy1 = cy + innerR * Math.sin(endAngle);
  const ix2 = cx + innerR * Math.cos(startAngle);
  const iy2 = cy + innerR * Math.sin(startAngle);
  return [
    `M ${ox1} ${oy1}`,
    `A ${outerR} ${outerR} 0 ${large} 1 ${ox2} ${oy2}`,
    `L ${ix1} ${iy1}`,
    `A ${innerR} ${innerR} 0 ${large} 0 ${ix2} ${iy2}`,
    'Z',
  ].join(' ');
}
