<script lang="ts">
  import { comboNav } from '../combo';
  import Icon from './ElucifiaIcons.svelte';

  type Option = { value: string; label: string };
  let {
    value = $bindable(''),
    options = [],
    label = '',
    up = false,
    ariaLabel = 'Choose',
    title = '',
    startOpen = false,
    onchange,
  }: {
    value?: string;
    options?: Array<string | Option>;
    label?: string;
    up?: boolean;
    ariaLabel?: string;
    title?: string;
    startOpen?: boolean;
    onchange?: (value: string) => void;
  } = $props();

  let open = $state(startOpen);
  let query = $state('');
  let highlight = $state(0);
  let box: HTMLInputElement | null = $state(null);

  const items = $derived(
    options.map((option) => (typeof option === 'string' ? { value: option, label: option } : option)),
  );
  const shown = $derived(
    items.filter((option) => {
      const q = query.trim().toLowerCase();
      if (!q) return true;
      return option.label.toLowerCase().includes(q) || option.value.toLowerCase().includes(q);
    }),
  );
  const current = $derived(items.find((option) => option.value === value));

  function toggle() {
    open = !open;
    query = '';
    highlight = Math.max(0, shown.findIndex((option) => option.value === value));
    queueMicrotask(() => box?.focus());
  }

  function pick(next: string) {
    value = next;
    open = false;
    query = '';
    onchange?.(next);
  }

  function onKey(event: KeyboardEvent) {
    const nav = comboNav(event, { open: true, highlight, count: shown.length });
    if (!nav) return;
    highlight = nav.highlight;
    if (nav.choose != null) {
      const option = shown[nav.choose];
      if (option) pick(option.value);
    }
    if (nav.close) open = false;
  }
</script>

<div class="combo" class:up>
  {#if label}<span class="label">{label}</span>{/if}
  <button type="button" class="face" aria-label={ariaLabel} title={title || ariaLabel} aria-expanded={open} onclick={toggle}>
    <span>{current?.label || value || 'Choose'}</span>
    <Icon name="search" size="0.8rem" />
  </button>
  {#if open}
    <div class="pop">
      <input bind:this={box} bind:value={query} placeholder="Search" aria-label={`Search ${ariaLabel}`} onkeydown={onKey} />
      <ul role="listbox">
        {#each shown as option, index (option.value)}
          <li>
            <button
              type="button"
              class:on={option.value === value || index === highlight}
              onmouseenter={() => (highlight = index)}
              onclick={() => pick(option.value)}
            >
              {#if option.value === value}<Icon name="check" size="0.75rem" />{/if}
              {option.label}
            </button>
          </li>
        {:else}
          <li class="empty">No match</li>
        {/each}
      </ul>
    </div>
  {/if}
</div>

<style>
  .combo { position: relative; display: grid; gap: 0.2rem; min-width: 8rem; }
  .combo.up { position: static; }
  .label { color: #a894ab; font-size: 0.75rem; }
  .face, input, .pop button {
    border: 1px solid rgba(201, 111, 208, 0.45);
    background: rgba(13, 9, 18, 0.84);
    color: #fff4e8;
    border-radius: 0.7rem;
  }
  .face { display: flex; align-items: center; justify-content: space-between; gap: 0.4rem; padding: 0.35rem 0.55rem; cursor: pointer; }
  .pop {
    position: absolute;
    z-index: 8;
    left: 0;
    right: 0;
    top: calc(100% + 0.25rem);
    display: grid;
    gap: 0.3rem;
    padding: 0.4rem;
    background: rgba(18, 12, 24, 0.96);
    border: 1px solid rgba(201, 111, 208, 0.55);
    border-radius: 0.8rem;
    box-shadow: 0 14px 34px rgba(0, 0, 0, 0.65);
  }
  .up .pop { top: 3.6rem; right: 0.65rem; bottom: 0.55rem; left: 0.65rem; width: auto; max-height: none; overflow: auto; }
  input { padding: 0.3rem 0.45rem; }
  ul { list-style: none; margin: 0; padding: 0; max-height: 12rem; overflow: auto; }
  .pop button { display: flex; align-items: center; gap: 0.3rem; width: 100%; text-align: left; padding: 0.3rem 0.45rem; }
  .pop button.on { border-color: #ff8c42; color: #ff8c42; }
  .empty { color: #a894ab; padding: 0.3rem; }
</style>
