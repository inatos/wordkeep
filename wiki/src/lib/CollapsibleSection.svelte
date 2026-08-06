<script lang="ts">
  import { untrack, type Snippet } from 'svelte';
  import { isSectionOpen, setSectionOpen } from './sectionCollapse';

  let {
    id,
    titleAttr = undefined,
    defaultOpen = true,
    children,
    heading,
  }: {
    id: string;
    titleAttr?: string;
    defaultOpen?: boolean;
    children: Snippet;
    heading: Snippet;
  } = $props();

  // Section ids are stable; capture localStorage once at create (not reactively).
  let open = $state(
    untrack(() => isSectionOpen(id, defaultOpen)),
  );

  function toggle() {
    open = !open;
    setSectionOpen(id, open);
  }
</script>

<section class="collapsible" class:collapsed={!open} data-section={id}>
  <button
    type="button"
    class="collapsible-toggle"
    title={titleAttr}
    aria-expanded={open}
    aria-controls={`section-body-${id}`}
    onclick={toggle}
  >
    <span class="chevron" aria-hidden="true">▾</span>
    <span class="heading">
      {@render heading()}
    </span>
  </button>
  {#if open}
    <div class="collapsible-body" id={`section-body-${id}`}>
      {@render children()}
    </div>
  {/if}
</section>

<style>
  .collapsible {
    margin: 0.15rem 0 0.35rem;
  }
  .collapsible-toggle {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    width: 100%;
    margin: 0.35rem 0 0.55rem;
    padding: 0.15rem 0.1rem;
    border: 0;
    background: transparent;
    color: var(--text);
    font: inherit;
    font-size: 0.95rem;
    font-weight: 600;
    text-align: left;
    cursor: pointer;
    border-radius: 6px;
  }
  .collapsible-toggle:hover {
    background: var(--accent-soft);
    color: var(--accent-strong);
  }
  .collapsible-toggle:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .chevron {
    display: inline-grid;
    place-items: center;
    width: 1rem;
    height: 1rem;
    flex: 0 0 auto;
    color: var(--accent-strong);
    font-size: 0.85rem;
    line-height: 1;
    transition: transform 0.16s var(--ease-snap);
  }
  .collapsed .chevron {
    transform: rotate(-90deg);
  }
  .heading {
    display: inline-flex;
    align-items: center;
    gap: 0.45rem;
    min-width: 0;
  }
  .heading :global(svg) {
    width: 1rem;
    height: 1rem;
    color: var(--accent-strong);
    flex: 0 0 auto;
  }
  .collapsible-body {
    min-width: 0;
  }
  :global(.card) > .collapsible {
    margin: 0;
  }
  :global(.card) > .collapsible .collapsible-toggle {
    margin-top: 0;
  }
</style>
