<script lang="ts">
  import Icon from './ElucifiaIcons.svelte';
  import MarkdownBar from './ElucifiaMarkdownBar.svelte';
  import ElucifiaSelect from './ElucifiaSelect.svelte';
  import { renderStudioMarkdown } from './markdown';

  type Persona = {
    id: string;
    name: string;
    kind: string;
    description: string;
    system_prompt: string;
    bubble: string;
    voice?: { model?: string; pitch?: number; rate?: number };
  };

  let {
    personas,
    onsave,
    onspeak,
  }: {
    personas: Persona[];
    onsave: (persona: Persona) => void;
    onspeak: (text: string) => void;
  } = $props();

  const voices = [
    { id: 'Qwen/Qwen3-TTS-12Hz-0.6B-CustomVoice', label: 'Qwen3-TTS' },
    { id: 'ResembleAI/chatterbox', label: 'Chatterbox' },
    { id: 'hexgrad/Kokoro-82M', label: 'Kokoro-82M' },
  ];
  let selected = $state(0);
  let draft = $state<Persona | null>(null);
  let box: HTMLTextAreaElement | null = $state(null);
  let preview = $state(false);

  $effect(() => {
    const source = personas[selected] || personas[0];
    if (!source) {
      draft = null;
      return;
    }
    if (draft?.id === source.id) return;
    draft = {
      ...source,
      voice: { model: source.voice?.model || 'Qwen/Qwen3-TTS-12Hz-0.6B-CustomVoice', pitch: source.voice?.pitch ?? 1, rate: source.voice?.rate ?? 1 },
    };
  });
</script>

<section class="personas">
  <header><h2>Personas</h2></header>
  <div class="split">
    <div class="roster">
      {#each personas as persona, index}
        <button type="button" class="card" class:on={index === selected} style:--bubble={persona.bubble || '#c96fd0'} onclick={() => { selected = index; draft = null; }}>
          <strong>{persona.kind === 'user' ? '⚡' : '✦'} {persona.name}</strong>
          <span>{persona.kind}</span>
        </button>
      {/each}
    </div>
    {#if draft}
      <form onsubmit={(event) => { event.preventDefault(); onsave(draft!); }}>
        <label>Name <input bind:value={draft.name} /></label>
        <label>Kind
          <ElucifiaSelect bind:value={draft.kind} options={[{ value: 'ai', label: 'ai' }, { value: 'user', label: 'user' }]} ariaLabel="Kind" />
        </label>
        <label>Bubble
          <span class="swatch"><input type="color" bind:value={draft.bubble} aria-label="Bubble color" /></span>
          <input class="hex" bind:value={draft.bubble} />
        </label>
        <label>Voice
          <ElucifiaSelect bind:value={draft.voice!.model} options={voices.map((voice) => ({ value: voice.id, label: voice.label }))} ariaLabel="Voice" />
        </label>
        <label>Pitch <input type="range" min="0.6" max="1.6" step="0.05" bind:value={draft.voice!.pitch} /></label>
        <label>Rate <input type="range" min="0.6" max="1.6" step="0.05" bind:value={draft.voice!.rate} /></label>
        <label>Description <textarea bind:value={draft.description} rows="3"></textarea></label>
        <p>System prompt</p>
        <MarkdownBar bind:value={draft.system_prompt} bind:preview textarea={box} />
        {#if preview}
          <div class="read">{@html renderStudioMarkdown(draft.system_prompt)}</div>
        {:else}
          <textarea bind:this={box} bind:value={draft.system_prompt} rows="8" aria-label="System prompt"></textarea>
        {/if}
        <div class="row">
          <button type="submit">Save persona</button>
          <button type="button" onclick={() => onspeak(draft?.system_prompt || draft?.name || '')}><Icon name="speak" size="0.85rem" /> Test speak</button>
        </div>
      </form>
    {/if}
  </div>
</section>

<style>
  .personas { display: flex; flex-direction: column; min-height: 0; height: 100%; color: #fff4e8; gap: 0.5rem; }
  h2 { font-family: "Cormorant Garamond", Georgia, serif; font-weight: 500; margin: 0; }
  .split { display: grid; grid-template-columns: 16rem minmax(0, 1fr); gap: 0.8rem; min-height: 0; flex: 1; }
  .roster, form { overflow: auto; display: grid; align-content: start; gap: 0.45rem; }
  .card { text-align: left; border: 1px solid color-mix(in srgb, var(--bubble) 55%, transparent); background: rgba(13, 9, 18, 0.72); color: inherit; border-radius: 0.8rem; padding: 0.55rem; }
  .card span { display: block; color: #a894ab; font-size: 0.75rem; }
  .on { box-shadow: 0 0 16px rgba(255, 140, 66, 0.35); }
  label { display: grid; gap: 0.2rem; color: #a894ab; font-size: 0.78rem; }
  input, select, textarea, button { border: 1px solid rgba(201, 111, 208, 0.35); background: rgba(13, 9, 18, 0.72); color: #f4ecf6; border-radius: 0.7rem; color-scheme: dark; }
  input, select, textarea { padding: 0.4rem 0.5rem; }
  .hex { font-family: "IBM Plex Mono", monospace; color: #7ec8e3; }
  .swatch input { width: 2.2rem; height: 1.6rem; padding: 0; }
  .row { display: flex; gap: 0.4rem; }
  button { display: inline-flex; align-items: center; gap: 0.25rem; }
  .read { color: #f3e6d4; line-height: 1.6; }
</style>
