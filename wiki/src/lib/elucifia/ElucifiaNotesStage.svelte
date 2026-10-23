<script lang="ts">
  import { setMarkdownTags } from '../frontmatter';
  import { frontmatterTags, renderStudioMarkdown } from './markdown';
  import Icon from './ElucifiaIcons.svelte';
  import TagBar from './ElucifiaTagBar.svelte';
  import MarkdownBar from './ElucifiaMarkdownBar.svelte';

  type Note = { id: string; title: string; body?: string; preview?: string; tags?: string[]; lane?: string; mood?: string };
  type ShelfTag = { name: string; color?: string };

  let {
    kind,
    notes,
    selectedId = '',
    palette = [],
    onsave,
    ondelete,
    ondream,
    ondistill,
  }: {
    kind: 'dreams' | 'journal';
    notes: Note[];
    selectedId?: string;
    palette?: ShelfTag[];
    onsave: (payload: { id: string; title: string; markdown: string; lane: string }) => void;
    ondelete: (payload: { id: string; lane: string }) => void;
    ondream: () => void;
    ondistill: () => void;
  } = $props();

  let lane = $state('episodic');
  let seenKind = $state('');
  let mode = $state<'read' | 'edit' | 'split'>('split');
  let noteId = $state('');
  let title = $state('');
  let body = $state('');
  let tags = $state<string[]>([]);
  let box: HTMLTextAreaElement | null = $state(null);
  let preview = $state(false);
  let editingNew = $state(false);

  const visible = $derived(notes.filter((note) => (kind === 'dreams' ? true : (note.lane || 'episodic') === lane)));
  const headings = $derived(
    body
      .split('\n')
      .filter((line) => /^#{1,3}\s+/.test(line))
      .map((line) => line.replace(/^#+\s+/, '')),
  );
  const words = $derived(body.trim() ? body.trim().split(/\s+/).length : 0);

  function open(note: Note) {
    editingNew = false;
    noteId = note.id;
    title = note.title;
    body = note.body || note.preview || '';
    tags = note.tags?.length ? note.tags : frontmatterTags(body);
    if (note.lane) lane = note.lane;
  }

  $effect(() => {
    if (kind !== seenKind) {
      seenKind = kind;
      lane = kind === 'dreams' ? 'reflections' : 'episodic';
      editingNew = false;
      noteId = '';
    }
    if (editingNew) return;
    const wanted = selectedId || noteId || visible[0]?.id || '';
    if (!wanted) return;
    const found = notes.find((note) => note.id === wanted);
    if (!found || found.id === noteId) return;
    open(found);
  });

  function fresh() {
    editingNew = true;
    noteId = '';
    title = kind === 'dreams' ? 'New reflection' : 'New page';
    body = `# ${title}\n\n`;
    tags = [];
    mode = 'edit';
  }

  function save() {
    const payload = { id: noteId, title: title.trim() || 'Untitled', markdown: setMarkdownTags(body, tags), lane };
    editingNew = false;
    onsave(payload);
  }
</script>

<section class="notes">
  <header>
    <h2>{kind === 'dreams' ? 'Dreams' : 'Journal'}</h2>
    <div class="row">
      {#if kind === 'journal'}
        <button type="button" class:on={lane === 'episodic'} onclick={() => (lane = 'episodic')}>Episodic</button>
        <button type="button" class:on={lane === 'semantic'} onclick={() => (lane = 'semantic')}>Semantic</button>
      {:else}
        <button type="button" class="on">Reflections</button>
      {/if}
      <button type="button" onclick={fresh}><Icon name="plus" size="0.85rem" /> New note</button>
      <button type="button" onclick={ondream}><Icon name="dream" size="0.85rem" /> Trigger dream</button>
      <button type="button" onclick={ondistill}><Icon name="distill" size="0.85rem" /> Distill thread</button>
    </div>
  </header>
  <div class="split">
    <aside>
      {#each visible as note}
        <button type="button" class:on={note.id === noteId} onclick={() => open(note)}>
          <strong>{note.title}</strong>
          <span>{note.mood || note.lane || kind}</span>
        </button>
      {/each}
    </aside>
    <article>
      <input class="title" bind:value={title} aria-label="Note title" />
      <TagBar {tags} {palette} onchange={(next) => (tags = next)} />
      <div class="modes">
        <button type="button" class:on={mode === 'read'} onclick={() => (mode = 'read')}>Read</button>
        <button type="button" class:on={mode === 'edit'} onclick={() => (mode = 'edit')}>Edit</button>
        <button type="button" class:on={mode === 'split'} onclick={() => (mode = 'split')}>Split</button>
        <button type="button" onclick={save}>Save</button>
        {#if noteId}<button type="button" class="danger" onclick={() => ondelete({ id: noteId, lane })}>Delete</button>{/if}
        <span class="count">{words} words · {body.length} chars</span>
      </div>
      {#if mode !== 'read'}
        <MarkdownBar bind:value={body} bind:preview textarea={box} />
      {/if}
      <div class="page" class:split={mode === 'split'}>
        {#if mode !== 'read'}
          <textarea bind:this={box} bind:value={body} aria-label="Note body"></textarea>
        {/if}
        {#if mode !== 'edit' || preview}
          <div class="read">{@html renderStudioMarkdown(setMarkdownTags(body, tags))}</div>
        {/if}
      </div>
      {#if headings.length}
        <ol class="toc">{#each headings as heading}<li>{heading}</li>{/each}</ol>
      {/if}
    </article>
  </div>
</section>

<style>
  .notes { display: flex; flex-direction: column; min-height: 0; height: 100%; color: #fff4e8; gap: 0.5rem; }
  header, .row, .modes { display: flex; flex-wrap: wrap; gap: 0.35rem; align-items: center; }
  h2, .title { font-family: "Cormorant Garamond", Georgia, serif; font-weight: 500; }
  h2 { margin: 0; }
  .title { width: 100%; background: transparent; border: 0; color: #fff4e8; font-size: 1.4rem; }
  button, textarea { border: 1px solid rgba(201, 111, 208, 0.35); background: rgba(13, 9, 18, 0.72); color: #f4ecf6; border-radius: 0.7rem; }
  button { display: inline-flex; align-items: center; gap: 0.25rem; }
  .on { border-color: #ff8c42; }
  .danger { color: #e06b7a; }
  .split { display: grid; grid-template-columns: 14rem minmax(0, 1fr); gap: 0.7rem; min-height: 0; flex: 1; }
  aside { overflow: auto; display: grid; align-content: start; gap: 0.3rem; }
  aside button { display: grid; text-align: left; padding: 0.4rem 0.5rem; }
  aside span, .count { color: #a894ab; font-size: 0.75rem; }
  article { display: flex; flex-direction: column; gap: 0.4rem; min-height: 0; min-width: 0; }
  .page { display: grid; gap: 0.4rem; min-height: 0; flex: 1; }
  .page.split { grid-template-columns: 1fr 1fr; }
  textarea, .read { min-height: 12rem; overflow: auto; padding: 0.6rem; }
  textarea { font-family: "IBM Plex Mono", monospace; resize: vertical; }
  .read { color: #f3e6d4; line-height: 1.6; }
  .toc { color: #7ec8e3; font-size: 0.8rem; }
</style>
