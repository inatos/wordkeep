<script lang="ts">
  import Icon from './ElucifiaIcons.svelte';
  type ViewItem = { id: string; label: string; glyph: string; letter?: string };
  type Named = { id: string; name: string; color?: string; kind?: string };
  type Note = { id: string; title: string; preview?: string; body?: string; mood?: string };
  type ThreadCard = { id: string; title?: string; branches?: unknown[]; turns?: unknown[] };

  let {
    railOpen,
    view,
    views,
    query = $bindable(''),
    newGroup = $bindable(''),
    tagFilter = $bindable(''),
    groups,
    tags,
    threads,
    threadId,
    dreams,
    journal,
    personas,
    peerLines,
    model = $bindable(''),
    models,
    showThinking = $bindable(true),
    sendOnEnter = $bindable(true),
    menuThread,
    mood,
    ontoggle,
    onview,
    onnewchat,
    onaddgroup,
    ondropgroup,
    onselect,
    onmenu,
    onmove,
    ondelete,
    onnote,
    onpersona,
    onnewpersona,
    onmodels,
    onclear,
  }: {
    railOpen: boolean;
    view: string;
    views: ViewItem[];
    query: string;
    newGroup: string;
    tagFilter: string;
    groups: Named[];
    tags: Named[];
    threads: ThreadCard[];
    threadId: string;
    dreams: Note[];
    journal: Note[];
    personas: Named[];
    peerLines: string[];
    model: string;
    models: string[];
    showThinking: boolean;
    sendOnEnter: boolean;
    menuThread: string;
    mood: string;
    ontoggle: () => void;
    onview: (id: string) => void;
    onnewchat: () => void;
    onaddgroup: () => void;
    ondropgroup: (id: string) => void;
    onselect: (id: string) => void;
    onmenu: (id: string) => void;
    onmove: (id: string, groupId: string) => void;
    ondelete: (id: string) => void;
    onnote: (note: Note, kind: string) => void;
    onpersona: (name: string) => void;
    onnewpersona: () => void;
    onmodels: () => void;
    onclear: () => void;
  } = $props();

  function pickView(id: string) {
    if (id === view) {
      ontoggle();
      return;
    }
    onview(id);
    if (!railOpen) ontoggle();
  }
</script>

