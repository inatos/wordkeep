<script lang="ts">
  import type { ElucifiaTurn } from '../api';
  import Icon from './ElucifiaIcons.svelte';
  import TagBar from './ElucifiaTagBar.svelte';
  let {
    editingTitle,
    titleDraft = $bindable(''),
    title,
    branch,
    room,
    book,
    turns,
    streaming,
    thinkSeconds,
    showThinking,
    model,
    mood,
    lane,
    markdown,
    tags = [],
    palette = [],
    ontags = () => {},
    ontagpalette = () => {},
    onbranch = () => {},
    onedit,
    onsave,
    onroom,
    onpalette,
    onprofile,
    onspeak,
    onfork,
    ondelete,
    onregen,
    onpin,
  }: {
    editingTitle: boolean;
    titleDraft: string;
    title: string;
    branch: string;
    room: string;
    book: string;
    turns: ElucifiaTurn[];
    streaming: boolean;
    thinkSeconds: number;
    showThinking: boolean;
    model: string;
    mood: string;
    lane: string;
    markdown: (text: string) => string;
    tags?: string[];
    palette?: { name: string; color?: string }[];
    ontags?: (tags: string[]) => void;
    ontagpalette?: (op: string, name: string, color: string, next?: string) => void;
    onbranch?: () => void;
    onedit: () => void;
    onsave: () => void;
    onroom: (room: 'vault' | 'workshop') => void;
    onpalette: () => void;
    onprofile: (name: string) => void;
    onspeak: (text: string, event: MouseEvent) => void;
    onfork: (index: number) => void;
    ondelete: (index: number) => void;
    onregen: (text: string) => void;
    onpin: (event: MouseEvent) => void;
  } = $props();

  let armed = $state(-1);
  let scroller: HTMLDivElement | null = $state(null);
  let fade = $state(
    typeof location !== 'undefined' && new URLSearchParams(location.search).get('fade') === '1' ? 1 : 0,
  );

  function onScroll() {
    const top = scroller?.scrollTop ?? 0;
    fade = Math.max(0, Math.min(1, top / 50));
  }
</script>

