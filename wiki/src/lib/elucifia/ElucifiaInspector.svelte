<script lang="ts">
  import type { ElucifiaJob, ElucifiaTurn } from '../api';
  import BarChart from '../charts/BarChart.svelte';
  import DonutChart from '../charts/DonutChart.svelte';
  import Sparkline from '../charts/Sparkline.svelte';
  import type { BarDatum, DonutDatum, SparkPoint } from '../charts/utils';
  import CollapsibleSection from '../CollapsibleSection.svelte';
  import Icon from './ElucifiaIcons.svelte';
  import { renderStudioMarkdown } from './markdown';

  let {
    open,
    room,
    book,
    focusText,
    continuity,
    mood,
    energy,
    focus,
    glance,
    salience,
    loraPairs,
    busy,
    moods,
    reconcileLine,
    peer,
    voice,
    jobs,
    peerLines,
    peerUp,
    chatLane,
    voiceLane,
    model,
    streaming,
    speaking,
    gpuSeries,
    perfSeries,
    memoryCounts,
    turns,
    pinging,
    peerLatched,
    restarting,
    restartArmed,
    receipt,
    receiptTick,
    onroom,
    onclose,
    onopen,
    onmood,
    ondistill,
    ondream,
    onping,
    onrestart,
  }: {
    open: boolean;
    room: string;
    book: string;
    focusText: string;
    continuity: string;
    mood: string;
    energy: number;
    focus: number;
    glance: string;
    salience: string;
    loraPairs: number;
    busy: string;
    moods: string[];
    reconcileLine: string;
    peer: {
      reachable: boolean;
      ollama_up: boolean;
      model: string;
      context: number;
      vram_used_mib: number;
      vram_total_mib: number;
      gpu_pct: number;
    };
    voice: string;
    jobs: ElucifiaJob[];
    peerLines: string[];
    peerUp: boolean;
    chatLane: string;
    voiceLane: string;
    model: string;
    streaming: boolean;
    speaking: boolean;
    gpuSeries: SparkPoint[];
    perfSeries: SparkPoint[];
    memoryCounts: { dreams: number; episodic: number; semantic: number; procedural?: number; loraPairs: number };
    turns: ElucifiaTurn[];
    pinging: boolean;
    peerLatched: boolean;
    restarting: boolean;
    restartArmed: boolean;
    receipt: { lane: string; ok: boolean; detail: string }[];
    receiptTick: number;
    onroom: (room: 'vault' | 'workshop') => void;
    onclose: () => void;
    onopen: () => void;
    onmood: (mood: string, event: MouseEvent) => void;
    ondistill: (event: MouseEvent) => void;
    ondream: (event: MouseEvent) => void;
    onping: () => void;
    onrestart: () => void;
  } = $props();

  let sparkMode = $state<'gpu' | 'perf'>('gpu');

  const vramPct = $derived(
    peer.vram_total_mib > 0 ? Math.round((100 * peer.vram_used_mib) / peer.vram_total_mib) : 0,
  );
  const gauges = $derived<BarDatum[]>([
    {
      label: 'GPU',
      value: Math.max(peer.gpu_pct || 0, 0.001),
      color: '#ff8c42',
      title: `GPU load ${peer.gpu_pct || 0}%. Sampled from the laptop peer facts.`,
    },
    {
      label: 'VRAM',
      value: Math.max(vramPct, 0.001),
      color: '#c96fd0',
      title: `Video RAM ${peer.vram_used_mib} of ${peer.vram_total_mib} MiB (${vramPct}%).`,
    },
    {
      label: 'Vitality',
      value: Math.max(energy, 0.001),
      color: '#6bcf8e',
      title: `Vitality ${energy}. Presentation energy, the inverse of GPU load. Not a gameplay stat.`,
    },
    {
      label: 'Focus',
      value: Math.max(focus, 0.001),
      color: '#7ec8e3',
      title: `Focus ${focus}, from the open thread length (${turns.length} turns).`,
    },
  ]);
  const affectBars = $derived<BarDatum[]>([
    {
      label: 'Vitality',
      value: Math.max(energy, 0.001),
      color: '#6bcf8e',
      title: `Vitality ${energy}. Rises when the GPU is quiet.`,
    },
    {
      label: 'Focus',
      value: Math.max(focus, 0.001),
      color: '#7ec8e3',
      title: `Focus ${focus}. Climbs with the open thread, capped at 100.`,
    },
  ]);
  const jobMix = $derived<DonutDatum[]>(groupJobs(jobs));
  const memoryMix = $derived<DonutDatum[]>([
    { label: 'Episodic', value: memoryCounts.episodic, color: '#7ec8e3', title: `${memoryCounts.episodic} episodic journal notes.` },
    { label: 'Semantic', value: memoryCounts.semantic, color: '#c96fd0', title: `${memoryCounts.semantic} semantic canon notes.` },
    { label: 'Procedural', value: memoryCounts.procedural || 0, color: '#d4a24a', title: `${memoryCounts.procedural || 0} procedural routes.` },
    { label: 'Dreams', value: memoryCounts.dreams, color: '#ff8c42', title: `${memoryCounts.dreams} dream notes in Lorekeep reflections.` },
    { label: 'LoRA', value: memoryCounts.loraPairs, color: '#6bcf8e', title: `${memoryCounts.loraPairs} LoRA training pairs distilled from chat.` },
  ]);
  const spark = $derived(sparkMode === 'gpu' ? gpuSeries : perfSeries);
  const resident = $derived(peer.model || 'none');
  const voiceRemote = $derived(/qwen/i.test(`${voice} ${voiceLane}`));
  const peerTitle = $derived(
    peer.reachable
      ? `Peer link up. Copper can reach the Myridos laptop${peerUp ? '' : ' (status still catching up)'}. Chat lane ${chatLane || 'unknown'}.`
      : 'Peer link down. The laptop is silent, so chat stays on the local stub.',
  );
  const ollamaTitle = $derived(
    peer.ollama_up
      ? `Ollama is resident on ${resident}, context ${peer.context}. The menu still sends ${model}.`
      : `Ollama is quiet. The menu model is ${model}. ${reconcileLine}`,
  );
  const voiceTitle = $derived(
    `Voice spine ${voice || 'unset'}. ${voiceRemote ? 'Qwen3-TTS on the remote peer.' : 'Kokoro-82M on the local stub.'} Lane ${voiceLane || (voiceRemote ? 'remote' : 'local')}. ${speaking ? 'Speaking now.' : 'Idle.'}`,
  );
  const affectTitle = $derived(
    `Mood ${mood}. Vitality ${energy}. Focus ${focus}. ${salience ? `Last salience gate: ${salience}.` : 'No salience gate on the latest turn.'}`,
  );
  const tail = $derived(peerLines.slice(-6));

  function groupJobs(items: ElucifiaJob[]): DonutDatum[] {
    const colors: Record<string, string> = {
      done: '#6bcf8e',
      running: '#ff8c42',
      'lan-stub': '#7ec8e3',
      cancelled: '#e06b7a',
    };
    const counts = new Map<string, number>();
    for (const job of items) {
      const key = job.status || 'unknown';
      counts.set(key, (counts.get(key) || 0) + 1);
    }
    return [...counts.entries()].map(([label, value]) => ({
      label,
      value,
      color: colors[label] || '#c96fd0',
      title: `${value} Palaestra job${value === 1 ? '' : 's'} in ${label}.`,
    }));
  }

  function jobIcon(status?: string) {
    if (status === 'done') return 'check';
    if (status === 'running') return 'pulse';
    if (status === 'cancelled') return 'close';
    if (status === 'lan-stub') return 'link';
    return 'layers';
  }
