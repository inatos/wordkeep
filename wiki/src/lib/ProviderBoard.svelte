<script lang="ts">
  import BarChart from './charts/BarChart.svelte';
  import DonutChart from './charts/DonutChart.svelte';
  import Sparkline from './charts/Sparkline.svelte';
  import type { BarDatum, DonutDatum, SparkPoint } from './charts/utils';
  import type { ProviderCard, ProviderPayload } from './api';
  import CollapsibleSection from './CollapsibleSection.svelte';
  import { matchesQuery } from './boardFilter';
  import {
    OUTCOME_HELP,
    PROVIDER_CHROME_HELP,
    PROVIDER_COLUMN_HELP,
    PROVIDER_FIELD_HELP,
    providerFieldHelp,
    toolHelp,
  } from './toolHelp';

  let {
    providers = [],
    query = '',
  }: {
    providers?: ProviderPayload[];
    query?: string;
  } = $props();

  /** Wall clock for live When labels (sidecar `ago` is bake-time only). */
  let nowSecs = $state(Math.floor(Date.now() / 1000));
  $effect(() => {
    const id = setInterval(() => {
      nowSecs = Math.floor(Date.now() / 1000);
    }, 1000);
    return () => clearInterval(id);
  });

  const DONUT_PALETTE = [
    'var(--ok)',
    'var(--accent)',
    'var(--warn)',
    'var(--danger)',
    'var(--muted)',
    'var(--accent-strong)',
  ];

  type Tone = 'ok' | 'warn' | 'danger' | 'muted' | 'accent';

  function sparkData(card: ProviderCard): SparkPoint[] {
    return (card.series || [])
      .map((p) => ({
        ts: typeof p.ts === 'number' ? p.ts : 0,
        value: typeof p.value === 'number' ? p.value : 0,
        label: typeof p.label === 'string' ? p.label : undefined,
      }))
      .filter((p) => Number.isFinite(p.ts) && Number.isFinite(p.value));
  }

  function barData(card: ProviderCard): BarDatum[] {
    return (card.series || [])
      .filter((p) => matchesQuery(query, [p.label, p.value]))
      .map((p) => {
        const label = String(p.label ?? '?');
        const value = typeof p.value === 'number' ? p.value : 0;
        return {
          label,
          value,
          color: 'var(--accent)',
          title: seriesTip(card, label, value),
        };
      });
  }

  function donutData(card: ProviderCard): DonutDatum[] {
    return (card.series || [])
      .filter((p) => matchesQuery(query, [p.label, p.value]))
      .map((p, i) => {
        const label = String(p.label ?? '?');
        const value = typeof p.value === 'number' ? p.value : 0;
        return {
          label,
          value,
          color: DONUT_PALETTE[i % DONUT_PALETTE.length]!,
          title: seriesTip(card, label, value),
        };
      });
  }

  function activityRows(card: ProviderCard) {
    return (card.series || []).filter((row) =>
      matchesQuery(query, [
        formatWhen(row),
        row.ago,
        row.ts,
        row.tool,
        row.label,
        row.outcome,
        row.detail,
        row.reason,
      ]),
    );
  }

  function cardVisible(card: ProviderCard): boolean {
    if (!query.trim()) return true;
    if (matchesQuery(query, [card.kind, card.title, card.value, card.detail])) return true;
    if (card.kind === 'activity') return activityRows(card).length > 0;
    if (card.kind === 'bar' || card.kind === 'donut' || card.kind === 'sparkline') {
      return (card.series || []).some((p) => matchesQuery(query, [p.label, p.value]));
    }
    return false;
  }

  function statusLabel(p: ProviderPayload): string {
    if (p.available) return 'live';
    if (p.reason === 'stale') return 'stale';
    if (p.reason === 'unavailable') return 'unavailable';
    return p.reason || 'down';
  }

  function providerTone(p: ProviderPayload): Tone {
    if (p.available) return 'ok';
    if (p.reason === 'stale') return 'warn';
    return 'danger';
  }

  function chromeHelp(p: ProviderPayload): string {
    if (p.available) return PROVIDER_CHROME_HELP.live;
    if (p.reason === 'stale') return PROVIDER_CHROME_HELP.stale;
    if (p.reason === 'unavailable') return PROVIDER_CHROME_HELP.unavailable;
    return `${PROVIDER_CHROME_HELP.provider} Status: ${statusLabel(p)}.`;
  }

  function metricTip(card: ProviderCard, valueLabel: string, detail?: string | null): string {
    const base = providerFieldHelp(card.title, card.kind);
    const cur = `Current: ${valueLabel}${detail ? ` · ${detail}` : ''}`;
    return `${base}\n${cur}`;
  }

  function outcomeTip(label: string): string {
    const key = label.toLowerCase().replace('_', '-');
    const meaning = OUTCOME_HELP[key] ?? (key === 'low_yield' ? OUTCOME_HELP['low-yield'] : undefined);
    if (meaning) return `${label} — ${meaning}`;
    return `Outcome: ${label}`;
  }

  function seriesTip(card: ProviderCard, label: string, value: number): string {
    const field = providerFieldHelp(card.title, card.kind);
    return `${field}\n${label}: ${value}`;
  }

  /** Map opaque sidecar scalars to icon + tone (no fabric-specific branches). */
  function stateOf(raw: string | number | null | undefined): { tone: Tone; kind: string; label: string } {
    const label = String(raw ?? '—').trim() || '—';
    const key = label.toLowerCase();
    // Bare integers are count metrics (e.g. Courier unread) — not boolean 0/1.
    if (/^\d+$/.test(key)) {
      return { tone: key === '0' ? 'muted' : 'accent', kind: 'dot', label };
    }
    if (['up', 'ok', 'live', 'ready', 'healthy', 'true'].includes(key)) {
      return { tone: 'ok', kind: 'check', label };
    }
    if (['stale', 'n/a', 'na', 'degraded', 'warn', 'warning'].includes(key)) {
      return { tone: 'warn', kind: 'clock', label };
    }
    if (['down', 'missing', 'error', 'fail', 'failed', 'false', 'unavailable'].includes(key)) {
      return { tone: 'danger', kind: 'x', label };
    }
    if (key === '—' || key === 'unknown' || key === 'none') {
      return { tone: 'muted', kind: 'dot', label };
    }
    return { tone: 'accent', kind: 'dot', label };
  }

  function outcomeOf(raw: string | null | undefined): { tone: Tone; kind: string; label: string } {
    const label = String(raw ?? '—').trim() || '—';
    const key = label.toLowerCase();
    if (key === 'ok' || key === 'pass' || key === 'done') return { tone: 'ok', kind: 'check', label };
    if (key === 'trunc' || key === 'empty' || key === 'low_yield' || key === 'warn' || key === 'warning') {
      return { tone: 'warn', kind: 'warn', label };
    }
    if (key === 'error' || key === 'invalid' || key === 'fail' || key === 'failed') {
      return { tone: 'danger', kind: 'x', label };
    }
    if (key === 'not_found') return { tone: 'muted', kind: 'search', label };
    return { tone: 'muted', kind: 'dot', label };
  }

  function formatAge(secs: number | undefined): string {
    if (secs == null) return '';
    if (secs < 60) return `${secs}s`;
    if (secs < 3600) return `${Math.floor(secs / 60)}m`;
    if (secs < 86400) return `${Math.floor(secs / 3600)}h`;
    return `${Math.floor(secs / 86400)}d`;
  }

  function formatWhen(row: {
    ts?: unknown;
    ago?: unknown;
  }): string {
    const raw = row.ts;
    const ts =
      typeof raw === 'number'
        ? raw
        : typeof raw === 'string'
          ? Number(raw)
          : NaN;
    if (Number.isFinite(ts) && ts > 0) {
      return formatAge(Math.max(0, nowSecs - Math.floor(ts))) || '0s';
    }
    return typeof row.ago === 'string' && row.ago ? row.ago : '—';
  }

  function sparkUnit(title: string): string {
    const t = title.trim().toLowerCase();
    if (t.includes('mbps') || t.includes('mbit')) return 'Mbps';
    if (t.includes('rtt') || t.endsWith(' ms') || t.includes('latency')) return 'ms';
    if (t.includes('byte')) return 'B';
    return 'value';
  }

  function partition(cards: ProviderCard[]) {
    const activity = cards.filter((c) => c.kind === 'activity');
    const metrics = cards.filter((c) => c.kind === 'metric');
    const sparks = cards.filter((c) => c.kind === 'sparkline');
    const charts = cards.filter((c) => c.kind === 'bar' || c.kind === 'donut');
    const rest = cards.filter(
      (c) => !['activity', 'metric', 'sparkline', 'bar', 'donut'].includes(c.kind),
    );
    return { activity, metrics, sparks, charts, rest };
  }

  let visibleProviders = $derived(
    providers.filter((p) =>
      matchesQuery(query, [
        p.name,
        p.reason,
        p.sidecar,
        ...(p.cards || []).flatMap((c) => [c.title, c.kind, c.value]),
      ]),
    ),
  );
