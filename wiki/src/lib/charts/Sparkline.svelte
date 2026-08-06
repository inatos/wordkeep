<script lang="ts">
  import {
    formatAgoShort,
    formatCompact,
    plotSeries,
    type SparkPoint,
  } from './utils';

  let {
    data = [],
    empty = 'No recent events.',
    width = 320,
    height = 160,
    stroke = 'var(--ok)',
    ariaLabel = 'Line chart of recent values',
    chartTitle = undefined,
    valueLabel = 'value',
    yAxisLabel = 'saved',
    xAxisLabel = 'time →',
  }: {
    data?: SparkPoint[];
    empty?: string;
    width?: number;
    height?: number;
    stroke?: string;
    ariaLabel?: string;
    chartTitle?: string;
    valueLabel?: string;
    yAxisLabel?: string;
    xAxisLabel?: string;
  } = $props();

  const margin = { left: 44, right: 10, top: 12, bottom: 28 };

  let hoverIdx = $state<number | null>(null);
  let svgEl = $state<SVGSVGElement | null>(null);

  let plot = $derived(plotSeries(data, width, height, margin));
  let last = $derived(plot.plotted.length ? plot.plotted[plot.plotted.length - 1]!.value : 0);
  let peak = $derived(plot.plotted.length ? plot.max : 0);

  let yTicks = $derived.by(() => {
    if (plot.plotted.length === 0) return [] as { y: number; label: string }[];
    const { min, max } = plot;
    const mid = min + (max - min) / 2;
    const innerH = height - margin.top - margin.bottom;
    const at = (v: number) => {
      const span = max - min || 1;
      return margin.top + innerH - ((v - min) / span) * innerH;
    };
    const ticks = [
      { y: at(max), label: formatCompact(max) },
      { y: at(mid), label: formatCompact(mid) },
      { y: at(min), label: formatCompact(min) },
    ];
    // Collapse duplicate labels when flat series.
    if (max === min) return [ticks[0]!];
    return ticks;
  });

  let xTicks = $derived.by(() => {
    const pts = plot.plotted;
    if (pts.length === 0) return [] as { x: number; label: string }[];
    const first = pts[0]!;
    const lastPt = pts[pts.length - 1]!;
    if (pts.length === 1) {
      return [{ x: first.x, label: formatAgoShort(first.ts) }];
    }
    const mid = pts[Math.floor(pts.length / 2)]!;
    return [
      { x: first.x, label: formatAgoShort(first.ts) },
      { x: mid.x, label: formatAgoShort(mid.ts) },
      { x: lastPt.x, label: formatAgoShort(lastPt.ts) },
    ];
  });

  let hover = $derived(hoverIdx != null ? plot.plotted[hoverIdx] ?? null : null);

  function indexFromClientX(clientX: number): number | null {
    if (!svgEl || plot.plotted.length === 0) return null;
    const rect = svgEl.getBoundingClientRect();
    if (rect.width <= 0) return null;
    const xSvg = ((clientX - rect.left) / rect.width) * width;
    let best = 0;
    let bestDist = Infinity;
    for (let i = 0; i < plot.plotted.length; i++) {
      const d = Math.abs(plot.plotted[i]!.x - xSvg);
      if (d < bestDist) {
        bestDist = d;
        best = i;
      }
    }
    return best;
  }

  function onPointerMove(event: PointerEvent) {
    hoverIdx = indexFromClientX(event.clientX);
  }

  function onPointerLeave() {
    hoverIdx = null;
  }

  function tipLeft(x: number): number {
    // Keep tooltip inside the chart card.
    const pct = (x / width) * 100;
    return Math.min(78, Math.max(4, pct));
  }
</script>

