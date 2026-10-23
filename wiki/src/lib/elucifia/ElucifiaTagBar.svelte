<script lang="ts">
  import { defaultTagColor, normalizeHex, resolveTagColor } from '../tagColors';
  import Icon from './ElucifiaIcons.svelte';

  type ShelfTag = { name: string; color?: string };
  let {
    tags = [],
    palette = [],
    onchange,
    onpalette,
  }: {
    tags?: string[];
    palette?: ShelfTag[];
    onchange?: (tags: string[]) => void;
    onpalette?: (op: string, name: string, color: string, next?: string) => void;
  } = $props();

  let adding = $state(false);
  let draft = $state('');
  let renaming = $state('');
  let renameValue = $state('');
  let copied = $state('');

  function colorOf(name: string) {
    return palette.find((tag) => tag.name === name)?.color || resolveTagColor({}, name) || defaultTagColor(name);
  }

  function setColor(name: string, color: string) {
    onpalette?.('upsert', name, normalizeHex(color));
  }

  function commitRename(name: string) {
    const next = renameValue.trim();
    renaming = '';
    if (!next || next === name) return;
    onpalette?.('rename', name, colorOf(name), next);
    onchange?.(tags.map((tag) => (tag === name ? next : tag)));
  }

  async function copyHex(name: string) {
    const hex = colorOf(name);
    try {
      await navigator.clipboard.writeText(hex);
      copied = name;
      setTimeout(() => (copied = ''), 900);
    } catch {
      copied = '';
    }
  }

  function addTag() {
    const name = draft.trim();
    draft = '';
    adding = false;
    if (!name || tags.includes(name)) return;
    if (!palette.some((tag) => tag.name === name)) onpalette?.('upsert', name, defaultTagColor(name));
    onchange?.([...tags, name]);
  }
</script>

<div class="tag-editor">
  <div class="tag-row">
    {#each tags as tag (tag)}
      <span class="chip tag-chip" style:--tag-color={colorOf(tag)}>
        <label class="tag-swatch" style:background={colorOf(tag)}>
          <input type="color" value={colorOf(tag)} aria-label={`Color for ${tag}`} onchange={(event) => setColor(tag, (event.currentTarget as HTMLInputElement).value)} />
        </label>
        {#if renaming === tag}
          <input class="tag-name" bind:value={renameValue} onkeydown={(event) => event.key === 'Enter' && commitRename(tag)} onblur={() => commitRename(tag)} />
        {:else}
          <button type="button" class="tag-name" onclick={() => { renaming = tag; renameValue = tag; }}>{tag}</button>
        {/if}
        <input class="tag-hex" value={colorOf(tag)} aria-label={`Hex for ${tag}`} onchange={(event) => setColor(tag, (event.currentTarget as HTMLInputElement).value)} />
        <button type="button" class="icon-btn" title="Copy hex" aria-label="Copy hex" onclick={() => copyHex(tag)}>
          <Icon name={copied === tag ? 'check' : 'copy'} size="0.85rem" />
        </button>
        <button type="button" class="tag-x" title="Remove tag" aria-label={`Remove ${tag}`} onclick={() => onchange?.(tags.filter((item) => item !== tag))}>×</button>
      </span>
    {/each}
    {#if adding}
      <form class="tag-add" onsubmit={(event) => { event.preventDefault(); addTag(); }}>
        <input bind:value={draft} placeholder="tag" aria-label="New tag" />
        <button type="submit" class="icon-btn" title="Add tag" aria-label="Add tag"><Icon name="plus" size="0.9rem" /></button>
      </form>
    {:else}
      <button type="button" class="icon-btn tag-add" title="Add tag" aria-label="Add tag" onclick={() => (adding = true)}><Icon name="plus" size="0.9rem" /></button>
    {/if}
  </div>
</div>

<style>
  .tag-editor { display: flex; min-width: 0; }
  .tag-row { display: flex; flex-wrap: wrap; gap: 0.35rem; align-items: center; }
  .tag-chip {
    display: inline-flex;
    align-items: center;
    gap: 0.28rem;
    border: 1px solid color-mix(in srgb, var(--tag-color, #c96fd0) 55%, transparent);
    background: color-mix(in srgb, var(--tag-color, #c96fd0) 16%, rgba(13, 9, 18, 0.7));
    color: #fff4e8;
    border-radius: 999px;
    padding: 0.12rem 0.35rem 0.12rem 0.2rem;
  }
  .tag-swatch {
    width: 0.85rem;
    height: 0.85rem;
    border-radius: 50%;
    overflow: hidden;
    border: 1px solid rgba(255, 244, 232, 0.4);
    cursor: pointer;
  }
  .tag-swatch input { opacity: 0; width: 100%; height: 100%; cursor: pointer; }
  .tag-name, .tag-hex, .tag-add input {
    background: transparent;
    border: 0;
    color: inherit;
    font: inherit;
    min-width: 0;
  }
  button.tag-name { cursor: text; padding: 0; }
  .tag-hex { width: 4.6rem; font-family: 'IBM Plex Mono', monospace; font-size: 0.68rem; color: #7ec8e3; }
  .tag-x, .icon-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: 0;
    background: transparent;
    color: #fff4e8;
    cursor: pointer;
    padding: 0.1rem;
  }
  .tag-add { display: inline-flex; align-items: center; gap: 0.2rem; }
  .tag-add input { width: 5rem; border-bottom: 1px solid rgba(126, 200, 227, 0.4); }
</style>
