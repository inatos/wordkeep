<script lang="ts">
  import Icon from './ElucifiaIcons.svelte';
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
    <div class="glass" class:persona={modal === 'profile' || modal === 'persona'} role="dialog" aria-label={modalTitle || 'Seraphina Elucifia'} onclick={(e) => e.stopPropagation()}>
      <header>
        <h2>{modal === 'models' ? 'Models' : modal === 'persona' ? 'Persona' : modal === 'message' ? 'Edit message' : modalTitle || 'Seraphina Elucifia'}</h2>
        <button type="button" class="close" class:icon={modal === 'profile' || modal === 'persona'} title="Close this card." aria-label="Close" onclick={onclose}>
          {#if modal === 'profile' || modal === 'persona'}
            <Icon name="close" size="1rem" />
          {:else}
            Close
          {/if}
        </button>
      </header>
      {#if modal === 'models'}
        <ul>{#each models as item}<li><button type="button" onclick={() => onmodel(item)}>{item}</button></li>{/each}</ul>
      {:else if modal === 'message'}
        <p class="hint">Saving rewrites this message and drops every reply after it, including forks that continued from here.</p>
        <MarkdownBar bind:value={editTurn} bind:preview={editPreview} textarea={editBox} />
        {#if editPreview}
          <div class="prose">{@html renderStudioMarkdown(editTurn)}</div>
        {:else}
          <textarea bind:this={editBox} bind:value={editTurn}></textarea>
        {/if}
        <button type="button" class="save" title="Rewrite this message and drop the chain after it." onclick={onsave}>Save and drop the chain</button>
      {:else if modal === 'persona' && personaEdit}
        <div class="orb" style:--bubble={personaEdit.bubble || '#ff8c42'} aria-hidden="true">✦</div>
        <label>Name <input bind:value={personaEdit.name} /></label>
        <label>Bubble <input bind:value={personaEdit.bubble} /></label>
        <label>Voice <textarea bind:value={personaEdit.system_prompt}></textarea></label>
        <button type="button" class="save" title="Keep this persona." onclick={onsavepersona}>Save persona</button>
      {:else if modal === 'profile'}
        <div class="orb" aria-hidden="true">✦</div>
        <p class="mood-pill">{mood}</p>
        <p class="book-line">{book}</p>
        {#if focusText}<p class="focus">{focusText}</p>{/if}
        <div class="prose canon">{@html markdown(modalBody || 'Lorekeep of the Atheneum.')}</div>
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
  .veil { position: fixed; inset: 0; background: rgba(8, 5, 12, 0.5); display: grid; place-items: center; z-index: 20; }
  .glass { width: min(640px, 92vw); max-height: 80vh; overflow: auto; padding: 1rem 1.1rem 1.2rem; background: #140e18; border: 1px solid rgba(201, 111, 208, 0.4); border-radius: 1.15rem; color: #f4ecf6; box-shadow: 0 24px 60px rgba(0, 0, 0, 0.45); animation: card-rise 0.28s cubic-bezier(0.22, 1, 0.36, 1); }
  .glass.persona { border-color: rgba(255, 140, 66, 0.55); box-shadow: 0 0 36px rgba(255, 140, 66, 0.22), 0 24px 60px rgba(0, 0, 0, 0.45); text-align: center; }
  header { display: flex; align-items: center; justify-content: space-between; gap: 0.8rem; margin-bottom: 0.35rem; }
  h2, .prose, .book-line { font-family: "Cormorant Garamond", Georgia, serif; font-weight: 500; }
  h2 { margin: 0; color: #fff4e8; font-size: 1.7rem; }
  .prose { font-size: 1.15rem; line-height: 1.45; text-align: left; }
  .glass img { width: 100%; max-height: 70vh; object-fit: contain; border-radius: 0.7rem; }
  button, input, textarea { border: 1px solid rgba(201, 111, 208, 0.35); background: rgba(13, 9, 18, 0.42); color: inherit; border-radius: 0.7rem; }
  textarea, input { width: 100%; margin: 0.3rem 0; padding: 0.45rem; }
  label { display: grid; gap: 0.15rem; text-align: left; color: #cbb8d0; font-size: 0.78rem; margin-top: 0.35rem; }
  .close { padding: 0.25rem 0.7rem; }
  .close.icon { display: grid; place-items: center; width: 2rem; height: 2rem; padding: 0; border-radius: 999px; background: transparent; }
  .save { margin-top: 0.7rem; border-color: #ff8c42; color: #fff4e8; box-shadow: 0 0 16px rgba(255, 140, 66, 0.28); }
  .hint { color: #cbb8d0; font-size: 0.82rem; text-align: left; }
  .orb { width: 3.4rem; height: 3.4rem; margin: 0.35rem auto 0.55rem; display: grid; place-items: center; border-radius: 999px; background: var(--bubble, #ff8c42); color: #1a0d12; font-size: 1.4rem; box-shadow: 0 0 28px 6px var(--bubble, #ff8c42); }
  .mood-pill { display: inline-flex; margin: 0.15rem 0 0.35rem; padding: 0.12rem 0.75rem; border-radius: 999px; border: 1px solid #ff8c42; color: #ff8c42; letter-spacing: 0.04em; }
  .book-line { margin: 0.2rem 0; color: #fff4e8; font-size: 1.35rem; }
  .focus { margin: 0.15rem 0 0.7rem; color: #cbb8d0; }
  .canon { margin-top: 0.4rem; padding: 0.85rem 1rem; border-radius: 0.9rem; border: 1px solid rgba(201, 111, 208, 0.35); border-left: 3px solid rgba(255, 140, 66, 0.8); background: rgba(13, 9, 18, 0.35); }
  @keyframes card-rise { from { opacity: 0; transform: translateY(10px) scale(0.98); } to { opacity: 1; transform: none; } }
  @media (prefers-reduced-motion: reduce) { .glass { animation: none; } }
</style>