</script>

<div class="rail" class:shut={!open} data-testid="elucifia-peer">
  <button class="edge" class:open type="button" onclick={() => (open ? onclose() : onopen())} title={open ? 'Collapse the systems inspector' : 'Open the systems inspector'} aria-label={open ? 'Collapse inspector' : 'Open inspector'}>
    <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M9 6l6 6-6 6" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" /></svg>
  </button>
  {#if open}
  <aside class="inspector">
  <div class="scroll">
  <div class="quick">
    <button type="button" class:on={room === 'vault'} title="Vault room: the active book, continuity, glance, and memory mix." onclick={() => onroom('vault')}>Vault</button>
    <button type="button" class:on={room === 'workshop'} title="Workshop room: GPU, throughput, Palaestra jobs, and the peer log." onclick={() => onroom('workshop')}>Workshop</button>
    <button type="button" class="ping" class:pulse={pinging} disabled={pinging} title="Ping the laptop peer now. The button stays pending until the probe finishes, then a result line names up or silent, the resident model, and the elapsed milliseconds." onclick={onping}>
      <Icon name="pulse" size="0.8rem" /> {pinging ? 'Pinging…' : 'Ping'}
    </button>
    <button type="button" class="ping" class:armed={restartArmed} class:pulse={restarting} disabled={restarting} title="First click arms a restart of the desktop feed, Ollama, Copper serve, and the voice spine. Second click runs it. Ping never kills the model." onclick={onrestart}>
      <Icon name="pulse" size="0.8rem" /> {restarting ? 'Restarting…' : restartArmed ? 'Restart now' : 'Restart'}
    </button>
    <button type="button" title="Close the inspector. The edge tab brings it back." onclick={onclose}>×</button>
  </div>

  <div class="systems" aria-label="Live systems">
    <span class="pill" title={peerTitle}>
      <i class="dot" class:wait={pinging || restarting} class:live={peer.reachable && !peerLatched} class:down={!peer.reachable || peerLatched}></i>
      <Icon name="link" size="0.8rem" /> Peer
    </span>
    <span class="pill" title={ollamaTitle}>
      <i class="dot" class:wait={pinging || restarting} class:live={peer.ollama_up && !peerLatched} class:down={!peer.ollama_up || peerLatched}></i>
      <Icon name="gpu" size="0.8rem" /> Ollama
    </span>
    <span class="pill" title={voiceTitle}>
      <i class="dot" class:live={speaking} class:warn={!speaking}></i>
      <Icon name="speak" size="0.8rem" /> Voice
    </span>
    <span class="pill" title={affectTitle}>
      <i class="dot live"></i>
      <Icon name="spark" size="0.8rem" /> {mood}
    </span>
  </div>
  {#if receipt.length}
    {#key receiptTick}
      <p class="receipt" class:bad={receipt.some((line) => !line.ok)} aria-live="polite" title="Probe or restart receipt. It fades after a few seconds. A failed peer stays red until a later success.">
        {#each receipt as line}
          <span>{line.detail}</span>
        {/each}
      </p>
    {/key}
  {/if}

  {#if room === 'vault'}
    <p class="lede">Active book</p>
    <strong class="book" title="The Lorekeep book this studio is writing into.">{book}</strong>
    <p class="lede" title="What the persona is attending to in the open thread.">{focusText || 'Focus follows the open thread.'}</p>
    {#if continuity}
      <div class="prose continuity" title="Continuity register for the active book.">{@html renderStudioMarkdown(continuity)}</div>
    {/if}
    <p class="lede">Glance</p>
    {#key glance}
      <div class="prose glance" title="Working buffer from the last context pack.">{@html renderStudioMarkdown(glance || 'The working buffer appears after the next refresh.')}</div>
    {/key}

    <CollapsibleSection id="elucifia-affect" titleAttr="Vitality and focus for the open persona. Presentation meters, not gameplay stats.">
      {#snippet heading()}
        <Icon name="pulse" size="0.85rem" /> Affect
      {/snippet}
      <div class="chart-fit">
        <BarChart data={affectBars} valueSuffix="%" chartTitle={affectTitle} ariaLabel="Vitality and focus" empty="Affect arrives with the next status poll." />
      </div>
    </CollapsibleSection>

    <CollapsibleSection id="elucifia-memory" titleAttr="How the vault splits episodic notes, semantic notes, dreams, and LoRA pairs.">
      {#snippet heading()}
        <Icon name="memory" size="0.85rem" /> Memory mix
      {/snippet}
      <div class="chart-fit">
        <DonutChart
          data={memoryMix}
          size={112}
          chartTitle="Memory mix across episodic notes, semantic notes, dreams, and LoRA pairs."
          centerLabel="notes"
          centerTitle="Notes and LoRA pairs currently on the shelf."
          valueLabel="items"
          ariaLabel="Memory mix"
          empty="The shelf is empty until journal or dreams load."
        />
      </div>
      <p class="mono" title={`${loraPairs} LoRA pairs distilled from chat.`}>LoRA pairs {loraPairs}</p>
    </CollapsibleSection>

    <div class="actions">
      <button type="button" disabled={busy === 'distill'} title="Distill the open thread into a LoRA pair and refresh the shelf." onclick={ondistill}><Icon name="distill" size="0.85rem" /> Distill</button>
      <button type="button" disabled={busy === 'dream'} title="Write a dream note from the open thread into Lorekeep reflections." onclick={ondream}><Icon name="dream" size="0.85rem" /> Dream</button>
    </div>
    <div class="moods">
      {#each moods as item}
        <button type="button" class:on={mood === item} title={`Set mood to ${item}. Presentation affect for the next reply.`} onclick={(e) => onmood(item, e)}>{item}</button>
      {/each}
    </div>
  {:else}
    <p class="lede" title="Whether the menu model matches the model resident on the laptop.">{reconcileLine}</p>

    <CollapsibleSection id="elucifia-compute" titleAttr="GPU load, video RAM, vitality, and focus. The number is the real percent. Bar length is relative to the largest gauge.">
      {#snippet heading()}
        <Icon name="gpu" size="0.85rem" /> Compute & vitals
      {/snippet}
      <div class="chart-fit">
        <BarChart
          data={gauges}
          valueSuffix="%"
          chartTitle="GPU load, video RAM used, vitality, and focus. Hover a row for MiB and the exact percent."
          ariaLabel="Compute and vitality gauges"
        />
      </div>
    </CollapsibleSection>

    <CollapsibleSection id="elucifia-spark" titleAttr="Rolling history. GPU percent from the peer heartbeat, or tokens per second from assistant turns.">
      {#snippet heading()}
        <Icon name="pulse" size="0.85rem" /> Live load
      {/snippet}
      <div class="spark-switch">
        <button type="button" class:on={sparkMode === 'gpu'} title="Last 24 GPU load samples from peer facts and the 4 second heartbeat." onclick={() => (sparkMode = 'gpu')}>GPU %</button>
        <button type="button" class:on={sparkMode === 'perf'} title="Tokens per second on recent assistant turns. Latency sits in the point tooltip." onclick={() => (sparkMode = 'perf')}>tok/s</button>
      </div>
      <div class="chart-fit">
        <Sparkline
          data={spark}
          width={248}
          height={128}
          stroke={sparkMode === 'gpu' ? '#ff8c42' : '#7ec8e3'}
          chartTitle={sparkMode === 'gpu' ? 'Live GPU load percent. Crosshair shows the sample.' : 'Turn throughput in tokens per second. The point label is latency in milliseconds.'}
          ariaLabel={sparkMode === 'gpu' ? 'GPU load history' : 'Turn throughput history'}
          valueLabel={sparkMode === 'gpu' ? 'GPU %' : 'tok/s'}
          yAxisLabel={sparkMode === 'gpu' ? 'GPU %' : 'tok/s'}
          empty={sparkMode === 'gpu' ? 'Waiting for the first peer sample.' : 'Throughput appears after a live reply reports tokens per second.'}
        />
      </div>
    </CollapsibleSection>

    <CollapsibleSection id="elucifia-jobs" titleAttr="Palaestra job states and the latest peer log lines.">
      {#snippet heading()}
        <Icon name="layers" size="0.85rem" /> Jobs & feed
      {/snippet}
      <div class="chart-fit">
        <DonutChart
          data={jobMix}
          size={112}
          chartTitle="Palaestra jobs by state: done, running, lan-stub, and cancelled."
          centerLabel="jobs"
          centerTitle="Jobs currently listed for this studio."
          valueLabel="jobs"
          ariaLabel="Palaestra job mix"
          empty="No Palaestra jobs yet."
        />
      </div>
      <ul class="jobs">
        {#each jobs as job}
          <li title={`${job.name}: ${job.status || 'unknown'}${job.note ? `. ${job.note}` : ''}.`}>
            <Icon name={jobIcon(job.status)} size="0.8rem" />
            <span>{job.name}</span>
            <em>{job.status || 'unknown'}</em>
          </li>
        {/each}
      </ul>
      <div class="feed-head">
        <span class="lede" title="Latest lines from the peer event stream. [DESKTOP] and [GIN] are access-log prefixes.">Peer feed</span>
        <button type="button" class:pulse={pinging} disabled={pinging} title="Fetch peer facts and status now." onclick={onping}>
          <Icon name="clock" size="0.75rem" /> Refresh
        </button>
      </div>
      <pre class="log" title={tail.length ? tail.join('\n') : 'No peer lines yet. The laptop log arrives over the event stream.'}>{tail.join('\n') || '[GIN] waiting for the laptop.'}</pre>
    </CollapsibleSection>
  {/if}
  </div>
  </aside>
  {/if}
</div>

<style>
  .rail { position: relative; z-index: 4; align-self: stretch; width: 19rem; min-width: 19rem; max-width: 19rem; height: 100%; min-height: 0; overflow: visible; transition: width 0.22s cubic-bezier(0.4, 0, 0.2, 1); }
  .rail.shut { width: 0; min-width: 0; max-width: 0; overflow: visible; pointer-events: none; }
  .inspector {
    --text: #f0e6f2;
    --muted: #a894ab;
    --border: rgba(201, 111, 208, 0.35);
    --bg-elevated: rgba(13, 9, 18, 0.55);
    --accent: #b153b8;
    --accent-strong: #c96fd0;
    --accent-soft: rgba(177, 83, 184, 0.18);
    --ok: #6bcf8e;
    --ease-snap: cubic-bezier(0.22, 1, 0.36, 1);
    display: flex;
    flex-direction: column;
    width: 100%;
    height: 100%;
    min-height: 0;
    overflow: hidden;
    padding: 0;
    background: rgba(22, 15, 29, 0.77);
    border-left: 1px solid rgba(201, 111, 208, 0.28);
    color: var(--text);
    font-family: "IBM Plex Sans", sans-serif;
  }
  .scroll { flex: 1; min-height: 0; overflow: auto; padding: 0.8rem 0.7rem 0.8rem 1rem; }
  .edge { position: absolute; left: -1.34rem; top: 3.6rem; width: 1.35rem; height: 2.75rem; padding: 0; display: grid; place-items: center; border-radius: 10px 0 0 10px; border: 1px solid rgba(201, 111, 208, 0.55); border-right: 0; background: rgba(22, 15, 29, 0.94); color: #ff8c42; z-index: 5; pointer-events: auto; transition: width 0.2s cubic-bezier(0.22, 1, 0.36, 1); }
  .edge:hover { width: 1.6rem; left: -1.59rem; }
  .edge svg { width: 1rem; height: 1rem; transform: rotate(180deg); transition: transform 0.36s cubic-bezier(0.22, 1, 0.36, 1); }
  .edge.open svg { transform: rotate(0deg); }
  .edge:hover svg { transform: translateX(-2px) rotate(180deg); }
  .edge.open:hover svg { transform: translateX(2px) rotate(0deg); }
  .rail.shut .edge { left: auto; right: 0; }
  .book { font-family: "Cormorant Garamond", Georgia, serif; font-size: 1.35rem; font-weight: 500; color: #fff4e8; }
  .prose { color: #f3e6d4; font-family: "Cormorant Garamond", Georgia, serif; line-height: 1.45; font-size: 0.98rem; }
  .prose.continuity { max-height: 7.5rem; overflow: auto; }
  .prose.glance { animation: vault-ink 0.42s ease; }
  .prose :global(h1), .prose :global(h2), .prose :global(h3), .prose :global(h4) {
    font-family: "Cormorant Garamond", Georgia, serif;
    font-weight: 500;
    color: #fff4e8;
    font-size: 1.05rem;
    margin: 0.7rem 0 0.25rem;
    border-bottom: 1px solid rgba(255, 140, 66, 0.35);
    padding-bottom: 0.12rem;
  }
  .prose :global(p) { margin: 0.35rem 0; }
  .prose :global(strong) { color: #fff4e8; }
  .prose :global(em) { color: #e4c6ea; }
  .prose :global(code), .prose :global(pre) { font-family: "IBM Plex Mono", monospace; color: #7ec8e3; font-size: 0.72rem; }
  .prose :global(pre) { white-space: pre-wrap; background: rgba(13, 9, 18, 0.55); border-radius: 0.5rem; padding: 0.4rem 0.5rem; }
  .prose :global(ul), .prose :global(ol) { margin: 0.3rem 0; padding-left: 1.1rem; }
  .prose :global(blockquote) { margin: 0.4rem 0; padding-left: 0.6rem; border-left: 2px solid rgba(201, 111, 208, 0.7); color: #e4c6ea; }
  .prose :global(hr) { border: 0; border-top: 1px solid rgba(201, 111, 208, 0.35); margin: 0.55rem 0; }
  .prose :global(a) { color: #7ec8e3; }
  @keyframes vault-ink { from { opacity: 0.2; } to { opacity: 1; } }
  .mono { font-family: "IBM Plex Mono", monospace; color: #7ec8e3; font-size: 0.78rem; }
  .quick, .moods, .actions, .spark-switch, .feed-head { display: flex; flex-wrap: wrap; gap: 0.3rem; align-items: center; }
  .feed-head { justify-content: space-between; margin-top: 0.35rem; }
  button { border: 1px solid rgba(201, 111, 208, 0.35); background: rgba(13, 9, 18, 0.42); color: inherit; border-radius: 0.7rem; }
  .on { border-color: #ff8c42; }
  .lede { color: #a894ab; font-size: 0.78rem; }
  .log { white-space: pre-wrap; font-family: "IBM Plex Mono", monospace; font-size: 0.72rem; color: #d7c6de; margin: 0.25rem 0 0; }
  .systems { display: grid; grid-template-columns: 1fr 1fr; gap: 0.35rem; margin: 0.55rem 0 0.65rem; }
  .pill { display: flex; align-items: center; gap: 0.32rem; min-width: 0; overflow: hidden; padding: 0.28rem 0.4rem; border-radius: 0.65rem; border: 1px solid rgba(201, 111, 208, 0.35); background: rgba(13, 9, 18, 0.42); font-size: 0.72rem; }
  .dot { width: 0.45rem; height: 0.45rem; border-radius: 50%; background: #6bcf8e; flex: 0 0 auto; transition: background-color 0.35s var(--ease-snap), box-shadow 0.35s var(--ease-snap); }
  .dot.live { animation: live-pulse 1.6s ease-in-out infinite; }
  .dot.down { background: #e06b7a; animation: none; }
  .dot.wait { background: #d4a24a; animation: none; }
  .dot.warn { background: #d4a24a; animation: none; }
  .receipt { display: grid; gap: 0.15rem; margin: 0 0 0.55rem; color: #d7f5e4; font-size: 0.72rem; animation: receipt-hold 4.4s ease forwards; }
  .receipt.bad { color: #f3c2c8; }
  .ping.armed { border-color: #ff8c42; color: #ff8c42; }
  .jobs { list-style: none; margin: 0.4rem 0 0; padding: 0; display: grid; gap: 0.28rem; }
  .jobs li { display: grid; grid-template-columns: auto 1fr auto; gap: 0.35rem; align-items: center; font-size: 0.75rem; }
  .jobs em { color: #a894ab; font-style: normal; }
  .chart-fit { width: 100%; max-width: 100%; overflow-x: auto; margin: 0.2rem 0 0.45rem; }
  .chart-fit :global(.bar-chart li) { grid-template-columns: minmax(3.2rem, 4.4rem) 1fr auto; }
  .ping.pulse, button.pulse { animation: ping-press 0.7s ease; }
  @keyframes live-pulse {
    0% { box-shadow: 0 0 0 0 rgba(107, 207, 142, 0.55); }
    70% { box-shadow: 0 0 0 6px rgba(107, 207, 142, 0); }
    100% { box-shadow: 0 0 0 0 rgba(107, 207, 142, 0); }
  }
  @keyframes ping-press {
    from { box-shadow: 0 0 0 0 rgba(255, 140, 66, 0.55); }
    to { box-shadow: 0 0 0 8px rgba(255, 140, 66, 0); }
  }
  @keyframes receipt-hold {
    0%, 82% { opacity: 1; }
    100% { opacity: 0; }
  }
  @media (prefers-reduced-motion: reduce) {
    .prose.glance, .dot.live, .ping.pulse, button.pulse { animation: none; }
    .dot { transition: none; }
    .receipt { animation: none; }
  }
</style>
