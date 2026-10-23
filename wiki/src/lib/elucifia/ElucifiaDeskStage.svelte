<script lang="ts">
  import Icon from './ElucifiaIcons.svelte';
  import ElucifiaSelect from './ElucifiaSelect.svelte';
  import { renderStudioMarkdown } from './markdown';

  type Job = { job?: string; name?: string; kind?: string; status?: string; bytes?: number };

  let {
    mode,
    lines,
    jobs,
    model = $bindable(''),
    models,
    voice = $bindable('Qwen3-TTS'),
    showThinking = $bindable(true),
    sendOnEnter = $bindable(true),
    autoSpeak = $bindable(false),
    sfxOn = $bindable(true),
    continuity = '',
    vault = '',
    onclear,
    onsave,
  }: {
    mode: 'logs' | 'config';
    lines: string[];
    jobs: Job[];
    model?: string;
    models: string[];
    voice?: string;
    showThinking?: boolean;
    sendOnEnter?: boolean;
    autoSpeak?: boolean;
    sfxOn?: boolean;
    continuity?: string;
    vault?: string;
    onclear: () => void;
    onsave: () => void;
  } = $props();

  const voices = [
    { id: 'Qwen/Qwen3-TTS-12Hz-0.6B-CustomVoice', label: 'Qwen3-TTS' },
    { id: 'ResembleAI/chatterbox', label: 'Chatterbox' },
    { id: 'hexgrad/Kokoro-82M', label: 'Kokoro-82M' },
  ];
  let filter = $state('all');
  let search = $state('');
  const shown = $derived(
    lines.filter((line) => {
      const hit = !search || line.toLowerCase().includes(search.toLowerCase());
      if (!hit) return false;
      if (filter === 'desktop') return /desktop|local|stub/i.test(line);
      if (filter === 'peer') return /peer|myridos|laptop/i.test(line);
      if (filter === 'jobs') return /job|palaestra/i.test(line);
      return true;
    }),
  );

  async function copy() {
    try {
      await navigator.clipboard.writeText(shown.join('\n'));
    } catch {
      /* clipboard stays with the page */
    }
  }
</script>

