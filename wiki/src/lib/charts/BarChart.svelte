<script lang="ts">
  import { formatCompact, maxValue, topBars, type BarDatum } from './utils';

  let {
    data = [],
    limit = 8,
    empty = 'No data yet.',
    valueSuffix = '',
    ariaLabel = 'Bar chart',
    chartTitle = undefined,
  }: {
    data?: BarDatum[];
    limit?: number;
    empty?: string;
    valueSuffix?: string;
    ariaLabel?: string;
    /** Tip on the chart root (what the series measures). */
    chartTitle?: string;
  } = $props();

  let bars = $derived(topBars(data, limit));
  let max = $derived(maxValue(bars));

  function rowTitle(bar: BarDatum): string {
    if (bar.title) return bar.title;
    if (bar.label === 'other') {
      return `other — remaining tools outside the top ${Math.max(1, limit - 1)} by this metric (${formatCompact(bar.value)}${valueSuffix})`;
    }
    return `${bar.label}: ${formatCompact(bar.value)}${valueSuffix}`;
  }
</script>

{#if bars.length === 0}
  <p class="muted empty" title={chartTitle}>{empty}</p>
{:else}
  <ul class="bar-chart" role="img" aria-label={ariaLabel} title={chartTitle}>
    {#each bars as bar}
      <li title={rowTitle(bar)}>
        <span class="label">{bar.label}</span>
        <div class="track">
          <div
            class="fill"
            style={`width:${max > 0 ? (bar.value / max) * 100 : 0}%; background:${bar.color || 'var(--accent)'}`}
          ></div>
        </div>
        <span class="value">{formatCompact(bar.value)}{valueSuffix}</span>
      </li>
    {/each}
  </ul>
{/if}

<style>
  .empty {
    margin: 0.4rem 0;
    font-size: 0.82rem;
  }
  .bar-chart {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 0.35rem;
  }
  .bar-chart li {
    display: grid;
    grid-template-columns: minmax(4.5rem, 7rem) 1fr auto;
    align-items: center;
    gap: 0.45rem;
    font-size: 0.78rem;
  }
  .label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--muted);
    font-family: "IBM Plex Mono", ui-monospace, monospace;
  }
  .track {
    height: 0.55rem;
    border-radius: 999px;
    background: rgba(0, 0, 0, 0.35);
    border: 1px solid var(--border);
    overflow: hidden;
  }
  .fill {
    height: 100%;
    border-radius: inherit;
    min-width: 0;
    transition: width 0.35s var(--ease-snap);
  }
  .value {
    font-variant-numeric: tabular-nums;
    color: var(--text);
    min-width: 2.5rem;
    text-align: right;
  }
</style>
