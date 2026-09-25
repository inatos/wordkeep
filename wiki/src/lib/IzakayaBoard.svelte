<script lang="ts">
  import { onMount } from 'svelte';
  import {
    fetchIzakaya,
    type DashboardActivity,
    type DashboardTool,
    type IzakayaPayload,
  } from './api';
  import BarChart from './charts/BarChart.svelte';
  import { matchesQuery } from './boardFilter';
  import CollapsibleSection from './CollapsibleSection.svelte';
  import { sortRows, toggleColumn, type ColumnSort } from './columnSort';
  import DonutChart from './charts/DonutChart.svelte';
  import Sparkline from './charts/Sparkline.svelte';
  import type { BarDatum, DonutDatum, SparkPoint } from './charts/utils';

  let {
    tools = [],
    activity = [],
    query = '',
  }: { tools?: DashboardTool[]; activity?: DashboardActivity[]; query?: string } = $props();

  let board = $state<IzakayaPayload | null>(null);
  let error = $state('');

  const filtering = $derived(query.trim().length > 0);
  const izakayaTools = $derived(
    tools.filter(
      (tool) =>
        tool.name.startsWith('izakaya_') &&
        matchesQuery(query, [tool.name, tool.last_ago, tool.calls, tool.avg_ms]),
    ),
  );
  const izakayaActivity = $derived(
    activity
      .filter(
        (row) =>
          row.tool.startsWith('izakaya_') &&
          matchesQuery(query, [row.tool, row.outcome, row.reason, row.ago, row.elapsed_ms]),
      )
      .slice(0, 12),
  );

  const viewAgents = $derived(
    (board?.agents ?? []).filter((agent) =>
      matchesQuery(query, [
        agent.agent_id,
        agent.state,
        agent.stale ? 'stale' : '',
        agent.role,
        agent.task,
        agent.summary,
        agent.branch,
        agent.head,
        agent.worktree,
        agent.checkpoint,
        agent.checkout_reason,
        agent.mas_session,
        claimLabel(agent),
        ...(agent.blockers ?? []),
      ]),
    ),
  );
  const viewEvents = $derived(
    (board?.events ?? []).filter((event) =>
      matchesQuery(query, [event.kind, event.agent_id, event.result, event.seq]),
    ),
  );
  const viewHandoffs = $derived(
    (board?.handoffs ?? []).filter((handoff) =>
      matchesQuery(query, [
        handoff.id,
        handoff.from,
        handoff.to,
        handoff.status,
        handoff.derived_status,
        handoff.summary,
        handoff.checkpoint,
        handoff.accepted_by,
        handoff.mas_session,
      ]),
    ),
  );
  const viewNotes = $derived(
    (board?.messages ?? []).filter((note) =>
      matchesQuery(query, [note.seq, note.from, note.to, note.body, note.acked ? 'acked' : '']),
    ),
  );
  const viewFindings = $derived(
    (board?.findings ?? []).filter((finding) => matchesQuery(query, [finding.kind, finding.detail])),
  );
  const viewCounts = $derived.by(() => {
    const counts = {
      live_code: 0,
      checked_in: 0,
      suspended: 0,
      checked_out: 0,
      stale: 0,
      handoffs_open: 0,
    };
    for (const agent of viewAgents) {
      if (agent.stale) counts.stale += 1;
      if (agent.state === 'live_code') counts.live_code += 1;
      else if (agent.state === 'checked_in') counts.checked_in += 1;
      else if (agent.state === 'suspended') counts.suspended += 1;
      else if (agent.state === 'checked_out') counts.checked_out += 1;
    }
    counts.handoffs_open = viewHandoffs.filter((handoff) =>
      ['offered', 'orphaned'].includes(handoff.derived_status || ''),
    ).length;
    return counts;
  });

  const presenceSlices = $derived.by((): DonutDatum[] => {
    const tally: Record<string, number> = {
      live_code: 0,
      checked_in: 0,
      suspended: 0,
      checked_out: 0,
      stale: 0,
    };
    for (const agent of viewAgents) {
      if (agent.stale) tally.stale += 1;
      else if (agent.state in tally) tally[agent.state] += 1;
    }
    return [
      { label: 'live', value: tally.live_code, color: 'var(--ok)', title: 'Agents writing live code' },
      {
        label: 'checked in',
        value: tally.checked_in,
        color: 'var(--accent-strong)',
        title: 'Checked in, not yet marked live',
      },
      { label: 'suspended', value: tally.suspended, color: 'var(--warn)', title: 'Suspended with a checkpoint' },
      { label: 'checked out', value: tally.checked_out, color: 'var(--muted)', title: 'Checked out' },
      { label: 'stale', value: tally.stale, color: 'var(--danger)', title: 'Lease expired; not auto-checked-out' },
    ];
  });

  const kindBars = $derived.by((): BarDatum[] => {
    const tally = new Map<string, number>();
    for (const event of viewEvents) {
      const kind = event.kind || 'unknown';
      tally.set(kind, (tally.get(kind) ?? 0) + 1);
    }
    return [...tally.entries()].map(([label, value]) => ({
      label,
      value,
      title: `${value} ${label} event(s) in the journal tail`,
    }));
  });

  const journalSpark = $derived.by((): SparkPoint[] =>
    viewEvents
      .filter((event) => event.ts > 0)
      .map((event) => ({
        ts: event.ts,
        value: event.seq,
        label: `${event.kind} · ${event.agent_id || '—'}`,
      })),
  );

  const callBars = $derived.by((): BarDatum[] =>
    izakayaTools.map((tool) => ({
      label: tool.name.replace(/^izakaya_/, ''),
      value: tool.calls,
      title: `${tool.name}: ${tool.calls} calls, avg ${tool.avg_ms} ms, last ${tool.last_ago || '—'}`,
    })),
  );

  const latencyBars = $derived.by((): BarDatum[] =>
    izakayaTools
      .filter((tool) => tool.calls > 0)
      .map((tool) => ({
        label: tool.name.replace(/^izakaya_/, ''),
        value: tool.avg_ms,
        color: tool.avg_ms >= 500 ? 'var(--warn)' : 'var(--accent-strong)',
        title: `${tool.name} average ${tool.avg_ms} ms over ${tool.calls} calls`,
      })),
  );

  const dirtyBars = $derived.by((): BarDatum[] =>
    viewAgents.map((agent) => ({
      label: agent.agent_id,
      value: agent.dirty_count ?? 0,
      color: (agent.dirty_count ?? 0) > 0 ? 'var(--warn)' : 'var(--muted)',
      title: `${agent.agent_id}: ${agent.dirty_count ?? 0} dirty path(s), state ${agent.state}`,
    })),
  );

  const claimRows = $derived.by(() => {
    const rows: { agent: string; state: string; path: string; symbols: string; intent: string }[] =
      [];
    for (const agent of viewAgents) {
      for (const claim of agent.claims ?? []) {
        rows.push({
          agent: agent.agent_id,
          state: agent.stale ? 'stale' : agent.state,
          path: claim.path || '—',
          symbols: (claim.symbols ?? []).filter(Boolean).join(', ') || '—',
          intent: claim.intent || '—',
        });
      }
    }
    return rows;
  });

  const journalRows = $derived([...viewEvents].reverse());

  let findingSort = $state<ColumnSort>({ key: '', dir: 'asc' });
  let agentSort = $state<ColumnSort>({ key: '', dir: 'asc' });
  let claimSort = $state<ColumnSort>({ key: '', dir: 'asc' });
  let handoffSort = $state<ColumnSort>({ key: '', dir: 'asc' });
  let journalSort = $state<ColumnSort>({ key: '', dir: 'asc' });
  let noteSort = $state<ColumnSort>({ key: '', dir: 'asc' });
  let toolSort = $state<ColumnSort>({ key: '', dir: 'asc' });
  let callSort = $state<ColumnSort>({ key: '', dir: 'asc' });

  function press(sort: ColumnSort, key: string, numeric = false) {
    const next = toggleColumn(sort, key, numeric);
    sort.key = next.key;
    sort.dir = next.dir;
  }

  /** Lower = more active/relevant. Used for default Agents order and State column. */
  function agentRelevance(agent: NonNullable<IzakayaPayload['agents']>[number]): number {
    if (agent.stale) return 3;
    switch (agent.state) {
      case 'live_code':
        return 0;
      case 'checked_in':
        return 1;
      case 'suspended':
        return 2;
      case 'checked_out':
        return 4;
      default:
        return 5;
    }
  }

  const sortedFindings = $derived(
    sortRows(viewFindings, findingSort, (row, key) => (key === 'kind' ? row.kind : row.detail)),
  );
  const sortedAgents = $derived.by(() => {
    if (!agentSort.key) {
      return [...viewAgents].sort((a, b) => {
        const byRank = agentRelevance(a) - agentRelevance(b);
        if (byRank !== 0) return byRank;
        const bySeen = (b.last_seen_at ?? 0) - (a.last_seen_at ?? 0);
        if (bySeen !== 0) return bySeen;
        return a.agent_id.localeCompare(b.agent_id);
      });
    }
    return sortRows(viewAgents, agentSort, (agent, key) => {
      switch (key) {
        case 'state':
          return agentRelevance(agent);
        case 'task':
          return agent.task || '';
        case 'claims':
          return claimLabel(agent);
        case 'dirty':
          return agent.dirty_count ?? 0;
        case 'seen':
          return agent.last_seen_at ?? 0;
        case 'lease':
          return agent.expires_at ?? 0;
        default:
          return agent.agent_id;
      }
    });
  });
  const sortedClaims = $derived(
    sortRows(claimRows, claimSort, (row, key) => row[key as keyof typeof row]),
  );
  const sortedHandoffs = $derived(
    sortRows(viewHandoffs, handoffSort, (row, key) => {
      switch (key) {
        case 'status':
          return row.derived_status || row.status;
        case 'from':
          return row.from;
        case 'to':
          return row.to;
        case 'when':
          return row.created_at ?? 0;
        case 'summary':
          return row.summary || '';
        default:
          return row.id;
      }
    }),
  );
  const sortedJournal = $derived(
    sortRows(journalRows, journalSort, (row, key) => {
      switch (key) {
        case 'when':
          return row.ts;
        case 'kind':
          return row.kind;
        case 'agent':
          return row.agent_id;
        case 'result':
          return row.result || '';
        default:
          return row.seq;
      }
    }),
  );
  const sortedNotes = $derived(
    sortRows(viewNotes, noteSort, (row, key) => {
      switch (key) {
        case 'from':
          return row.from;
        case 'to':
          return row.to;
        case 'ack':
          return row.acked ? 1 : 0;
        case 'body':
          return row.body;
        default:
          return row.seq;
      }
    }),
  );
  const sortedTools = $derived(
    sortRows(izakayaTools, toolSort, (row, key) => {
      switch (key) {
        case 'calls':
          return row.calls;
        case 'avg':
          return row.avg_ms;
        case 'errors':
          return row.error_count;
        case 'trunc':
          return row.trunc_count;
        case 'saved':
          return row.saved;
        case 'last':
          return row.last_ts ?? 0;
        default:
          return row.name;
      }
    }),
  );
  const sortedCalls = $derived(
    sortRows(izakayaActivity, callSort, (row, key) => {
      switch (key) {
        case 'when':
          return row.ts ?? 0;
        case 'tool':
          return row.tool;
        case 'outcome':
          return row.outcome;
        case 'ms':
          return row.elapsed_ms;
        case 'saved':
          return row.saved;
        default:
          return row.reason || '';
      }
    }),
  );

  function ago(now: number | undefined, ts: number | undefined): string {
    if (!ts) return '—';
    const base = now && now > 0 ? now : Math.floor(Date.now() / 1000);
    const delta = Math.max(0, base - ts);
    if (delta < 5) return 'just now';
    if (delta < 60) return `${delta}s ago`;
    if (delta < 3600) return `${Math.floor(delta / 60)}m ago`;
    if (delta < 86400) return `${Math.floor(delta / 3600)}h ago`;
    return `${Math.floor(delta / 86400)}d ago`;
  }

  function leaseLeft(now: number | undefined, agent: NonNullable<IzakayaPayload['agents']>[number]): string {
    if (agent.state === 'checked_out') return '—';
    if (!agent.expires_at) return '—';
    const base = now && now > 0 ? now : Math.floor(Date.now() / 1000);
    const left = agent.expires_at - base;
    if (left <= 0) return 'expired';
    if (left < 60) return `${left}s`;
    if (left < 3600) return `${Math.floor(left / 60)}m`;
    return `${Math.floor(left / 3600)}h`;
  }

  function claimLabel(agent: NonNullable<IzakayaPayload['agents']>[number]): string {
    const claims = agent.claims ?? [];
    if (!claims.length) return '—';
    return claims
      .map((claim) => {
        const symbols = (claim.symbols ?? []).filter(Boolean).join(', ');
        if (claim.path && symbols) return `${claim.path} · ${symbols}`;
        return claim.path || symbols || '—';
      })
      .join('; ');
  }

  onMount(() => {
    let stopped = false;
    const tick = async () => {
      try {
        const next = await fetchIzakaya();
        if (!stopped) {
          board = next;
          error = '';
        }
      } catch (e) {
        if (!stopped) error = e instanceof Error ? e.message : String(e);
      }
    };
    void tick();
    const handle = setInterval(() => void tick(), 2000);
    return () => {
      stopped = true;
      clearInterval(handle);
    };
  });
