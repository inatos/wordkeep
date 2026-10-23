<script lang="ts">
  import MarkdownBar from './ElucifiaMarkdownBar.svelte';
  import { renderStudioMarkdown } from './markdown';
  type Persona = { id: string; name: string; kind: string; description: string; system_prompt: string; bubble: string };
  let {
    modal,
    modalTitle,
    modalBody,
    models,
    personaEdit = $bindable(null as Persona | null),
    editTurn = $bindable(''),
    book,
    focusText,
    mood,
    lightbox,
    lightboxSrc = '',
    palette,
    paletteQuery = $bindable(''),
    paletteHits,
    markdown,
    onclose,
    onmodel,
    onsave,
    onsavepersona,
    onlightbox,
    onpalette,
    onpick,
  }: {
    modal: string;
    modalTitle: string;
    modalBody: string;
    models: string[];
    personaEdit: Persona | null;
    editTurn: string;
    book: string;
    focusText: string;
    mood: string;
    lightbox: string;
    lightboxSrc?: string;
    palette: boolean;
    paletteQuery: string;
    paletteHits: string[];
    markdown: (text: string) => string;
    onclose: () => void;
    onmodel: (model: string) => void;
    onsave: () => void;
    onsavepersona: () => void;
    onlightbox: () => void;
    onpalette: () => void;
    onpick: (hit: string) => void;
  } = $props();
  let editBox: HTMLTextAreaElement | null = $state(null);
  let editPreview = $state(false);
</script>

{#if modal}
  <div class="veil" role="presentation" onclick={onclose}>
    <div class="glass" role="dialog" onclick={(e) => e.stopPropagation()}>
      <header><h2>{modal === 'models' ? 'Models' : modal === 'persona' ? 'Persona' : modal === 'message' ? 'Edit and fork' : modalTitle || 'Seraphina Elucifia'}</h2><button type="button" onclick={onclose}>Close</button></header>
      {#if modal === 'models'}
        <ul>{#each models as item}<li><button type="button" onclick={() => onmodel(item)}>{item}</button></li>{/each}</ul>
      {:else if modal === 'message'}
        <MarkdownBar bind:value={editTurn} bind:preview={editPreview} textarea={editBox} />
        {#if editPreview}
          <div class="prose">{@html renderStudioMarkdown(editTurn)}</div>
        {:else}
          <textarea bind:this={editBox} bind:value={editTurn}></textarea>
        {/if}
        <button type="button" onclick={onsave}>Save and truncate</button>
      {:else if modal === 'persona' && personaEdit}
        <input bind:value={personaEdit.name} />
        <input bind:value={personaEdit.bubble} />
        <textarea bind:value={personaEdit.system_prompt}></textarea>
        <button type="button" onclick={onsavepersona}>Save persona</button>
      {:else if modal === 'profile'}
        <p>{mood}</p>
        <p>{book} · {focusText}</p>
        <div class="prose">{@html markdown(modalBody || 'Lorekeep of the Atheneum.')}</div>
      {:else}
        <div class="prose">{@html markdown(modalBody)}</div>
      {/if}
    </div>
  </div>
{/if}
{#if lightbox}
  <div class="veil" role="presentation" onclick={onlightbox}><div class="glass" onclick={(event) => event.stopPropagation()}>{#if lightboxSrc}<img src={lightboxSrc} alt={lightbox} />{:else}<p>{lightbox}</p>{/if}<button type="button" onclick={onlightbox}>Close</button></div></div>
{/if}
{#if palette}
  <div class="veil" role="presentation" onclick={onpalette}>
    <div class="glass" onclick={(e) => e.stopPropagation()}>
      <input placeholder="Jump" bind:value={paletteQuery} />
      {#each paletteHits as hit}
        <button type="button" onclick={() => onpick(hit)}>{hit}</button>
      {/each}
    </div>
  </div>
{/if}

<style>
  .veil { position: fixed; inset: 0; background: rgba(8, 5, 12, 0.62); display: grid; place-items: center; z-index: 20; }
  .glass { width: min(640px, 92vw); max-height: 80vh; overflow: auto; padding: 1rem 1.1rem; background: rgba(22, 15, 29, 0.88); border: 1px solid rgba(201, 111, 208, 0.35); border-radius: 1rem; backdrop-filter: blur(16px); color: #f4ecf6; }
  h2, .prose { font-family: "Cormorant Garamond", Georgia, serif; font-weight: 500; }
  .prose { font-size: 1.15rem; line-height: 1.45; }
  .glass img { width: 100%; max-height: 70vh; object-fit: contain; border-radius: 0.7rem; }
  button, input, textarea { border: 1px solid rgba(201, 111, 208, 0.35); background: rgba(13, 9, 18, 0.55); color: inherit; border-radius: 0.7rem; }
  textarea, input { width: 100%; margin: 0.3rem 0; padding: 0.45rem; }
</style>