{#if plot.plotted.length === 0}
  <p class="muted empty" title={chartTitle}>{empty}</p>
{:else}
  <div class="spark" title={chartTitle}>
    <div class="plot-wrap">
      <svg
        bind:this={svgEl}
        role="img"
        aria-label={ariaLabel}
        viewBox={`0 0 ${width} ${height}`}
        width="100%"
        height={height}
        preserveAspectRatio="xMidYMid meet"
        onpointermove={onPointerMove}
        onpointerleave={onPointerLeave}
        onpointerdown={onPointerMove}
      >
        <title
          >{chartTitle ||
            `${valueLabel} over ${plot.plotted.length} recent events (oldest → newest)`}</title
        >

        <!-- plot frame -->
        <rect
          class="frame"
          x={margin.left}
          y={margin.top}
          width={width - margin.left - margin.right}
          height={height - margin.top - margin.bottom}
        />

        <!-- Y grid + labels -->
        {#each yTicks as tick}
          <line
            class="grid"
            x1={margin.left}
            x2={width - margin.right}
            y1={tick.y}
            y2={tick.y}
          />
          <text class="tick y-tick" x={margin.left - 6} y={tick.y + 3} text-anchor="end"
            >{tick.label}</text
          >
        {/each}

        <!-- X axis line + labels -->
        <line
          class="axis"
          x1={margin.left}
          x2={width - margin.right}
          y1={height - margin.bottom}
          y2={height - margin.bottom}
        />
        <line
          class="axis"
          x1={margin.left}
          x2={margin.left}
          y1={margin.top}
          y2={height - margin.bottom}
        />
        {#each xTicks as tick, i}
          <text
            class="tick x-tick"
            x={tick.x}
            y={height - 8}
            text-anchor={i === 0 ? 'start' : i === xTicks.length - 1 ? 'end' : 'middle'}
            >{tick.label}</text
          >
        {/each}

        <text
          class="axis-label"
          x={margin.left - 36}
          y={(margin.top + height - margin.bottom) / 2}
          text-anchor="middle"
          transform={`rotate(-90 ${margin.left - 36} ${(margin.top + height - margin.bottom) / 2})`}
          >{yAxisLabel}</text
        >
        <text
          class="axis-label"
          x={(margin.left + width - margin.right) / 2}
          y={height - 1}
          text-anchor="middle">{xAxisLabel}</text
        >

        <polyline
          class="series"
          fill="none"
          stroke={stroke}
          stroke-width="2"
          stroke-linecap="round"
          stroke-linejoin="round"
          points={plot.polyline}
        />

        {#if hover}
          <line
            class="crosshair"
            x1={hover.x}
            x2={hover.x}
            y1={margin.top}
            y2={height - margin.bottom}
          />
          <circle class="dot" cx={hover.x} cy={hover.y} r="4" fill={stroke} />
        {/if}
      </svg>

      {#if hover}
        <div
          class="tip"
          style={`left:${tipLeft(hover.x)}%`}
          role="tooltip"
        >
          <strong>{formatCompact(hover.value)}</strong>
          <span>{valueLabel}</span>
          {#if hover.label}
            <span class="tool">{hover.label}</span>
          {/if}
          <span class="ago">{formatAgoShort(hover.ts)} ago</span>
        </div>
      {/if}
    </div>

    <div class="meta muted">
      <span title="Number of recent MCP events in this window">n={plot.plotted.length}</span>
      <span title={`Most recent event’s ${valueLabel}`}>last {formatCompact(last)}</span>
      <span title={`Highest ${valueLabel} in this window`}>peak {formatCompact(peak)}</span>
    </div>
  </div>
{/if}

<style>
  .empty {
    margin: 0.4rem 0;
    font-size: 0.82rem;
  }
  .spark {
    display: grid;
    gap: 0.35rem;
  }
  .plot-wrap {
    position: relative;
  }
  svg {
    display: block;
    width: 100%;
    background: rgba(0, 0, 0, 0.22);
    border: 1px solid var(--border);
    border-radius: 8px;
    cursor: crosshair;
    touch-action: none;
  }
  .frame {
    fill: transparent;
    stroke: var(--border);
    stroke-width: 1;
  }
  .grid {
    stroke: rgba(168, 148, 171, 0.18);
    stroke-width: 1;
    stroke-dasharray: 3 3;
  }
  .axis {
    stroke: rgba(168, 148, 171, 0.45);
    stroke-width: 1;
  }
  .tick {
    fill: var(--muted);
    font-size: 9px;
    font-family: "IBM Plex Mono", ui-monospace, monospace;
  }
  .axis-label {
    fill: var(--muted);
    font-size: 8px;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }
  .crosshair {
    stroke: rgba(240, 230, 242, 0.35);
    stroke-width: 1;
    stroke-dasharray: 2 3;
    pointer-events: none;
  }
  .dot {
    stroke: var(--bg-elevated);
    stroke-width: 1.5;
    pointer-events: none;
  }
  .tip {
    position: absolute;
    top: 0.45rem;
    transform: translateX(-50%);
    z-index: 2;
    display: grid;
    gap: 0.1rem;
    padding: 0.35rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: rgba(26, 20, 28, 0.94);
    box-shadow: 0 6px 18px rgba(0, 0, 0, 0.35);
    font-size: 0.72rem;
    line-height: 1.25;
    pointer-events: none;
    min-width: 5.5rem;
  }
  .tip strong {
    color: var(--ok);
    font-family: "IBM Plex Mono", ui-monospace, monospace;
    font-size: 0.85rem;
  }
  .tip .tool {
    color: var(--accent-strong);
    font-family: "IBM Plex Mono", ui-monospace, monospace;
  }
  .tip .ago,
  .tip span:not(.tool) {
    color: var(--muted);
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: 0.65rem;
    font-size: 0.72rem;
    font-variant-numeric: tabular-nums;
  }
</style>
