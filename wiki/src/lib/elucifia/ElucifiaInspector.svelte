<script lang="ts">
  import type { ElucifiaJob } from '../api';
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
    onroom,
    onclose,
    onopen,
    onmood,
    ondistill,
    ondream,
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
    peer: { vram_used_mib: number; vram_total_mib: number; gpu_pct: number };
    voice: string;
    jobs: ElucifiaJob[];
    peerLines: string[];
    onroom: (room: 'vault' | 'workshop') => void;
    onclose: () => void;
    onopen: () => void;
    onmood: (mood: string, event: MouseEvent) => void;
    ondistill: (event: MouseEvent) => void;
    ondream: (event: MouseEvent) => void;
  } = $props();
</script>

<div class="rail" class:shut={!open} data-testid="elucifia-peer">
  <button class="edge" class:open type="button" onclick={() => (open ? onclose() : onopen())} aria-label={open ? 'Collapse inspector' : 'Open inspector'}>
    <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M9 6l6 6-6 6" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" /></svg>
  </button>
  {#if open}
  <aside class="inspector">
  <div class="scroll">
  <div class="quick">
    <button type="button" class:on={room === 'vault'} onclick={() => onroom('vault')}>Vault</button>
    <button type="button" class:on={room === 'workshop'} onclick={() => onroom('workshop')}>Workshop</button>
    <button type="button" onclick={onclose}>×</button>
  </div>
  {#if room === 'vault'}
    <p class="lede">Active book</p>
    <strong class="book">{book}</strong>
    <p class="lede">{focusText || 'Focus follows the open thread.'}</p>
    {#if continuity}
      <div class="prose continuity">{@html renderStudioMarkdown(continuity)}</div>
    {/if}
    <p>Mood {mood} · vitality {energy}</p>
    <p>Focus {focus}</p>
    <p class="lede">Glance</p>
    {#key glance}
      <div class="prose glance">{@html renderStudioMarkdown(glance || 'The working buffer appears after the next refresh.')}</div>
    {/key}
    {#if salience}<p>Salience {salience}</p>{/if}
    <p class="mono">LoRA pairs {loraPairs}</p>
    <button type="button" disabled={busy === 'distill'} onclick={ondistill}><Icon name="distill" size="0.85rem" /> Distill</button>
    <button type="button" disabled={busy === 'dream'} onclick={ondream}><Icon name="dream" size="0.85rem" /> Dream</button>
    <div class="moods">
      {#each moods as item}
        <button type="button" class:on={mood === item} onclick={(e) => onmood(item, e)}>{item}</button>
      {/each}
    </div>
  {:else}
    <p>{reconcileLine}</p>
    <p class="mono">VRAM {peer.vram_used_mib} / {peer.vram_total_mib} MiB · GPU {peer.gpu_pct}%</p>
    <p>Voice {voice}</p>
    <ul>{#each jobs as job}<li>{job.name} · {job.status}</li>{/each}</ul>
    <pre class="log">{peerLines.slice(-6).join('\n')}</pre>
  {/if}
  </div>
  </aside>
  {/if}
</div>

<style>
  .rail { position: relative; z-index: 4; align-self: stretch; width: 16rem; min-width: 0; height: 100%; min-height: 0; overflow: visible; transition: width 0.22s cubic-bezier(0.4, 0, 0.2, 1); }
  .rail.shut { width: 0; max-width: 0; overflow: visible; pointer-events: none; }
  .inspector { display: flex; flex-direction: column; width: 100%; height: 100%; min-height: 0; overflow: hidden; padding: 0; background: rgba(22, 15, 29, 0.88); border-left: 1px solid rgba(201, 111, 208, 0.28); color: #f4ecf6; font-family: "IBM Plex Sans", sans-serif; }
  .scroll { flex: 1; min-height: 0; overflow: auto; padding: 0.8rem 0.8rem 0.8rem 1.1rem; }
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
  @media (prefers-reduced-motion: reduce) { .prose.glance { animation: none; } }
  .mono { font-family: "IBM Plex Mono", monospace; color: #7ec8e3; font-size: 0.78rem; }
  .quick, .moods { display: flex; flex-wrap: wrap; gap: 0.3rem; }
  button { border: 1px solid rgba(201, 111, 208, 0.35); background: rgba(13, 9, 18, 0.55); color: inherit; border-radius: 0.7rem; }
  .on { border-color: #ff8c42; }
  .lede { color: #a894ab; font-size: 0.78rem; }
  .log { white-space: pre-wrap; font-family: "IBM Plex Mono", monospace; font-size: 0.75rem; }
</style>