</script>

<div class="izakaya" aria-label="Izakaya board">
  {#snippet col(sort: ColumnSort, label: string, key: string, numeric = false)}
    <th aria-sort={sort.key === key ? (sort.dir === 'asc' ? 'ascending' : 'descending') : 'none'}>
      <button
        type="button"
        class="sort-btn"
        title="Press to sort. Press again to reverse."
        onclick={() => press(sort, key, numeric)}
      >
        {label}{sort.key === key ? (sort.dir === 'asc' ? ' ↑' : ' ↓') : ''}
      </button>
    </th>
  {/snippet}
  {#if error}
    <p class="muted">Izakaya unavailable: <code>{error}</code></p>
  {:else if !board}
    <p class="muted">Loading Izakaya…</p>
  {:else if board.available === false}
    <p class="muted">{board.message || 'Izakaya board unavailable.'}</p>
  {:else}
    <p class="meta muted">
      seq <code>{board.seq ?? 0}</code>
      {#if board.journal_seq != null && board.journal_seq !== board.seq}
        · journal <code>{board.journal_seq}</code>
      {/if}
      · group <code>{board.coordination_id || '—'}</code>
      · updated {ago(board.now, board.updated_at)}
      · policy <code>{board.active_policy || 'none'}</code>
      {#if filtering}
        · filter <code>{query.trim()}</code>
      {/if}
    </p>

    {#if board.projection_behind}
      <p class="warn">
        The projection is behind the journal. The next <code>izakaya_status</code> rebuilds it;
        this page does not write.
      </p>
    {/if}
    {#if (board.malformed ?? 0) > 0}
      <p class="warn">{board.malformed} malformed journal line(s) were skipped on the last rebuild.</p>
    {/if}

    <div class="cards">
      <article class="card">
        <h3>Live</h3>
        <p class="metric">{viewCounts.live_code}</p>
      </article>
      <article class="card">
        <h3>Checked in</h3>
        <p class="metric">{viewCounts.checked_in}</p>
      </article>
      <article class="card">
        <h3>Suspended</h3>
        <p class="metric">{viewCounts.suspended}</p>
      </article>
      <article class="card">
        <h3>Stale</h3>
        <p class="metric" class:warn={viewCounts.stale > 0}>{viewCounts.stale}</p>
      </article>
      <article class="card">
        <h3>Open handoffs</h3>
        <p class="metric">{viewCounts.handoffs_open}</p>
      </article>
      <article class="card">
        <h3>Checked out</h3>
        <p class="metric">{viewCounts.checked_out}</p>
      </article>
    </div>

    <div class="charts" aria-label="Izakaya charts">
      <article class="card chart-card" title="Agents by state. Stale leases are their own slice, not double-counted.">
        <h3>Presence</h3>
        <DonutChart
          data={presenceSlices}
          empty="No agents yet."
          ariaLabel="Agents by state"
          chartTitle="Mix of agent states in this coordination group"
          centerLabel="agents"
          centerTitle="Agents in the coordination group"
          valueLabel="agents"
        />
      </article>
      <article class="card chart-card" title="Journal sequence over the retained event tail.">
        <h3>Journal sequence</h3>
        <Sparkline
          data={journalSpark}
          empty="No journal timestamps yet."
          ariaLabel="Journal sequence over time"
          chartTitle="Each point is one journal event; the value is its sequence number"
          valueLabel="seq"
          yAxisLabel="seq"
          xAxisLabel="older → newer"
        />
      </article>
      <article class="card chart-card" title="Event kinds in the retained journal tail.">
        <h3>Event kinds</h3>
        <BarChart
          data={kindBars}
          empty="No journal events yet."
          ariaLabel="Journal events by kind"
          chartTitle="How many of each event kind are in the retained tail"
        />
      </article>
      <article class="card chart-card" title="Dirty paths reported on the last check-in or update.">
        <h3>Dirty paths</h3>
        <BarChart
          data={dirtyBars}
          empty="No dirty paths reported."
          ariaLabel="Dirty paths by agent"
          chartTitle="Dirty path counts from the last git snapshot on each agent"
        />
      </article>
      <article class="card chart-card" title="izakaya_* calls recorded in savings telemetry.">
        <h3>MCP calls</h3>
        <BarChart
          data={callBars}
          empty="No izakaya_* calls yet."
          ariaLabel="Izakaya tool calls"
          chartTitle="Call counts for izakaya tools"
        />
      </article>
      <article class="card chart-card" title="Average latency of izakaya_* tools.">
        <h3>MCP latency</h3>
        <BarChart
          data={latencyBars}
          empty="No timed izakaya_* calls yet."
          valueSuffix=" ms"
          ariaLabel="Izakaya tool average latency"
          chartTitle="Average milliseconds per izakaya tool"
        />
      </article>
    </div>

    {#if board.message && board.empty}
      <p class="muted">{board.message}</p>
    {/if}

    <CollapsibleSection id="izakaya-calls" titleAttr="izakaya_* calls recorded in savings telemetry">
      {#snippet heading()}MCP calls{/snippet}
      {#if !izakayaTools.length}
        <p class="muted">{filtering ? 'No izakaya_* calls match this filter.' : 'No izakaya_* calls in savings telemetry yet.'}</p>
      {:else}
        <div class="table-wrap">
          <table class="dash-table">
            <thead>
              <tr>
                {@render col(toolSort, 'Tool', 'tool')}
                {@render col(toolSort, 'Calls', 'calls', true)}
                {@render col(toolSort, 'Avg', 'avg', true)}
                {@render col(toolSort, 'Errors', 'errors', true)}
                {@render col(toolSort, 'Trunc', 'trunc', true)}
                {@render col(toolSort, 'Saved', 'saved', true)}
                {@render col(toolSort, 'Last', 'last', true)}
              </tr>
            </thead>
            <tbody>
              {#each sortedTools as tool}
                <tr>
                  <td class="tool-name"><code>{tool.name}</code></td>
                  <td class="num">{tool.calls_fmt ?? tool.calls}</td>
                  <td class="num">{tool.avg_ms}ms</td>
                  <td class="num">{tool.error_count}</td>
                  <td class="num">{tool.trunc_count}</td>
                  <td class="num good">{tool.saved_fmt ?? tool.saved}</td>
                  <td class="muted">{tool.last_ago || '—'}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
    </CollapsibleSection>

    <CollapsibleSection id="izakaya-recent" titleAttr="Recent izakaya_* calls from savings telemetry">
      {#snippet heading()}Recent calls{/snippet}
      {#if !izakayaActivity.length}
        <p class="muted">{filtering ? 'No recent calls match this filter.' : 'No recent izakaya_* calls yet.'}</p>
      {:else}
        <div class="table-wrap">
          <table class="dash-table">
            <thead>
              <tr>
                {@render col(callSort, 'When', 'when', true)}
                {@render col(callSort, 'Tool', 'tool')}
                {@render col(callSort, 'Outcome', 'outcome')}
                {@render col(callSort, 'ms', 'ms', true)}
                {@render col(callSort, 'Saved', 'saved', true)}
                {@render col(callSort, 'Reason', 'reason')}
              </tr>
            </thead>
            <tbody>
              {#each sortedCalls as row}
                <tr>
                  <td class="muted">{row.ago}</td>
                  <td class="tool-name"><code>{row.tool}</code></td>
                  <td>
                    {#if row.outcome !== 'ok'}
                      <span class="chip warn">{row.outcome}</span>
                    {:else}
                      <span class="muted">ok</span>
                    {/if}
                  </td>
                  <td class="num">{row.elapsed_ms}ms</td>
                  <td class="num good">{row.saved_fmt ?? row.saved}</td>
                  <td class="text">{row.reason || '—'}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
    </CollapsibleSection>

    <CollapsibleSection id="izakaya-agents" titleAttr="Agents in this coordination group; default sort is active-first">
      {#snippet heading()}Agents{/snippet}
      {#if !viewAgents.length}
        <p class="muted">{filtering ? 'No agents match this filter.' : 'No agents in this coordination group.'}</p>
      {:else}
        <div class="table-wrap">
          <table class="dash-table">
            <thead>
              <tr>
                {@render col(agentSort, 'Agent', 'agent')}
                {@render col(agentSort, 'State', 'state')}
                {@render col(agentSort, 'Task', 'task')}
                {@render col(agentSort, 'Claims', 'claims')}
                {@render col(agentSort, 'Dirty', 'dirty', true)}
                {@render col(agentSort, 'Seen', 'seen', true)}
                {@render col(agentSort, 'Lease', 'lease', true)}
              </tr>
            </thead>
            <tbody>
              {#each sortedAgents as agent}
                <tr>
                  <td>
                    <code>{agent.agent_id}</code>
                    {#if agent.role}<span class="muted"> {agent.role}</span>{/if}
                    {#if agent.branch}
                      <div class="muted tiny">{agent.branch}{agent.head ? ` @ ${agent.head}` : ''}</div>
                    {/if}
                  </td>
                  <td>
                    <span class="badge {agent.stale ? 'stale' : agent.state}">{agent.stale ? 'stale' : agent.state}</span>
                    {#if agent.stale}<div class="muted tiny">was {agent.state}</div>{/if}
                  </td>
                  <td class="text">
                    {agent.task || '—'}
                    {#if agent.summary}<div class="muted tiny">{agent.summary}</div>{/if}
                    {#if agent.checkpoint}<div class="muted tiny">checkpoint {agent.checkpoint}</div>{/if}
                  </td>
                  <td class="text">{claimLabel(agent)}</td>
                  <td class="num" class:warn={(agent.dirty_count ?? 0) > 0}>{agent.dirty_count ?? 0}</td>
                  <td class="muted">{ago(board.now, agent.last_seen_at)}</td>
                  <td class="muted">{leaseLeft(board.now, agent)}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
    </CollapsibleSection>

    {#if viewFindings.length}
      <CollapsibleSection id="izakaya-advisory" titleAttr="Advisory findings from the coordination board">
        {#snippet heading()}Advisory{/snippet}
        <div class="table-wrap">
          <table class="dash-table">
            <thead>
              <tr>
                {@render col(findingSort, 'Kind', 'kind')}
                {@render col(findingSort, 'Detail', 'detail')}
              </tr>
            </thead>
            <tbody>
              {#each sortedFindings as finding}
                <tr>
                  <td class="tool-name"><code>{finding.kind}</code></td>
                  <td class="text">{finding.detail}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      </CollapsibleSection>
    {/if}

    <CollapsibleSection id="izakaya-claims" titleAttr="Advisory path and symbol claims">
      {#snippet heading()}Claims{/snippet}
      {#if !claimRows.length}
        <p class="muted">{filtering ? 'No claims match this filter.' : 'No advisory claims on the board.'}</p>
      {:else}
        <div class="table-wrap">
          <table class="dash-table">
            <thead>
              <tr>
                {@render col(claimSort, 'Agent', 'agent')}
                {@render col(claimSort, 'State', 'state')}
                {@render col(claimSort, 'Path', 'path')}
                {@render col(claimSort, 'Symbols', 'symbols')}
                {@render col(claimSort, 'Intent', 'intent')}
              </tr>
            </thead>
            <tbody>
              {#each sortedClaims as row}
                <tr>
                  <td><code>{row.agent}</code></td>
                  <td><span class="badge {row.state}">{row.state}</span></td>
                  <td class="text">{row.path}</td>
                  <td class="text">{row.symbols}</td>
                  <td class="text">{row.intent}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
    </CollapsibleSection>

    <CollapsibleSection id="izakaya-handoffs" titleAttr="Handoffs offered in this coordination group">
      {#snippet heading()}Handoffs{/snippet}
      {#if !viewHandoffs.length}
        <p class="muted">{filtering ? 'No handoffs match this filter.' : 'No handoffs offered.'}</p>
      {:else}
        <div class="table-wrap">
          <table class="dash-table">
            <thead>
              <tr>
                {@render col(handoffSort, 'Id', 'id')}
                {@render col(handoffSort, 'Status', 'status')}
                {@render col(handoffSort, 'From', 'from')}
                {@render col(handoffSort, 'To', 'to')}
                {@render col(handoffSort, 'When', 'when', true)}
                {@render col(handoffSort, 'Summary', 'summary')}
              </tr>
            </thead>
            <tbody>
              {#each sortedHandoffs as handoff}
                <tr>
                  <td><code>{handoff.id}</code></td>
                  <td
                    ><span class="badge {handoff.derived_status}"
                      >{handoff.derived_status || handoff.status}</span
                    ></td
                  >
                  <td>{handoff.from || '—'}</td>
                  <td>{handoff.to || 'unclaimed'}</td>
                  <td class="muted">{ago(board.now, handoff.created_at)}</td>
                  <td class="text">
                    {handoff.summary || '—'}
                    {#if handoff.checkpoint}
                      <div class="muted tiny">checkpoint {handoff.checkpoint}</div>
                    {/if}
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
    </CollapsibleSection>

    <CollapsibleSection id="izakaya-journal" titleAttr="Retained journal tail">
      {#snippet heading()}Journal{/snippet}
      {#if !journalRows.length}
        <p class="muted">{filtering ? 'No journal events match this filter.' : 'No events yet.'}</p>
      {:else}
        <div class="table-wrap">
          <table class="dash-table">
            <thead>
              <tr>
                {@render col(journalSort, '#', 'seq', true)}
                {@render col(journalSort, 'When', 'when', true)}
                {@render col(journalSort, 'Kind', 'kind')}
                {@render col(journalSort, 'Agent', 'agent')}
                {@render col(journalSort, 'Result', 'result')}
              </tr>
            </thead>
            <tbody>
              {#each sortedJournal as event}
                <tr>
                  <td class="num">{event.seq}</td>
                  <td class="muted">{ago(board.now, event.ts)}</td>
                  <td class="tool-name"><code>{event.kind}</code></td>
                  <td class="tool-name"><code>{event.agent_id || '—'}</code></td>
                  <td class="text">{event.result || '—'}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
    </CollapsibleSection>

    <CollapsibleSection id="izakaya-notes" titleAttr="Notes left on the board">
      {#snippet heading()}Notes{/snippet}
      {#if !viewNotes.length}
        <p class="muted">{filtering ? 'No notes match this filter.' : 'No notes.'}</p>
      {:else}
        <div class="table-wrap">
          <table class="dash-table">
            <thead>
              <tr>
                {@render col(noteSort, '#', 'seq', true)}
                {@render col(noteSort, 'From', 'from')}
                {@render col(noteSort, 'To', 'to')}
                {@render col(noteSort, 'Ack', 'ack', true)}
                {@render col(noteSort, 'Body', 'body')}
              </tr>
            </thead>
            <tbody>
              {#each sortedNotes as note}
                <tr>
                  <td class="num">{note.seq}</td>
                  <td>{note.from || '—'}</td>
                  <td>{note.to || '—'}</td>
                  <td>{note.acked ? 'yes' : 'no'}</td>
                  <td class="text">{note.body || '—'}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
    </CollapsibleSection>
  {/if}
</div>

<style>
  .izakaya {
    display: flex;
    flex-direction: column;
    gap: 1rem;
    min-width: 0;
    max-width: 100%;
    width: 100%;
  }
  .izakaya :global(.collapsible),
  .izakaya :global(.collapsible-body) {
    min-width: 0;
    max-width: 100%;
  }
  .meta {
    margin: 0;
  }
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(8.5rem, 1fr));
    gap: 0.6rem;
  }
  .charts {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 0.75rem;
  }
  .chart-card h3 {
    margin-bottom: 0.55rem;
  }
  .card {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-elevated);
    padding: 0.65rem 0.75rem;
  }
  .card h3 {
    margin: 0 0 0.35rem;
    font-size: 0.78rem;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .metric {
    margin: 0;
    font-size: 1.35rem;
    font-variant-numeric: tabular-nums;
  }
  .metric.warn,
  p.warn {
    color: var(--warn);
  }
  p.warn {
    margin: 0;
  }
  .dash-table .warn {
    color: var(--warn);
  }
  .dash-table .chip.warn {
    border-color: var(--warn);
    color: var(--warn);
    background: rgba(212, 162, 74, 0.12);
  }
  .tiny {
    font-size: 0.78rem;
  }
  .badge {
    display: inline-block;
    border-radius: 999px;
    padding: 0.05rem 0.45rem;
    border: 1px solid var(--border);
    font-size: 0.75rem;
  }
  .badge.live_code {
    color: var(--ok);
    border-color: color-mix(in srgb, var(--ok) 55%, var(--border));
  }
  .badge.checked_in,
  .badge.offered {
    color: var(--accent-strong);
    border-color: color-mix(in srgb, var(--accent) 55%, var(--border));
  }
  .badge.suspended,
  .badge.stale,
  .badge.orphaned {
    color: var(--warn);
    border-color: color-mix(in srgb, var(--warn) 55%, var(--border));
  }
  .badge.checked_out,
  .badge.accepted {
    color: var(--muted);
  }
  @media (max-width: 800px) {
    .charts {
      grid-template-columns: 1fr;
    }
  }
</style>
