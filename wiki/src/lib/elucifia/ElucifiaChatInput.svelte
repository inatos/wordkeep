<script lang="ts">
  import type { ElucifiaBranch } from '../api';
  import Icon from './ElucifiaIcons.svelte';
  import MarkdownBar from './ElucifiaMarkdownBar.svelte';
  import ElucifiaSelect from './ElucifiaSelect.svelte';
  import { renderStudioMarkdown } from './markdown';
  let {
    draft = $bindable(''),
    model = $bindable(''),
    models,
    branch,
    branches,
    branchMenu,
    chips,
    dockHeight,
    streaming,
    autoSpeak,
    onbranch,
    onswitch,
    onrename,
    ondelete,
    onfork,
    onchip,
    onmodels,
    onspeak,
    onsend,
    onkey,
    onresize,
  }: {
    draft: string;
    model: string;
    models: string[];
    branch: string;
    branches: ElucifiaBranch[];
    branchMenu: boolean;
    chips: string[];
    dockHeight: number;
    streaming: boolean;
    autoSpeak: boolean;
    onbranch: () => void;
    onswitch: (id: string) => void;
    onrename: (id: string) => void;
    ondelete: (id: string) => void;
    onfork: () => void;
    onchip: (chip: string) => void;
    onmodels: () => void;
    onspeak: () => void;
    onsend: () => void;
    onkey: (event: KeyboardEvent) => void;
    onresize: (event: PointerEvent) => void;
  } = $props();
  let box: HTMLTextAreaElement | null = $state(null);
  let preview = $state(false);

  const chipHelp: Record<string, string> = {
    '@vault': 'Search the Atheneum and fold those passages into this send.',
    '@lorekeep': 'Search Lorekeep memory and fold the hits into this send.',
    '@continuity': 'Mark this send for the continuity register. State and continuity already ride in working memory.',
    '@peer': 'Mark the laptop peer on this send. The workshop shows whether that peer is up.',
    '@thread': 'Mark the open thread on this send. Recent turns already go with the prompt.',
    '@page': 'Mark the wiki page open behind the studio on this send.',
  };
</script>

<form class="dock" class:preview style:min-height="{preview ? Math.max(dockHeight, 320) : dockHeight}px" onsubmit={(e) => { e.preventDefault(); onsend(); }}>
  <div class="resize" onpointerdown={onresize} role="separator" aria-label="Resize composer"></div>
  <div class="toolbar">
    <button type="button" title="Conversation path. Switch, rename, or start a fork." onclick={onbranch}><Icon name="branch" size="0.85rem" /> {branch || 'main'}</button>
    {#each chips as chip}
      <button type="button" class="pill" title={chipHelp[chip] || chip} onclick={() => onchip(chip)}>{chip}</button>
    {/each}
    <span class="grow"></span>
    <ElucifiaSelect bind:value={model} options={models} up ariaLabel="Model" title="Model for the next send." />
    <button type="button" class="icon-btn" title="Open the full model list." aria-label="Open the full model list" onclick={onmodels}><Icon name="plus" size="0.9rem" /></button>
  </div>
  <MarkdownBar bind:value={draft} bind:preview textarea={box} />
  {#if branchMenu}
    <div class="menu">
      {#each branches as item}
        <button type="button" title="Switch the conversation onto this path." onclick={() => onswitch(item.id)}>{item.name || item.id}</button>
        <button type="button" title="Rename this path." onclick={() => onrename(item.id)}>Rename</button>
        {#if item.id !== 'main'}<button type="button" title="Delete this path. Main stays." onclick={() => ondelete(item.id)}>Delete</button>{/if}
      {/each}
      <button type="button" title="Fork the current messages into a new path." onclick={onfork}>New path</button>
    </div>
  {/if}
  {#if preview}
    <div class="read">{@html renderStudioMarkdown(draft)}</div>
  {:else}
    <textarea bind:this={box} bind:value={draft} placeholder="Write to Seraphina" onkeydown={onkey}></textarea>
  {/if}
  <div class="sendrow">
    <span title="Characters in the draft.">{draft.length}</span>
    <button type="button" class:on={autoSpeak} title="When on, replies are spoken as they arrive." onclick={onspeak}><Icon name="speak" size="0.85rem" /> Speak</button>
    <button type="submit" title="Send this message to Seraphina." disabled={streaming}><Icon name="send" size="0.85rem" /> Send</button>
  </div>
</form>

<style>
  .dock { position: relative; z-index: 3; box-sizing: border-box; flex: 0 0 auto; width: min(880px, calc(100% - 1rem)); margin: -1.15rem auto 0.15rem; display: flex; flex-direction: column; gap: 0.35rem; padding: 0.35rem 0.7rem 0.7rem; border-radius: 1.1rem; overflow: hidden; background: rgba(22, 15, 29, 0.77); backdrop-filter: blur(14px); border: 1px solid rgba(255, 140, 66, 0.35); box-shadow: 0 18px 40px rgba(0, 0, 0, 0.28); color: #f4ecf6; }
  .dock select, .dock textarea { color-scheme: dark; background: rgba(13, 9, 18, 0.4); color: #f4ecf6; }
  .resize { height: 0.45rem; cursor: ns-resize; border-radius: 99px; background: rgba(255, 140, 66, 0.45); }
  .toolbar, .sendrow { display: flex; flex-wrap: wrap; gap: 0.3rem; align-items: center; }
  button, select, textarea { border: 1px solid rgba(201, 111, 208, 0.35); background: rgba(13, 9, 18, 0.4); color: inherit; border-radius: 0.7rem; }
  textarea, .read { flex: 1 1 auto; min-height: 2.4rem; }
  textarea { resize: none; padding: 0.45rem 0.6rem; }
  .sendrow { flex: 0 0 auto; }
  .grow { flex: 1; }
  .on { border-color: #ff8c42; box-shadow: 0 0 16px rgba(255, 140, 66, 0.35); }
  .menu { display: grid; gap: 0.2rem; }
  .read { overflow: auto; color: #f3e6d4; padding: 0.2rem 0.1rem; }
  .sendrow button, .toolbar button { display: inline-flex; align-items: center; gap: 0.25rem; }
</style>