<div class="rail" class:collapsed={!railOpen}>
  <button class="edge" class:open={railOpen} type="button" onclick={ontoggle} aria-label={railOpen ? 'Collapse drawer' : 'Open drawer'}>
    <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M9 6l6 6-6 6" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" /></svg>
  </button>
  <aside class="drawer">
  <div class="scroll">
  <div class="tabs" class:orbs={!railOpen}>
    {#each views as item}
      <button
        type="button"
        class:sao-orb={!railOpen}
        class:on={view === item.id}
        onclick={() => pickView(item.id)}
        title={item.label}
        aria-label={item.label}
      >
        {#if railOpen}
          <span>{item.glyph}</span>
          <em>{item.label}</em>
        {:else}
          {item.letter || item.label.slice(0, 1)}
        {/if}
      </button>
    {/each}
  </div>
  {#if railOpen}
    {#if view === 'chats'}
      <button class="new" type="button" onclick={onnewchat}><Icon name="chat" size="0.9rem" /> New chat</button>
      <label class="find-wrap"><Icon name="search" size="0.85rem" /><input class="find" placeholder="Search chats" bind:value={query} /></label>
      <div class="row">
        <input class="field" placeholder="Group" bind:value={newGroup} />
        <button type="button" class="icon-btn" title="Add group" aria-label="Add group" onclick={onaddgroup}><Icon name="plus" size="1rem" /></button>
      </div>
      {#each groups as group}
        <div class="chip-row"><i style:background={group.color}></i>{group.name}<button type="button" onclick={() => ondropgroup(group.id)}>×</button></div>
      {/each}
      <div class="filters">
        <button type="button" class:on={!tagFilter} onclick={() => (tagFilter = '')}>All</button>
        {#each tags as tag}
          <button type="button" class="chip tag-chip" class:on={tagFilter === tag.name} style:--tag-color={tag.color || '#c96fd0'} onclick={() => (tagFilter = tag.name)}>{tag.name}</button>
        {/each}
      </div>
      {#each threads as thread}
        <button class="card" type="button" class:on={thread.id === threadId} onclick={() => onselect(thread.id)} oncontextmenu={(e) => { e.preventDefault(); onmenu(thread.id); }}>
          <strong>{thread.title || thread.id}</strong>
          <span>{thread.branches?.length || 1} paths · {thread.turns?.length || 0}</span>
        </button>
      {/each}
      {#if menuThread}
        <div class="menu">
          {#each groups as group}
            <button type="button" onclick={() => onmove(menuThread, group.id)}>Move to {group.name}</button>
          {/each}
          <button type="button" onclick={() => ondelete(menuThread)}>Delete chat</button>
        </div>
      {/if}
    {:else if view === 'dreams' || view === 'journal'}
      <p class="lede">{view === 'dreams' ? 'Reflections from the vault' : 'Episodic pages'}</p>
      {#each (view === 'dreams' ? dreams : journal) as note}
        <button class="card" type="button" onclick={() => onnote(note, view === 'dreams' ? 'dream' : 'journal')}>
          <strong>{note.title}</strong>
          <span>{note.mood || mood}</span>
        </button>
      {/each}
    {:else if view === 'personas'}
      <button class="new" type="button" onclick={onnewpersona}>✦ New persona</button>
      {#each personas as persona}
        <button class="card" type="button" onclick={() => onpersona(persona.name)}>
          <strong>{persona.kind === 'user' ? '⚡' : '✦'} {persona.name}</strong>
          <span>{persona.kind}</span>
        </button>
      {/each}
    {:else if view === 'logs'}
      <pre class="log">{#each peerLines as line}[DESKTOP] {line}
{/each}{#if !peerLines.length}[GIN] waiting for the laptop{/if}</pre>
    {:else if view === 'config'}
      <p class="lede">Settings live on the center deck.</p>
      <button type="button" onclick={onmodels}>Manage models</button>
      <button class="danger" type="button" onclick={onclear}>Clear current chat</button>
    {:else}
      <p class="lede">Canvas opens on the stage. The drawer stays here.</p>
    {/if}
  {/if}
  </div>
  </aside>
</div>

<style>
  .rail { position: relative; z-index: 4; align-self: stretch; width: 18rem; height: 100%; min-height: 0; overflow: visible; transition: width 0.22s cubic-bezier(0.4, 0, 0.2, 1); }
  .rail.collapsed { width: 4.35rem; }
  .drawer { display: flex; flex-direction: column; width: 100%; height: 100%; min-height: 0; padding: 0; background: rgba(22, 15, 29, 0.88); backdrop-filter: blur(14px); border-right: 1px solid rgba(201, 111, 208, 0.28); overflow: hidden; color: #f4ecf6; font-family: "IBM Plex Sans", sans-serif; }
  .scroll { flex: 1; min-height: 0; overflow: auto; padding: 0.8rem; }
  .rail.collapsed .scroll { padding-inline: 0.4rem; }
  .edge { position: absolute; left: calc(100% - 1px); top: 1.15rem; width: 1.35rem; height: 2.75rem; padding: 0; display: grid; place-items: center; border-radius: 0 10px 10px 0; border: 1px solid rgba(201, 111, 208, 0.55); border-left: 0; background: rgba(22, 15, 29, 0.94); color: #ff8c42; z-index: 5; pointer-events: auto; transition: width 0.2s cubic-bezier(0.22, 1, 0.36, 1); }
  .edge:hover { width: 1.6rem; }
  .edge svg { width: 1rem; height: 1rem; transition: transform 0.36s cubic-bezier(0.22, 1, 0.36, 1); }
  .edge.open svg { transform: rotate(180deg); }
  .edge:hover svg { transform: translateX(2px); }
  .edge.open:hover svg { transform: translateX(-2px) rotate(180deg); }
  .tabs { display: grid; grid-template-columns: 1fr 1fr; gap: 0.35rem; margin-bottom: 0.7rem; }
  .tabs.orbs { grid-template-columns: 1fr; justify-items: center; }
  button, .find, .field, select { border: 1px solid rgba(201, 111, 208, 0.35); background: rgba(13, 9, 18, 0.72); color: #f4ecf6; border-radius: 0.7rem; color-scheme: dark; }
  .tabs button { display: grid; justify-items: center; padding: 0.35rem; font-size: 0.72rem; }
  .tabs em { font-style: normal; }
  .on { border-color: #ff8c42; box-shadow: 0 0 16px rgba(255, 140, 66, 0.35); }
  .new, .find, .field, select { width: 100%; margin: 0.25rem 0; padding: 0.45rem 0.6rem; }
  .new, .find-wrap, .icon-btn { display: inline-flex; align-items: center; gap: 0.35rem; }
  .find-wrap { width: 100%; }
  .find-wrap .find { border: 0; background: transparent; margin: 0; padding: 0; flex: 1; }
  .icon-btn { width: auto; padding: 0.4rem; }
  .tag-chip { border-color: color-mix(in srgb, var(--tag-color, #c96fd0) 65%, transparent); background: color-mix(in srgb, var(--tag-color, #c96fd0) 18%, rgba(13, 9, 18, 0.72)); }
  .row { flex: 1; }
  .row .field { flex: 1; min-width: 0; }
  .row, .chip-row, .filters { display: flex; flex-wrap: wrap; gap: 0.3rem; align-items: center; }
  .card { display: grid; width: 100%; text-align: left; padding: 0.45rem 0.55rem; margin: 0.25rem 0; }
  .card span, .lede { color: #cbb8d0; font-size: 0.78rem; }
  .chip-row i { width: 0.55rem; height: 0.55rem; border-radius: 99px; display: inline-block; }
  .log { white-space: pre-wrap; font-family: "IBM Plex Mono", monospace; font-size: 0.75rem; }
  .danger { color: #e06b7a; }
  .switch { display: flex; gap: 0.4rem; align-items: center; }
  .menu { display: grid; gap: 0.2rem; }
</style>
