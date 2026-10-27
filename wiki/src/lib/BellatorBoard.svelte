<script lang="ts">
  import { onMount } from 'svelte';
  import { rippleFromEvent } from './press-ripple';
  import { safeStorage } from './safeStorage';
  import BarChart from './charts/BarChart.svelte';
  import DonutChart from './charts/DonutChart.svelte';
  import Sparkline from './charts/Sparkline.svelte';
  import type { BarDatum, DonutDatum, SparkPoint } from './charts/utils';
  import BellatorSidebar, { type BellatorSection } from './BellatorSidebar.svelte';
  import BellatorInspector from './BellatorInspector.svelte';

  let {
    compact = false,
    query = '',
    onOpenStudio,
  }: {
    compact?: boolean;
    query?: string;
    onOpenStudio?: () => void;
  } = $props();

  const CSRF_HEADERS = {
    'content-type': 'application/json',
    'x-bellator-csrf': 'bellator-local',
  };

  let railCollapsed = $state(safeStorage.getItem('bellator-rail-collapsed') === '1');
  let inspectorCollapsed = $state(safeStorage.getItem('bellator-inspector-collapsed') === '1');
  let sfxOn = $state(safeStorage.getItem('bellator-sfx-on') !== '0');
  let section = $state<BellatorSection>('overview');

  let dash = $state<any>(null);
  let findingsPayload = $state<any>({ findings: [], run_id: null });
  let triagePayload = $state<any>({ entries: [], counts: { suspicious: 0, quarantined: 0, cleared: 0 } });
  let runsPayload = $state<any>({ runs: [], diff: {} });
  let policyPayload = $state<any>(null);
  let liveScan = $state<any>(null);
  let busy = $state(false);
  let engineActive = $state(false);
  let loading = $state(true);
  let errorMsg = $state('');
  let receipt = $state('');
  let trafficPulse = $state(false);

  // Scan launcher controls
  let selectedProfile = $state('full-nexus');
  let selectedPreset = $state('quick');
  let customRunners = $state<string[]>([]);
  let trivyImage = $state('');

  // Findings filters & explain drawer
  let sevFilter = $state('all');
  let toolFilter = $state('all');
  let statusFilter = $state('open');
  let searchFilter = $state('');
  let selectedFinding = $state<any>(null);
  let explainCard = $state<any>(null);
  let triageNote = $state('');
  let triageOwner = $state('inatos');

  // Runs compare state
  let compareBase = $state('');
  let compareHead = $state('');
  let compareResult = $state<any>(null);

  // Suppressions draft state
  let suppFindingId = $state('');
  let suppRuleId = $state('');
  let suppReason = $state('');
  let suppOwner = $state('inatos');

  // Two-step arm for advisory refresh
  let refreshArmed = $state(false);
  let refreshBusy = $state(false);
  let suppExpires = $state('2026-12-31');

  function playCue(kind: 'tick' | 'arm' | 'chime' | 'alert') {
    if (!sfxOn) return;
    if (
      typeof window !== 'undefined' &&
      window.matchMedia?.('(prefers-reduced-motion: reduce)').matches
    ) {
      return;
    }
    try {
      const file =
        kind === 'chime'
          ? '/elucifia/sfx/chime.wav'
          : kind === 'arm' || kind === 'alert'
            ? '/elucifia/sfx/mood.wav'
            : '/elucifia/sfx/tick.wav';
      const audio = new Audio(file);
      audio.volume = kind === 'tick' ? 0.16 : 0.24;
      audio.play().catch(() => {
        const Ctx = window.AudioContext || (window as any).webkitAudioContext;
        if (!Ctx) return;
        const ctx = new Ctx();
        const osc = ctx.createOscillator();
        const gain = ctx.createGain();
        osc.type = 'sine';
        osc.frequency.value =
          kind === 'chime' ? 660 : kind === 'alert' ? 260 : kind === 'arm' ? 440 : 520;
        gain.gain.setValueAtTime(0.04, ctx.currentTime);
        gain.gain.exponentialRampToValueAtTime(0.001, ctx.currentTime + 0.11);
        osc.connect(gain);
        gain.connect(ctx.destination);
        osc.start();
        osc.stop(ctx.currentTime + 0.12);
      });
    } catch {
      // ignore audio errors
    }
  }

  function showReceipt(msg: string, cue: 'chime' | 'arm' | 'alert' = 'chime') {
    receipt = msg;
    trafficPulse = true;
    playCue(cue);
    setTimeout(() => (trafficPulse = false), 750);
    setTimeout(() => {
      if (receipt === msg) receipt = '';
    }, 4400);
  }

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

  async function refreshAll() {
    try {
      const [dRes, fRes, tRes, rRes] = await Promise.all([
        fetch('/api/bellator/dashboard'),
        fetch(
          `/api/bellator/findings?severity=${encodeURIComponent(sevFilter)}&tool=${encodeURIComponent(toolFilter)}&status=${encodeURIComponent(statusFilter)}&q=${encodeURIComponent(searchFilter || query)}`
        ),
        fetch('/api/bellator/triage'),
        fetch('/api/bellator/runs'),
      ]);
      if (dRes.ok) {
        dash = await dRes.json();
        busy = Boolean(dash.busy);
        engineActive = Boolean(dash.engine_active);
        liveScan = dash.live_scan ?? null;
        if (!selectedProfile && dash.default_profile) {
          selectedProfile = dash.default_profile;
        }
      }
      if (fRes.ok) findingsPayload = await fRes.json();
      if (tRes.ok) triagePayload = await tRes.json();
      if (rRes.ok) {
        runsPayload = await rRes.json();
        const runs = runsPayload.runs ?? [];
        if (runs.length >= 2 && (!compareBase || !compareHead)) {
          compareHead = runs[0].id;
          compareBase = runs[1].id;
        }
      }
      errorMsg = '';
    } catch (e) {
      errorMsg = String(e);
    } finally {
      loading = false;
    }
  }

  async function refreshPolicy() {
    try {
      const res = await fetch('/api/bellator/policy');
      if (res.ok) policyPayload = await res.json();
    } catch {
      // ignore
    }
  }

  async function triggerScan(profileOverride?: string, presetOverride?: string) {
    const profile = profileOverride ?? selectedProfile;
    const preset = presetOverride ?? selectedPreset;
    try {
      const body: Record<string, any> = { profile, preset };
      if (preset === 'custom' && customRunners.length > 0) {
        body.runners = customRunners;
      }
      if (trivyImage.trim()) {
        body.image = trivyImage.trim();
      }
      const res = await fetch('/api/bellator/scans', {
        method: 'POST',
        headers: CSRF_HEADERS,
        body: JSON.stringify(body),
      });
      const data = await res.json();
      if (!res.ok) {
        showReceipt(`Scan blocked: ${data.error || res.statusText}`, 'alert');
        return;
      }
      showReceipt(`Queued ${profile} (${preset}) → ${data.run_id}`, 'chime');
      await refreshAll();
    } catch (e) {
      showReceipt(`Failed to start scan: ${e}`, 'alert');
    }
  }

  async function handlePauseResume(pause: boolean) {
    try {
      const res = await fetch('/api/bellator/scans/pause', {
        method: 'POST',
        headers: CSRF_HEADERS,
        body: JSON.stringify({ pause }),
      });
      if (res.ok) {
        showReceipt(pause ? 'Scanner paused (SIGSTOP)' : 'Scanner resumed (SIGCONT)', 'arm');
        await refreshAll();
      }
    } catch (e) {
      showReceipt(`Pause/resume failed: ${e}`, 'alert');
    }
  }

  async function handleCancelScan() {
    try {
      const res = await fetch('/api/bellator/scans/cancel', {
        method: 'POST',
        headers: CSRF_HEADERS,
        body: JSON.stringify({ run_id: liveScan?.run_id }),
      });
      if (res.ok) {
        showReceipt('Cancel requested for active scan', 'arm');
        await refreshAll();
      }
    } catch (e) {
      showReceipt(`Cancel failed: ${e}`, 'alert');
    }
  }

  async function handleSyncDefects() {
    try {
      const res = await fetch('/api/bellator/sync-defects', {
        method: 'POST',
        headers: CSRF_HEADERS,
        body: JSON.stringify({}),
      });
      const data = await res.json();
      if (res.ok) {
        showReceipt(
          `Synced ${data.upserted ?? 0} High/Critical finding(s) to .wordkeep/defects.json`,
          'chime'
        );
      }
    } catch (e) {
      showReceipt(`Defect sync failed: ${e}`, 'alert');
    }
  }

  async function handleInstallUserTimers() {
    try {
      const res = await fetch('/api/bellator/schedule/install-user', {
        method: 'POST',
        headers: CSRF_HEADERS,
      });
      const data = await res.json();
      if (res.ok && data.ok) {
        showReceipt('Installed & enabled systemd --user Bellator timers', 'chime');
        await refreshAll();
      } else {
        showReceipt(`Timer install failed: ${data.stderr || 'error'}`, 'alert');
      }
    } catch (e) {
      showReceipt(`Timer install error: ${e}`, 'alert');
    }
  }

  async function handleAdvisoryRefresh() {
    if (!refreshArmed) {
      refreshArmed = true;
      playCue('arm');
      showReceipt('Armed — click again to refresh advisories / signatures / CISA KEV', 'arm');
      setTimeout(() => {
        refreshArmed = false;
      }, 8000);
      return;
    }
    refreshArmed = false;
    refreshBusy = true;
    try {
      const res = await fetch('/api/bellator/schedule/refresh', {
        method: 'POST',
        headers: CSRF_HEADERS,
      });
      const data = await res.json();
      if (res.ok) {
        showReceipt(
          `Advisory refresh done (${data.findings ?? 0} findings${data.advisory_stale ? ', still stale' : ''})`,
          data.advisory_stale ? 'alert' : 'chime'
        );
        await refreshAll();
      } else {
        showReceipt(`Advisory refresh failed: ${data.error || res.statusText}`, 'alert');
      }
    } catch (e) {
      showReceipt(`Advisory refresh error: ${e}`, 'alert');
    } finally {
      refreshBusy = false;
    }
  }

  async function toggleScheduleFlag(key: string, nextVal: boolean) {
    try {
      const res = await fetch('/api/bellator/schedule/toggle', {
        method: 'POST',
        headers: CSRF_HEADERS,
        body: JSON.stringify({ [key]: nextVal }),
      });
      if (res.ok) {
        showReceipt(`Updated schedule.${key} = ${nextVal}`, 'tick');
        await refreshAll();
      }
    } catch (e) {
      showReceipt(`Failed to update schedule: ${e}`, 'alert');
    }
  }

  async function openExplain(item: any) {
    selectedFinding = item;
    explainCard = null;
    playCue('tick');
    const id = item?.finding?.id || item?.finding?.rule_id || item;
    try {
      const res = await fetch(`/api/bellator/explain/${encodeURIComponent(id)}`);
      if (res.ok) explainCard = await res.json();
    } catch {
      // ignore
    }
  }

  async function applyTriage(finding: any, state: string) {
    try {
      const res = await fetch('/api/bellator/triage', {
        method: 'PUT',
        headers: CSRF_HEADERS,
        body: JSON.stringify({
          finding_id: finding.id,
          state,
          owner: triageOwner || 'inatos',
          note: triageNote || undefined,
          source_run: findingsPayload.run_id,
          title: finding.title,
          severity: finding.severity,
          tool: finding.tool,
        }),
      });
      if (res.ok) {
        showReceipt(`Marked ${finding.id} as ${state}`, 'chime');
        await refreshAll();
      }
    } catch (e) {
      showReceipt(`Triage update failed: ${e}`, 'alert');
    }
  }

  async function runCompare() {
    if (!compareBase || !compareHead) return;
    try {
      const res = await fetch(
        `/api/bellator/runs/compare?base=${encodeURIComponent(compareBase)}&head=${encodeURIComponent(compareHead)}`
      );
      if (res.ok) {
        compareResult = await res.json();
        playCue('tick');
      }
    } catch {
      // ignore
    }
  }

  async function submitSuppressionDraft() {
    try {
      const res = await fetch('/api/bellator/suppressions/draft', {
        method: 'POST',
        headers: CSRF_HEADERS,
        body: JSON.stringify({
          finding_id: suppFindingId || undefined,
          rule_id: suppRuleId || undefined,
          reason: suppReason,
          owner: suppOwner,
          expires: suppExpires,
        }),
      });
      const data = await res.json();
      if (res.ok) {
        showReceipt(`Appended draft to ${data.draft_path}`, 'chime');
        suppReason = '';
        await refreshPolicy();
      } else {
        showReceipt(`Draft error: ${data.error || res.statusText}`, 'alert');
      }
    } catch (e) {
      showReceipt(`Draft error: ${e}`, 'alert');
    }
  }

  function selectSection(next: BellatorSection) {
    section = next;
    playCue('tick');
    if (next === 'policy') void refreshPolicy();
  }

  onMount(() => {
    void refreshAll();
    void refreshPolicy();

    const poll = setInterval(() => {
      void refreshAll();
    }, 3000);

    let es: EventSource | null = null;
    try {
      es = new EventSource('/api/bellator/events');
      es.onmessage = (ev) => {
        try {
          const frame = JSON.parse(ev.data);
          if (frame && frame.type === 'live') {
            busy = Boolean(frame.busy);
            engineActive = Boolean(frame.engine_active);
            liveScan = frame.live ?? null;
          }
        } catch {
          // ignore parse error
        }
      };
    } catch {
      // fallback to 3s polling
    }

    const onKey = (ev: KeyboardEvent) => {
      if (
        ev.target instanceof HTMLInputElement ||
        ev.target instanceof HTMLTextAreaElement ||
        ev.target instanceof HTMLSelectElement
      ) {
        return;
      }
      if (ev.key === '[') {
        railCollapsed = !railCollapsed;
        safeStorage.setItem('bellator-rail-collapsed', railCollapsed ? '1' : '0');
      } else if (ev.key === ']') {
        inspectorCollapsed = !inspectorCollapsed;
        safeStorage.setItem('bellator-inspector-collapsed', inspectorCollapsed ? '1' : '0');
      } else if (ev.key === '1') selectSection('overview');
      else if (ev.key === '2') selectSection('scans');
      else if (ev.key === '3') selectSection('schedule');
      else if (ev.key === '4') selectSection('findings');
      else if (ev.key === '5') selectSection('watchlist');
      else if (ev.key === '6') selectSection('host');
      else if (ev.key === '7') selectSection('runs');
      else if (ev.key === '8') selectSection('policy');
    };
    window.addEventListener('keydown', onKey);

    return () => {
      clearInterval(poll);
      if (es) es.close();
      window.removeEventListener('keydown', onKey);
    };
  });

  const severityDonut = $derived<DonutDatum[]>(
    dash?.severity
      ? Object.entries(dash.severity).map(([label, value]) => ({
          label,
          value: Number(value) || 0,
        }))
      : []
  );

  const toolBars = $derived<BarDatum[]>(
    dash?.by_tool
      ? Object.entries(dash.by_tool).map(([label, value]) => ({
          label,
          value: Number(value) || 0,
        }))
      : []
  );

  const trendSpark = $derived<SparkPoint[]>(
    (dash?.trend ?? []).map((pt: any, idx: number) => ({
      ts: idx + 1,
      value: Number(pt.findings) || 0,
      label: `${pt.profile}: ${pt.findings} open`,
    }))
  );

  const activeTimersCount = $derived<number>(
    (dash?.timers ?? []).filter((t: any) => t.active).length
  );

  const watchlistEntries = $derived<any[]>(
    (triagePayload?.entries ?? []).filter(
      (e: any) => e.state === 'suspicious' || e.state === 'quarantined'
    )
  );

  const hostFindings = $derived<any[]>(
    (findingsPayload?.findings ?? []).filter((row: any) =>
      [
        'persistence-audit',
        'network-socket-audit',
        'lfs-integrity-audit',
        'clamav',
        'clamav-quick',
        'clamav-full',
      ].includes(row.finding?.tool)
    )
  );
