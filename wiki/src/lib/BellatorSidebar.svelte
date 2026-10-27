<script lang="ts">
  import { rippleFromEvent } from './press-ripple';

  export type BellatorSection =
    | 'overview'
    | 'scans'
    | 'schedule'
    | 'findings'
    | 'watchlist'
    | 'host'
    | 'runs'
    | 'policy';

  let {
    collapsed = false,
    section = 'overview' as BellatorSection,
    openCount = 0,
    highPlusCount = 0,
    watchlistCount = 0,
    activeTimersCount = 0,
    busy = false,
    onToggleCollapse,
    onSelectSection,
    onQuickLaunch,
  }: {
    collapsed: boolean;
    section: BellatorSection;
    openCount: number;
    highPlusCount: number;
    watchlistCount: number;
    activeTimersCount: number;
    busy: boolean;
    onToggleCollapse: () => void;
    onSelectSection: (next: BellatorSection) => void;
    onQuickLaunch: (profile: string, preset: string) => void;
  } = $props();

  const NAV_ITEMS: {
    id: BellatorSection;
    label: string;
    short: string;
    key: string;
    glyph: string;
    badge?: () => number;
  }[] = [
    { id: 'overview', label: 'Overview & Posture', short: 'Posture', key: '1', glyph: '◈' },
    { id: 'scans', label: 'Scans & ClamAV', short: 'Scans', key: '2', glyph: '⚔' },
    {
      id: 'schedule',
      label: 'Scheduled Timers',
      short: 'Timers',
      key: '3',
      glyph: '⏱',
      badge: () => activeTimersCount,
    },
    {
      id: 'findings',
      label: 'Findings & Triage',
      short: 'Findings',
      key: '4',
      glyph: '⚑',
      badge: () => openCount,
    },
    {
      id: 'watchlist',
      label: 'Watchlist & Quarantine',
      short: 'Watchlist',
      key: '5',
      glyph: '⛨',
      badge: () => watchlistCount,
    },
    { id: 'host', label: 'Host & Persistence', short: 'Host Sec', key: '6', glyph: '⌘' },
    { id: 'runs', label: 'Runs & Delta Diff', short: 'Runs', key: '7', glyph: '⇄' },
    { id: 'policy', label: 'Policy & Suppressions', short: 'Policy', key: '8', glyph: '§' },
  ];
</script>