{#if mode === 'logs'}
  <section class="desk">
    <header>
      <h2>Logs</h2>
      <button type="button" class:on={filter === 'all'} onclick={() => (filter = 'all')}>All</button>
      <button type="button" class:on={filter === 'desktop'} onclick={() => (filter = 'desktop')}>Desktop</button>
      <button type="button" class:on={filter === 'peer'} onclick={() => (filter = 'peer')}>Myridos Peer</button>
      <button type="button" class:on={filter === 'jobs'} onclick={() => (filter = 'jobs')}>Palaestra Jobs</button>
      <input placeholder="Search" bind:value={search} aria-label="Search logs" />
      <button type="button" onclick={copy}><Icon name="copy" size="0.85rem" /> Copy</button>
      <button type="button" onclick={onclear}><Icon name="trash" size="0.85rem" /> Clear</button>
    </header>
    <pre>{shown.join('\n')}</pre>
    <div class="table-wrap">
      <table class="dash-table">
        <thead><tr><th>Job</th><th>Kind</th><th>Status</th><th>Bytes</th></tr></thead>
        <tbody>
          {#each jobs as job}
            <tr><td>{job.name || job.job}</td><td>{job.kind}</td><td>{job.status}</td><td>{job.bytes ?? ''}</td></tr>
          {/each}
        </tbody>
      </table>
    </div>
  </section>
{:else}
  <section class="desk config">
    <header><h2>Config</h2></header>
    <div class="deck">
      <article class="glass">
        <header>
          <h3>Models & Voice</h3>
          <button type="button" class="save" onclick={onsave}>Save settings</button>
        </header>
        <ElucifiaSelect bind:value={model} options={models} label="Default model" ariaLabel="Default model" startOpen={typeof window !== 'undefined' && new URLSearchParams(window.location.search).get('combo') === '1'} />
        <ElucifiaSelect bind:value={voice} options={voices.map((item) => ({ value: item.id, label: item.label }))} label="Voice engine" ariaLabel="Voice engine" />
      </article>
      <article class="glass">
        <h3>Studio behavior</h3>
        <label class="switch"><input type="checkbox" bind:checked={showThinking} /> Show thinking</label>
        <label class="switch"><input type="checkbox" bind:checked={sendOnEnter} /> Send on Enter</label>
        <label class="switch"><input type="checkbox" bind:checked={autoSpeak} /> Auto speak</label>
        <label class="switch"><input type="checkbox" bind:checked={sfxOn} /> Sound</label>
      </article>
      <article class="glass prose-card">
        <h3>Continuity</h3>
        <div class="prose">{@html renderStudioMarkdown(continuity || 'Local glance.')}</div>
      </article>
      <article class="glass prose-card">
        <h3>Vault</h3>
        <div class="prose">{@html renderStudioMarkdown(vault || 'Lorekeep stays quiet until @vault or @lorekeep.')}</div>
      </article>
    </div>
  </section>
{/if}

<style>
  .desk { display: flex; flex-direction: column; gap: 0.6rem; min-height: 0; min-width: 0; height: 100%; color: #fff4e8; overflow: auto; }
  header { display: flex; flex-wrap: wrap; gap: 0.35rem; align-items: center; flex: 0 0 auto; }
  h2 { font-family: "Cormorant Garamond", Georgia, serif; font-weight: 500; margin: 0 0.4rem 0 0; }
  button, input, select { border: 1px solid rgba(201, 111, 208, 0.35); background: rgba(13, 9, 18, 0.72); color: #f4ecf6; border-radius: 0.7rem; color-scheme: dark; }
  button { display: inline-flex; align-items: center; gap: 0.25rem; }
  .on { border-color: #ff8c42; }
  pre { margin: 0; white-space: pre-wrap; font-family: "IBM Plex Mono", monospace; color: #7ec8e3; font-size: 0.78rem; }
  label { display: grid; gap: 0.25rem; color: #a894ab; }
  .switch { display: flex; align-items: center; gap: 0.4rem; }
  .deck { display: grid; grid-template-columns: 1fr 1fr; gap: 0.8rem; }
  .glass { display: grid; gap: 0.55rem; padding: 0.85rem; background: rgba(22, 15, 29, 0.88); border: 1px solid rgba(201, 111, 208, 0.35); border-radius: 1rem; backdrop-filter: blur(14px); }
  .glass header { display: flex; justify-content: space-between; align-items: center; }
  .save { width: fit-content; }
  h3 { margin: 0; font-family: "Cormorant Garamond", Georgia, serif; font-weight: 500; color: #fff4e8; }
  .prose { max-height: 18rem; overflow: auto; color: #f3e6d4; }
  .prose :global(h1), .prose :global(h2), .prose :global(h3) { font-family: "Cormorant Garamond", Georgia, serif; color: #fff4e8; }
  .prose :global(strong) { color: #fff4e8; }
  .prose :global(code) { color: #7ec8e3; }
  @media (max-width: 1100px) { .deck { grid-template-columns: 1fr; } }
  .table-wrap { min-width: 0; max-width: 100%; overflow: auto; }
  table { width: 100%; border-collapse: collapse; }
  th, td { text-align: left; padding: 0.35rem 0.45rem; border-bottom: 1px solid rgba(201, 111, 208, 0.25); }
  .table-wrap :global(.dash-table thead th) {
    padding: 0.55rem 0.7rem;
    line-height: 1.4;
    text-align: left;
    background: rgba(22, 15, 29, 0.96);
  }
  .table-wrap :global(.dash-table th:first-child),
  .table-wrap :global(.dash-table td:first-child) {
    background: rgba(22, 15, 29, 0.96);
  }
</style>