<div class="column">
<header class="band">
  <div class="quick">
    <button type="button" class:on={room === 'vault'} onclick={() => onroom('vault')}><Icon name="vault" size="0.9rem" /> Vault</button>
    <button type="button" class:on={room === 'workshop'} onclick={() => onroom('workshop')}><Icon name="workshop" size="0.9rem" /> Workshop</button>
    <button type="button" onclick={onpalette}><Icon name="command" size="0.9rem" /> Ctrl+K</button>
  </div>
  <div class="title">
    {#if editingTitle}
      <input class="title-edit" bind:value={titleDraft} onblur={onsave} onkeydown={(e) => e.key === 'Enter' && onsave()} />
    {:else}
      <button class="mark" type="button" onclick={onedit}>✦ {titleDraft || title || 'Seraphina Elucifia'} ✦</button>
    {/if}
    <button type="button" class="branch-pill" onclick={onbranch}><Icon name="branch" size="0.5rem" /> - {branch || 'main'} -</button>
  </div>
  <TagBar {tags} {palette} onchange={ontags} onpalette={ontagpalette} />
</header>
<div class="stream" bind:this={scroller} onscroll={onScroll} style:--fade={fade}>
  {#if !turns.length}
    <div class="welcome"><span>✦</span><h2>Seraphina Elucifia</h2><p>The Atheneum is listening. {book} is the active book.</p></div>
  {/if}
  {#each turns as turn, index}
    <article class="msg" class:user={turn.role === 'user'}>
      <button class="avatar" class:hers={turn.role !== 'user'} type="button" style:--bubble={turn.role === 'user' ? '#c96fd0' : '#ff8c42'} onclick={() => onprofile(turn.role === 'user' ? 'Inatos' : 'Seraphina Elucifia')}>{turn.role === 'user' ? '⚡' : '✦'}</button>
      <div class="bubble">
        {#if streaming && index === turns.length - 1 && !turn.content}
          <p class="think">Thinking… {thinkSeconds}s</p>
          <div class="shimmer"></div>
        {:else}
          {@html markdown(turn.content)}
        {/if}
        {#if showThinking && turn.thoughts}<pre class="think">{turn.thoughts}</pre>{/if}
        <footer class="bubble-foot">
          <div class="acts">
            <button type="button" title="Read this message aloud" onclick={(e) => onspeak(turn.content, e)}><Icon name="speak" size="0.85rem" /> Speak</button>
            <button type="button" title="Rewrite this message and drop every reply after it, including forks that continued from here" onclick={() => onfork(index)}><Icon name="fork" size="0.85rem" /> Edit</button>
            <button type="button" title="Remove this message and every reply after it, including forks that continued from here" onclick={() => { if (armed === index) { armed = -1; ondelete(index); } else armed = index; }}>{armed === index ? 'Drop chain' : 'Delete'}</button>
            <button type="button" title="Ask again from this message" onclick={() => onregen(turn.role === 'user' ? turn.content : 'Continue from the last line.')}><Icon name="regen" size="0.85rem" /> Regenerate</button>
            <button type="button" title="Copy this message" onclick={() => navigator.clipboard.writeText(turn.content)}><Icon name="copy" size="0.85rem" /> Copy</button>
            <button type="button" title="Distill this thread into Lorekeep" onclick={onpin}><Icon name="pin" size="0.85rem" /> Pin to Lorekeep</button>
          </div>
          <span class="mark-line">{turn.role === 'user' ? 'Inatos' : 'Seraphina Elucifia'} · {model} · {turn.mood || mood} · {turn.tok_per_sec || lane}</span>
        </footer>
      </div>
    </article>
  {/each}
</div>
</div>

<style>
  .column { display: flex; flex-direction: column; flex: 1 1 auto; min-height: 0; min-width: 0; }
  .band { display: flex; flex-direction: column; align-items: stretch; gap: 0.15rem; margin-bottom: 0.6rem; flex: 0 0 auto; }
  .title { display: flex; flex-direction: column; align-items: center; gap: 0.12rem; text-align: center; color: #fff4e8; }
  .mark, h2 { font-family: "Cormorant Garamond", Georgia, serif; font-weight: 500; color: #fff4e8; }
  .mark { background: none; border: 0; font-size: clamp(1.25rem, 2.4vw, 1.7rem); white-space: nowrap; }
  .think { color: #a894ab; }
  .branch-pill { display: inline-flex; align-items: center; gap: 0.15rem; background: rgba(13, 9, 18, 0.84); border: 1px solid rgba(126, 200, 227, 0.55); color: #7ec8e3; text-shadow: 0 1px 4px rgba(0, 0, 0, 0.85); border-radius: 999px; padding: 0.02rem 0.28rem; font-family: "IBM Plex Mono", monospace; font-size: 0.62rem; line-height: 1.2; }
  .quick { display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 0.3rem; }
  .bubble { color: #f3e6d4; }
  .msg.user .bubble { color: #e4c6ea; }
  .mark-line { color: #7ec8e3; }
  .avatar.hers:hover { filter: brightness(1.2); box-shadow: 0 0 22px #ff8c42; }
  button { border: 1px solid rgba(201, 111, 208, 0.35); background: rgba(13, 9, 18, 0.55); color: inherit; border-radius: 0.7rem; }
  .on { border-color: #ff8c42; }
  .stream { --fade: 0; flex: 1 1 auto; min-height: 0; overflow: auto; width: min(850px, 100%); margin: 0 auto; display: grid; align-content: start; gap: 0.8rem; padding: 2.6rem 2.4rem 2.8rem; -webkit-mask-image: linear-gradient(to bottom, rgba(0, 0, 0, calc(1 - var(--fade))) 0, #000 calc(var(--fade) * 3.85rem)); mask-image: linear-gradient(to bottom, rgba(0, 0, 0, calc(1 - var(--fade))) 0, #000 calc(var(--fade) * 3.85rem)); }
  .welcome { text-align: center; padding: 2rem 0 1rem; }
  .welcome p { color: #f3e6d4; font-family: "Cormorant Garamond", Georgia, serif; font-size: 1.15rem; }
  .welcome span { display: inline-grid; place-items: center; width: 3.1rem; height: 3.1rem; border-radius: 999px; font-size: 1.35rem; color: #1a0d12; background: #ff8c42; box-shadow: 0 0 28px 8px rgba(255, 140, 66, 0.85); }
  .msg { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: 0.6rem; justify-items: start; }
  .msg.user { grid-template-columns: minmax(0, 1fr) auto; justify-items: end; }
  .msg.user .avatar { grid-column: 2; grid-row: 1; }
  .msg.user .bubble { grid-column: 1; grid-row: 1; justify-self: end; }
  .avatar { width: 2.4rem; height: 2.4rem; border-radius: 999px; border: 0; color: #1a0d12; background: var(--bubble); box-shadow: 0 0 22px 4px var(--bubble); }
  .bubble { max-width: min(760px, 100%); padding: 0.75rem 0.9rem 0.55rem; border-radius: 1rem; background: rgba(22, 15, 29, 0.78); border: 1px solid rgba(255, 140, 66, 0.2); }
  .bubble-foot { display: flex; flex-wrap: wrap; justify-content: space-between; align-items: flex-end; gap: 0.45rem; margin-top: 0.55rem; }
  .acts { display: flex; flex-wrap: wrap; gap: 0.25rem; }
  .acts button { display: inline-flex; align-items: center; gap: 0.25rem; }
  .mark-line { font-family: "IBM Plex Mono", monospace; font-size: 0.72rem; color: #7ec8e3; }
  .shimmer { height: 0.35rem; border-radius: 99px; background: linear-gradient(90deg, transparent, #ff8c42, transparent); }
  .title-edit { width: min(28rem, 90%); border-radius: 0.7rem; padding: 0.4rem 0.6rem; background: rgba(13, 9, 18, 0.72); color: inherit; }
</style>