</script>

{#if compact}
  <div class="compact-wrap">
    <div class="traffic" class:pulse={trafficPulse}>
      <span
        class="dot"
        class:live={Boolean(liveScan) || (dash?.gate_ok ?? true)}
        class:down={dash && !dash.gate_ok}
      ></span>
      <strong>Bellator Admin-Sec</strong>
      {#if liveScan}
        <span class="work">
          [{liveScan.profile}] {liveScan.current_runner ?? 'scanner'} · {formatDuration(
            liveScan.elapsed_secs ?? 0
          )} · {liveScan.current_file ?? ''}
        </span>
      {:else}
        <span class="work">
          Gate: {dash?.gate_ok !== false ? 'PASS' : 'BLOCK'} · Open: {dash?.open_total ?? 0} (High+:
          {dash?.open_high_plus ?? 0}) · Timers: {activeTimersCount}/{(dash?.timers ?? []).length} active
        </span>
      {/if}
      {#if dash?.last_green_delta}
        <span class="delta-chip">
          +{dash.last_green_delta.new_count} / -{dash.last_green_delta.cleared_count} vs green
        </span>
      {/if}
      {#if onOpenStudio}
        <button
          type="button"
          class="open-studio-btn"
          onclick={(e) => {
            rippleFromEvent(e);
            onOpenStudio();
          }}
        >
          Open Full Studio →
        </button>
      {/if}
    </div>

    <div class="cards dash-overview">
      <article class="card">
        <h3>Security Gate</h3>
        <p class="metric" class:good={dash?.gate_ok !== false} class:bad={dash && !dash.gate_ok}>
          <code>{dash?.gate_ok !== false ? 'PASS' : 'BLOCK'}</code>
        </p>
      </article>
      <article class="card">
        <h3>Open / High+</h3>
        <p class="metric">
          <code>{dash?.open_total ?? 0} · {dash?.open_high_plus ?? 0} High+</code>
        </p>
      </article>
      <article class="card">
        <h3>Scheduled Timers</h3>
        <p class="metric">
          <code>{activeTimersCount} / {(dash?.timers ?? []).length} active</code>
        </p>
      </article>
      <article class="card">
        <h3>Live Scanner</h3>
        <p class="metric">
          <code>
            {#if liveScan}
              {liveScan.current_runner ?? 'clamscan'} ({formatDuration(liveScan.elapsed_secs ?? 0)})
            {:else}
              Idle
            {/if}
          </code>
        </p>
      </article>
    </div>
  </div>
{:else}
  <div class="studio">
    <BellatorSidebar
      collapsed={railCollapsed}
      {section}
      openCount={dash?.open_total ?? 0}
      highPlusCount={dash?.open_high_plus ?? 0}
      watchlistCount={watchlistEntries.length}
      {activeTimersCount}
      {busy}
      onToggleCollapse={() => {
        railCollapsed = !railCollapsed;
        safeStorage.setItem('bellator-rail-collapsed', railCollapsed ? '1' : '0');
      }}
      onSelectSection={selectSection}
      onQuickLaunch={(prof, pres) => void triggerScan(prof, pres)}
    />

    <div class="stage">
      <div class="wash" aria-hidden="true">
        <div class="blob a"></div>
        <div class="blob b"></div>
        <div class="scan"></div>
      </div>

      <div class="traffic" class:pulse={trafficPulse} role="status">
        <span
          class="dot"
          class:live={Boolean(liveScan) || (dash?.gate_ok ?? true)}
          class:wait={engineActive}
          class:down={dash && !dash.gate_ok}
        ></span>
        <strong>
          {#if liveScan}
            {liveScan.is_external_root_sweep ? 'LIVE ROOT SWEEP' : 'SCANNING'}
          {:else if dash?.gate_ok === false}
            GATE BLOCKED
          {:else}
            GUARDIAN READY
          {/if}
        </strong>
        <span class="work">
          {#if liveScan}
            {liveScan.current_runner ?? 'scanner'} (PID {liveScan.child_pid ?? liveScan.pid}) ·
            {formatDuration(liveScan.elapsed_secs ?? 0)} · {(liveScan.cpu_pct ?? 0).toFixed(1)}% CPU ·
            {liveScan.current_file ?? 'walking filesystem…'}
          {:else}
            Open findings: {dash?.open_total ?? 0} ({dash?.open_high_plus ?? 0} High+) · Coverage:
            {dash?.scanner_coverage?.available ?? 0}/{dash?.scanner_coverage?.total ?? 0} runners ·
            Timers: {activeTimersCount} active
          {/if}
        </span>
        {#if dash?.last_green_delta}
          <span
            class="delta-chip"
            title={`Compared against baseline ${dash.last_green_delta.base_id}`}
          >
            +{dash.last_green_delta.new_count} new · -{dash.last_green_delta.cleared_count} cleared ·
            {dash.last_green_delta.still_open_count} open
          </span>
        {/if}
      </div>

      <div class="stage-body">
        {#if errorMsg}
          <p class="err-banner">Bellator API error: <code>{errorMsg}</code></p>
        {/if}

        {#if section === 'overview'}
          <div class="deck">
            <header class="deck-head">
              <div>
                <h2>Nexus Security Posture & Live Sweep Status</h2>
                <p class="sub">
                  Normalized CVE, secret, workflow, ClamAV malware, persistence, socket, and Git LFS
                  gates.
                </p>
              </div>
              <div class="head-actions">
                <button
                  type="button"
                  class="act-btn"
                  disabled={busy}
                  onclick={(e) => {
                    rippleFromEvent(e);
                    void triggerScan('host-sec', 'quick');
                  }}>⚡ Quick Audit</button
                >
                <button
                  type="button"
                  class="act-btn"
                  disabled={busy}
                  onclick={(e) => {
                    rippleFromEvent(e);
                    void triggerScan('clamav-quick', 'malware');
                  }}>🛡 ClamAV Quick</button
                >
              </div>
            </header>

            <div class="cards dash-overview">
              <article class="card">
                <h3>Deploy & Host Gate</h3>
                <p
                  class="metric"
                  class:good={dash?.gate_ok !== false}
                  class:bad={dash && !dash.gate_ok}
                >
                  <code>{dash?.gate_ok !== false ? 'PASS' : 'BLOCK'}</code>
                </p>
              </article>
              <article class="card">
                <h3>Open Findings</h3>
                <p class="metric">
                  <code>{dash?.open_total ?? 0} ({dash?.open_high_plus ?? 0} High+)</code>
                </p>
              </article>
              <article class="card">
                <h3>Scanner Coverage</h3>
                <p class="metric">
                  <code
                    >{dash?.scanner_coverage?.available ?? 0} / {dash?.scanner_coverage?.total ?? 0}
                    ready</code
                  >
                </p>
              </article>
              <article class="card">
                <h3>Watchlist / Quarantine</h3>
                <p class="metric">
                  <code
                    >{dash?.triage?.suspicious ?? 0} susp · {dash?.triage?.quarantined ?? 0} quar</code
                  >
                </p>
              </article>
            </div>

            {#if liveScan}
              <div class="live-banner">
                <div class="live-head">
                  <strong>
                    Active Scan: {liveScan.run_id} ({liveScan.profile}) —
                    {liveScan.is_external_root_sweep
                      ? 'Adopted External Root ClamAV Sweep'
                      : 'Managed Bellator Worker'}
                  </strong>
                  <span>{liveScan.progress_pct ?? 50}% est</span>
                </div>
                <div class="progress-track">
                  <div
                    class="progress-fill"
                    style={`width:${Math.min(100, Math.max(4, liveScan.progress_pct ?? 50))}%`}
                  ></div>
                </div>
                <div class="live-meta">
                  <span>PID: <code>{liveScan.child_pid ?? liveScan.pid}</code></span>
                  <span>Elapsed: <code>{formatDuration(liveScan.elapsed_secs ?? 0)}</code></span>
                  <span>CPU: <code>{(liveScan.cpu_pct ?? 0).toFixed(1)}%</code></span>
                  <span>RSS: <code>{formatBytes(liveScan.rss_bytes ?? 0)}</code></span>
                  <span>Target: <code>{liveScan.current_file ?? '-'}</code></span>
                </div>
              </div>
            {/if}

            <div class="chart-grid">
              <div class="chart-card">
                <h3>Open Findings by Severity</h3>
                {#if severityDonut.length > 0}
                  <DonutChart data={severityDonut} />
                {:else}
                  <p class="muted">Zero open findings in latest run.</p>
                {/if}
              </div>
              <div class="chart-card">
                <h3>Findings by Scanner Tool</h3>
                {#if toolBars.length > 0}
                  <BarChart data={toolBars} />
                {:else}
                  <p class="muted">All scanners clean.</p>
                {/if}
              </div>
              <div class="chart-card">
                <h3>Open Findings Trend (Last 20 Runs)</h3>
                {#if trendSpark.length > 0}
                  <Sparkline points={trendSpark} />
                {:else}
                  <p class="muted">No run history yet.</p>
                {/if}
              </div>
            </div>

            <h3>Recent Scan Runs</h3>
            <div class="table-wrap">
              <table class="dash-table">
                <thead>
                  <tr>
                    <th>Run ID</th>
                    <th>Profile</th>
                    <th>Status</th>
                    <th>Gate</th>
                    <th class="num">Open</th>
                    <th class="num">High+</th>
                    <th class="num">Duration</th>
                    <th>Started</th>
                  </tr>
                </thead>
                <tbody>
                  {#each dash?.recent_runs ?? [] as r}
                    <tr class:inverted={!r.gate_ok}>
                      <td class="tool-name"><code>{r.id}</code></td>
                      <td><code>{r.profile}</code></td>
                      <td>{r.status}</td>
                      <td>
                        <span class="chip" class:active={r.gate_ok}>
                          {r.gate_ok ? 'PASS' : 'BLOCK'}
                        </span>
                      </td>
                      <td class="num">{r.counts?.open ?? 0}</td>
                      <td class="num">{(r.counts?.critical ?? 0) + (r.counts?.high ?? 0)}</td>
                      <td class="num">{r.duration_ms} ms</td>
                      <td><code>{r.started_at}</code></td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
          </div>
        {:else if section === 'scans'}
          <div class="deck">
            <header class="deck-head">
              <div>
                <h2>Scans & ClamAV Malware Sweeps</h2>
                <p class="sub">
                  Run targeted or full-nexus sweeps with live `/proc/pid/fd` file tracking and
                  Megistus `SIGSTOP` engine protection.
                </p>
              </div>
            </header>

            <div class="launcher-grid">
              <label>
                <span>Profile</span>
                <select bind:value={selectedProfile}>
                  {#each dash?.profiles ?? [] as p}
                    <option value={p.id}>
                      {p.id} ({p.roots?.length ? p.roots.join(', ') : 'workspace root'})
                    </option>
                  {/each}
                </select>
              </label>

              <label>
                <span>Preset</span>
                <select bind:value={selectedPreset}>
                  {#each dash?.presets ?? ['quick', 'host', 'malware', 'clamav-full', 'dependencies', 'secrets', 'workflows', 'full'] as pr}
                    <option value={pr}>{pr}</option>
                  {/each}
                </select>
              </label>

              <label>
                <span>Digest-Pinned Image (Optional Trivy)</span>
                <input
                  type="text"
                  bind:value={trivyImage}
                  placeholder="ghcr.io/inatos/arathyll-frontend@sha256:…"
                />
              </label>

              <div class="launch-act">
                <button
                  type="button"
                  class="act-btn primary"
                  disabled={busy}
                  onclick={(e) => {
                    rippleFromEvent(e);
                    void triggerScan();
                  }}
                >
                  {busy ? 'Scan Running…' : 'Launch Scan'}
                </button>
              </div>
            </div>

            <h3>Scanner Catalog & Availability</h3>
            <div class="table-wrap">
              <table class="dash-table">
                <thead>
                  <tr>
                    <th>Runner</th>
                    <th>Kind</th>
                    <th>Status</th>
                    <th>Version</th>
                    <th>Presets</th>
                    <th>Description / Install Hint</th>
                  </tr>
                </thead>
                <tbody>
                  {#each dash?.runners ?? [] as r}
                    <tr>
                      <td class="tool-name"><code>{r.id}</code></td>
                      <td>{r.kind}</td>
                      <td>
                        <span class="chip" class:active={r.available}>
                          {r.available ? 'Available' : 'Missing'}
                        </span>
                      </td>
                      <td><code>{r.version ?? '-'}</code></td>
                      <td><code>{(r.presets ?? []).join(', ')}</code></td>
                      <td class="text">
                        {r.description}
                        {#if !r.available && r.install_hint}
                          <br /><code>{r.install_hint}</code>
                        {/if}
                      </td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
          </div>
        {:else if section === 'schedule'}
          <div class="deck">
            <header class="deck-head">
              <div>
                <h2>Scheduled Scans & Megistus Zero-Hitch Cgroup Fence</h2>
                <p class="sub">
                  Systemd user timers run under <code>Nice=19</code>,
                  <code>CPUSchedulingPolicy=idle</code>, <code>IOSchedulingClass=idle</code>, and
                  <code>CPUQuota=200%</code>, pausing automatically if Betwixt or Tracy starts.
                </p>
              </div>
              <div class="deck-actions">
                <button
                  type="button"
                  class="act-btn"
                  class:armed={refreshArmed}
                  disabled={refreshBusy || busy}
                  onclick={(e) => {
                    rippleFromEvent(e);
                    void handleAdvisoryRefresh();
                  }}
                >
                  {#if refreshBusy}
                    Refreshing…
                  {:else if refreshArmed}
                    Confirm refresh now
                  {:else}
                    Refresh advisories now
                  {/if}
                </button>
                <button
                  type="button"
                  class="act-btn"
                  onclick={(e) => {
                    rippleFromEvent(e);
                    void handleInstallUserTimers();
                  }}
                >
                  ↻ Reinstall & Enable User Timers
                </button>
              </div>
            </header>

            <div class="toggle-grid">
              {#if dash?.schedule}
                <label class="toggle-card">
                  <input
                    type="checkbox"
                    checked={dash.schedule.enabled}
                    onchange={(e) =>
                      void toggleScheduleFlag('enabled', (e.currentTarget as HTMLInputElement).checked)}
                  />
                  <div>
                    <strong>Master Schedule Enabled</strong>
                    <span>Allow scheduled timers to execute `bellator schedule run-due`</span>
                  </div>
                </label>

                <label class="toggle-card">
                  <input
                    type="checkbox"
                    checked={dash.schedule.daily_advisory_refresh ?? true}
                    onchange={(e) =>
                      void toggleScheduleFlag(
                        'daily_advisory_refresh',
                        (e.currentTarget as HTMLInputElement).checked
                      )}
                  />
                  <div>
                    <strong>Daily Advisory / Signature / KEV Refresh</strong>
                    <span>Refresh DBs + CISA KEV at 03:45 (and before scheduled scans)</span>
                  </div>
                </label>

                <label class="toggle-card">
                  <input
                    type="checkbox"
                    checked={dash.schedule.daily_quick_clamav}
                    onchange={(e) =>
                      void toggleScheduleFlag(
                        'daily_quick_clamav',
                        (e.currentTarget as HTMLInputElement).checked
                      )}
                  />
                  <div>
                    <strong>Daily Quick ClamAV Sweep</strong>
                    <span>Scan ingress, Wine prefixes, and web/arathyll daily at 04:15</span>
                  </div>
                </label>

                <label class="toggle-card">
                  <input
                    type="checkbox"
                    checked={dash.schedule.weekly_full_clamav}
                    onchange={(e) =>
                      void toggleScheduleFlag(
                        'weekly_full_clamav',
                        (e.currentTarget as HTMLInputElement).checked
                      )}
                  />
                  <div>
                    <strong>Weekly Deep ClamAV & Full-Nexus</strong>
                    <span>Full workspace + media malware & CVE sweep Sundays at 03:30</span>
                  </div>
                </label>

                <label class="toggle-card">
                  <input
                    type="checkbox"
                    checked={dash.schedule.pause_on_engine_active}
                    onchange={(e) =>
                      void toggleScheduleFlag(
                        'pause_on_engine_active',
                        (e.currentTarget as HTMLInputElement).checked
                      )}
                  />
                  <div>
                    <strong>Megistus Engine Watchdog (`SIGSTOP`)</strong>
                    <span>Freeze active background scanners whenever `betwixt` or `tracy-capture` runs</span>
                  </div>
                </label>
              {/if}
            </div>

            <h3>Systemd User Timers</h3>
            <div class="table-wrap">
              <table class="dash-table">
                <thead>
                  <tr>
                    <th>Timer Unit</th>
                    <th>Label</th>
                    <th>Cadence</th>
                    <th>Active</th>
                    <th>Enabled</th>
                    <th>Next Trigger</th>
                    <th>Last Trigger</th>
                  </tr>
                </thead>
                <tbody>
                  {#each dash?.timers ?? [] as t}
                    <tr>
                      <td class="tool-name"><code>{t.unit}</code></td>
                      <td>{t.label}</td>
                      <td><code>{t.cadence}</code></td>
                      <td>
                        <span class="chip" class:active={t.active}>
                          {t.active ? 'active' : 'inactive'}
                        </span>
                      </td>
                      <td>{t.enabled ? 'enabled' : 'disabled'}</td>
                      <td><code>{t.next_elapses ?? 'n/a'}</code></td>
                      <td><code>{t.last_trigger ?? 'never'}</code></td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>

            <h3>
              Advisory & Signature Database Freshness Receipts
              {#if dash?.advisory_stale}
                <span class="chip">stale</span>
              {:else}
                <span class="chip active">fresh</span>
              {/if}
            </h3>
            <p class="sub">
              Required sources:
              {(dash?.advisory_stale_detail?.required_sources ?? ['clamav-db', 'rustsec-db', 'osv-db', 'cisa-kev']).join(', ')}
              · window {dash?.stale_advisory_db_hours ?? 36}h
            </p>
            <div class="table-wrap">
              <table class="dash-table">
                <thead>
                  <tr>
                    <th>Source</th>
                    <th>Required</th>
                    <th>Status</th>
                    <th>Age (h)</th>
                    <th>Last Refreshed</th>
                    <th>Version / Note</th>
                  </tr>
                </thead>
                <tbody>
                  {#each Object.values(dash?.advisory_receipts?.sources ?? {}) as rec}
                    {@const r = rec as any}
                    {@const req = (dash?.advisory_stale_detail?.required_sources ?? []).includes(r.source)}
                    {@const row = (dash?.advisory_stale_detail?.breakdown ?? []).find(
                      (b: any) => b.source === r.source
                    )}
                    <tr>
                      <td class="tool-name"><code>{r.source}</code></td>
                      <td>{req ? 'yes' : 'optional'}</td>
                      <td>
                        <span class="chip" class:active={r.ok && !row?.stale}>
                          {row?.stale ? 'Stale' : r.ok ? 'Fresh' : 'Error'}
                        </span>
                      </td>
                      <td>
                        <code
                          >{row?.age_hours != null ? Number(row.age_hours).toFixed(1) : '—'}</code
                        >
                      </td>
                      <td><code>{r.refreshed_at_iso}</code></td>
                      <td><code>{r.version_or_note ?? '-'}</code></td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
          </div>
        {:else if section === 'findings'}
          <div class="deck">
            <header class="deck-head">
              <div>
                <h2>Findings & Remediation Advisor (`bellator explain`)</h2>
                <p class="sub">
                  Click any finding row to open its OWASP ASVS remediation drawer and logical triage
                  controls.
                </p>
              </div>
            </header>

            <div class="filter-bar">
              <select bind:value={sevFilter} onchange={() => void refreshAll()}>
                <option value="all">All Severities</option>
                <option value="critical">Critical</option>
                <option value="high">High</option>
                <option value="medium">Medium</option>
                <option value="low">Low</option>
              </select>

              <select bind:value={statusFilter} onchange={() => void refreshAll()}>
                <option value="open">Open / Unreviewed</option>
                <option value="all">All Statuses</option>
                <option value="suspicious">Suspicious</option>
                <option value="quarantined">Quarantined</option>
                <option value="cleared">Cleared</option>
                <option value="suppressed">Suppressed</option>
              </select>

              <input
                type="search"
                bind:value={searchFilter}
                placeholder="Filter findings by id, rule, tool, or path…"
                oninput={() => void refreshAll()}
              />
            </div>

            <div class="split-findings">
              <div class="table-wrap">
                <table class="dash-table">
                  <thead>
                    <tr>
                      <th>Severity</th>
                      <th>Tool</th>
                      <th>Rule</th>
                      <th>Title</th>
                      <th>Status</th>
                      <th>Targets</th>
                    </tr>
                  </thead>
                  <tbody>
                    {#each findingsPayload?.findings ?? [] as row}
                      <tr
                        class:inverted={row.finding.severity === 'critical' ||
                          row.finding.severity === 'high'}
                        onclick={() => void openExplain(row)}
                        style="cursor:pointer"
                      >
                        <td><code>{row.finding.severity}</code></td>
                        <td class="tool-name"><code>{row.finding.tool}</code></td>
                        <td><code>{row.finding.rule_id}</code></td>
                        <td class="text">{row.finding.title}</td>
                        <td><code>{row.effective_status}</code></td>
                        <td><code>{(row.finding.targets ?? []).join(', ')}</code></td>
                      </tr>
                    {/each}
                  </tbody>
                </table>
              </div>

              {#if selectedFinding}
                <div class="explain-drawer">
                  <div class="drawer-top">
                    <strong>{selectedFinding.finding.title}</strong>
                    <button type="button" class="close-btn" onclick={() => (selectedFinding = null)}
                      >✕</button
                    >
                  </div>
                  <p class="sub">
                    ID: <code>{selectedFinding.finding.id}</code> · Tool:
                    <code>{selectedFinding.finding.tool}</code>
                  </p>
                  {#if selectedFinding.finding.evidence}
                    <p class="ev-line">Evidence: <code>{selectedFinding.finding.evidence}</code></p>
                  {/if}

                  {#if explainCard}
                    <div class="asvs-box">
                      <span class="k">{explainCard.asvs_category}</span>
                      <p>{explainCard.summary}</p>
                      <p class="why">{explainCard.why_it_matters}</p>
                      <ol>
                        {#each explainCard.remediation_steps ?? [] as step}
                          <li>{step}</li>
                        {/each}
                      </ol>
                      <div class="verify-line">
                        <span>Verify:</span>
                        <code>{explainCard.verify_command}</code>
                      </div>
                    </div>
                  {/if}

                  <div class="triage-actions">
                    <input
                      type="text"
                      bind:value={triageNote}
                      placeholder="Optional operator note…"
                    />
                    <div class="btn-row">
                      <button
                        type="button"
                        class="act-btn"
                        onclick={() => void applyTriage(selectedFinding.finding, 'suspicious')}
                        >Mark Suspicious</button
                      >
                      <button
                        type="button"
                        class="act-btn danger-btn"
                        onclick={() => void applyTriage(selectedFinding.finding, 'quarantined')}
                        >Quarantine (Logical)</button
                      >
                      <button
                        type="button"
                        class="act-btn"
                        onclick={() => void applyTriage(selectedFinding.finding, 'cleared')}
                        >Mark Cleared</button
                      >
                    </div>
                  </div>
                </div>
              {/if}
            </div>
          </div>
        {:else if section === 'watchlist'}
          <div class="deck">
            <header class="deck-head">
              <div>
                <h2>Watchlist & Logical Quarantine</h2>
                <p class="sub">
                  Non-destructive operator triage overlay (`tools/bellator/runs/triage.json`).
                  Bellator never deletes workspace files automatically.
                </p>
              </div>
            </header>

            <div class="table-wrap">
              <table class="dash-table">
                <thead>
                  <tr>
                    <th>Finding ID</th>
                    <th>State</th>
                    <th>Severity</th>
                    <th>Tool</th>
                    <th>Title / Note</th>
                    <th>Updated</th>
                    <th>Action</th>
                  </tr>
                </thead>
                <tbody>
                  {#each triagePayload?.entries ?? [] as e}
                    <tr class:inverted={e.state === 'quarantined'}>
                      <td class="tool-name"><code>{e.finding_id}</code></td>
                      <td><code>{e.state}</code></td>
                      <td><code>{e.severity ?? '-'}</code></td>
                      <td><code>{e.tool ?? '-'}</code></td>
                      <td class="text">
                        {e.title ?? e.finding_id}
                        {#if e.note}<br /><span class="sub">Note: {e.note}</span>{/if}
                      </td>
                      <td><code>{e.updated_at}</code></td>
                      <td>
                        <button
                          type="button"
                          class="act-btn"
                          onclick={() =>
                            void applyTriage(
                              {
                                id: e.finding_id,
                                title: e.title,
                                severity: e.severity,
                                tool: e.tool,
                              },
                              'cleared'
                            )}>Clear</button
                        >
                      </td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
          </div>
        {:else if section === 'host'}
          <div class="deck">
            <header class="deck-head">
              <div>
                <h2>Host Persistence, Wine Prefixes, Listening Sockets & Git LFS</h2>
                <p class="sub">
                  Fast builtin endpoint & repository hygiene checks (`persistence-audit`,
                  `network-socket-audit`, `lfs-integrity-audit`) plus ClamAV sweeps.
                </p>
              </div>
              <button
                type="button"
                class="act-btn primary"
                disabled={busy}
                onclick={(e) => {
                  rippleFromEvent(e);
                  void triggerScan('host-sec', 'host');
                }}>Run Host-Sec Suite</button
              >
            </header>

            <div class="table-wrap">
              <table class="dash-table">
                <thead>
                  <tr>
                    <th>Tool</th>
                    <th>Severity</th>
                    <th>Rule</th>
                    <th>Title</th>
                    <th>Target</th>
                  </tr>
                </thead>
                <tbody>
                  {#if hostFindings.length === 0}
                    <tr>
                      <td colspan="5" class="text">
                        No open host persistence, Wine Run key, wildcard socket, or Git LFS
                        integrity findings in the latest run.
                      </td>
                    </tr>
                  {:else}
                    {#each hostFindings as row}
                      <tr onclick={() => void openExplain(row)} style="cursor:pointer">
                        <td class="tool-name"><code>{row.finding.tool}</code></td>
                        <td><code>{row.finding.severity}</code></td>
                        <td><code>{row.finding.rule_id}</code></td>
                        <td class="text">{row.finding.title}</td>
                        <td><code>{(row.finding.targets ?? []).join(', ')}</code></td>
                      </tr>
                    {/each}
                  {/if}
                </tbody>
              </table>
            </div>
          </div>
        {:else if section === 'runs'}
          <div class="deck">
            <header class="deck-head">
              <div>
                <h2>Immutable Run History & Delta Diff</h2>
                <p class="sub">
                  Compare any two runs or inspect regressions against the last green snapshot.
                </p>
              </div>
            </header>

            <div class="compare-bar">
              <label>
                <span>Base Run</span>
                <select bind:value={compareBase}>
                  {#each runsPayload?.runs ?? [] as r}
                    <option value={r.id}>{r.id} ({r.profile} · {r.counts?.open ?? 0} open)</option>
                  {/each}
                </select>
              </label>

              <label>
                <span>Head Run</span>
                <select bind:value={compareHead}>
                  {#each runsPayload?.runs ?? [] as r}
                    <option value={r.id}>{r.id} ({r.profile} · {r.counts?.open ?? 0} open)</option>
                  {/each}
                </select>
              </label>

              <button type="button" class="act-btn" onclick={() => void runCompare()}>
                Compare Runs
              </button>
            </div>

            {#if compareResult?.compare}
              <div class="cards dash-overview">
                <article class="card">
                  <h3>New Findings</h3>
                  <p class="metric bad"><code>+{compareResult.compare.new_count}</code></p>
                </article>
                <article class="card">
                  <h3>Cleared Findings</h3>
                  <p class="metric good"><code>-{compareResult.compare.cleared_count}</code></p>
                </article>
                <article class="card">
                  <h3>Still Open</h3>
                  <p class="metric"><code>{compareResult.compare.still_open_count}</code></p>
                </article>
              </div>
            {/if}

            <div class="table-wrap">
              <table class="dash-table">
                <thead>
                  <tr>
                    <th>Run ID</th>
                    <th>Profile</th>
                    <th>Gate</th>
                    <th class="num">Open</th>
                    <th class="num">Critical</th>
                    <th class="num">High</th>
                    <th class="num">Duration</th>
                    <th>Started</th>
                  </tr>
                </thead>
                <tbody>
                  {#each runsPayload?.runs ?? [] as r}
                    <tr class:inverted={!r.gate_ok}>
                      <td class="tool-name"><code>{r.id}</code></td>
                      <td><code>{r.profile}</code></td>
                      <td>{r.gate_ok ? 'PASS' : 'BLOCK'}</td>
                      <td class="num">{r.counts?.open ?? 0}</td>
                      <td class="num">{r.counts?.critical ?? 0}</td>
                      <td class="num">{r.counts?.high ?? 0}</td>
                      <td class="num">{r.duration_ms} ms</td>
                      <td><code>{r.started_at}</code></td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
          </div>
        {:else if section === 'policy'}
          <div class="deck">
            <header class="deck-head">
              <div>
                <h2>Policy Pack & Expiry-Dated Suppressions</h2>
                <p class="sub">
                  Every suppression requires non-empty <code>owner</code>, <code>expires</code>, and
                  <code>reason</code>.
                </p>
              </div>
            </header>

            <div class="launcher-grid">
              <label>
                <span>Finding ID (or leave blank if using Rule ID)</span>
                <input type="text" bind:value={suppFindingId} placeholder="osv:GHSA-…" />
              </label>
              <label>
                <span>Rule ID</span>
                <input type="text" bind:value={suppRuleId} placeholder="RUSTSEC-…" />
              </label>
              <label>
                <span>Reason</span>
                <input
                  type="text"
                  bind:value={suppReason}
                  placeholder="Transitive dependency; unreachable in build…"
                />
              </label>
              <label>
                <span>Owner & Expires (YYYY-MM-DD)</span>
                <div style="display:flex;gap:0.35rem">
                  <input type="text" bind:value={suppOwner} style="width:45%" />
                  <input type="text" bind:value={suppExpires} style="width:55%" />
                </div>
              </label>
              <div class="launch-act">
                <button
                  type="button"
                  class="act-btn primary"
                  onclick={() => void submitSuppressionDraft()}
                >
                  Append Draft Suppression
                </button>
              </div>
            </div>

            <h3>Active Suppressions (`suppressions.toml`)</h3>
            <div class="table-wrap">
              <table class="dash-table">
                <thead>
                  <tr>
                    <th>Finding / Rule</th>
                    <th>Owner</th>
                    <th>Expires</th>
                    <th>Reason</th>
                  </tr>
                </thead>
                <tbody>
                  {#each policyPayload?.suppressions?.suppress ?? [] as s}
                    <tr>
                      <td class="tool-name"><code>{s.finding_id ?? s.rule_id}</code></td>
                      <td><code>{s.owner}</code></td>
                      <td><code>{s.expires}</code></td>
                      <td class="text">{s.reason}</td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
          </div>
        {/if}
      </div>
    </div>

    <BellatorInspector
      collapsed={inspectorCollapsed}
      gateOk={dash?.gate_ok !== false}
      advisoryStale={Boolean(dash?.advisory_stale)}
      advisoryAgeHours={dash?.advisory_receipts?.sources?.['clamav-db']
        ? (Date.now() / 1000 - dash.advisory_receipts.sources['clamav-db'].refreshed_at_unix) / 3600
        : null}
      timersActive={activeTimersCount > 0}
      {engineActive}
      {busy}
      {liveScan}
      {sfxOn}
      {receipt}
      rootOptInCmd={dash?.root_opt_in_cmd ??
        'sudo bash tools/bellator/scripts/install_bellator_root_sweep_opt_in.sh'}
      onToggleCollapse={() => {
        inspectorCollapsed = !inspectorCollapsed;
        safeStorage.setItem('bellator-inspector-collapsed', inspectorCollapsed ? '1' : '0');
      }}
      onToggleSfx={() => {
        sfxOn = !sfxOn;
        safeStorage.setItem('bellator-sfx-on', sfxOn ? '1' : '0');
        playCue('tick');
      }}
      onStartScan={() => void triggerScan()}
      onPauseResume={(pause) => void handlePauseResume(pause)}
      onCancelScan={() => void handleCancelScan()}
      onSyncDefects={() => void handleSyncDefects()}
      onInstallUserTimers={() => void handleInstallUserTimers()}
    />
  </div>
{/if}

<style>
  .studio {
    --obsidian: #0d0912;
    --glass: rgba(22, 15, 29, 0.82);
    position: relative;
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    align-items: stretch;
    height: 100%;
    min-height: 76vh;
    overflow: hidden;
    color: #f4ecf6;
    background: var(--obsidian);
    border-radius: 0.75rem;
    border: 1px solid rgba(201, 111, 208, 0.25);
  }
  .stage {
    position: relative;
    z-index: 1;
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    height: 100%;
    overflow: hidden;
    padding: 0.75rem 1.25rem;
  }
  .stage-body {
    position: relative;
    z-index: 1;
    display: flex;
    flex-direction: column;
    flex: 1 1 auto;
    min-height: 0;
    min-width: 0;
    overflow-y: auto;
    gap: 0.75rem;
  }
  .wash {
    position: absolute;
    inset: 0;
    z-index: 0;
    overflow: hidden;
    pointer-events: none;
  }
  .blob {
    position: absolute;
    border-radius: 50%;
    filter: blur(32px);
    opacity: 0.25;
  }
  .blob.a {
    width: 38rem;
    height: 20rem;
    left: -12%;
    top: -14%;
    background: radial-gradient(circle, #ff8c42, transparent 65%);
  }
  .blob.b {
    width: 36rem;
    height: 20rem;
    right: -12%;
    bottom: -14%;
    background: radial-gradient(circle, #c96fd0, transparent 65%);
  }
  .scan {
    position: absolute;
    inset: 0;
    background: repeating-linear-gradient(
      transparent 0 3px,
      rgba(201, 111, 208, 0.08) 3px 4px
    );
    pointer-events: none;
  }
  .traffic {
    position: relative;
    z-index: 2;
    display: flex;
    align-items: center;
    gap: 0.55rem;
    min-width: 0;
    margin-bottom: 0.55rem;
    padding: 0.42rem 0.75rem;
    border-radius: 0.7rem;
    border: 1px solid rgba(201, 111, 208, 0.35);
    background: rgba(22, 15, 29, 0.9);
    color: #fff4e8;
    font-size: 0.78rem;
  }
  .traffic.pulse {
    box-shadow: 0 0 16px rgba(255, 140, 66, 0.55);
  }
  .traffic .dot {
    width: 0.5rem;
    height: 0.5rem;
    border-radius: 50%;
    background: #6b5a72;
    flex: 0 0 auto;
  }
  .traffic .dot.live {
    background: #6bcf8e;
    box-shadow: 0 0 8px rgba(107, 207, 142, 0.85);
  }
  .traffic .dot.wait {
    background: #d4a24a;
  }
  .traffic .dot.down {
    background: #e06b7a;
  }
  .traffic .work {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: #7ec8e3;
  }
  .delta-chip {
    font-size: 0.7rem;
    padding: 0.12rem 0.48rem;
    border-radius: 999px;
    background: rgba(255, 140, 66, 0.16);
    border: 1px solid rgba(255, 140, 66, 0.45);
    color: #ff8c42;
    white-space: nowrap;
  }
  .open-studio-btn {
    border: 1px solid #ff8c42;
    background: rgba(255, 140, 66, 0.16);
    color: #fff4e8;
    border-radius: 0.5rem;
    padding: 0.22rem 0.55rem;
    font-size: 0.74rem;
    cursor: pointer;
  }
  .deck {
    background: rgba(22, 15, 29, 0.84);
    backdrop-filter: blur(14px);
    border: 1px solid rgba(201, 111, 208, 0.26);
    border-radius: 0.8rem;
    padding: 0.85rem 1rem;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }
  .deck-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 0.75rem;
    flex-wrap: wrap;
  }
  .deck-head h2 {
    margin: 0;
    font-size: 1.05rem;
    color: #fff4e8;
  }
  .sub {
    margin: 0.15rem 0 0;
    font-size: 0.78rem;
    color: #a894ab;
  }
  .head-actions,
  .deck-actions {
    display: flex;
    gap: 0.4rem;
    flex-wrap: wrap;
  }
  .act-btn {
    border: 1px solid rgba(201, 111, 208, 0.4);
    background: rgba(13, 9, 18, 0.75);
    color: #f0e6f2;
    border-radius: 0.55rem;
    padding: 0.38rem 0.7rem;
    font-size: 0.78rem;
    cursor: pointer;
  }
  .act-btn.primary {
    border-color: #ff8c42;
    background: rgba(255, 140, 66, 0.18);
  }
  .act-btn.armed {
    border-color: #ff8c42;
    background: rgba(255, 140, 66, 0.28);
    color: #fff4e8;
  }
  .act-btn.danger-btn {
    border-color: #e06b7a;
    color: #ffb4be;
  }
  .act-btn:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
  .live-banner {
    padding: 0.65rem 0.8rem;
    border-radius: 0.7rem;
    border: 1px solid rgba(126, 200, 227, 0.45);
    background: rgba(13, 22, 32, 0.68);
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }
  .live-head {
    display: flex;
    justify-content: space-between;
    font-size: 0.8rem;
    color: #7ec8e3;
  }
  .progress-track {
    height: 0.42rem;
    border-radius: 999px;
    background: rgba(255, 255, 255, 0.08);
    overflow: hidden;
  }
  .progress-fill {
    height: 100%;
    background: linear-gradient(90deg, #ff8c42, #6bcf8e);
  }
  .live-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 0.75rem;
    font-size: 0.74rem;
    color: #cbb8d0;
  }
  .chart-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(230px, 1fr));
    gap: 0.65rem;
  }
  .chart-card {
    padding: 0.65rem;
    border-radius: 0.65rem;
    border: 1px solid rgba(201, 111, 208, 0.22);
    background: rgba(13, 9, 18, 0.55);
  }
  .chart-card h3 {
    margin: 0 0 0.4rem;
    font-size: 0.8rem;
    color: #cbb8d0;
  }
  .launcher-grid,
  .compare-bar,
  .filter-bar {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(190px, 1fr));
    gap: 0.55rem;
    align-items: end;
  }
  .launcher-grid label span,
  .compare-bar label span {
    display: block;
    font-size: 0.72rem;
    color: #a894ab;
    margin-bottom: 0.2rem;
  }
  select,
  input[type='text'],
  input[type='search'] {
    width: 100%;
    padding: 0.42rem 0.55rem;
    border-radius: 0.55rem;
    border: 1px solid rgba(201, 111, 208, 0.35);
    background: rgba(13, 9, 18, 0.78);
    color: #f0e6f2;
    font-size: 0.78rem;
  }
  .toggle-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(250px, 1fr));
    gap: 0.55rem;
  }
  .toggle-card {
    display: flex;
    align-items: flex-start;
    gap: 0.55rem;
    padding: 0.55rem 0.65rem;
    border-radius: 0.65rem;
    border: 1px solid rgba(201, 111, 208, 0.28);
    background: rgba(13, 9, 18, 0.6);
    cursor: pointer;
  }
  .toggle-card strong {
    display: block;
    font-size: 0.8rem;
    color: #fff4e8;
  }
  .toggle-card span {
    display: block;
    font-size: 0.72rem;
    color: #a894ab;
  }
  .split-findings {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    gap: 0.75rem;
    align-items: start;
  }
  .explain-drawer {
    width: 22rem;
    padding: 0.75rem;
    border-radius: 0.7rem;
    border: 1px solid rgba(255, 140, 66, 0.45);
    background: rgba(16, 11, 22, 0.95);
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .drawer-top {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 0.4rem;
  }
  .close-btn {
    border: 0;
    background: transparent;
    color: #a894ab;
    cursor: pointer;
    font-size: 0.9rem;
  }
  .asvs-box {
    padding: 0.5rem;
    border-radius: 0.55rem;
    background: rgba(255, 255, 255, 0.03);
    border: 1px solid rgba(201, 111, 208, 0.22);
    font-size: 0.75rem;
  }
  .asvs-box .k {
    display: block;
    color: #ff8c42;
    font-weight: 600;
    margin-bottom: 0.25rem;
  }
  .asvs-box ol {
    margin: 0.35rem 0;
    padding-left: 1.1rem;
  }
  .verify-line {
    margin-top: 0.35rem;
    font-size: 0.7rem;
  }
  .verify-line code {
    display: block;
    margin-top: 0.15rem;
    padding: 0.25rem 0.35rem;
    background: rgba(0, 0, 0, 0.45);
    border-radius: 0.35rem;
    color: #6bcf8e;
    word-break: break-all;
  }
  .triage-actions {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }
  .btn-row {
    display: flex;
    flex-wrap: wrap;
    gap: 0.35rem;
  }
  .err-banner {
    padding: 0.45rem 0.65rem;
    border-radius: 0.5rem;
    background: rgba(224, 107, 122, 0.18);
    border: 1px solid #e06b7a;
    color: #ffb4be;
    font-size: 0.78rem;
  }
  .compact-wrap {
    display: flex;
    flex-direction: column;
    gap: 0.65rem;
  }
</style>
