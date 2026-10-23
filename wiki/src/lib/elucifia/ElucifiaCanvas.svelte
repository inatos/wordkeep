<script lang="ts">
  import ElucifiaBlueprintGraph from './ElucifiaBlueprintGraph.svelte';
  import type { GraphDoc } from './blueprint-graph';
  type Tab = { id: string; label: string };
  type Node = { id: string; label: string; x: number; y: number };
  type Item = { job: string; name: string; kind: string; ext?: string; bytes?: number; modified?: number };
  let {
    tabs,
    canvasTab,
    presets,
    preset,
    busy,
    nodes,
    jobPrompt = $bindable(''),
    aspect = $bindable('1:1'),
    steps = $bindable(20),
    seed = $bindable(0),
    showAdvanced = $bindable(false),
    gallery,
    models = [],
    jobNames = [],
    savedGraphs = [],
    thumb = (_job: string, _name: string) => '',
    ontab,
    onpreset,
    onqueue,
    onrun,
    onnodedown,
    onnodemove,
    onnodeup,
    onopen,
    onpersist,
    onpromote,
  }: {
    tabs: Tab[];
    canvasTab: string;
    presets: { name: string; job: string }[];
    preset: string;
    busy: string;
    nodes: Node[];
    jobPrompt: string;
    aspect: string;
    steps: number;
    seed: number;
    showAdvanced: boolean;
    gallery: Item[];
    models?: string[];
    jobNames?: string[];
    savedGraphs?: GraphDoc[];
    thumb?: (job: string, name: string) => string;
    ontab: (id: string) => void;
    onpreset: (name: string) => void;
    onqueue: (event: MouseEvent) => void;
    onrun: (job: string) => void;
    onnodedown: (id: string, event: PointerEvent) => void;
    onnodemove: (event: PointerEvent) => void;
    onnodeup: () => void;
    onopen: (name: string) => void;
    onpersist?: (body: Record<string, unknown>) => void;
    onpromote?: (title: string, body: string) => void;
  } = $props();
  function stamp(value?: number) {
    if (!value) return '';
    return new Date(value * 1000).toLocaleString();
  }
  function sizeLabel(bytes?: number) {
    const n = bytes ?? 0;
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
    return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  }
</script>