</script>

{#snippet statusIcon(kind: string)}
  {#if kind === 'check'}
    <svg viewBox="0 0 24 24" aria-hidden="true"
      ><path
        d="M5 13l4 4L19 7"
        fill="none"
        stroke="currentColor"
        stroke-width="2.4"
        stroke-linecap="round"
        stroke-linejoin="round"
      /></svg
    >
  {:else if kind === 'x'}
    <svg viewBox="0 0 24 24" aria-hidden="true"
      ><path
        d="M6 6l12 12M18 6L6 18"
        fill="none"
        stroke="currentColor"
        stroke-width="2.4"
        stroke-linecap="round"
      /></svg
    >
  {:else if kind === 'clock'}
    <svg viewBox="0 0 24 24" aria-hidden="true"
      ><circle cx="12" cy="12" r="8" fill="none" stroke="currentColor" stroke-width="2" /><path
        d="M12 8v5l3 2"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
      /></svg
    >
  {:else if kind === 'warn'}
    <svg viewBox="0 0 24 24" aria-hidden="true"
      ><path
        d="M12 3l9 16H3L12 3z"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linejoin="round"
      /><path
        d="M12 10v4M12 17h.01"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
      /></svg
    >
  {:else if kind === 'search'}
    <svg viewBox="0 0 24 24" aria-hidden="true"
      ><circle cx="11" cy="11" r="6" fill="none" stroke="currentColor" stroke-width="2" /><path
        d="M16 16l4 4"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
      /></svg
    >
  {:else if kind === 'pulse'}
    <svg viewBox="0 0 24 24" aria-hidden="true"
      ><path
        d="M3 12h4l2-6 4 12 2-6h6"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
      /></svg
    >
  {:else}
    <svg viewBox="0 0 24 24" aria-hidden="true"
      ><circle cx="12" cy="12" r="3.5" fill="currentColor" /></svg
    >
  {/if}
{/snippet}

{#if providers.length === 0}
  <p class="muted">No telemetry providers configured in <code>.wordkeep/config.json</code>.</p>
{:else if visibleProviders.length === 0}
  <p class="muted">No provider cards match the filter.</p>
{:else}
  {#each visibleProviders as provider (provider.name)}
    {@const tone = providerTone(provider)}
    {@const cards = (provider.cards || []).filter(cardVisible)}
    {@const parts = partition(cards)}
    {@const slug = provider.name.replace(/\s+/g, '-').toLowerCase()}

    <header class="provider-chrome">
      <span class="chrome-name">{provider.name}</span>
      <span class="status-chip tone-{tone}" title={chromeHelp(provider)}>
        <span class="status-ico">{@render statusIcon(provider.available ? 'pulse' : provider.reason === 'stale' ? 'clock' : 'x')}</span>
        {statusLabel(provider)}
      </span>
      {#if provider.age_secs != null}
        <span class="age" title={PROVIDER_CHROME_HELP.age}>
          <span class="status-ico muted">{@render statusIcon('clock')}</span>
          {formatAge(provider.age_secs)} ago
        </span>
      {/if}
    </header>

    {#if !provider.available && cards.length === 0}
      <p class="muted empty-msg">
        Sidecar {provider.reason || 'unavailable'}
        {#if provider.sidecar}
          — <code>{provider.sidecar}</code>
        {/if}
      </p>
    {:else}
      {#each parts.activity as card, idx (`${provider.name}-act-${idx}`)}
        <CollapsibleSection
          id={`dash-provider-${slug}-activity-${idx}`}
          titleAttr={providerFieldHelp(card.title, card.kind)}
        >
          {#snippet heading()}
            <svg viewBox="0 0 24 24" aria-hidden="true"
              ><path
                d="M12 8v5l3 2M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20z"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
                stroke-linejoin="round"
              /></svg
            >
            {card.title}
            <span class="muted count">{activityRows(card).length}</span>
          {/snippet}
          {#if activityRows(card).length === 0}
            <p class="muted empty-msg">No recent calls.</p>
          {:else}
            <div class="table-wrap">
              <table class="dash-table">
                <thead>
                  <tr>
                    <th>
                      <span class="sort-btn" title={PROVIDER_COLUMN_HELP.when}>When</span>
                    </th>
                    <th class="tool-name">
                      <span class="sort-btn" title={PROVIDER_COLUMN_HELP.tool}>Tool</span>
                    </th>
                    <th>
                      <span class="sort-btn" title={PROVIDER_COLUMN_HELP.outcome}>Outcome</span>
                    </th>
                    <th class="text">
                      <span class="sort-btn" title={PROVIDER_COLUMN_HELP.detail}>Detail</span>
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {#each activityRows(card) as row, rIdx (`${provider.name}-row-${rIdx}`)}
                    {@const oc = outcomeOf(
                      typeof row.outcome === 'string' ? row.outcome : String(row.outcome ?? ''),
                    )}
                    {@const toolName = String(row.tool ?? row.label ?? '—')}
                    <tr class:inverted={oc.tone === 'danger'}>
                      <td class="muted">{formatWhen(row)}</td>
                      <td class="tool-name" title={toolHelp(toolName)}>
                        <code>{toolName}</code>
                      </td>
                      <td class="num">
                        <span class="outcome-ico tone-{oc.tone}" title={outcomeTip(oc.label)}>
                          {@render statusIcon(oc.kind)}
                        </span>
                        <span class="sr-only">{oc.label}</span>
                      </td>
                      <td
                        class="text"
                        title={row.detail || row.reason ? String(row.detail ?? row.reason) : undefined}
                      >{row.detail ?? row.reason ?? ''}</td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
          {/if}
        </CollapsibleSection>
      {/each}

      {#if parts.metrics.length || parts.sparks.length}
        <CollapsibleSection
          id={`dash-provider-${slug}-status`}
          titleAttr={PROVIDER_FIELD_HELP.status}
        >
          {#snippet heading()}
            <svg viewBox="0 0 24 24" aria-hidden="true"
              ><path
                d="M5 13l4 4L19 7"
                fill="none"
                stroke="currentColor"
                stroke-width="2.4"
                stroke-linecap="round"
                stroke-linejoin="round"
              /></svg
            >
            Status
          {/snippet}
          {#if parts.metrics.length}
            <div class="cards dash-overview">
              {#each parts.metrics as card, mIdx (`${provider.name}-m-${mIdx}`)}
                {@const st = stateOf(card.value)}
                <article class="card" title={metricTip(card, st.label, card.detail)}>
                  <h3>
                    <span class="status-ico tone-{st.tone}">{@render statusIcon(st.kind)}</span>
                    {card.title}
                  </h3>
                  <p
                    class="metric"
                    class:good={st.tone === 'ok'}
                    class:warn={st.tone === 'warn'}
                    class:bad={st.tone === 'danger'}
                  >
                    <code>{st.label}</code>
                    {#if card.detail}
                      <span class="muted"> · {card.detail}</span>
                    {/if}
                  </p>
                </article>
              {/each}
            </div>
          {/if}
          {#if parts.sparks.length}
            <div class="dash-charts">
              {#each parts.sparks as card, sIdx (`${provider.name}-s-${sIdx}`)}
                {@const unit = sparkUnit(card.title)}
                <article class="card chart-card" title={providerFieldHelp(card.title, card.kind)}>
                  <h3>{card.title}</h3>
                  <Sparkline
                    data={sparkData(card)}
                    empty="No series yet."
                    ariaLabel={card.title}
                    chartTitle={card.title}
                    valueLabel={unit}
                    yAxisLabel={unit}
                    xAxisLabel="older → newer"
                  />
                </article>
              {/each}
            </div>
          {/if}
        </CollapsibleSection>
      {/if}

      {#if parts.charts.length}
        <CollapsibleSection id={`dash-provider-${slug}-mix`} titleAttr={PROVIDER_FIELD_HELP.mix}>
          {#snippet heading()}
            <svg viewBox="0 0 24 24" aria-hidden="true"
              ><path
                d="M4 19V9M10 19V5M16 19v-7M22 19H2"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
                stroke-linejoin="round"
              /></svg
            >
            Mix
          {/snippet}
          <div class="dash-charts">
            {#each parts.charts as card, cIdx (`${provider.name}-c-${cIdx}`)}
              <article class="card chart-card" title={providerFieldHelp(card.title, card.kind)}>
                <h3>{card.title}</h3>
                {#if card.kind === 'bar'}
                  <BarChart
                    data={barData(card)}
                    empty="No series yet."
                    ariaLabel={card.title}
                    chartTitle={card.title}
                  />
                {:else}
                  <DonutChart
                    data={donutData(card)}
                    empty="No series yet."
                    ariaLabel={card.title}
                    chartTitle={card.title}
                    centerTitle={card.title}
                    valueLabel="count"
                  />
                {/if}
              </article>
            {/each}
          </div>
        </CollapsibleSection>
      {/if}

      {#if parts.rest.length}
        <CollapsibleSection
          id={`dash-provider-${slug}-other`}
          titleAttr="Additional provider cards that do not map to activity/status/mix."
        >
          {#snippet heading()}
            Other
          {/snippet}
          <div class="cards dash-overview">
            {#each parts.rest as card, rIdx (`${provider.name}-r-${rIdx}`)}
              <article class="card" title={providerFieldHelp(card.title, card.kind)}>
                <h3>{card.title}</h3>
                <p class="metric"><code>{card.kind}</code></p>
              </article>
            {/each}
          </div>
        </CollapsibleSection>
      {/if}
    {/if}
  {/each}
{/if}

<style>
  .provider-chrome {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.15rem 0.35rem;
    margin: 0.15rem 0 0.55rem;
    padding: 0.15rem 0.1rem;
  }
  .chrome-name {
    font-size: 0.95rem;
    font-weight: 650;
  }

  .count {
    margin-left: 0.45rem;
    font-size: 0.8rem;
    font-weight: 500;
    font-variant-numeric: tabular-nums;
  }

  /* Match Telemetry / Ereshkigal: plain .table-wrap + .dash-table (no height cage). */
  :global(.table-wrap) :global(th .sort-btn) {
    cursor: default;
  }
  :global(.table-wrap) :global(th.tool-name),
  :global(.table-wrap) :global(th.text) {
    text-align: left;
  }
  :global(.table-wrap) :global(th.tool-name .sort-btn),
  :global(.table-wrap) :global(th.text .sort-btn) {
    text-align: left;
  }

  .status-ico {
    display: inline-flex;
    width: 0.95em;
    height: 0.95em;
    margin-right: 0.28rem;
    vertical-align: -0.12em;
    flex: 0 0 auto;
  }
  .status-ico :global(svg) {
    width: 100%;
    height: 100%;
  }
  .status-ico.muted {
    color: var(--muted);
  }
  .status-ico.tone-ok {
    color: var(--ok);
  }
  .status-ico.tone-warn {
    color: var(--warn);
  }
  .status-ico.tone-danger {
    color: var(--danger);
  }
  .status-ico.tone-muted {
    color: var(--muted);
  }
  .status-ico.tone-accent {
    color: var(--accent);
  }

  .status-chip {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    margin-left: 0.55rem;
    padding: 0.12rem 0.5rem;
    border-radius: 999px;
    font-size: 0.72rem;
    font-weight: 650;
    letter-spacing: 0.02em;
    text-transform: lowercase;
    background: color-mix(in oklab, var(--panel-2, #222) 88%, transparent);
    border: 1px solid color-mix(in oklab, currentColor 35%, transparent);
  }
  .status-chip .status-ico {
    margin-right: 0;
  }
  .status-chip.tone-ok {
    color: var(--ok);
  }
  .status-chip.tone-warn {
    color: var(--warn);
  }
  .status-chip.tone-danger {
    color: var(--danger);
  }
  .status-chip.tone-muted {
    color: var(--muted);
  }
  .status-chip.tone-accent {
    color: var(--accent);
  }

  .age {
    display: inline-flex;
    align-items: center;
    gap: 0.28rem;
    margin-left: 0.55rem;
    font-size: 0.78rem;
    font-weight: 450;
    color: var(--muted);
  }
  .age .status-ico {
    margin-right: 0;
  }

  /* Compact overview headings: icon + title on one line. */
  :global(.dash-overview .card h3) {
    display: flex;
    align-items: center;
    gap: 0;
  }

  .outcome-ico {
    display: inline-flex;
    width: 1rem;
    height: 1rem;
    vertical-align: middle;
  }
  .outcome-ico :global(svg) {
    width: 100%;
    height: 100%;
  }
  .outcome-ico.tone-ok {
    color: var(--ok);
  }
  .outcome-ico.tone-warn {
    color: var(--warn);
  }
  .outcome-ico.tone-danger {
    color: var(--danger);
  }
  .outcome-ico.tone-muted,
  .outcome-ico.tone-accent {
    color: var(--muted);
  }
  .empty-msg {
    margin: 0.15rem 0 0;
  }

  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    padding: 0;
    margin: -1px;
    overflow: hidden;
    clip: rect(0, 0, 0, 0);
    white-space: nowrap;
    border: 0;
  }
</style>