<aside class="rail" class:collapsed aria-label="Bellator Admin-Sec Navigation">
  <button
    type="button"
    class="edge"
    title={collapsed ? 'Expand Bellator rail ([)' : 'Collapse Bellator rail ([)'}
    onclick={(e) => {
      rippleFromEvent(e);
      onToggleCollapse();
    }}
  >
    {collapsed ? '›' : '‹'}
  </button>

  {#if !collapsed}
    <div class="rail-head">
      <div class="brand-row">
        <span class="shield-icon" aria-hidden="true">⛨</span>
        <div>
          <strong>BELLATOR</strong>
          <span class="sub">Nexus Admin-Sec Plane</span>
        </div>
      </div>
      {#if highPlusCount > 0}
        <span class="alert-pill danger" title="Open High/Critical findings">
          {highPlusCount} High+
        </span>
      {:else}
        <span class="alert-pill ok" title="Zero open High/Critical findings">Clean</span>
      {/if}
    </div>
  {/if}

  <nav class="nav-list" role="tablist">
    {#each NAV_ITEMS as item}
      {@const count = item.badge ? item.badge() : 0}
      <button
        type="button"
        role="tab"
        aria-selected={section === item.id}
        class="nav-btn"
        class:sao-orb={collapsed}
        class:active={section === item.id}
        title={`${item.label} (Shortcut: ${item.key})`}
        onclick={(e) => {
          rippleFromEvent(e);
          onSelectSection(item.id);
        }}
      >
        <span class="glyph" aria-hidden="true">{item.glyph}</span>
        {#if !collapsed}
          <span class="lbl">{item.label}</span>
          {#if count > 0}
            <span class="badge" class:warn={item.id === 'findings' && highPlusCount > 0}
              >{count}</span
            >
          {:else}
            <kbd>{item.key}</kbd>
          {/if}
        {/if}
      </button>
    {/each}
  </nav>

  {#if !collapsed}
    <div class="quick-box">
      <div class="quick-title">Quick Launchers</div>
      <button
        type="button"
        class="launch-btn"
        disabled={busy}
        title="Run fast builtin secret, path-leak, persistence, socket, and Git LFS audit"
        onclick={(e) => {
          rippleFromEvent(e);
          onQuickLaunch('host-sec', 'quick');
        }}
      >
        <span>⚡ Quick Host & Repo Audit</span>
        <code>&lt;2s</code>
      </button>
      <button
        type="button"
        class="launch-btn"
        disabled={busy}
        title="Targeted ClamAV malware signature sweep + Wine/autostart persistence check"
        onclick={(e) => {
          rippleFromEvent(e);
          onQuickLaunch('clamav-quick', 'malware');
        }}
      >
        <span>🛡 ClamAV Quick Sweep</span>
        <code>malware</code>
      </button>
      <button
        type="button"
        class="launch-btn"
        disabled={busy}
        title="Full-nexus CVE, secret, workflow, and host policy scan"
        onclick={(e) => {
          rippleFromEvent(e);
          onQuickLaunch('full-nexus', 'full');
        }}
      >
        <span>🌐 Full-Nexus Security Gate</span>
        <code>full</code>
      </button>
    </div>
  {/if}
</aside>

<style>
  .rail {
    position: relative;
    width: 17.5rem;
    min-height: 100%;
    padding: 0.75rem 0.7rem;
    background: rgba(22, 15, 29, 0.82);
    backdrop-filter: blur(14px);
    border-right: 1px solid rgba(201, 111, 208, 0.28);
    display: flex;
    flex-direction: column;
    gap: 0.7rem;
    overflow-y: auto;
    transition: width 180ms var(--ease-snap, cubic-bezier(0.22, 1, 0.36, 1));
  }
  .rail.collapsed {
    width: 4.35rem;
    padding: 0.75rem 0.45rem;
    align-items: center;
  }
  .edge {
    position: absolute;
    right: -0.75rem;
    top: 1rem;
    width: 1.55rem;
    height: 1.55rem;
    border-radius: 999px;
    border: 1px solid #c96fd0;
    background: #160f1d;
    color: #f4ecf6;
    cursor: pointer;
    z-index: 4;
    display: grid;
    place-items: center;
    font-size: 0.9rem;
    line-height: 1;
  }
  .rail-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.4rem;
    padding-bottom: 0.45rem;
    border-bottom: 1px solid rgba(201, 111, 208, 0.22);
  }
  .brand-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .shield-icon {
    font-size: 1.35rem;
    color: #ff8c42;
    text-shadow: 0 0 12px rgba(255, 140, 66, 0.55);
  }
  .brand-row strong {
    display: block;
    font-size: 0.88rem;
    letter-spacing: 0.08em;
    color: #fff4e8;
  }
  .brand-row .sub {
    display: block;
    font-size: 0.7rem;
    color: #a894ab;
  }
  .alert-pill {
    font-size: 0.7rem;
    padding: 0.15rem 0.48rem;
    border-radius: 999px;
    font-weight: 600;
  }
  .alert-pill.ok {
    background: rgba(107, 207, 142, 0.16);
    color: #6bcf8e;
    border: 1px solid rgba(107, 207, 142, 0.45);
  }
  .alert-pill.danger {
    background: rgba(224, 107, 122, 0.18);
    color: #e06b7a;
    border: 1px solid rgba(224, 107, 122, 0.5);
  }
  .nav-list {
    display: flex;
    flex-direction: column;
    gap: 0.32rem;
    width: 100%;
  }
  .nav-btn {
    display: flex;
    align-items: center;
    gap: 0.55rem;
    width: 100%;
    padding: 0.48rem 0.6rem;
    border-radius: 0.65rem;
    border: 1px solid rgba(201, 111, 208, 0.26);
    background: rgba(13, 9, 18, 0.58);
    color: #f0e6f2;
    cursor: pointer;
    text-align: left;
    font-size: 0.8rem;
    transition: border-color 140ms ease, box-shadow 140ms ease, background 140ms ease;
  }
  .nav-btn:hover {
    background: rgba(38, 28, 40, 0.85);
    border-color: rgba(201, 111, 208, 0.55);
  }
  .nav-btn.active {
    border-color: #ff8c42;
    box-shadow: 0 0 14px rgba(255, 140, 66, 0.32);
    background: rgba(42, 24, 38, 0.88);
  }
  .nav-btn.sao-orb {
    width: 2.75rem;
    height: 2.75rem;
    padding: 0;
    border-radius: 999px;
    justify-content: center;
  }
  .glyph {
    font-size: 1rem;
    width: 1.25rem;
    text-align: center;
    flex: 0 0 auto;
    color: #c96fd0;
  }
  .nav-btn.active .glyph {
    color: #ff8c42;
  }
  .lbl {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .badge {
    font-size: 0.68rem;
    padding: 0.08rem 0.42rem;
    border-radius: 999px;
    background: rgba(177, 83, 184, 0.24);
    border: 1px solid rgba(201, 111, 208, 0.45);
    font-variant-numeric: tabular-nums;
  }
  .badge.warn {
    background: rgba(224, 107, 122, 0.24);
    border-color: #e06b7a;
    color: #ffb4be;
  }
  kbd {
    font-size: 0.66rem;
    color: #a894ab;
    background: rgba(255, 255, 255, 0.05);
    padding: 0.06rem 0.32rem;
    border-radius: 4px;
    border: 1px solid rgba(255, 255, 255, 0.08);
  }
  .quick-box {
    margin-top: auto;
    padding-top: 0.55rem;
    border-top: 1px solid rgba(201, 111, 208, 0.22);
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
  }
  .quick-title {
    font-size: 0.7rem;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: #a894ab;
  }
  .launch-btn {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.4rem;
    padding: 0.42rem 0.55rem;
    border-radius: 0.6rem;
    border: 1px solid rgba(201, 111, 208, 0.3);
    background: rgba(18, 13, 20, 0.72);
    color: #f0e6f2;
    font-size: 0.75rem;
    cursor: pointer;
  }
  .launch-btn:hover:not(:disabled) {
    border-color: #ff8c42;
    background: rgba(38, 24, 36, 0.9);
  }
  .launch-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .launch-btn code {
    font-size: 0.66rem;
    color: #7ec8e3;
  }
</style>
