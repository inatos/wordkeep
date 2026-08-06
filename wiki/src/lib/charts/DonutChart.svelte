<script lang="ts">
  import { donutSlicePath, formatCompact, type DonutDatum } from './utils';

  let {
    data = [],
    empty = 'No data yet.',
    size = 120,
    ariaLabel = 'Donut chart',
    chartTitle = undefined,
    centerLabel = 'calls',
    centerTitle = 'Total calls across all outcome buckets',
    valueLabel = 'calls',
  }: {
    data?: DonutDatum[];
    empty?: string;
    size?: number;
    ariaLabel?: string;
    /** Tip on the chart root (what the mix measures). */
    chartTitle?: string;
    centerLabel?: string;
    centerTitle?: string;
    /** Noun used in hover metrics, e.g. "calls". */
    valueLabel?: string;
  } = $props();

  let hoverIdx = $state<number | null>(null);

  const cx = $derived(size / 2);
  const cy = $derived(size / 2);
  const outerR = $derived(size * 0.42);
  const hoverOuterR = $derived(size * 0.46);
  const innerR = $derived(size * 0.24);

  type Slice = {
    datum: DonutDatum;
    pct: number;
    path: string;
    hoverPath: string;
    midAngle: number;
  };

  let slices = $derived.by((): Slice[] => {
    const items = data.filter((d) => d.value > 0);
    const total = items.reduce((sum, d) => sum + d.value, 0);
    if (total <= 0) return [];
    // Start at 12 o'clock.
    let angle = -Math.PI / 2;
    return items.map((datum) => {
      const sweep = (datum.value / total) * Math.PI * 2;
      const start = angle;
      const end = angle + sweep;
      angle = end;
      // Full circle: SVG arc of 360° is degenerate — nudge end slightly.
      const safeEnd = sweep >= Math.PI * 2 - 1e-6 ? start + Math.PI * 2 - 0.001 : end;
      const midAngle = start + (safeEnd - start) / 2;
      return {
        datum,
        pct: Math.round((datum.value / total) * 100),
        path: donutSlicePath(cx, cy, outerR, innerR, start, safeEnd),
        hoverPath: donutSlicePath(cx, cy, hoverOuterR, innerR, start, safeEnd),
        midAngle,
      };
    });
  });

  let total = $derived(data.reduce((sum, d) => sum + (d.value > 0 ? d.value : 0), 0));
  let hover = $derived(hoverIdx != null ? slices[hoverIdx] ?? null : null);

  let displayValue = $derived(hover ? hover.datum.value : total);
  let displayLabel = $derived(hover ? hover.datum.label : centerLabel);
  let displayTitle = $derived(
    hover
      ? `${hover.datum.title || hover.datum.label}\n${formatCompact(hover.datum.value)} ${valueLabel} · ${hover.pct}%`
      : `${centerTitle}: ${formatCompact(total)}`,
  );

  function tipStyle(slice: Slice): string {
    // Place tip near the slice midpoint, clamped inside the card.
    const r = size * 0.34;
    const x = cx + Math.cos(slice.midAngle) * r;
    const y = cy + Math.sin(slice.midAngle) * r;
    const left = Math.min(82, Math.max(8, (x / size) * 100));
    const top = Math.min(72, Math.max(8, (y / size) * 100));
    return `left:${left}%; top:${top}%`;
  }
</script>

