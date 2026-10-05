<script lang="ts">
  import { onMount } from 'svelte';
  import {
    fetchSemif,
    type SemifEvent,
    type SemifPayload,
  } from './api';
  import BarChart from './charts/BarChart.svelte';
  import CollapsibleSection from './CollapsibleSection.svelte';
  import DonutChart from './charts/DonutChart.svelte';
  import Sparkline from './charts/Sparkline.svelte';
  import { matchesQuery } from './boardFilter';
  import { sortRows, toggleColumn, type ColumnSort } from './columnSort';
  import { ERESHKIGAL_COLUMN_HELP, ERESHKIGAL_HELP } from './toolHelp';
  import type { BarDatum, DonutDatum, SparkPoint } from './charts/utils';

  let { query = '' }: { query?: string } = $props();

  let board = $state<SemifPayload | null>(null);
  let error = $state('');
  let callSort = $state<ColumnSort>({ key: '', dir: 'asc' });
  let bakeoffSort = $state<ColumnSort>({ key: '', dir: 'asc' });

  async function refresh() {
    try {
      board = await fetchSemif();
      error = '';
    } catch (e) {
      error = String(e);
    }
  }

  onMount(() => {
    void refresh();
    const id = setInterval(() => void refresh(), 2000);
    return () => clearInterval(id);
  });

  function press(sort: ColumnSort, key: string, numeric = false) {
    const next = toggleColumn(sort, key, numeric);
    sort.key = next.key;
    sort.dir = next.dir;
  }

  function colTitle(help: string): string {
    return `${help} Press to sort. Press again to reverse.`;
  }

  const events = $derived(
    (board?.events ?? []).filter((row) =>
      matchesQuery(query, [
        row.ts,
        row.id,
        row.chosen,
        row.scorer,
        row.cascade_source,
        row.kind,
        row.timing_us,
        row.fallback,
      ]),
    ),
  );

  function ago(ts: number | undefined): string {
    if (!ts) return '—';
    const delta = Math.max(0, Math.floor(Date.now() / 1000) - ts);
    if (delta < 60) return `${delta}s ago`;
    if (delta < 3600) return `${Math.floor(delta / 60)}m ago`;
    if (delta < 86400) return `${Math.floor(delta / 3600)}h ago`;
    return `${Math.floor(delta / 86400)}d ago`;
  }

  const sortedEvents = $derived(
    sortRows(events, callSort, (row, key) => {
      switch (key) {
        case 'when':
          return Number(row.ts ?? 0);
        case 'kind':
          return row.kind ?? '';
        case 'id':
          return row.id ?? row.kind ?? '';
        case 'chosen':
          return row.chosen ?? '';
        case 'scorer':
          return row.scorer ?? '';
        case 'us':
          return Number(row.timing_us ?? 0);
        case 'cascade':
          return row.cascade_source ?? '';
        case 'fallback':
          return row.fallback ? 1 : 0;
        default:
          return '';
      }
    }),
  );

  const displayedEvents = $derived(
    callSort.key ? sortedEvents : [...sortedEvents].reverse(),
  );

  const sortedBakeoff = $derived(
    sortRows(board?.bakeoff ?? [], bakeoffSort, (row, key) =>
      key === 'number' ? row.value ?? '' : row.caption ?? '',
    ),
  );

  const spark: SparkPoint[] = $derived(
    events.slice(-40).map((row) => ({
      ts: Number(row.ts ?? 0),
      value: Number(row.timing_us ?? 0) / 1000,
      label: String(row.id ?? row.kind ?? ''),
    })),
  );

  const donut: DonutDatum[] = $derived.by(() => {
    let gguf = 0;
    let other = 0;
    let err = 0;
    for (const row of events) {
      if (row.fallback || row.cascade_source === 'error') err += 1;
      else if (String(row.scorer || '').includes('ereshkigal')) gguf += 1;
      else other += 1;
    }
    return [
      { label: 'GGUF', value: gguf, color: 'var(--ok)' },
      { label: 'Heuristic / other', value: other, color: 'var(--accent)' },
      { label: 'Fallback / error', value: err, color: 'var(--warn)' },
    ].filter((d) => d.value > 0);
  });

  const cascadeBar: BarDatum[] = $derived.by(() => {
    const counts = new Map<string, number>();
    for (const row of events) {
      const k = String(row.cascade_source || 'none');
      counts.set(k, (counts.get(k) ?? 0) + 1);
    }
    return [...counts.entries()].map(([label, value]) => ({ label, value }));
  });

  function kindHelp(row: SemifEvent): string {
    return ERESHKIGAL_COLUMN_HELP.kind;
  }
</script>

