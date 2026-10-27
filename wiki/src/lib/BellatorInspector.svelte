<script lang="ts">
  import { rippleFromEvent } from './press-ripple';

  let {
    collapsed = false,
    gateOk = true,
    advisoryStale = false,
    advisoryAgeHours = null as number | null,
    timersActive = false,
    engineActive = false,
    busy = false,
    liveScan = null as any,
    sfxOn = true,
    receipt = '',
    rootOptInCmd = 'sudo bash tools/bellator/scripts/install_bellator_root_sweep_opt_in.sh',
    onToggleCollapse,
    onToggleSfx,
    onStartScan,
    onPauseResume,
    onCancelScan,
    onSyncDefects,
    onInstallUserTimers,
  }: {
    collapsed: boolean;
    gateOk: boolean;
    advisoryStale: boolean;
    advisoryAgeHours: number | null;
    timersActive: boolean;
    engineActive: boolean;
    busy: boolean;
    liveScan: any;
    sfxOn: boolean;
    receipt: string;
    rootOptInCmd: string;
    onToggleCollapse: () => void;
    onToggleSfx: () => void;
    onStartScan: () => void;
    onPauseResume: (pause: boolean) => void;
    onCancelScan: () => void;
    onSyncDefects: () => void;
    onInstallUserTimers: () => void;
  } = $props();

  let scanArmed = $state(false);
  let cancelArmed = $state(false);
  let syncArmed = $state(false);
  let copiedRootCmd = $state(false);

  function formatBytes(bytes: number): string {
    if (!bytes || bytes <= 0) return '0 B';
    const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
    let v = bytes;
    let u = 0;
    while (v >= 1024 && u < units.length - 1) {
      v /= 1024;
      u += 1;
    }
    return `${v.toFixed(u > 1 ? 1 : 0)} ${units[u]}`;
  }

  function formatDuration(secs: number): string {
    if (!secs || secs <= 0) return '0s';
    const h = Math.floor(secs / 3600);
    const m = Math.floor((secs % 3600) / 60);
    const s = secs % 60;
    if (h > 0) return `${h}h ${m}m ${s}s`;
    if (m > 0) return `${m}m ${s}s`;
    return `${s}s`;
  }

  function handleArmScan(e: MouseEvent) {
    rippleFromEvent(e);
    if (!scanArmed) {
      scanArmed = true;
      setTimeout(() => (scanArmed = false), 4000);
      return;
    }
    scanArmed = false;
    onStartScan();
  }

  function handleArmCancel(e: MouseEvent) {
    rippleFromEvent(e);
    if (!cancelArmed) {
      cancelArmed = true;
      setTimeout(() => (cancelArmed = false), 4000);
      return;
    }
    cancelArmed = false;
    onCancelScan();
  }

  function handleArmSync(e: MouseEvent) {
    rippleFromEvent(e);
    if (!syncArmed) {
      syncArmed = true;
      setTimeout(() => (syncArmed = false), 4000);
      return;
    }
    syncArmed = false;
    onSyncDefects();
  }

  async function copyRootInstaller(e: MouseEvent) {
    rippleFromEvent(e);
    try {
      await navigator.clipboard.writeText(rootOptInCmd);
      copiedRootCmd = true;
      setTimeout(() => (copiedRootCmd = false), 2500);
    } catch {
      copiedRootCmd = false;
    }
  }
</script>

