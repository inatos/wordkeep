<script lang="ts">
  import { applyWrap } from './markdown';
  import Icon from './ElucifiaIcons.svelte';

  let {
    value = $bindable(''),
    preview = $bindable(false),
    textarea = null as HTMLTextAreaElement | null,
  }: {
    value?: string;
    preview?: boolean;
    textarea?: HTMLTextAreaElement | null;
  } = $props();

  function wrap(before: string, after = before) {
    const box = textarea;
    const start = box?.selectionStart ?? value.length;
    const end = box?.selectionEnd ?? value.length;
    const next = applyWrap(value, start, end, before, after);
    value = next.value;
    queueMicrotask(() => {
      box?.focus();
      box?.setSelectionRange(next.start, next.end);
    });
  }

  const tools = [
    { name: 'bold', title: 'Bold. Wraps the selection in **.', before: '**', after: '**' },
    { name: 'italic', title: 'Italic. Wraps the selection in *.', before: '*', after: '*' },
    { name: 'code', title: 'Inline code. Wraps the selection in backticks.', before: '`', after: '`' },
    { name: 'code', title: 'Code block. Wraps the selection in a fenced block.', before: '```\n', after: '\n```' },
    { name: 'heading', title: 'Heading. Prefixes the selection with ##.', before: '## ', after: '' },
    { name: 'list', title: 'Bullet list. Prefixes the selection with a dash.', before: '- ', after: '' },
    { name: 'quote', title: 'Quote. Prefixes the selection with >.', before: '> ', after: '' },
    { name: 'link', title: 'Link. Wraps the selection as link text.', before: '[', after: '](https://)' },
  ];
</script>

<div class="md-bar">
  {#each tools as tool}
    <button type="button" class="icon-btn" title={tool.title} aria-label={tool.title} onclick={() => wrap(tool.before, tool.after)}>
      <Icon name={tool.name} size="0.95rem" />
    </button>
  {/each}
  <button type="button" class="icon-btn preview" class:on={preview} title={preview ? 'Return to the editor.' : 'Show the rendered draft.'} aria-label={preview ? 'Return to the editor' : 'Show the rendered draft'} onclick={() => (preview = !preview)}>
    <Icon name={preview ? 'pencil' : 'eye'} size="0.95rem" />
    <span>{preview ? 'Write' : 'Preview'}</span>
  </button>
</div>

<style>
  .md-bar { display: flex; flex-wrap: wrap; gap: 0.2rem; align-items: center; }
  .icon-btn {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
    border: 1px solid rgba(201, 111, 208, 0.28);
    background: rgba(13, 9, 18, 0.55);
    color: #fff4e8;
    border-radius: 8px;
    padding: 0.22rem 0.35rem;
    cursor: pointer;
  }
  .icon-btn:hover, .preview.on { color: #ff8c42; border-color: rgba(255, 140, 66, 0.7); }
  .preview span { font-size: 0.72rem; }
</style>
