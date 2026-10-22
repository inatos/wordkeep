<script lang="ts">
  type Mood = 'Attentive' | 'Excited' | 'Skeptical' | 'Tender' | 'Playful';

  const MOODS: Mood[] = ['Attentive', 'Excited', 'Skeptical', 'Tender', 'Playful'];

  let mood = $state<Mood>('Attentive');
  let energy = $state(0.72);
  let focus = $state(0.64);
  let peerUp = $state(true);
  let speaking = $state(false);
  let note = $state('Vault first. Then the tool that already owns the job.');

  const voice = $derived(
    peerUp ? 'Qwen/Qwen3-TTS-12Hz-0.6B-CustomVoice' : 'hexgrad/Kokoro-82M'
  );
  const lane = $derived(peerUp ? 'laptop peer' : 'this machine');

  function pulse() {
    speaking = true;
    window.setTimeout(() => {
      speaking = false;
    }, 700);
  }
</script>

<div class="elucifia" data-mood={mood} data-speaking={speaking} data-testid="elucifia-board">
  <header class="panel-head">
    <h2 class="section-title">Seraphina Elucifia</h2>
    <p class="lede">Co-architect of Betwixt. Lorekeep of the Atheneum. Bonded to Inatos.</p>
  </header>

  <div class="vitality" aria-label="Affect">
    <span class="mood-chip">{mood}</span>
    <label>Energy <progress max="1" value={energy}></progress></label>
    <label>Focus <progress max="1" value={focus}></progress></label>
  </div>

  <div class="pills">
    <span class="pill">gemma4:12b</span>
    <span class="pill">{lane}</span>
    <span class="pill">{voice}</span>
  </div>

  <div class="moods" role="group" aria-label="Mood">
    {#each MOODS as item (item)}
      <button
        type="button"
        class="mood-btn"
        class:active={mood === item}
        onclick={() => (mood = item)}
      >
        {item}
      </button>
    {/each}
  </div>

  <label class="note">
    Working note
    <textarea bind:value={note} rows="3"></textarea>
  </label>

  <div class="actions">
    <button type="button" class="act" onclick={() => (peerUp = !peerUp)}>
      {peerUp ? 'Peer up' : 'Peer down'}
    </button>
    <button type="button" class="act" onclick={pulse}>Speak</button>
  </div>

  <ul class="routes">
    <li>knowledge_search</li>
    <li>decree promote_hold / compute_peer / capture_named</li>
    <li>copper_status then task_submit</li>
    <li>arte and akasha</li>
    <li>lorekeep memory recall</li>
  </ul>
  <p class="hint">
    The CLI prints the route. Chat and speech do not run inside FixedUpdate. Frame gates stay on this desktop.
  </p>
  {#if speaking}
    <div class="wave" aria-hidden="true"><span></span><span></span><span></span><span></span></div>
  {/if}
</div>

<style>
  .elucifia {
    position: relative;
    padding: 0.25rem 0.1rem 1rem;
    border-radius: 12px;
    background:
      radial-gradient(420px 180px at 12% 0%, var(--glow), transparent 70%),
      transparent;
  }
  .elucifia[data-mood='Attentive'] { --glow: rgba(177, 83, 184, 0.35); }
  .elucifia[data-mood='Excited'] { --glow: rgba(219, 143, 224, 0.45); }
  .elucifia[data-mood='Skeptical'] { --glow: rgba(212, 162, 74, 0.4); }
  .elucifia[data-mood='Tender'] { --glow: rgba(120, 140, 190, 0.4); }
  .elucifia[data-mood='Playful'] { --glow: rgba(201, 111, 208, 0.45); }
  .lede { color: var(--muted); margin: 0.2rem 0 0.8rem; }
  .vitality, .pills, .moods, .actions { display: flex; flex-wrap: wrap; gap: 0.5rem; align-items: center; margin: 0.6rem 0; }
  .mood-chip, .pill {
    border: 1px solid var(--border);
    border-radius: 999px;
    padding: 0.15rem 0.7rem;
    background: var(--accent-soft);
  }
  progress { width: 7rem; accent-color: var(--accent); }
  .mood-btn, .act {
    border: 1px solid var(--border);
    background: var(--bg-elevated);
    border-radius: 8px;
    padding: 0.35rem 0.7rem;
    cursor: pointer;
  }
  .mood-btn:active, .act:active { transform: scale(0.97); }
  .mood-btn.active { border-color: var(--accent); background: var(--accent-soft); }
  .note { display: grid; gap: 0.35rem; color: var(--muted); }
  textarea {
    width: 100%;
    resize: vertical;
    background: #0a070b;
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 0.6rem;
  }
  .routes { margin: 0.6rem 0; padding-left: 1.1rem; color: var(--text); }
  .hint { color: var(--muted); }
  .wave { display: flex; gap: 4px; height: 18px; align-items: flex-end; margin-top: 0.4rem; }
  .wave span {
    width: 4px;
    background: var(--accent-strong);
    animation: eq 0.7s ease-in-out infinite alternate;
  }
  .wave span:nth-child(2) { animation-delay: 0.1s; }
  .wave span:nth-child(3) { animation-delay: 0.2s; }
  .wave span:nth-child(4) { animation-delay: 0.3s; }
  @keyframes eq {
    from { height: 4px; }
    to { height: 16px; }
  }
</style>