<aside class="inspector" class:collapsed aria-label="Bellator Systems Inspector">
  <button
    type="button"
    class="edge"
    title={collapsed ? 'Expand Systems Inspector (])' : 'Collapse Systems Inspector (])'}
    onclick={(e) => {
      rippleFromEvent(e);
      onToggleCollapse();
    }}
  >
    {collapsed ? '‹' : '›'}
  </button>

  {#if !collapsed}
    <header class="insp-head">
      <strong>SYSTEMS & TELEMETRY</strong>
      <button
        type="button"
        class="sfx-btn"
        class:on={sfxOn}
        title={sfxOn ? 'UI audio cues enabled (click to mute)' : 'UI audio cues muted'}
        onclick={(e) => {
          rippleFromEvent(e);
          onToggleSfx();
        }}
      >
        {sfxOn ? '🔊 SFX' : '🔇 Mute'}
      </button>
    </header>

    <div class="systems" role="group" aria-label="Security posture pills">
      <div class="pill" title="CI & Host Security Gate status">
        <span class="dot" class:live={gateOk} class:down={!gateOk}></span>
        <div>
          <span class="k">Gate</span>
          <strong>{gateOk ? 'PASS' : 'BLOCK'}</strong>
        </div>
      </div>

      <div
        class="pill"
        title={advisoryAgeHours != null
          ? `Signature / Advisory DB age: ${advisoryAgeHours.toFixed(1)}h`
          : 'ClamAV / Advisory DB freshness'}
      >
        <span class="dot" class:live={!advisoryStale} class:wait={advisoryStale}></span>
        <div>
          <span class="k">ClamAV / DB</span>
          <strong>
            {advisoryAgeHours != null ? `${advisoryAgeHours.toFixed(1)}h` : 'Fresh'}
          </strong>
        </div>
      </div>

      <div class="pill" title="systemd --user scheduled scan timers">
        <span class="dot" class:live={timersActive} class:wait={!timersActive}></span>
        <div>
          <span class="k">User Timers</span>
          <strong>{timersActive ? 'Active' : 'Off'}</strong>
        </div>
      </div>

      <div
        class="pill"
        title="Megistus zero-hitch Engine Guard (SIGSTOP background scans while Betwixt/Tracy runs)"
      >
        <span class="dot" class:live={!engineActive} class:wait={engineActive}></span>
        <div>
          <span class="k">Engine Guard</span>
          <strong>{engineActive ? 'Paused' : 'Clear'}</strong>
        </div>
      </div>
    </div>

    <div class="gauges">
      <div class="gauge-title">Live `/proc` Scanner Telemetry</div>
      {#if liveScan}
        <div class="gauge-grid">
          <div class="g-cell">
            <span class="k">PID / Mode</span>
            <code>
              {liveScan.child_pid ?? liveScan.pid}
              {liveScan.is_external_root_sweep ? '(Root)' : '(User)'}
            </code>
          </div>
          <div class="g-cell">
            <span class="k">Elapsed</span>
            <code>{formatDuration(liveScan.elapsed_secs ?? 0)}</code>
          </div>
          <div class="g-cell">
            <span class="k">CPU / RSS</span>
            <code>
              {(liveScan.cpu_pct ?? 0).toFixed(1)}% · {formatBytes(liveScan.rss_bytes ?? 0)}
            </code>
          </div>
          <div class="g-cell">
            <span class="k">Disk Read</span>
            <code>
              {formatBytes(liveScan.bytes_read ?? 0)}
              {#if (liveScan.read_mbps ?? 0) > 0}
                ({liveScan.read_mbps.toFixed(1)} MB/s)
              {/if}
            </code>
          </div>
        </div>
        {#if liveScan.current_file}
          <div class="live-file" title={liveScan.current_file}>
            <span class="k">Open `/proc/pid/fd` target:</span>
            <code>{liveScan.current_file}</code>
          </div>
        {/if}
      {:else}
        <p class="muted-note">No active scan process. Idle cgroup ready.</p>
      {/if}
    </div>

    <div class="controls">
      <div class="gauge-title">Two-Step Armed Controls</div>
      <div class="btn-grid">
        <button
          type="button"
          class="ping"
          class:armed={scanArmed}
          disabled={busy}
          onclick={handleArmScan}
        >
          {scanArmed ? 'Confirm Scan Now' : 'Scan Now (2-Step)'}
        </button>

        {#if liveScan && !liveScan.is_external_root_sweep && (liveScan.status === 'running' || liveScan.status === 'paused_for_engine')}
          <button
            type="button"
            class="ping"
            onclick={(e) => {
              rippleFromEvent(e);
              onPauseResume(liveScan.status !== 'paused_for_engine');
            }}
          >
            {liveScan.status === 'paused_for_engine' ? 'Resume Scanner' : 'Pause Scanner'}
          </button>
        {/if}

        <button
          type="button"
          class="ping danger-btn"
          class:armed={cancelArmed}
          disabled={!busy}
          onclick={handleArmCancel}
        >
          {cancelArmed ? 'Confirm Cancel' : 'Cancel Scan'}
        </button>

        <button
          type="button"
          class="ping"
          class:armed={syncArmed}
          onclick={handleArmSync}
        >
          {syncArmed ? 'Confirm Defect Sync' : 'Sync High+ → Wordkeep'}
        </button>

        <button
          type="button"
          class="ping"
          onclick={(e) => {
            rippleFromEvent(e);
            onInstallUserTimers();
          }}
        >
          Enable/Refresh User Timers
        </button>
      </div>

      {#if receipt}
        <div class="receipt" role="status">{receipt}</div>
      {/if}
    </div>

    <div class="root-box">
      <div class="gauge-title">Opt-In Root Full-System Timer</div>
      <p class="muted-note">
        Never auto-runs <code>sudo</code>. Copy for your own Konsole TTY to install a weekly root
        sweep writing <code>0644</code> logs to <code>/var/lib/bellator/</code>:
      </p>
      <div class="cmd-row">
        <code>{rootOptInCmd}</code>
        <button type="button" class="copy-btn" onclick={copyRootInstaller}>
          {copiedRootCmd ? 'Copied' : 'Copy'}
        </button>
      </div>
    </div>

    <div class="log-box">
      <div class="gauge-title">Live Scanner Log Tail</div>
      <pre class="log-tail">{#if liveScan?.recent_log_lines?.length}{liveScan.recent_log_lines.join('\n')}{:else}Scanner idle — launch a profile or inspect recent runs.{/if}</pre>
    </div>
  {/if}
</aside>

<style>
  .inspector {
    position: relative;
    width: 19.5rem;
    min-height: 100%;
    padding: 0.75rem 0.75rem;
    background: rgba(22, 15, 29, 0.82);
    backdrop-filter: blur(14px);
    border-left: 1px solid rgba(201, 111, 208, 0.28);
    display: flex;
    flex-direction: column;
    gap: 0.65rem;
    overflow-y: auto;
    transition: width 180ms var(--ease-snap, cubic-bezier(0.22, 1, 0.36, 1));
  }
  .inspector.collapsed {
    width: 2.2rem;
    padding: 0.75rem 0.2rem;
  }
  .edge {
    position: absolute;
    left: -0.75rem;
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
  }
  .insp-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.4rem;
    padding-bottom: 0.4rem;
    border-bottom: 1px solid rgba(201, 111, 208, 0.22);
    font-size: 0.74rem;
    letter-spacing: 0.07em;
    color: #cbb8d0;
  }
  .sfx-btn {
    border: 1px solid rgba(201, 111, 208, 0.35);
    background: rgba(13, 9, 18, 0.65);
    color: #f0e6f2;
    border-radius: 999px;
    padding: 0.15rem 0.5rem;
    font-size: 0.7rem;
    cursor: pointer;
  }
  .sfx-btn.on {
    border-color: #6bcf8e;
    color: #6bcf8e;
  }
  .systems {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.4rem;
  }
  .pill {
    display: flex;
    align-items: center;
    gap: 0.45rem;
    padding: 0.4rem 0.5rem;
    border-radius: 0.65rem;
    border: 1px solid rgba(201, 111, 208, 0.3);
    background: rgba(13, 9, 18, 0.58);
  }
  .pill .k {
    display: block;
    font-size: 0.66rem;
    color: #a894ab;
  }
  .pill strong {
    font-size: 0.78rem;
    color: #fff4e8;
  }
  .dot {
    width: 0.52rem;
    height: 0.52rem;
    border-radius: 50%;
    background: #6b5a72;
    flex: 0 0 auto;
  }
  .dot.live {
    background: #6bcf8e;
    box-shadow: 0 0 8px rgba(107, 207, 142, 0.85);
    animation: live-pulse 2.2s ease-in-out infinite;
  }
  .dot.wait {
    background: #d4a24a;
    box-shadow: 0 0 8px rgba(212, 162, 74, 0.7);
  }
  .dot.down {
    background: #e06b7a;
    box-shadow: 0 0 8px rgba(224, 107, 122, 0.8);
  }
  @keyframes live-pulse {
    0%,
    100% {
      transform: scale(1);
      opacity: 0.9;
    }
    50% {
      transform: scale(1.22);
      opacity: 1;
    }
  }
  .gauges,
  .controls,
  .root-box,
  .log-box {
    padding: 0.5rem 0.55rem;
    border-radius: 0.7rem;
    border: 1px solid rgba(201, 111, 208, 0.24);
    background: rgba(13, 9, 18, 0.52);
    display: flex;
    flex-direction: column;
    gap: 0.38rem;
  }
  .gauge-title {
    font-size: 0.69rem;
    text-transform: uppercase;
    letter-spacing: 0.07em;
    color: #a894ab;
  }
  .gauge-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.35rem;
  }
  .g-cell {
    background: rgba(22, 15, 29, 0.75);
    padding: 0.3rem 0.4rem;
    border-radius: 0.45rem;
    border: 1px solid rgba(201, 111, 208, 0.18);
  }
  .g-cell .k {
    display: block;
    font-size: 0.64rem;
    color: #a894ab;
  }
  .g-cell code {
    font-size: 0.73rem;
    color: #7ec8e3;
  }
  .live-file {
    font-size: 0.7rem;
    background: rgba(22, 15, 29, 0.9);
    padding: 0.32rem 0.45rem;
    border-radius: 0.45rem;
    border: 1px solid rgba(126, 200, 227, 0.3);
    overflow: hidden;
  }
  .live-file .k {
    display: block;
    color: #a894ab;
    font-size: 0.64rem;
  }
  .live-file code {
    display: block;
    color: #fff4e8;
    word-break: break-all;
    font-size: 0.7rem;
  }
  .muted-note {
    margin: 0;
    font-size: 0.72rem;
    color: #a894ab;
    line-height: 1.35;
  }
  .btn-grid {
    display: flex;
    flex-direction: column;
    gap: 0.32rem;
  }
  .ping {
    width: 100%;
    padding: 0.42rem 0.6rem;
    border-radius: 0.55rem;
    border: 1px solid rgba(201, 111, 208, 0.35);
    background: rgba(22, 15, 29, 0.85);
    color: #f0e6f2;
    font-size: 0.76rem;
    cursor: pointer;
    transition: border-color 140ms ease, box-shadow 140ms ease;
  }
  .ping:hover:not(:disabled) {
    border-color: #c96fd0;
  }
  .ping.armed {
    border-color: #ff8c42;
    color: #ff8c42;
    box-shadow: 0 0 14px rgba(255, 140, 66, 0.45);
  }
  .ping.danger-btn:hover:not(:disabled) {
    border-color: #e06b7a;
  }
  .ping:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
  .receipt {
    font-size: 0.72rem;
    padding: 0.35rem 0.5rem;
    border-radius: 0.5rem;
    background: rgba(107, 207, 142, 0.16);
    border: 1px solid rgba(107, 207, 142, 0.45);
    color: #6bcf8e;
  }
  .cmd-row {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    background: rgba(8, 5, 12, 0.75);
    padding: 0.32rem 0.45rem;
    border-radius: 0.45rem;
    border: 1px solid rgba(201, 111, 208, 0.25);
  }
  .cmd-row code {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 0.68rem;
    color: #ff8c42;
  }
  .copy-btn {
    border: 1px solid rgba(201, 111, 208, 0.4);
    background: rgba(22, 15, 29, 0.9);
    color: #f0e6f2;
    border-radius: 0.4rem;
    padding: 0.18rem 0.45rem;
    font-size: 0.68rem;
    cursor: pointer;
  }
  .log-tail {
    margin: 0;
    max-height: 11rem;
    overflow: auto;
    font-family: 'IBM Plex Mono', monospace;
    font-size: 0.68rem;
    line-height: 1.35;
    color: #d7c4de;
    white-space: pre-wrap;
    word-break: break-word;
  }
</style>