<div class="canvas"><header class="title"><h1>✦ Canvas ~ Concept Majik Atelier ✦</h1></header>
<div class="canvas-tabs">
  {#each tabs as tab}
    <button type="button" class:on={canvasTab === tab.id} onclick={() => ontab(tab.id)}>{tab.label}</button>
  {/each}
</div>
{#if canvasTab === 'graph'}
  <ElucifiaBlueprintGraph
    {models}
    jobs={jobNames}
    saved={savedGraphs}
    onrun={(job, prompt) => { jobPrompt = prompt; onrun(job); }}
    {onpersist}
    {onpromote}
  />
{:else if canvasTab === 'gen2d' || canvasTab === 'gen3d'}
  <div class="split">
    <form class="form" onsubmit={(e) => { e.preventDefault(); onrun(canvasTab === 'gen2d' ? 'image-scout' : 'hunyuan-img2mesh'); }}>
      <p class="badge">{canvasTab === 'gen2d' ? 'image-scout' : 'hunyuan-img2mesh'}</p>
      <textarea bind:value={jobPrompt} placeholder="Describe the concept"></textarea>
      <div class="filters">
        {#each ['1:1', '16:9', '9:16'] as ratio}
          <button type="button" class:on={aspect === ratio} onclick={() => (aspect = ratio)}>{ratio}</button>
        {/each}
      </div>
      <button type="button" onclick={() => (showAdvanced = !showAdvanced)}>▼ Advanced options</button>
      {#if showAdvanced}
        <label>Steps <input type="number" bind:value={steps} min="1" max="40" /></label>
        <label>Seed <input type="number" bind:value={seed} /></label>
      {/if}
      {#if busy}<div class="bar"></div>{/if}
      <button type="submit">Generate</button>
    </form>
    <div class="preview"><p>{gallery.find((item) => (canvasTab === 'gen3d' ? item.kind === 'mesh' : item.kind === 'image'))?.name || 'The preview waits on a Palaestra receipt.'}</p></div>
  </div>
{:else}
  <div class="gallery">
    <p class="count">{gallery.filter((item) => (canvasTab === 'gal3d' ? item.kind !== 'image' : item.kind === 'image')).length} files</p>
    {#each gallery.filter((item) => (canvasTab === 'gal3d' ? item.kind !== 'image' : item.kind === 'image')) as item}
      <article class="card">
        <div class="frame">
          {#if item.kind === 'image'}
            <img src={thumb(item.job, item.name)} alt={item.name} loading="lazy" />
          {:else}
            <span class="glyph">{item.ext || item.kind}</span>
          {/if}
        </div>
        <strong title={item.name}>{item.name}</strong>
        <span>{item.ext || item.kind} · {sizeLabel(item.bytes)}</span>
        <span>{item.job}</span>
        <time>{stamp(item.modified)}</time>
        <button type="button" onclick={() => onopen(`${item.job}/${item.name}`)}>Open</button>
      </article>
    {/each}
  </div>
{/if}
</div>

<style>
  .canvas { display: flex; flex-direction: column; min-height: 0; height: 100%; overflow: auto; gap: 0.45rem; padding-bottom: 0.6rem; }
  .title { text-align: center; }
  h1 { font-family: "Cormorant Garamond", Georgia, serif; font-weight: 500; color: #fff4e8; }
  .canvas-tabs, .presets, .filters, .inspector { display: flex; flex-wrap: wrap; gap: 0.3rem; align-items: center; }
  button, textarea, input, select { border: 1px solid rgba(201, 111, 208, 0.35); background: rgba(13, 9, 18, 0.72); color: #f4ecf6; border-radius: 0.7rem; color-scheme: dark; }
  .inspector { padding: 0.4rem 0.55rem; border-radius: 0.8rem; background: rgba(22, 15, 29, 0.88); border: 1px solid rgba(126, 200, 227, 0.35); color: #7ec8e3; }
  .card { min-width: 0; overflow: hidden; display: flex; flex-direction: column; gap: 0.45rem; padding: 0.75rem; text-align: left; background: rgba(22, 15, 29, 0.88); border: 1px solid rgba(201, 111, 208, 0.35); border-radius: 1rem; }
  .card strong { display: block; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 100%; }
  .frame { min-height: 8rem; display: grid; place-items: center; overflow: hidden; border-radius: 0.7rem; background: rgba(13, 9, 18, 0.72); color: #a894ab; }
  .frame img { width: 100%; height: 8rem; object-fit: cover; }
  .glyph { font-family: "IBM Plex Mono", monospace; color: #7ec8e3; }
  time, .card span { color: #7ec8e3; font-family: "IBM Plex Mono", monospace; font-size: 0.75rem; }
  .on { border-color: #ff8c42; box-shadow: 0 0 16px rgba(255, 140, 66, 0.35); }
  textarea { width: 100%; min-height: 6rem; padding: 0.5rem; }
  .graph { position: relative; min-height: 420px; border-radius: 1rem; background: rgba(13, 9, 18, 0.45); overflow: hidden; }
  .graph svg { position: absolute; inset: 0; width: 100%; height: 100%; }
  .graph path { fill: none; stroke: #c96fd0; stroke-width: 0.6; vector-effect: non-scaling-stroke; }
  .graph path.pulse { stroke: #ff8c42; }
  .node { position: absolute; max-width: 9rem; padding: 0.45rem; }
  .split { display: grid; grid-template-columns: 1fr 1fr; gap: 0.8rem; }
  .preview, .gallery button { background: rgba(22, 15, 29, 0.88); border: 1px solid rgba(201, 111, 208, 0.35); border-radius: 1rem; }
  .preview { min-height: 320px; display: grid; place-items: center; padding: 1rem; }
  .gallery { display: grid; grid-template-columns: repeat(auto-fill, minmax(240px, 1fr)); gap: 0.85rem; padding: 0.25rem 0 1rem; }
  .count { margin: 0; color: #a894ab; grid-column: 1 / -1; }
  .bar { height: 0.3rem; background: linear-gradient(90deg, #ff8c42, #c96fd0); }
  @media (max-width: 1100px) { .split { grid-template-columns: 1fr; } }
</style>