{#snippet col(sort: ColumnSort, label: string, key: string, help: string, numeric = false)}
  <th
    aria-sort={sort.key === key ? (sort.dir === 'asc' ? 'ascending' : 'descending') : 'none'}
    title={colTitle(help)}
  >
    <button
      type="button"
      class="sort-btn"
      title={colTitle(help)}
      onclick={() => press(sort, key, numeric)}
    >
      {label}{sort.key === key ? (sort.dir === 'asc' ? ' ↑' : ' ↓') : ''}
    </button>
  </th>
{/snippet}

{#if error}
  <p class="muted">Ereshkigal dashboard unavailable: <code>{error}</code></p>
{:else if !board}
  <p class="muted">Loading Ereshkigal telemetry…</p>
{:else}
  <CollapsibleSection id="eresh-status" titleAttr={ERESHKIGAL_HELP.status}>
    {#snippet heading()}Status{/snippet}
    <article class="card status-panel">
      <dl class="status-grid">
        <div title={ERESHKIGAL_HELP.backend}>
          <dt title={ERESHKIGAL_HELP.backend}>Backend</dt>
          <dd><code>{board.status?.backend ?? '—'}</code></dd>
        </div>
        <div title={ERESHKIGAL_HELP.mode + ' ' + ERESHKIGAL_HELP.debias}>
          <dt title={ERESHKIGAL_HELP.mode + ' ' + ERESHKIGAL_HELP.debias}>Mode / debias</dt>
          <dd><code>{board.status?.mode ?? '—'} · {board.status?.debias ?? '—'}</code></dd>
        </div>
        <div title={ERESHKIGAL_HELP.gpu}>
          <dt title={ERESHKIGAL_HELP.gpu}>GPU layers</dt>
          <dd><code>{board.status?.n_gpu_layers_used ?? board.status?.n_gpu_layers ?? '—'}</code></dd>
        </div>
        <div title={ERESHKIGAL_HELP.gguf}>
          <dt title={ERESHKIGAL_HELP.gguf}>GGUF</dt>
          <dd>
            <code class:status-miss={!board.status?.gguf_loaded}
              >{board.status?.gguf_loaded ? 'loaded' : 'missing'}</code
            >
          </dd>
        </div>
        <div class="status-span" title={ERESHKIGAL_HELP.checkpoint}>
          <dt title={ERESHKIGAL_HELP.checkpoint}>Checkpoint</dt>
          <dd>
            <code>{board.status?.gguf ?? '—'}</code>
            {#if board.status?.gguf_verify}
              <span class="status-verify">verify <code>{board.status.gguf_verify}</code></span>
            {/if}
          </dd>
        </div>
      </dl>
      {#if board.status?.cargo_feature === false}
        <p class="status-note">
          MCP binary was built without <code>--features ereshkigal</code>. Reload Wordkeep MCP after
          <code>mcp.sh</code> so GGUF scoring can load.
        </p>
      {/if}
      {#if !board.status?.gguf_loaded && board.status?.load_error}
        <p class="status-note">
          GGUF not loaded: <code>{board.status.load_error}</code>. Decisions error until a checkpoint
          is configured.
        </p>
      {/if}
      <p class="status-note">
        Lab bakeoff is this GTX 1080 Ti (Vulkan <code>n_gpu_layers=99</code>). Published SemIf BF16
        is a different checkpoint, not a latency table.
      </p>
    </article>
    <article class="card eresh-charts">
      <section class="eresh-chart" title={ERESHKIGAL_HELP.latency}>
        <h3 title={ERESHKIGAL_HELP.latency}>Latency (ms)</h3>
        <Sparkline
          data={spark}
          chartTitle={ERESHKIGAL_HELP.latency}
          ariaLabel="Ereshkigal score latency"
          valueLabel="ms"
          yAxisLabel="ms"
        />
      </section>
      <section class="eresh-chart" title={ERESHKIGAL_HELP.mix}>
        <h3 title={ERESHKIGAL_HELP.mix}>Scorer mix</h3>
        <DonutChart
          data={donut}
          chartTitle={ERESHKIGAL_HELP.mix}
          ariaLabel="Scorer mix"
          centerLabel="rows"
          centerTitle="Recent SemIf rows in this window"
          valueLabel="rows"
        />
      </section>
      <section class="eresh-chart" title={ERESHKIGAL_HELP.cascade}>
        <h3 title={ERESHKIGAL_HELP.cascade}>Cascade</h3>
        <BarChart
          data={cascadeBar}
          empty="No cascade rows yet."
          ariaLabel="Cascade source counts"
          chartTitle={ERESHKIGAL_HELP.cascade}
        />
      </section>
    </article>
  </CollapsibleSection>

  <CollapsibleSection id="eresh-events" titleAttr={ERESHKIGAL_HELP.calls}>
    {#snippet heading()}Recent calls{/snippet}
    {#if !displayedEvents.length}
      <p class="muted">{query.trim() ? 'No SemIf calls match this filter.' : 'No SemIf calls recorded yet.'}</p>
    {:else}
      <div class="table-wrap">
        <table class="dash-table">
          <thead>
            <tr>
              {@render col(callSort, 'When', 'when', ERESHKIGAL_COLUMN_HELP.when, true)}
              {@render col(callSort, 'Kind', 'kind', ERESHKIGAL_COLUMN_HELP.kind)}
              {@render col(callSort, 'Id', 'id', ERESHKIGAL_COLUMN_HELP.id)}
              {@render col(callSort, 'Chosen', 'chosen', ERESHKIGAL_COLUMN_HELP.chosen)}
              {@render col(callSort, 'Scorer', 'scorer', ERESHKIGAL_COLUMN_HELP.scorer)}
              {@render col(callSort, 'µs', 'us', ERESHKIGAL_COLUMN_HELP.us, true)}
              {@render col(callSort, 'Cascade', 'cascade', ERESHKIGAL_COLUMN_HELP.cascade)}
              {@render col(callSort, 'Fallback', 'fallback', ERESHKIGAL_COLUMN_HELP.fallback, true)}
            </tr>
          </thead>
          <tbody>
            {#each displayedEvents as row}
              <tr class:inverted={row.fallback}>
                <td class="muted" title={ERESHKIGAL_COLUMN_HELP.when}>{ago(row.ts)}</td>
                <td class="tool-name" title={kindHelp(row)}><code>{row.kind ?? '—'}</code></td>
                <td class="text" title={ERESHKIGAL_COLUMN_HELP.id}><code>{row.id ?? '—'}</code></td>
                <td class="text" title={ERESHKIGAL_COLUMN_HELP.chosen}>{row.chosen ?? '—'}</td>
                <td class="tool-name" title={ERESHKIGAL_COLUMN_HELP.scorer}
                  ><code>{row.scorer ?? '—'}</code></td
                >
                <td class="num" title={ERESHKIGAL_COLUMN_HELP.us}>{row.timing_us ?? '—'}</td>
                <td class="text" title={ERESHKIGAL_COLUMN_HELP.cascade}
                  >{row.cascade_source ?? '—'}</td
                >
                <td class="num" title={ERESHKIGAL_COLUMN_HELP.fallback}>{row.fallback ? 'yes' : '—'}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </CollapsibleSection>

  <CollapsibleSection id="eresh-bakeoff" titleAttr={ERESHKIGAL_HELP.bakeoff}>
    {#snippet heading()}Bakeoff{/snippet}
    <div class="table-wrap">
      <table class="dash-table">
        <thead>
          <tr>
            {@render col(bakeoffSort, 'Number', 'number', ERESHKIGAL_COLUMN_HELP.number)}
            {@render col(bakeoffSort, 'Caption', 'caption', ERESHKIGAL_COLUMN_HELP.caption)}
          </tr>
        </thead>
        <tbody>
          {#each sortedBakeoff as row}
            <tr>
              <td class="tool-name" title={ERESHKIGAL_COLUMN_HELP.number}
                ><code>{row.value}</code></td
              >
              <td class="text" title={row.caption ?? ERESHKIGAL_COLUMN_HELP.caption}>{row.caption}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
    <p class="muted">
      Standalone scorer:
      <a href="https://github.com/inatos/ereshkigal">github.com/inatos/ereshkigal</a>
      ·
      <a href="https://github.com/inatos/ereshkigal/tree/main/webgpu-demo">WebGPU demo</a>
    </p>
  </CollapsibleSection>
{/if}

<style>
  .status-panel {
    padding: 0.55rem 0.75rem 0.45rem;
    margin: 0.15rem 0 0.55rem;
  }
  .status-grid {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: 0.35rem 0.85rem;
    margin: 0;
  }
  .status-grid > div {
    min-width: 0;
  }
  .status-grid dt {
    margin: 0;
    font-size: 0.68rem;
    font-weight: 600;
    letter-spacing: 0.02em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .status-grid dd {
    margin: 0.12rem 0 0;
    font-size: 0.82rem;
    line-height: 1.25;
  }
  .status-grid code {
    font-size: 0.95em;
    word-break: break-all;
  }
  .status-span {
    grid-column: 1 / -1;
  }
  .status-verify {
    display: inline;
    color: var(--muted);
  }
  .status-verify::before {
    content: ' · ';
  }
  .status-miss {
    color: var(--warn, #e8a0a8);
  }
  .status-note {
    margin: 0.4rem 0 0;
    font-size: 0.75rem;
    line-height: 1.35;
    color: var(--muted);
  }
  .status-note code {
    word-break: break-all;
  }
  .eresh-charts {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 0.55rem 0.85rem;
    align-items: start;
    padding: 0.55rem 0.75rem 0.65rem;
    margin: 0 0 0.75rem;
  }
  .eresh-chart {
    min-width: 0;
    cursor: help;
  }
  .eresh-chart h3 {
    margin: 0 0 0.45rem;
    font-size: 0.72rem;
    font-weight: 600;
    letter-spacing: 0.02em;
    text-transform: uppercase;
    color: var(--muted);
  }
  @media (max-width: 720px) {
    .status-grid {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }
</style>