{#if slices.length === 0}
  <p class="muted empty" title={chartTitle}>{empty}</p>
{:else}
  <div class="donut" title={chartTitle}>
    <div class="plot-wrap">
      <svg
        role="img"
        aria-label={ariaLabel}
        viewBox={`0 0 ${size} ${size}`}
        width={size}
        height={size}
        onpointerleave={() => (hoverIdx = null)}
      >
        {#each slices as slice, i}
          <path
            class="slice"
            class:active={hoverIdx === i}
            class:dimmed={hoverIdx != null && hoverIdx !== i}
            d={hoverIdx === i ? slice.hoverPath : slice.path}
            fill={slice.datum.color}
            stroke="var(--bg-elevated)"
            stroke-width="1.5"
            onpointerenter={() => (hoverIdx = i)}
            onpointermove={() => (hoverIdx = i)}
            onfocus={() => (hoverIdx = i)}
            onblur={() => {
              if (hoverIdx === i) hoverIdx = null;
            }}
            tabindex="0"
            role="button"
            aria-label={`${slice.datum.label}: ${formatCompact(slice.datum.value)} ${valueLabel}, ${slice.pct}%`}
          ></path>
        {/each}
        <g class="center" style="pointer-events: none">
          <title>{displayTitle}</title>
          <text x={cx} y={cy - 4} text-anchor="middle" class="center-value"
            >{formatCompact(displayValue)}</text
          >
          <text x={cx} y={cy + 12} text-anchor="middle" class="center-label">{displayLabel}</text>
        </g>
      </svg>

      {#if hover}
        <div class="tip" style={tipStyle(hover)} role="tooltip">
          <strong style={`color:${hover.datum.color}`}>{hover.datum.label}</strong>
          <span class="metric"
            >{formatCompact(hover.datum.value)} {valueLabel} · {hover.pct}%</span
          >
          {#if hover.datum.title}
            <span class="detail">{hover.datum.title}</span>
          {/if}
          <span class="share"
            >{formatCompact(hover.datum.value)} of {formatCompact(total)} total</span
          >
        </div>
      {/if}
    </div>

    <ul class="legend">
      {#each slices as slice, i}
        <li
          class:active={hoverIdx === i}
          class:dimmed={hoverIdx != null && hoverIdx !== i}
          title={slice.datum.title || slice.datum.label}
          onpointerenter={() => (hoverIdx = i)}
          onpointerleave={() => (hoverIdx = null)}
        >
          <span class="swatch" style={`background:${slice.datum.color}`}></span>
          <span class="name">{slice.datum.label}</span>
          <span class="pct">{slice.pct}%</span>
          <span class="count muted">{formatCompact(slice.datum.value)}</span>
        </li>
      {/each}
    </ul>
  </div>
{/if}

<style>
  .empty {
    margin: 0.4rem 0;
    font-size: 0.82rem;
  }
  .donut {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.75rem 1rem;
  }
  .plot-wrap {
    position: relative;
    flex: 0 0 auto;
  }
  svg {
    display: block;
    cursor: default;
  }
  .slice {
    cursor: pointer;
    outline: none;
    transition:
      opacity 0.14s var(--ease-snap),
      filter 0.14s var(--ease-snap);
  }
  .slice:focus-visible {
    filter: brightness(1.15);
  }
  .slice.dimmed {
    opacity: 0.38;
  }
  .slice.active {
    filter: brightness(1.08);
  }
  .center-value {
    fill: var(--text);
    font-size: 0.85rem;
    font-weight: 700;
    font-family: "IBM Plex Mono", ui-monospace, monospace;
  }
  .center-label {
    fill: var(--muted);
    font-size: 0.55rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .tip {
    position: absolute;
    z-index: 2;
    transform: translate(-50%, -110%);
    display: grid;
    gap: 0.12rem;
    padding: 0.4rem 0.55rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: rgba(26, 20, 28, 0.95);
    box-shadow: 0 6px 18px rgba(0, 0, 0, 0.35);
    font-size: 0.72rem;
    line-height: 1.25;
    pointer-events: none;
    min-width: 7rem;
    max-width: 12rem;
  }
  .tip strong {
    font-family: "IBM Plex Mono", ui-monospace, monospace;
    font-size: 0.82rem;
    text-transform: lowercase;
  }
  .tip .metric {
    color: var(--text);
    font-variant-numeric: tabular-nums;
    font-family: "IBM Plex Mono", ui-monospace, monospace;
  }
  .tip .detail,
  .tip .share {
    color: var(--muted);
  }
  .legend {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 0.28rem;
    font-size: 0.78rem;
    min-width: 7.5rem;
  }
  .legend li {
    display: grid;
    grid-template-columns: 0.65rem 1fr auto auto;
    align-items: center;
    gap: 0.4rem;
    padding: 0.12rem 0.2rem;
    border-radius: 4px;
    cursor: pointer;
    transition:
      background 0.14s var(--ease-snap),
      opacity 0.14s var(--ease-snap);
  }
  .legend li:hover,
  .legend li.active {
    background: var(--accent-soft);
  }
  .legend li.dimmed {
    opacity: 0.45;
  }
  .swatch {
    width: 0.55rem;
    height: 0.55rem;
    border-radius: 2px;
  }
  .name {
    color: var(--muted);
  }
  .pct,
  .count {
    font-variant-numeric: tabular-nums;
  }
  .count {
    min-width: 2.2rem;
    text-align: right;
  }
</style>
