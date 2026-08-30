<script lang="ts">
  import { onMount, tick } from 'svelte';
  import {
    fetchBacklinks,
    fetchDashboard,
    fetchGarden,
    fetchHealth,
    fetchPage,
    fetchRecent,
    fetchTree,
    htmlIdForAnchor,
    normalizeTree,
    recordClick,
    savePage,
    search,
    snippetOf,
    type DashboardActivity,
    type DashboardPayload,
    type GardenPayload,
    type OutlineSection,
    type RecentFile,
    type SearchHit,
    type DashboardTool,
    type SearchTelemetry,
    type TreeNode,
  } from './lib/api';
  import { rippleFromEvent } from './lib/press-ripple';
  import {
    CHART_HELP,
    COLUMN_HELP,
    METRIC_HELP,
    OUTCOME_HELP,
    TERMINAL_DASHBOARD_CMD,
    WIKI_SERVE_CMD,
    toolHelp,
  } from './lib/toolHelp';
  import { KIND_OPTIONS, kindOption } from './lib/filterMeta';
  import { normalizeTag, setMarkdownTags } from './lib/frontmatter';
  import { comboNav } from './lib/combo';
  import {
    loadTagColors,
    normalizeHex,
    resolveTagColor,
    saveTagColors,
    type TagColorMap,
  } from './lib/tagColors';
  import CollapsibleSection from './lib/CollapsibleSection.svelte';
  import { safeSessionStorage, safeStorage } from './lib/safeStorage';
  import BarChart from './lib/charts/BarChart.svelte';
  import DonutChart from './lib/charts/DonutChart.svelte';
  import Sparkline from './lib/charts/Sparkline.svelte';
  import type { BarDatum, DonutDatum, SparkPoint } from './lib/charts/utils';
  import RuntimeHealth from './lib/RuntimeHealth.svelte';

  type Tab = 'search' | 'reader' | 'dashboard' | 'health';
  type EditorTab = { path: string; pinned: boolean };
  type SortKey =
    | 'name'
    | 'calls'
    | 'avg_ms'
    | 'trunc_count'
    | 'error_count'
    | 'invalid_count'
    | 'baseline_tokens'
    | 'returned_tokens'
    | 'saved'
    | 'reduction_pct'
    | 'peak_saved'
    | 'last_ts';

  const TABS: Tab[] = ['search', 'reader', 'dashboard', 'health'];
  const SIDEBAR_MIN = 240;
  const SIDEBAR_MAX = 720;
  const SIDEBAR_DEFAULT = 320;
  const SIDEBAR_KEY = 'wordkeep-wiki-sidebar-width';
  const SIDEBAR_COLLAPSED_KEY = 'wordkeep-wiki-sidebar-collapsed';
  const TREE_COLLAPSED_KEY = 'wordkeep-wiki-tree-collapsed';
  const EDITOR_TABS_KEY = 'wordkeep-wiki-editor-tabs';
  const DASH_BASELINE_KEY = 'wordkeep-wiki-dash-baseline';
  const DASH_POLL_MS = 2000;

  let tab = $state<Tab>('search');
  let healthView = $state<'runtime' | 'knowledge'>('runtime');
  let urlReady = $state(false);
  let query = $state('');
  let kind = $state('');
  let rootFilter = $state('');
  let tagFilter = $state('');
  let hits = $state<SearchHit[]>([]);
  let status = $state('Ready.');
  let pageHtml = $state('<p class="muted">Select a search hit or tree entry.</p>');
  let pageMarkdown = $state('');
  let pagePath = $state('');
  let pageTags = $state<string[]>([]);
  let outline = $state<OutlineSection[]>([]);
  let backlinks = $state<{ path: string; heading?: string }[]>([]);
  let tree = $state<TreeNode[]>([]);
  let recent = $state<RecentFile[]>([]);
  let collapsedDirs = $state<Set<string>>(new Set());
  let editorTabs = $state<EditorTab[]>([]);
  let health = $state<string>('unknown');
  let healthDetail = $state('');
  let retainSearchQueries = $state(false);
  let readOnly = $state(false);
  let projectName = $state('Project');
  let brandTitle = $derived(`${projectName} ~ Wiki`);
  let garden = $state<GardenPayload | null>(null);
  let searchTelemetry = $state<SearchTelemetry | null>(null);
  let dashboard = $state<DashboardPayload | null>(null);
  let dashboardError = $state('');
  let sessionSavedFmt = $state('0');
  let sortKey = $state<SortKey>('saved');
  let sortDir = $state<'asc' | 'desc'>('desc');
  let copyFlash = $state('');
  let tagDraft = $state('');
  let tagBusy = $state(false);
  let tagError = $state('');
  let renamingTag = $state<string | null>(null);
  let renameDraft = $state('');
  let tagColors = $state<TagColorMap>({});
  let colorHexDrafts = $state<Record<string, string>>({});
  let rootMenuOpen = $state(false);
  let rootHighlight = $state(0);
  let kindQuery = $state('All kinds');
  let kindMenuOpen = $state(false);
  let kindHighlight = $state(0);
  let tagQuery = $state('All tags');
  let tagMenuOpen = $state(false);
  let tagHighlight = $state(0);
  let recentQuery = $state('');
  let recentMenuOpen = $state(false);
  let recentHighlight = $state(0);
  let searching = $state(false);
  let lastLatency = $state<number | null>(null);
  let lastCount = $state(0);
  let debounceHandle: ReturnType<typeof setTimeout> | null = null;
  let autosaveHandle: ReturnType<typeof setTimeout> | null = null;
  let dashPollHandle: ReturnType<typeof setInterval> | null = null;
  let editing = $state(false);
  let draftMarkdown = $state('');
  let saving = $state(false);
  let saveError = $state('');
  let sidebarWidth = $state(SIDEBAR_DEFAULT);
  let sidebarCollapsed = $state(false);
  let headerEl: HTMLElement | null = $state(null);
  let mainChromeEl: HTMLElement | null = $state(null);
  let mainEl: HTMLElement | null = $state(null);
  let resizing = $state(false);
  let resizeMove: ((event: PointerEvent) => void) | null = null;
  let resizeUp: (() => void) | null = null;

  const AUTOSAVE_MS = 450;

  onMount(() => {
    const stored = Number(safeStorage.getItem(SIDEBAR_KEY));
    if (Number.isFinite(stored)) {
      sidebarWidth = clampSidebar(stored);
    }
    sidebarCollapsed = safeStorage.getItem(SIDEBAR_COLLAPSED_KEY) === '1';
    try {
      const raw = safeStorage.getItem(TREE_COLLAPSED_KEY);
      if (raw) {
        const parsed = JSON.parse(raw);
        if (Array.isArray(parsed)) collapsedDirs = new Set(parsed.filter((k) => typeof k === 'string'));
      }
    } catch {
      /* ignore bad cache */
    }
    try {
      const raw = safeStorage.getItem(EDITOR_TABS_KEY);
      if (raw) {
        const parsed = JSON.parse(raw);
        if (Array.isArray(parsed)) {
          editorTabs = parsed
            .filter((t) => t && typeof t.path === 'string' && t.path.length > 0)
            .map((t) => ({ path: t.path as string, pinned: !!t.pinned }));
        }
      }
    } catch {
      /* ignore bad cache */
    }
    tagColors = loadTagColors();
    void bootstrap();
    const onKey = (event: KeyboardEvent) => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'k') {
        event.preventDefault();
        document.querySelector<HTMLInputElement>('input[type="search"]')?.focus();
      }
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 's' && editing) {
        event.preventDefault();
        void flushAutosave();
      }
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'e' && pagePath) {
        event.preventDefault();
        if (editing) void doneEditing();
        else startEditing();
      }
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'w' && pagePath) {
        event.preventDefault();
        void closeEditorTab(pagePath);
      }
      if (event.key === 'Escape' && editing) {
        void doneEditing();
      }
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'b') {
        event.preventDefault();
        toggleSidebarCollapsed();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('keydown', onKey);
      if (debounceHandle) clearTimeout(debounceHandle);
      if (autosaveHandle) clearTimeout(autosaveHandle);
      if (dashPollHandle) clearInterval(dashPollHandle);
      stopResize();
    };
  });

  $effect(() => {
    if (!headerEl) return;
    syncHeaderHeight();
    const ro = new ResizeObserver(() => syncHeaderHeight());
    ro.observe(headerEl);
    return () => ro.disconnect();
  });

  $effect(() => {
    if (tab !== 'reader') return;
    editorTabs.length;
    mainChromeEl;
    headerEl;
    void tick().then(syncPageActionsTop);
    const ro = new ResizeObserver(() => syncPageActionsTop());
    if (mainChromeEl) ro.observe(mainChromeEl);
    if (headerEl) ro.observe(headerEl);
    const main = mainEl;
    main?.addEventListener('scroll', syncPageActionsTop, { passive: true });
    window.addEventListener('resize', syncPageActionsTop);
    return () => {
      ro.disconnect();
      main?.removeEventListener('scroll', syncPageActionsTop);
      window.removeEventListener('resize', syncPageActionsTop);
    };
  });

  $effect(() => {
    if (tab !== 'dashboard') {
      if (dashPollHandle) {
        clearInterval(dashPollHandle);
        dashPollHandle = null;
      }
      return;
    }
    void refreshDashboard();
    if (dashPollHandle) clearInterval(dashPollHandle);
    dashPollHandle = setInterval(() => {
      void refreshDashboard();
    }, DASH_POLL_MS);
    return () => {
      if (dashPollHandle) {
        clearInterval(dashPollHandle);
        dashPollHandle = null;
      }
    };
  });

  $effect(() => {
    if (!urlReady) return;
    const url = new URL(location.href);
    if (tab === 'search') url.searchParams.delete('tab');
    else url.searchParams.set('tab', tab);
    if (tab === 'health' && healthView !== 'knowledge') url.searchParams.set('view', healthView);
    else if (tab === 'health') url.searchParams.set('view', 'knowledge');
    else url.searchParams.delete('view');
    const next = `${url.pathname}${url.search}${url.hash}`;
    const cur = `${location.pathname}${location.search}${location.hash}`;
    if (next !== cur) history.replaceState({}, '', url);
  });

  function parseTab(raw: string | null): Tab | null {
    if (raw && (TABS as string[]).includes(raw)) return raw as Tab;
    return null;
  }

  function setTab(next: Tab) {
    tab = next;
    if (next === 'health') void refreshHealth();
  }

  function clampSidebar(width: number): number {
    return Math.min(SIDEBAR_MAX, Math.max(SIDEBAR_MIN, Math.round(width)));
  }

  function syncPageActionsTop() {
    if (tab !== 'reader') return;
    const bodyPad = 20;
    let top: number;
    if (mainChromeEl) {
      top = mainChromeEl.getBoundingClientRect().bottom + bodyPad;
    } else if (headerEl) {
      top = headerEl.getBoundingClientRect().bottom + bodyPad;
    } else {
      top = bodyPad;
    }
    document.documentElement.style.setProperty('--page-actions-top', `${top}px`);
  }

  function syncHeaderHeight() {
    if (!headerEl) return;
    document.documentElement.style.setProperty('--header-h', `${headerEl.offsetHeight}px`);
  }

  function toggleSidebarCollapsed() {
    sidebarCollapsed = !sidebarCollapsed;
    safeStorage.setItem(SIDEBAR_COLLAPSED_KEY, sidebarCollapsed ? '1' : '0');
  }

  /* Mobile: hold sidebar chrome / empty chrome to toggle; double-tap chrome label. */
  const SIDEBAR_HOLD_MS = 480;
  const SIDEBAR_DOUBLE_MS = 340;
  let sidebarHoldTimer = 0;
  let sidebarLastTapAt = 0;
  let contentZoom = $state(1);
  let pinchStartDist = 0;
  let pinchStartZoom = 1;
  const ZOOM_MIN = 0.7;
  const ZOOM_MAX = 2.4;

  function clearSidebarHold() {
    if (sidebarHoldTimer) {
      clearTimeout(sidebarHoldTimer);
      sidebarHoldTimer = 0;
    }
  }

  function isSidebarGestureTarget(target: EventTarget | null): boolean {
    if (!(target instanceof Element)) return false;
    if (
      target.closest(
        'input, textarea, select, a, button, .tree-item, .tree-dir, .search, .hit-list, .filter-row'
      )
    ) {
      return false;
    }
    return !!target.closest('aside, .sidebar-chrome');
  }

  function onSidebarPointerDown(event: PointerEvent) {
    if (event.pointerType === 'mouse' && event.button !== 0) return;
    if (!isSidebarGestureTarget(event.target)) return;
    clearSidebarHold();
    sidebarHoldTimer = window.setTimeout(() => {
      sidebarHoldTimer = 0;
      toggleSidebarCollapsed();
    }, SIDEBAR_HOLD_MS);
  }

  function onSidebarPointerUp(event: PointerEvent) {
    const wasHolding = sidebarHoldTimer !== 0;
    clearSidebarHold();
    if (!wasHolding) return;
    if (!isSidebarGestureTarget(event.target)) return;
    const now = performance.now();
    if (now - sidebarLastTapAt < SIDEBAR_DOUBLE_MS) {
      sidebarLastTapAt = 0;
      toggleSidebarCollapsed();
      return;
    }
    sidebarLastTapAt = now;
  }

  function onSidebarPointerCancel() {
    clearSidebarHold();
  }

  function touchDist(a: Touch, b: Touch): number {
    const dx = a.clientX - b.clientX;
    const dy = a.clientY - b.clientY;
    return Math.hypot(dx, dy);
  }

  function onShellTouchStart(event: TouchEvent) {
    if (event.touches.length === 2) {
      pinchStartDist = touchDist(event.touches[0], event.touches[1]);
      pinchStartZoom = contentZoom;
    }
  }

  function onShellTouchMove(event: TouchEvent) {
    if (event.touches.length !== 2 || pinchStartDist <= 0) return;
    event.preventDefault();
    const dist = touchDist(event.touches[0], event.touches[1]);
    const next = pinchStartZoom * (dist / pinchStartDist);
    contentZoom = Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, next));
  }

  function onShellTouchEnd(event: TouchEvent) {
    if (event.touches.length < 2) {
      pinchStartDist = 0;
    }
  }

  function startResize(event: PointerEvent) {
    event.preventDefault();
    resizing = true;
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';
    resizeMove = (move: PointerEvent) => {
      sidebarWidth = clampSidebar(move.clientX);
    };
    resizeUp = () => {
      stopResize();
      safeStorage.setItem(SIDEBAR_KEY, String(sidebarWidth));
    };
    window.addEventListener('pointermove', resizeMove);
    window.addEventListener('pointerup', resizeUp, { once: true });
  }

  function stopResize() {
    resizing = false;
    document.body.style.cursor = '';
    document.body.style.userSelect = '';
    if (resizeMove) window.removeEventListener('pointermove', resizeMove);
    if (resizeUp) window.removeEventListener('pointerup', resizeUp);
    resizeMove = null;
    resizeUp = null;
  }

  async function bootstrap() {
    try {
      tree = normalizeTree(await fetchTree());
    } catch (e) {
      status = `Tree unavailable: ${e}`;
    }
    try {
      recent = (await fetchRecent(16)).files || [];
    } catch {
      recent = [];
    }
    await refreshHealth();
    const params = new URLSearchParams(location.search);
    const path = params.get('path');
    const restoredTab = parseTab(params.get('tab'));
    const view = params.get('view');
    if (view === 'runtime' || view === 'knowledge') healthView = view;
    if (path) {
      await openPath(path, undefined, params.get('anchor') || undefined);
      if (restoredTab && restoredTab !== 'reader') {
        tab = restoredTab;
      }
    } else if (restoredTab) {
      tab = restoredTab;
    }
    urlReady = true;
  }

  async function refreshHealth() {
    try {
      const h = await fetchHealth();
      health = h.status;
      retainSearchQueries = !!h.retain_search_queries;
      if (h.project_name?.trim()) {
        projectName = h.project_name.trim();
        document.title = `${projectName} ~ Wiki`;
      }
      const files = h.manifest?.files ?? '?';
      const chunks = h.manifest?.chunks ?? '?';
      healthDetail = `manifest ${files} files / ${chunks} chunks`;
      searchTelemetry = h.search_telemetry || null;
      readOnly = Boolean(h.read_only);
    } catch {
      health = 'down';
      healthDetail = '';
    }
    try {
      garden = await fetchGarden();
    } catch (e) {
      garden = { broken_count: 0, orphan_count: 0, duplicate_heading_count: 0 };
      status = `Garden scan failed: ${e}`;
    }
  }

  function commafyClient(n: number): string {
    if (n >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(2).replace(/\.?0+$/, '')}B`;
    if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2).replace(/\.?0+$/, '')}M`;
    return Math.max(0, Math.round(n)).toLocaleString('en-US');
  }

  async function refreshDashboard() {
    try {
      const payload = await fetchDashboard();
      dashboard = payload;
      dashboardError = '';
      const saved = payload.overview?.saved_tokens ?? 0;
      let baseline = Number(safeSessionStorage.getItem(DASH_BASELINE_KEY));
      if (!Number.isFinite(baseline)) {
        baseline = saved;
        safeSessionStorage.setItem(DASH_BASELINE_KEY, String(baseline));
      }
      sessionSavedFmt = commafyClient(Math.max(0, saved - baseline));
    } catch (e) {
      dashboardError = String(e);
    }
  }

  function scheduleSearch() {
    if (debounceHandle) clearTimeout(debounceHandle);
    debounceHandle = setTimeout(() => {
      void runSearch();
    }, 220);
  }

  async function runSearch(event?: Event) {
    event?.preventDefault();
    const q = query.trim();
    if (!q) {
      hits = [];
      lastCount = 0;
      status = 'Ready.';
      return;
    }
    searching = true;
    status = 'Searching…';
    try {
      const res = await search(q, {
        kind: kind || undefined,
        root: rootFilter || undefined,
        tag: tagFilter || undefined,
        limit: 30,
      });
      hits = res.hits || [];
      lastLatency = res.clientLatencyMs ?? res.processingTimeMs ?? null;
      lastCount = res.estimatedTotalHits ?? hits.length;
      status = `${hits.length} hit(s)` + (lastLatency != null ? ` · ${lastLatency} ms` : '');
      tab = 'search';
    } catch (e) {
      hits = [];
      status = `Search failed: ${e}`;
    } finally {
      searching = false;
    }
  }

  async function openHit(hit: SearchHit, rank: number) {
    void recordClick(rank).catch(() => {});
    await openPath(hit.path, hit.heading, hit.anchor);
  }

  async function openPath(path: string, heading?: string, anchor?: string) {
    if (editing) {
      await flushAutosave();
      if (saveError) {
        const leave = confirm('Save failed. Leave anyway and discard unsaved edits?');
        if (!leave) return;
      }
      editing = false;
    }
    tab = 'reader';
    pagePath = path;
    ensureEditorTab(path);
    expandAncestors(path);
    status = `Loading ${path}…`;
    const url = new URL(location.href);
    url.searchParams.set('path', path);
    url.searchParams.set('tab', 'reader');
    if (anchor) url.searchParams.set('anchor', anchor);
    else url.searchParams.delete('anchor');
    history.replaceState({}, '', url);

    try {
      const page = await fetchPage(path);
      applyPage(page);
      status = path;
      const bl = await fetchBacklinks(path);
      backlinks = bl.backlinks || bl.links || [];
      const scrollId = htmlIdForAnchor(anchor) || (heading ? slugify(heading) : '');
      if (scrollId) {
        queueMicrotask(() => {
          document.getElementById(scrollId)?.scrollIntoView({ behavior: 'smooth', block: 'start' });
        });
      }
    } catch (e) {
      pageHtml = `<p class="muted">Failed to load page: ${e}</p>`;
      pageMarkdown = '';
      draftMarkdown = '';
      outline = [];
      pageTags = [];
      status = String(e);
    }
  }

  function applyPage(page: {
    path: string;
    html: string;
    markdown?: string;
    outline?: OutlineSection[];
    tags?: string[];
  }) {
    pagePath = page.path;
    pageHtml = page.html;
    pageMarkdown = page.markdown ?? '';
    draftMarkdown = pageMarkdown;
    outline = page.outline || [];
    pageTags = page.tags || [];
    ensureEditorTab(page.path);
  }

  function persistEditorTabs() {
    safeStorage.setItem(EDITOR_TABS_KEY, JSON.stringify(editorTabs));
  }

  function tabLabel(path: string): string {
    const parts = path.split('/').filter(Boolean);
    return parts[parts.length - 1] || path;
  }

  function ensureEditorTab(path: string) {
    if (!path || editorTabs.some((t) => t.path === path)) return;
    editorTabs = [...editorTabs, { path, pinned: false }];
    persistEditorTabs();
  }

  function togglePinTab(path: string, event?: Event) {
    event?.stopPropagation();
    event?.preventDefault();
    const next = editorTabs.map((t) =>
      t.path === path ? { path: t.path, pinned: !t.pinned } : t,
    );
    editorTabs = [...next.filter((t) => t.pinned), ...next.filter((t) => !t.pinned)];
    persistEditorTabs();
  }

  async function closeEditorTab(path: string, event?: Event) {
    event?.stopPropagation();
    event?.preventDefault();
    const idx = editorTabs.findIndex((t) => t.path === path);
    if (idx < 0) return;
    if (editing && pagePath === path) {
      await flushAutosave();
      if (saveError) {
        const leave = confirm('Save failed. Close tab anyway and discard unsaved edits?');
        if (!leave) return;
      }
      editing = false;
    }
    const wasActive = pagePath === path;
    const remaining = editorTabs.filter((t) => t.path !== path);
    editorTabs = remaining;
    persistEditorTabs();
    if (!wasActive) return;
    const neighbor = remaining[Math.min(idx, remaining.length - 1)];
    if (neighbor) {
      await openPath(neighbor.path);
      return;
    }
    pagePath = '';
    pageHtml = '<p class="muted">No open tabs — pick a file from Recent, Tree, or Search.</p>';
    pageMarkdown = '';
    draftMarkdown = '';
    outline = [];
    pageTags = [];
    backlinks = [];
    status = 'Ready.';
    const url = new URL(location.href);
    url.searchParams.delete('path');
    url.searchParams.delete('anchor');
    history.replaceState({}, '', url);
  }

  async function activateEditorTab(path: string) {
    if (pagePath === path && tab === 'reader') return;
    await openPath(path);
  }

  function onEditorTabAuxclick(path: string, event: MouseEvent) {
    if (event.button === 1) {
      event.preventDefault();
      void closeEditorTab(path, event);
    }
  }

  function startEditing() {
    if (!pagePath) return;
    draftMarkdown = pageMarkdown;
    saveError = '';
    editing = true;
    tab = 'reader';
    status = `Editing ${pagePath}`;
  }

  function onDraftInput() {
    saveError = '';
    scheduleAutosave();
  }

  function scheduleAutosave() {
    if (!editing) return;
    if (autosaveHandle) clearTimeout(autosaveHandle);
    autosaveHandle = setTimeout(() => {
      void flushAutosave();
    }, AUTOSAVE_MS);
  }

  async function flushAutosave() {
    if (autosaveHandle) {
      clearTimeout(autosaveHandle);
      autosaveHandle = null;
    }
    if (!pagePath || !editing) return;
    const snapshot = draftMarkdown;
    if (snapshot === pageMarkdown) return;
    saving = true;
    saveError = '';
    status = 'Saving…';
    try {
      const page = await savePage(pagePath, snapshot);
      pagePath = page.path;
      pageHtml = page.html;
      pageMarkdown = page.markdown ?? snapshot;
      outline = page.outline || [];
      pageTags = page.tags || [];
      if (draftMarkdown === snapshot) {
        draftMarkdown = pageMarkdown;
        status = `Saved ${pagePath}`;
      } else {
        status = 'Saving…';
        scheduleAutosave();
      }
      try {
        recent = (await fetchRecent(16)).files || recent;
      } catch {
        /* keep existing recent list */
      }
    } catch (e) {
      saveError = String(e);
      status = `Save failed: ${e}`;
    } finally {
      saving = false;
    }
  }

  async function doneEditing() {
    await flushAutosave();
    if (saveError) return;
    editing = false;
    status = pagePath || 'Ready.';
  }

  let dirty = $derived(editing && draftMarkdown !== pageMarkdown);
  let saveHint = $derived(
    !editing ? '' : saving ? 'Saving…' : saveError ? 'Save failed' : dirty ? 'Unsaved' : 'Saved',
  );

  function slugify(heading: string): string {
    return heading
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, '-')
      .replace(/^-|-$/g, '');
  }

  function copyPath() {
    if (!pagePath) return;
    void copyText(pagePath, `Copied ${pagePath}`);
  }

  async function persistPageTags(nextTags: string[], okMessage?: string) {
    if (!pagePath || tagBusy) return;
    tagBusy = true;
    tagError = '';
    const markdown = setMarkdownTags(editing ? draftMarkdown : pageMarkdown, nextTags);
    try {
      const page = await savePage(pagePath, markdown);
      applyPage(page);
      if (editing) {
        draftMarkdown = page.markdown ?? markdown;
      }
      status = okMessage || `Tags updated on ${pagePath}`;
      try {
        garden = await fetchGarden();
      } catch {
        /* keep current garden */
      }
    } catch (e) {
      tagError = String(e);
      status = `Tag update failed: ${e}`;
    } finally {
      tagBusy = false;
    }
  }

  async function addPageTag() {
    const tag = normalizeTag(tagDraft);
    if (!tag || !pagePath) return;
    if (pageTags.some((existing) => existing.toLowerCase() === tag.toLowerCase())) {
      tagDraft = '';
      return;
    }
    tagDraft = '';
    await persistPageTags([...pageTags, tag], `Added tag “${tag}”`);
  }

  async function removePageTag(tag: string) {
    await persistPageTags(
      pageTags.filter((existing) => existing !== tag),
      `Removed tag “${tag}”`,
    );
  }

  function beginRenameTag(tag: string) {
    renamingTag = tag;
    renameDraft = tag;
  }

  async function commitRenameTag() {
    if (!renamingTag) return;
    const from = renamingTag;
    const to = normalizeTag(renameDraft);
    renamingTag = null;
    renameDraft = '';
    if (!to || to === from) return;
    const next = pageTags.map((tag) => (tag === from ? to : tag));
    const deduped = next.filter(
      (tag, index) => next.findIndex((other) => other.toLowerCase() === tag.toLowerCase()) === index,
    );
    if (tagColors[from]) {
      const nextColors = { ...tagColors };
      nextColors[to] = nextColors[from]!;
      delete nextColors[from];
      tagColors = nextColors;
      saveTagColors(tagColors);
      const drafts = { ...colorHexDrafts };
      drafts[to] = drafts[from] || nextColors[to]!;
      delete drafts[from];
      colorHexDrafts = drafts;
    }
    await persistPageTags(deduped, `Renamed tag “${from}” → “${to}”`);
  }

  function cancelRenameTag() {
    renamingTag = null;
    renameDraft = '';
  }

  function tagColor(tag: string): string {
    return resolveTagColor(tagColors, tag);
  }

  function setTagColor(tag: string, raw: string) {
    const hex = normalizeHex(raw);
    if (!hex) return;
    tagColors = { ...tagColors, [tag]: hex };
    colorHexDrafts = { ...colorHexDrafts, [tag]: hex };
    saveTagColors(tagColors);
  }

  function onTagHexInput(tag: string, value: string) {
    colorHexDrafts = { ...colorHexDrafts, [tag]: value };
    const hex = normalizeHex(value);
    if (hex) {
      tagColors = { ...tagColors, [tag]: hex };
      saveTagColors(tagColors);
    }
  }

  function commitTagHex(tag: string) {
    const draft = colorHexDrafts[tag] ?? tagColor(tag);
    const hex = normalizeHex(draft) || tagColor(tag);
    setTagColor(tag, hex);
  }

  function hexDraft(tag: string): string {
    return colorHexDrafts[tag] ?? tagColor(tag);
  }

  function selectRoot(root: string) {
    rootFilter = root;
    rootMenuOpen = false;
    rootHighlight = 0;
    scheduleSearch();
  }

  function onRootKeydown(event: KeyboardEvent) {
    const items = [{ value: '' }, ...rootSuggestions.map((value) => ({ value }))];
    const nav = comboNav(event, {
      open: rootMenuOpen,
      highlight: rootHighlight,
      count: items.length,
    });
    if (!nav) return;
    rootMenuOpen = nav.open;
    rootHighlight = nav.highlight;
    if (nav.choose != null) selectRoot(items[nav.choose]?.value ?? '');
  }

  function selectKind(value: string) {
    kind = value;
    kindQuery = kindOption(value).label;
    kindMenuOpen = false;
    kindHighlight = 0;
    scheduleSearch();
  }

  function onKindKeydown(event: KeyboardEvent) {
    const nav = comboNav(event, {
      open: kindMenuOpen,
      highlight: kindHighlight,
      count: kindSuggestions.length,
    });
    if (!nav) return;
    kindMenuOpen = nav.open;
    kindHighlight = nav.highlight;
    if (nav.choose != null) {
      const option = kindSuggestions[nav.choose];
      if (option) selectKind(option.value);
    }
    if (nav.close) kindQuery = kindOption(kind).label;
  }

  function selectTag(tag: string) {
    tagFilter = tag;
    tagQuery = tag || 'All tags';
    tagMenuOpen = false;
    tagHighlight = 0;
    scheduleSearch();
  }

  function onTagKeydown(event: KeyboardEvent) {
    const nav = comboNav(event, {
      open: tagMenuOpen,
      highlight: tagHighlight,
      count: tagSuggestions.length,
    });
    if (!nav) return;
    tagMenuOpen = nav.open;
    tagHighlight = nav.highlight;
    if (nav.choose != null) {
      const option = tagSuggestions[nav.choose];
      if (option) selectTag(option.value);
    }
    if (nav.close) tagQuery = tagFilter || 'All tags';
  }

  function selectRecent(path: string) {
    recentMenuOpen = false;
    recentHighlight = 0;
    recentQuery = '';
    void openPath(path);
  }

  function onRecentKeydown(event: KeyboardEvent) {
    const nav = comboNav(event, {
      open: recentMenuOpen,
      highlight: recentHighlight,
      count: recentSuggestions.length,
    });
    if (!nav) return;
    recentMenuOpen = nav.open;
    recentHighlight = nav.highlight;
    if (nav.choose != null) {
      const file = recentSuggestions[nav.choose];
      if (file) selectRecent(file.path);
    }
  }

  async function copyText(text: string, okMessage?: string) {
    try {
      await navigator.clipboard.writeText(text);
      copyFlash = okMessage || `Copied: ${text}`;
      status = copyFlash;
      setTimeout(() => {
        if (copyFlash === (okMessage || `Copied: ${text}`)) copyFlash = '';
      }, 1800);
    } catch (e) {
      status = `Copy failed: ${e}`;
    }
  }

  function toggleSort(key: SortKey) {
    if (sortKey === key) {
      sortDir = sortDir === 'asc' ? 'desc' : 'asc';
      return;
    }
    sortKey = key;
    sortDir = key === 'name' ? 'asc' : 'desc';
  }

  function sortMark(key: SortKey): string {
    if (sortKey !== key) return '';
    return sortDir === 'asc' ? ' ↑' : ' ↓';
  }

  function activityTitle(event: DashboardActivity): string {
    const help = toolHelp(event.tool);
    if (event.outcome === 'ok') return help;
    const reason = (event.reason || '').trim();
    if (reason) return `${event.outcome}: ${reason}`;
    return `${help}\nOutcome: ${event.outcome}`;
  }

  function toolValue(tool: DashboardTool, key: SortKey): string | number {
    switch (key) {
      case 'name':
        return tool.name;
      case 'calls':
        return tool.calls;
      case 'avg_ms':
        return tool.avg_ms;
      case 'trunc_count':
        return tool.trunc_count;
      case 'error_count':
        return tool.error_count;
      case 'invalid_count':
        return tool.invalid_count ?? 0;
      case 'baseline_tokens':
        return tool.baseline_tokens;
      case 'returned_tokens':
        return tool.returned_tokens;
      case 'saved':
        return tool.saved;
      case 'reduction_pct':
        return tool.reduction_pct;
      case 'peak_saved':
        return tool.peak_saved ?? 0;
      case 'last_ts':
        return tool.last_ts ?? 0;
    }
  }

  function persistCollapsed() {
    safeStorage.setItem(TREE_COLLAPSED_KEY, JSON.stringify([...collapsedDirs]));
  }

  function toggleDir(key: string) {
    const next = new Set(collapsedDirs);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    collapsedDirs = next;
    persistCollapsed();
  }

  function collectDirKeys(nodes: TreeNode[], parentKey = ''): string[] {
    const keys: string[] = [];
    for (const n of nodes) {
      const isDir = !n.path && !!n.children?.length;
      const key = n.path ?? (parentKey ? `${parentKey}/${n.name}` : n.name);
      if (isDir) {
        keys.push(key);
        keys.push(...collectDirKeys(n.children || [], key));
      } else if (n.children?.length) {
        keys.push(...collectDirKeys(n.children, key));
      }
    }
    return keys;
  }

  function expandAllDirs() {
    collapsedDirs = new Set();
    persistCollapsed();
  }

  function collapseAllDirs() {
    collapsedDirs = new Set(collectDirKeys(tree));
    persistCollapsed();
  }

  /** Expand every ancestor folder of a file path so the leaf is visible. */
  function expandAncestors(path: string) {
    const parts = path.split('/').filter(Boolean);
    if (parts.length < 2) return;
    let changed = false;
    const next = new Set(collapsedDirs);
    let key = '';
    for (let i = 0; i < parts.length - 1; i++) {
      key = key ? `${key}/${parts[i]}` : parts[i]!;
      if (next.delete(key)) changed = true;
    }
    if (!changed) return;
    collapsedDirs = next;
    persistCollapsed();
  }

  function flattenTree(
    nodes: TreeNode[],
    depth = 0,
    parentKey = '',
  ): { node: TreeNode; depth: number; dirKey?: string; expanded?: boolean }[] {
    const out: { node: TreeNode; depth: number; dirKey?: string; expanded?: boolean }[] = [];
    for (const n of nodes) {
      const isDir = !n.path && !!n.children?.length;
      const key = n.path ?? (parentKey ? `${parentKey}/${n.name}` : n.name);
      if (isDir) {
        const expanded = !collapsedDirs.has(key);
        out.push({ node: n, depth, dirKey: key, expanded });
        if (expanded) out.push(...flattenTree(n.children || [], depth + 1, key));
      } else {
        out.push({ node: n, depth });
        if (n.children?.length) out.push(...flattenTree(n.children, depth + 1, key));
      }
    }
    return out;
  }

  let flatTree = $derived(flattenTree(tree));
  let pinnedTabs = $derived(editorTabs.filter((t) => t.pinned));
  let unpinnedTabs = $derived(editorTabs.filter((t) => !t.pinned));
  let tagOptions = $derived(
    Object.keys(garden?.tags || {})
      .sort((a, b) => a.localeCompare(b))
      .slice(0, 80),
  );
  let pathRoots = $derived(collectDirKeys(tree).sort((a, b) => a.localeCompare(b)));
  let rootSuggestions = $derived.by(() => {
    const q = rootFilter.trim().toLowerCase();
    const roots = pathRoots.filter((root) => !q || root.toLowerCase().includes(q));
    return roots.slice(0, 40);
  });
  let kindSuggestions = $derived.by(() => {
    const q = kindQuery.trim().toLowerCase();
    if (!q || q === kindOption(kind).label.toLowerCase()) return KIND_OPTIONS;
    return KIND_OPTIONS.filter(
      (option) =>
        option.label.toLowerCase().includes(q) ||
        option.value.toLowerCase().includes(q) ||
        option.tip.toLowerCase().includes(q),
    );
  });
  let tagSuggestions = $derived.by(() => {
    const all = [{ value: '', label: 'All tags', tip: 'Do not filter by tag' }].concat(
      tagOptions.map((tag) => ({
        value: tag,
        label: tag,
        tip: `Only pages tagged “${tag}”`,
      })),
    );
    const q = tagQuery.trim().toLowerCase();
    if (!q || q === 'all tags' || q === tagFilter.toLowerCase()) return all;
    return all.filter(
      (option) =>
        option.label.toLowerCase().includes(q) || option.value.toLowerCase().includes(q),
    );
  });
  let recentSuggestions = $derived.by(() => {
    const q = recentQuery.trim().toLowerCase();
    const files = !q
      ? recent
      : recent.filter((file) => file.path.toLowerCase().includes(q));
    return files.slice(0, 40);
  });
  let kindTip = $derived(kindOption(kind).tip);
  let terminalCmd = $derived(dashboard?.terminal_hint || TERMINAL_DASHBOARD_CMD);
  let sortedTools = $derived.by(() => {
    const tools = [...(dashboard?.tools || [])];
    const dir = sortDir === 'asc' ? 1 : -1;
    tools.sort((a, b) => {
      const av = toolValue(a, sortKey);
      const bv = toolValue(b, sortKey);
      if (typeof av === 'string' && typeof bv === 'string') {
        return av.localeCompare(bv) * dir;
      }
      return ((Number(av) || 0) - (Number(bv) || 0)) * dir;
    });
    return tools;
  });

  let chartSavedBars = $derived.by((): BarDatum[] =>
    (dashboard?.tools || []).map((tool) => ({
      label: tool.name,
      value: tool.saved,
      color: tool.inverted ? 'var(--danger)' : 'var(--ok)',
      title: [
        toolHelp(tool.name),
        `Tokens saved (distill − return): ${tool.saved_fmt ?? tool.saved}`,
        tool.inverted ? 'Net-negative: returned more tokens than the distilled baseline.' : '',
      ]
        .filter(Boolean)
        .join('\n'),
    })),
  );

  let chartCallBars = $derived.by((): BarDatum[] =>
    (dashboard?.tools || []).map((tool) => ({
      label: tool.name,
      value: tool.calls,
      color: 'var(--accent)',
      title: `${toolHelp(tool.name)}\nLifetime calls: ${tool.calls_fmt ?? tool.calls}`,
    })),
  );

  let chartOutcomes = $derived.by((): DonutDatum[] => {
    const tools = dashboard?.tools || [];
    let calls = 0;
    let trunc = 0;
    let error = 0;
    let invalid = 0;
    let low = 0;
    for (const tool of tools) {
      calls += tool.calls;
      trunc += tool.trunc_count;
      error += tool.error_count;
      invalid += tool.invalid_count ?? 0;
      low += tool.low_yield_count ?? 0;
    }
    const flagged = trunc + error + invalid + low;
    const ok = Math.max(0, calls - flagged);
    return [
      { label: 'ok', value: ok, color: 'var(--ok)', title: OUTCOME_HELP.ok },
      { label: 'trunc', value: trunc, color: 'var(--warn)', title: OUTCOME_HELP.trunc },
      { label: 'error', value: error, color: 'var(--danger)', title: OUTCOME_HELP.error },
      { label: 'invalid', value: invalid, color: '#c47ad0', title: OUTCOME_HELP.invalid },
      {
        label: 'low-yield',
        value: low,
        color: 'var(--muted)',
        title: OUTCOME_HELP['low-yield'],
      },
    ];
  });

  let chartSparkSaved = $derived.by((): SparkPoint[] =>
    (dashboard?.activity || [])
      .filter((event) => typeof event.ts === 'number' && event.ts > 0)
      .map((event) => ({
        ts: event.ts as number,
        value: event.saved,
        label: event.tool,
      })),
  );
</script>

<div
  class="shell"
  class:resizing
  class:sidebar-collapsed={sidebarCollapsed}
  style={`--sidebar-width:${sidebarWidth}px;--content-zoom:${contentZoom}`}
  ontouchstart={onShellTouchStart}
  ontouchmove={onShellTouchMove}
  ontouchend={onShellTouchEnd}
  ontouchcancel={onShellTouchEnd}
>
  <header class="top" bind:this={headerEl}>
    <div class="brand">
      <a
        class="navbar-brand logo"
        href="https://arathyll.com/home"
        target="_blank"
        rel="noreferrer"
        title="Arathyll"
        aria-label="Arathyll home"
      ></a>
      <strong title={brandTitle}>{brandTitle}</strong>
      <span
        class="chip"
        class:active={health === 'ok'}
        title={healthDetail ? `${health} · ${healthDetail}` : health}
        >{health === 'ok' ? '●' : '○'}</span
      >
    </div>
    <nav>
      <button
        class="icon-btn"
        class:active={!sidebarCollapsed}
        title="Sidebar — browse & search (Ctrl/⌘B)"
        aria-label="Toggle sidebar"
        aria-expanded={!sidebarCollapsed}
        onclick={toggleSidebarCollapsed}
      >
        <svg viewBox="0 0 24 24" aria-hidden="true"
          ><path
            d="M4 6h16M4 12h10M4 18h16"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
          /></svg
        >
      </button>
      <button
        class="icon-btn"
        class:active={tab === 'search'}
        title="Search — find Markdown chunks by keyword (Ctrl/⌘K)"
        aria-label="Search"
        onclick={() => setTab('search')}
      >
        <svg viewBox="0 0 24 24" aria-hidden="true"
          ><circle cx="11" cy="11" r="7" fill="none" stroke="currentColor" stroke-width="2" /><path
            d="M20 20l-3.5-3.5"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
          /></svg
        >
      </button>
      <button
        class="icon-btn"
        class:active={tab === 'reader'}
        title="Reader — open a page with TOC, tags, backlinks, and optional edit"
        aria-label="Reader"
        onclick={() => setTab('reader')}
      >
        <svg viewBox="0 0 24 24" aria-hidden="true"
          ><path
            d="M4 5h7v14H4zM13 5h7v14h-7z"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linejoin="round"
          /></svg
        >
      </button>
      <button
        class="icon-btn"
        class:active={tab === 'dashboard'}
        title="Dashboard — MCP token savings telemetry (GUI). Terminal: cargo run -p wordkeep --features dashboard -- dashboard"
        aria-label="Dashboard"
        onclick={() => setTab('dashboard')}
      >
        <svg viewBox="0 0 24 24" aria-hidden="true"
          ><path
            d="M4 19V9M10 19V5M16 19v-7M22 19H2"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
          /></svg
        >
      </button>
      <button
        class="icon-btn"
        class:active={tab === 'health'}
        title="Health — Runtime memory map, NUMA, pools; or Knowledge (Meilisearch / garden)"
        aria-label="Health"
        onclick={() => setTab('health')}
      >
        <svg viewBox="0 0 24 24" aria-hidden="true"
          ><path
            d="M3 12h4l2-5 4 10 2-5h6"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
          /></svg
        >
      </button>
    </nav>
  </header>

  <aside
    aria-hidden={sidebarCollapsed}
    onpointerdown={onSidebarPointerDown}
    onpointerup={onSidebarPointerUp}
    onpointercancel={onSidebarPointerCancel}
  >
    <div class="sidebar-chrome">
      <span class="sidebar-chrome-label" title="Double-tap or hold to hide sidebar">Browse</span>
      <div class="sidebar-chrome-actions">
        <button
          type="button"
          class="icon-btn sidebar-chrome-btn"
          title="Collapse sidebar (Ctrl/⌘B)"
          aria-label="Collapse sidebar"
          onclick={toggleSidebarCollapsed}
        >
          <svg viewBox="0 0 24 24" aria-hidden="true"
            ><path
              d="M15 6l-6 6 6 6"
              fill="none"
              stroke="currentColor"
              stroke-width="2"
              stroke-linecap="round"
              stroke-linejoin="round"
            /></svg
          >
        </button>
      </div>
    </div>
    <form class="search" onsubmit={runSearch}>
      <input
        bind:value={query}
        type="search"
        placeholder="Search… (Ctrl/⌘K)"
        aria-label="Search"
        title="Instant search over indexed Markdown (debounced). Ctrl/⌘K focuses here."
        oninput={scheduleSearch}
      />
      <button
        class="icon-btn"
        type="submit"
        disabled={searching}
        title="Run search now"
        aria-label="Run search"
      >
        <svg viewBox="0 0 24 24" aria-hidden="true"
          ><circle cx="11" cy="11" r="7" fill="none" stroke="currentColor" stroke-width="2" /><path
            d="M20 20l-3.5-3.5"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
          /></svg
        >
      </button>
    </form>
    <div class="filters" title="Narrow live search by kind, path root, or frontmatter tag">
      <div class="filter-field combo-field" title={kindTip}>
        <span class="filter-icon" title={kindTip} aria-hidden="true">
          {#if kind === 'doc'}
            <svg viewBox="0 0 24 24"
              ><path
                d="M5 4h9l5 5v11H5z"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linejoin="round"
              /></svg
            >
          {:else if kind === 'rule'}
            <svg viewBox="0 0 24 24"
              ><path
                d="M12 3l8 4v6c0 5-3.5 8.5-8 10-4.5-1.5-8-5-8-10V7l8-4z"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linejoin="round"
              /></svg
            >
          {:else if kind === 'note'}
            <svg viewBox="0 0 24 24"
              ><path
                d="M6 3h9l3 3v15H6zM9 9h6M9 13h6M9 17h4"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
                stroke-linejoin="round"
              /></svg
            >
          {:else if kind === 'lore'}
            <svg viewBox="0 0 24 24"
              ><path
                d="M4 5h7v14H4zM13 5h7v14h-7z"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linejoin="round"
              /></svg
            >
          {:else}
            <svg viewBox="0 0 24 24"
              ><path
                d="M4 7h16M4 12h16M4 17h16"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
              /></svg
            >
          {/if}
        </span>
        <div class="combo">
          <input
            bind:value={kindQuery}
            type="text"
            role="combobox"
            aria-expanded={kindMenuOpen}
            aria-controls="kind-suggestions"
            aria-autocomplete="list"
            placeholder="Kind — type to search…"
            aria-label="Document kind"
            title={kindTip}
            onfocus={() => {
              kindMenuOpen = true;
              kindQuery = '';
              kindHighlight = 0;
            }}
            oninput={() => {
              kindMenuOpen = true;
              kindHighlight = 0;
            }}
            onkeydown={onKindKeydown}
            onblur={() => {
              setTimeout(() => {
                kindMenuOpen = false;
                kindQuery = kindOption(kind).label;
              }, 120);
            }}
          />
          {#if kindMenuOpen}
            <ul
              id="kind-suggestions"
              class="combo-menu"
              role="listbox"
              title="Document kinds"
            >
              {#each kindSuggestions as option, index}
                <li role="option" aria-selected={index === kindHighlight}>
                  <button
                    type="button"
                    class="combo-item"
                    class:active={index === kindHighlight || kind === option.value}
                    title={option.tip}
                    onmousedown={(e) => e.preventDefault()}
                    onmouseenter={() => (kindHighlight = index)}
                    onclick={() => selectKind(option.value)}
                    >{option.label}</button
                  >
                </li>
              {:else}
                <li class="muted combo-empty">No matching kinds</li>
              {/each}
            </ul>
          {/if}
        </div>
      </div>

      <div
        class="filter-field combo-field"
        title="Limit search to files whose path starts with this folder prefix (e.g. docs/designs)."
      >
        <span
          class="filter-icon"
          title="Path root — type to filter folder prefixes from the doc tree"
          aria-hidden="true"
        >
          <svg viewBox="0 0 24 24"
            ><path
              d="M3 7h7l2 2h9v10H3z"
              fill="none"
              stroke="currentColor"
              stroke-width="2"
              stroke-linejoin="round"
            /></svg
          >
        </span>
        <div class="combo">
          <input
            bind:value={rootFilter}
            type="text"
            role="combobox"
            aria-expanded={rootMenuOpen}
            aria-controls="root-suggestions"
            aria-autocomplete="list"
            placeholder="Path root — type to search…"
            aria-label="Path root"
            title="Path root — typeahead over indexed folders; clear for all roots"
            onfocus={() => {
              rootMenuOpen = true;
            }}
            oninput={() => {
              rootMenuOpen = true;
              rootHighlight = 0;
              scheduleSearch();
            }}
            onkeydown={onRootKeydown}
            onblur={() => {
              setTimeout(() => {
                rootMenuOpen = false;
              }, 120);
            }}
          />
          {#if rootMenuOpen}
            <ul
              id="root-suggestions"
              class="combo-menu"
              role="listbox"
              title="Indexed folder prefixes"
            >
              <li role="option" aria-selected={rootHighlight === 0 && !rootFilter}>
                <button
                  type="button"
                  class="combo-item"
                  class:active={rootHighlight === 0 || !rootFilter}
                  title="Clear path root filter"
                  onmousedown={(e) => e.preventDefault()}
                  onmouseenter={() => (rootHighlight = 0)}
                  onclick={() => selectRoot('')}
                  >All roots</button
                >
              </li>
              {#each rootSuggestions as root, index}
                <li role="option" aria-selected={rootHighlight === index + 1}>
                  <button
                    type="button"
                    class="combo-item"
                    class:active={rootHighlight === index + 1 || rootFilter === root}
                    title={`Filter to paths under ${root}`}
                    onmousedown={(e) => e.preventDefault()}
                    onmouseenter={() => (rootHighlight = index + 1)}
                    onclick={() => selectRoot(root)}
                    ><code>{root}</code></button
                  >
                </li>
              {:else}
                <li class="muted combo-empty">No matching folders</li>
              {/each}
            </ul>
          {/if}
        </div>
      </div>

      <div
        class="filter-field combo-field"
        title="Filter hits to pages that carry this frontmatter tag."
      >
        <span
          class="filter-icon"
          title="Tag filter — type to search frontmatter tags"
          aria-hidden="true"
        >
          <svg viewBox="0 0 24 24"
            ><path
              d="M3 12l9-9h6l3 3v6l-9 9-9-9zM14 7.5a1.5 1.5 0 1 1 0 0.01"
              fill="none"
              stroke="currentColor"
              stroke-width="2"
              stroke-linejoin="round"
            /></svg
          >
        </span>
        <div class="combo">
          <input
            bind:value={tagQuery}
            type="text"
            role="combobox"
            aria-expanded={tagMenuOpen}
            aria-controls="tag-suggestions"
            aria-autocomplete="list"
            placeholder="Tag — type to search…"
            aria-label="Tag filter"
            title="Tag filter — typeahead over garden tags; All tags clears the filter"
            onfocus={() => {
              tagMenuOpen = true;
              tagQuery = '';
              tagHighlight = 0;
            }}
            oninput={() => {
              tagMenuOpen = true;
              tagHighlight = 0;
            }}
            onkeydown={onTagKeydown}
            onblur={() => {
              setTimeout(() => {
                tagMenuOpen = false;
                tagQuery = tagFilter || 'All tags';
              }, 120);
            }}
          />
          {#if tagMenuOpen}
            <ul id="tag-suggestions" class="combo-menu" role="listbox" title="Frontmatter tags">
              {#each tagSuggestions as option, index}
                <li role="option" aria-selected={index === tagHighlight}>
                  <button
                    type="button"
                    class="combo-item"
                    class:active={index === tagHighlight || tagFilter === option.value}
                    title={option.tip}
                    onmousedown={(e) => e.preventDefault()}
                    onmouseenter={() => (tagHighlight = index)}
                    onclick={() => selectTag(option.value)}
                  >
                    {#if option.value}
                      <span
                        class="combo-swatch"
                        style={`background:${tagColor(option.value)}`}
                        aria-hidden="true"
                      ></span>
                      <code>{option.label}</code>
                    {:else}
                      {option.label}
                    {/if}
                  </button>
                </li>
              {:else}
                <li class="muted combo-empty">No matching tags</li>
              {/each}
            </ul>
          {/if}
        </div>
      </div>
    </div>
    <p class="muted status" title={status}><code>{status}</code></p>

    <div
      class="recent-combo combo-field"
      title="Recently modified Markdown — type to filter, Enter to open"
    >
      <h3 class="section-title" title="Recently modified Markdown under configured doc roots">
        <svg viewBox="0 0 24 24" aria-hidden="true"
          ><circle cx="12" cy="12" r="8" fill="none" stroke="currentColor" stroke-width="2" /><path
            d="M12 8v5l3 2"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
          /></svg
        >
        Recent
      </h3>
      <div class="combo">
        <input
          bind:value={recentQuery}
          type="search"
          role="combobox"
          aria-expanded={recentMenuOpen}
          aria-controls="recent-suggestions"
          aria-autocomplete="list"
          placeholder="Recent files — type to search…"
          aria-label="Recent files"
          title="Search recently modified Markdown paths"
          onfocus={() => {
            recentMenuOpen = true;
            recentHighlight = 0;
          }}
          oninput={() => {
            recentMenuOpen = true;
            recentHighlight = 0;
          }}
          onkeydown={onRecentKeydown}
          onblur={() => {
            setTimeout(() => {
              recentMenuOpen = false;
            }, 120);
          }}
        />
        {#if recentMenuOpen}
          <ul
            id="recent-suggestions"
            class="combo-menu"
            role="listbox"
            title="Recently modified files"
          >
            {#each recentSuggestions as file, index}
              <li role="option" aria-selected={index === recentHighlight}>
                <button
                  type="button"
                  class="combo-item"
                  class:active={index === recentHighlight || pagePath === file.path}
                  title={file.path}
                  onmousedown={(e) => e.preventDefault()}
                  onmouseenter={() => (recentHighlight = index)}
                  onclick={() => selectRecent(file.path)}
                  ><code>{file.path}</code></button
                >
              </li>
            {:else}
              <li class="muted combo-empty">No recent matches</li>
            {/each}
          </ul>
        {/if}
      </div>
    </div>

    <div class="tree-head">
      <h3
        class="section-title"
        title="Browse indexed Markdown by folder. Click a folder to expand/collapse; click a file to open in Reader."
      >
        <svg viewBox="0 0 24 24" aria-hidden="true"
          ><path
            d="M12 3v6M12 9H7v5M12 9h5v5M7 14v4M17 14v4"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
          /><circle cx="12" cy="3" r="1.5" fill="currentColor" /><circle
            cx="7"
            cy="19"
            r="1.5"
            fill="currentColor"
          /><circle cx="17" cy="19" r="1.5" fill="currentColor" /></svg
        >
        Tree
      </h3>
      <div class="tree-actions">
        <button
          type="button"
          class="icon-btn tree-action"
          title="Expand all folders"
          aria-label="Expand all folders"
          onclick={expandAllDirs}
        >
          <svg viewBox="0 0 24 24" aria-hidden="true"
            ><path
              d="M4 12h16M12 4v16"
              fill="none"
              stroke="currentColor"
              stroke-width="2"
              stroke-linecap="round"
            /></svg
          >
        </button>
        <button
          type="button"
          class="icon-btn tree-action"
          title="Collapse all folders"
          aria-label="Collapse all folders"
          onclick={collapseAllDirs}
        >
          <svg viewBox="0 0 24 24" aria-hidden="true"
            ><path
              d="M5 12h14"
              fill="none"
              stroke="currentColor"
              stroke-width="2"
              stroke-linecap="round"
            /></svg
          >
        </button>
      </div>
    </div>
    <div class="tree">
      {#each flatTree as { node, depth, dirKey, expanded }}
        {#if node.path}
          <button
            class="tree-item"
            class:active={pagePath === node.path}
            style={`--depth:${depth}`}
            title={node.path}
            onclick={(event) => {
              rippleFromEvent(event);
              openPath(node.path!);
            }}
          >
            <span class="tree-indent" aria-hidden="true"></span>
            <svg class="leaf" viewBox="0 0 24 24" aria-hidden="true"
              ><path
                d="M7 3h7l4 4v14H7z"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linejoin="round"
              /></svg
            >
            <span class="tree-label">{node.name}</span>
          </button>
        {:else if dirKey}
          <button
            type="button"
            class="tree-dir"
            class:collapsed={expanded === false}
            style={`--depth:${depth}`}
            title={expanded
              ? `Collapse ${dirKey}`
              : `Expand ${dirKey}`}
            aria-expanded={expanded}
            aria-label={`${expanded ? 'Collapse' : 'Expand'} folder ${node.name}`}
            onclick={(event) => {
              rippleFromEvent(event);
              toggleDir(dirKey);
            }}
          >
            <span class="tree-indent" aria-hidden="true"></span>
            <svg class="chevron" viewBox="0 0 24 24" aria-hidden="true"
              ><path
                d="M9 6l6 6-6 6"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
                stroke-linejoin="round"
              /></svg
            >
            <svg class="leaf" viewBox="0 0 24 24" aria-hidden="true"
              ><path
                d="M3 7h7l2 2h9v10H3z"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linejoin="round"
              /></svg
            >
            <span class="tree-label">{node.name}</span>
          </button>
        {/if}
      {/each}
      {#if flatTree.length === 0}
        <p class="muted">No markdown under configured doc roots.</p>
      {/if}
    </div>
  </aside>

  {#if sidebarCollapsed}
    <button
      type="button"
      class="sidebar-expand-tab"
      aria-label="Expand sidebar"
      title="Expand sidebar (Ctrl/⌘B)"
      onclick={toggleSidebarCollapsed}
    >
      <svg viewBox="0 0 24 24" aria-hidden="true"
        ><path
          d="M9 6l6 6-6 6"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
          stroke-linecap="round"
          stroke-linejoin="round"
        /></svg
      >
    </button>
  {/if}

  <button
    type="button"
    class="sidebar-resizer"
    aria-label="Resize sidebar"
    title="Drag to resize sidebar"
    onpointerdown={startResize}
    onkeydown={(event) => {
      if (event.key === 'ArrowLeft') {
        sidebarWidth = clampSidebar(sidebarWidth - 16);
        safeStorage.setItem(SIDEBAR_KEY, String(sidebarWidth));
      } else if (event.key === 'ArrowRight') {
        sidebarWidth = clampSidebar(sidebarWidth + 16);
        safeStorage.setItem(SIDEBAR_KEY, String(sidebarWidth));
      }
    }}
  ></button>

  <main bind:this={mainEl}>
    {#if editorTabs.length}
      <div class="main-chrome" bind:this={mainChromeEl}>
      <div
        class="editor-tabs-stack"
        title="Open pages as tabs. Pinned tabs stay on the top row."
      >
        {#if pinnedTabs.length}
          <div
            class="editor-tabs pinned-row"
            role="tablist"
            aria-label="Pinned tabs"
            title="Pinned tabs — stay open across sessions until unpinned/closed"
          >
            {#each pinnedTabs as etab}
              <div
                class="editor-tab"
                class:active={pagePath === etab.path && tab === 'reader'}
                class:pinned={etab.pinned}
                role="tab"
                aria-selected={pagePath === etab.path && tab === 'reader'}
                tabindex="0"
                title={etab.path}
                onclick={() => activateEditorTab(etab.path)}
                onauxclick={(e) => onEditorTabAuxclick(etab.path, e)}
                onkeydown={(e) => {
                  if (e.key === 'Enter' || e.key === ' ') {
                    e.preventDefault();
                    void activateEditorTab(etab.path);
                  }
                }}
              >
                <svg class="tab-file" viewBox="0 0 24 24" aria-hidden="true"
                  ><path
                    d="M7 3h7l4 4v14H7z"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="2"
                    stroke-linejoin="round"
                  /></svg
                >
                <span class="tab-label">{tabLabel(etab.path)}</span>
                <button
                  type="button"
                  class="tab-action pin active-pin"
                  title="Unpin tab"
                  aria-label={`Unpin ${etab.path}`}
                  onclick={(e) => togglePinTab(etab.path, e)}
                >
                  <svg viewBox="0 0 24 24" aria-hidden="true"
                    ><path
                      d="M12 2v8M8 6h8M9 14l-2 8h10l-2-8"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="2"
                      stroke-linecap="round"
                      stroke-linejoin="round"
                    /></svg
                  >
                </button>
                <button
                  type="button"
                  class="tab-action close"
                  title="Close tab (Ctrl/⌘W)"
                  aria-label={`Close ${etab.path}`}
                  onclick={(e) => closeEditorTab(etab.path, e)}
                >
                  <svg viewBox="0 0 24 24" aria-hidden="true"
                    ><path
                      d="M6 6l12 12M18 6L6 18"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="2"
                      stroke-linecap="round"
                    /></svg
                  >
                </button>
              </div>
            {/each}
          </div>
        {/if}
        {#if unpinnedTabs.length}
          <div
            class="editor-tabs"
            role="tablist"
            aria-label="Open tabs"
            title="Open tabs — pin to keep on the top row"
          >
            {#each unpinnedTabs as etab}
              <div
                class="editor-tab"
                class:active={pagePath === etab.path && tab === 'reader'}
                role="tab"
                aria-selected={pagePath === etab.path && tab === 'reader'}
                tabindex="0"
                title={etab.path}
                onclick={() => activateEditorTab(etab.path)}
                onauxclick={(e) => onEditorTabAuxclick(etab.path, e)}
                onkeydown={(e) => {
                  if (e.key === 'Enter' || e.key === ' ') {
                    e.preventDefault();
                    void activateEditorTab(etab.path);
                  }
                }}
              >
                <svg class="tab-file" viewBox="0 0 24 24" aria-hidden="true"
                  ><path
                    d="M7 3h7l4 4v14H7z"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="2"
                    stroke-linejoin="round"
                  /></svg
                >
                <span class="tab-label">{tabLabel(etab.path)}</span>
                <button
                  type="button"
                  class="tab-action pin"
                  title="Pin tab to top row"
                  aria-label={`Pin ${etab.path}`}
                  onclick={(e) => togglePinTab(etab.path, e)}
                >
                  <svg viewBox="0 0 24 24" aria-hidden="true"
                    ><path
                      d="M12 2v8M8 6h8M9 14l-2 8h10l-2-8"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="2"
                      stroke-linecap="round"
                      stroke-linejoin="round"
                    /></svg
                  >
                </button>
                <button
                  type="button"
                  class="tab-action close"
                  title="Close tab (Ctrl/⌘W · middle-click)"
                  aria-label={`Close ${etab.path}`}
                  onclick={(e) => closeEditorTab(etab.path, e)}
                >
                  <svg viewBox="0 0 24 24" aria-hidden="true"
                    ><path
                      d="M6 6l12 12M18 6L6 18"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="2"
                      stroke-linecap="round"
                    /></svg
                  >
                </button>
              </div>
            {/each}
          </div>
        {/if}
      </div>
      </div>
    {/if}

    <div class="main-body">
    {#if tab === 'search'}
      <section class="panel enter">
        <header class="panel-head">
          <h2 class="section-title">
            <svg viewBox="0 0 24 24" aria-hidden="true"
              ><circle cx="11" cy="11" r="7" fill="none" stroke="currentColor" stroke-width="2" /><path
                d="M20 20l-3.5-3.5"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
              /></svg
            >
            Results
          </h2>
          <span class="muted"><code>{lastCount}</code> estimated · click to open</span>
        </header>
        <ul class="hits">
          {#each hits as hit, index}
            <li>
              <button onclick={() => openHit(hit, index + 1)}>
                <strong>{hit.heading || '(intro)'}</strong>
                <span class="muted"
                  >{hit.path}{#if hit.kind} · {hit.kind}{/if}{#if hit.tags?.length}
                    · {hit.tags.join(', ')}{/if}</span
                >
                <p>{@html snippetOf(hit)}</p>
              </button>
            </li>
          {:else}
            <li class="muted">No hits yet. Start typing — search is live.</li>
          {/each}
        </ul>
      </section>
    {:else if tab === 'reader'}
      <section class="reader panel enter">
        <div class="page-actions-float" aria-label="Page actions">
          <div class="actions">
            <button
              class="icon-btn"
              onclick={copyPath}
              disabled={!pagePath}
              title="Copy path"
              aria-label="Copy path"
            >
              <svg viewBox="0 0 24 24" aria-hidden="true"
                ><rect
                  x="8"
                  y="8"
                  width="11"
                  height="11"
                  rx="1.5"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="2"
                /><path
                  d="M5 15V5h10"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="2"
                  stroke-linecap="round"
                /></svg
              >
            </button>
            {#if editing}
              <button
                class="icon-btn primary"
                onclick={doneEditing}
                disabled={saving && dirty}
                title="Done editing (Esc) · autosaved"
                aria-label="Done editing"
              >
                <svg viewBox="0 0 24 24" aria-hidden="true"
                  ><path
                    d="M4 12l5 5L20 6"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="2"
                    stroke-linecap="round"
                    stroke-linejoin="round"
                  /></svg
                >
              </button>
            {:else if !readOnly}
              <button
                class="icon-btn"
                onclick={startEditing}
                disabled={!pagePath}
                title="Edit Markdown (Ctrl/⌘E) · autosaves"
                aria-label="Edit"
              >
                <svg viewBox="0 0 24 24" aria-hidden="true"
                  ><path
                    d="M4 20l4.5-1L19 8.5 15.5 5 5 15.5 4 20z"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="2"
                    stroke-linejoin="round"
                  /><path
                    d="M13.5 6.5l4 4"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="2"
                    stroke-linecap="round"
                  /></svg
                >
              </button>
            {/if}
          </div>
        </div>
        <header class="panel-head reader-head">
          <div class="title-block">
            <h2 class="section-title" title={pagePath || 'Reader'}>
              <svg viewBox="0 0 24 24" aria-hidden="true"
                ><path
                  d="M4 5h7v14H4zM13 5h7v14h-7z"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="2"
                  stroke-linejoin="round"
                /></svg
              >
              <span>{pagePath || 'Reader'}</span>
            </h2>
            {#if editing}
              <span
                class="chip save-chip"
                class:active={!dirty && !saveError}
                class:warn={dirty || !!saveError}
                title={saveError || (editing ? 'Autosaves while you type · Esc to close editor' : '')}
                ><code>{saveHint}</code></span
              >
            {/if}
            {#if pagePath}
              <div class="tag-editor" title="Create, rename, or delete frontmatter tags for this page">
                <div class="tag-row">
                  {#each pageTags as tag}
                    {#if renamingTag === tag}
                      <span class="chip tag-chip editing">
                        <input
                          class="tag-rename"
                          bind:value={renameDraft}
                          aria-label={`Rename tag ${tag}`}
                          title="Rename tag · Enter to save · Esc to cancel"
                          disabled={tagBusy}
                          onkeydown={(e) => {
                            if (e.key === 'Enter') {
                              e.preventDefault();
                              void commitRenameTag();
                            } else if (e.key === 'Escape') {
                              e.preventDefault();
                              cancelRenameTag();
                            }
                          }}
                        />
                        <button
                          type="button"
                          class="tag-x"
                          title="Save rename"
                          aria-label="Save rename"
                          disabled={tagBusy}
                          onclick={() => commitRenameTag()}
                          >✓</button
                        >
                      </span>
                    {:else}
                      <span
                        class="chip tag-chip"
                        style={`--tag-color:${tagColor(tag)}`}
                        title={`tag:${tag} — rename, recolor, or delete`}
                      >
                        <label class="tag-swatch" title={`Pick color for “${tag}”`}>
                          <input
                            type="color"
                            value={tagColor(tag)}
                            aria-label={`Color for tag ${tag}`}
                            disabled={tagBusy}
                            oninput={(e) =>
                              setTagColor(tag, (e.currentTarget as HTMLInputElement).value)}
                          />
                        </label>
                        <button
                          type="button"
                          class="tag-name"
                          disabled={tagBusy}
                          title="Click to rename"
                          onclick={() => beginRenameTag(tag)}>{tag}</button
                        >
                        <input
                          class="tag-hex"
                          value={hexDraft(tag)}
                          aria-label={`Hex color for tag ${tag}`}
                          title="Paste or edit hex color (#rgb / #rrggbb)"
                          spellcheck="false"
                          disabled={tagBusy}
                          oninput={(e) =>
                            onTagHexInput(tag, (e.currentTarget as HTMLInputElement).value)}
                          onblur={() => commitTagHex(tag)}
                          onkeydown={(e) => {
                            if (e.key === 'Enter') {
                              e.preventDefault();
                              commitTagHex(tag);
                              (e.currentTarget as HTMLInputElement).blur();
                            }
                          }}
                        />
                        <button
                          type="button"
                          class="tag-x"
                          title={`Copy hex ${tagColor(tag)}`}
                          aria-label={`Copy hex for ${tag}`}
                          onclick={() => copyText(tagColor(tag), `Copied ${tagColor(tag)}`)}
                        >
                          <svg viewBox="0 0 24 24" aria-hidden="true"
                            ><rect
                              x="8"
                              y="8"
                              width="12"
                              height="12"
                              rx="2"
                              fill="none"
                              stroke="currentColor"
                              stroke-width="2"
                            /><path
                              d="M4 16V6a2 2 0 0 1 2-2h10"
                              fill="none"
                              stroke="currentColor"
                              stroke-width="2"
                              stroke-linecap="round"
                            /></svg
                          >
                        </button>
                        <button
                          type="button"
                          class="tag-x"
                          title={`Delete tag “${tag}”`}
                          aria-label={`Delete tag ${tag}`}
                          disabled={tagBusy}
                          onclick={() => removePageTag(tag)}>×</button
                        >
                      </span>
                    {/if}
                  {:else}
                    <span class="muted tag-empty" title="No frontmatter tags on this page yet"
                      >No tags</span
                    >
                  {/each}
                </div>
                <form
                  class="tag-add"
                  onsubmit={(e) => {
                    e.preventDefault();
                    void addPageTag();
                  }}
                >
                  <input
                    bind:value={tagDraft}
                    type="text"
                    placeholder="Add tag…"
                    aria-label="Add tag"
                    title="Create a new frontmatter tag on this page"
                    disabled={!pagePath || tagBusy}
                  />
                  <button
                    class="icon-btn"
                    type="submit"
                    disabled={!pagePath || tagBusy || !normalizeTag(tagDraft)}
                    title="Add tag"
                    aria-label="Add tag"
                  >
                    <svg viewBox="0 0 24 24" aria-hidden="true"
                      ><path
                        d="M12 5v14M5 12h14"
                        fill="none"
                        stroke="currentColor"
                        stroke-width="2"
                        stroke-linecap="round"
                      /></svg
                    >
                  </button>
                </form>
                {#if tagError}
                  <p class="muted tag-error" title={tagError}><code>{tagError}</code></p>
                {/if}
              </div>
            {/if}
          </div>
        </header>
        {#if editing}
          <textarea
            class="editor"
            bind:value={draftMarkdown}
            oninput={onDraftInput}
            spellcheck="true"
            aria-label="Markdown editor"
            title="Autosaves as you type"
          ></textarea>
        {:else}
          {#if outline.length}
            <nav class="toc" aria-label="Table of contents">
              <h3 class="section-title">
                <svg viewBox="0 0 24 24" aria-hidden="true"
                  ><path
                    d="M5 6h14M5 12h10M5 18h12"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="2"
                    stroke-linecap="round"
                  /></svg
                >
                On this page
              </h3>
              <ul>
                {#each outline as section}
                  <li style={`padding-left:${(section.heading_level - 1) * 0.75}rem`}>
                    <button onclick={() => openPath(pagePath, undefined, section.anchor)}
                      >{section.heading}</button
                    >
                  </li>
                {/each}
              </ul>
            </nav>
          {/if}
          <article>{@html pageHtml}</article>
          {#if backlinks.length}
            <footer class="backlinks">
              <h3 class="section-title">
                <svg viewBox="0 0 24 24" aria-hidden="true"
                  ><path
                    d="M10 13a5 5 0 0 0 7 0l2-2a5 5 0 0 0-7-7l-1 1"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="2"
                    stroke-linecap="round"
                  /><path
                    d="M14 11a5 5 0 0 0-7 0l-2 2a5 5 0 0 0 7 7l1-1"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="2"
                    stroke-linecap="round"
                  /></svg
                >
                Backlinks
              </h3>
              <ul>
                {#each backlinks as link}
                  <li>
                    <button onclick={() => openPath(link.path)}
                      >{link.heading || link.path}</button
                    >
                  </li>
                {/each}
              </ul>
            </footer>
          {/if}
        {/if}
      </section>
    {:else if tab === 'dashboard'}
      <section class="dashboard panel enter">
        <header class="panel-head">
          <h2
            class="section-title"
            title="Live MCP token-savings telemetry from ~/.cache/wordkeep/savings.json. Same data as the terminal dashboard."
          >
            <svg viewBox="0 0 24 24" aria-hidden="true"
              ><path
                d="M4 19V9M10 19V5M16 19v-7M22 19H2"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
                stroke-linejoin="round"
              /></svg
            >
            MCP telemetry
          </h2>
          <button
            class="icon-btn"
            onclick={() => refreshDashboard()}
            title="Refresh now (also auto-polls every 2s while this tab is open)"
            aria-label="Refresh dashboard"
          >
            <svg viewBox="0 0 24 24" aria-hidden="true"
              ><path
                d="M20 12a8 8 0 1 1-2.3-5.6"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
              /><path
                d="M20 4v5h-5"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
                stroke-linejoin="round"
              /></svg
            >
          </button>
        </header>

        {#if dashboardError}
          <p class="muted">Dashboard unavailable: <code>{dashboardError}</code></p>
        {:else if !dashboard}
          <p class="muted">Loading telemetry…</p>
        {:else if !dashboard.available}
          <p class="muted">{dashboard.message || 'No savings.json yet.'}</p>
        {:else}
          <CollapsibleSection id="dash-overview" titleAttr={CHART_HELP.overview}>
            {#snippet heading()}
              <svg viewBox="0 0 24 24" aria-hidden="true"
                ><path
                  d="M4 4h7v7H4V4zm9 0h7v7h-7V4zM4 13h7v7H4v-7zm9 4h7v3h-7v-3z"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="2"
                  stroke-linejoin="round"
                /></svg
              >
              Overview
            {/snippet}
            <div class="cards dash-overview">
              <article class="card" title={METRIC_HELP.calls}>
                <h3>Calls</h3>
                <p class="metric"><code>{dashboard.overview?.calls_fmt ?? 0}</code></p>
              </article>
              <article class="card" title={METRIC_HELP.distilled}>
                <h3>Distilled</h3>
                <p class="metric"><code>{dashboard.overview?.baseline_fmt ?? 0}</code></p>
              </article>
              <article class="card" title={METRIC_HELP.returned}>
                <h3>Returned</h3>
                <p class="metric"><code>{dashboard.overview?.returned_fmt ?? 0}</code></p>
              </article>
              <article class="card" title={METRIC_HELP.saved}>
                <h3>Saved</h3>
                <p class="metric good"
                  ><code
                    >{dashboard.overview?.saved_fmt ?? 0}
                    · {dashboard.overview?.reduction_pct ?? 0}%</code
                  ></p
                >
              </article>
              <article class="card" title={METRIC_HELP.session}>
                <h3>Session</h3>
                <p class="metric"><code>{sessionSavedFmt}</code></p>
              </article>
              <article class="card" title={METRIC_HELP.tracking}>
                <h3>Tracking</h3>
                <p class="metric"><code>{dashboard.overview?.since_label ?? '—'}</code></p>
              </article>
            </div>
          </CollapsibleSection>

          <CollapsibleSection id="dash-charts" titleAttr={CHART_HELP.section}>
            {#snippet heading()}
              <svg viewBox="0 0 24 24" aria-hidden="true"
                ><path
                  d="M4 19V9M10 19V5M16 19v-7M22 19H2"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="2"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                /></svg
              >
              Charts
            {/snippet}
            <div class="dash-charts">
              <article class="card chart-card" title={CHART_HELP.saved}>
                <h3 title={CHART_HELP.saved}>Tokens saved by tool</h3>
                <BarChart
                  data={chartSavedBars}
                  empty="No tool savings yet."
                  ariaLabel="Tokens saved by tool"
                  chartTitle={CHART_HELP.saved}
                />
              </article>
              <article class="card chart-card" title={CHART_HELP.calls}>
                <h3 title={CHART_HELP.calls}>Calls by tool</h3>
                <BarChart
                  data={chartCallBars}
                  empty="No tool calls yet."
                  ariaLabel="Calls by tool"
                  chartTitle={CHART_HELP.calls}
                />
              </article>
              <article class="card chart-card" title={CHART_HELP.outcomes}>
                <h3 title={CHART_HELP.outcomes}>Outcomes</h3>
                <DonutChart
                  data={chartOutcomes}
                  empty="No outcomes yet."
                  ariaLabel="Call outcomes"
                  chartTitle={CHART_HELP.outcomes}
                  centerTitle="Total MCP calls across all outcome buckets"
                  valueLabel="calls"
                />
              </article>
              <article class="card chart-card" title={CHART_HELP.spark}>
                <h3 title={CHART_HELP.spark}>Recent savings</h3>
                <Sparkline
                  data={chartSparkSaved}
                  empty="No recent events with timestamps."
                  ariaLabel="Recent tokens saved per call"
                  chartTitle={CHART_HELP.spark}
                  valueLabel="tokens saved"
                  yAxisLabel="tokens saved"
                  xAxisLabel="older → newer"
                />
              </article>
            </div>
          </CollapsibleSection>

          <div class="dash-split">
            <CollapsibleSection id="dash-activity" titleAttr={CHART_HELP.activity}>
              {#snippet heading()}
                <svg viewBox="0 0 24 24" aria-hidden="true"
                  ><path
                    d="M12 8v5l3 2M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20z"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="2"
                    stroke-linecap="round"
                    stroke-linejoin="round"
                  /></svg
                >
                Recent activity
              {/snippet}
              <div class="table-wrap">
                <table class="dash-table">
                  <thead>
                    <tr>
                      <th class="static" title="Time since the call">When</th>
                      <th class="static" title="MCP tool name">Tool</th>
                      <th class="static" title="Wall time for the call">Ms</th>
                      <th class="static" title="Estimated tokens without wordkeep">Distill</th>
                      <th class="static" title="Tokens actually returned">Return</th>
                      <th class="static" title="Call outcome (ok / trunc / error / low-yield)"
                        >Outcome</th
                      >
                    </tr>
                  </thead>
                  <tbody>
                    {#each dashboard.activity || [] as event}
                      <tr title={activityTitle(event)}>
                        <td class="muted">{event.ago}</td>
                        <td class="tool-name"
                          ><code title={activityTitle(event)}>{event.tool}</code></td
                        >
                        <td>{event.elapsed_ms}ms</td>
                        <td class="good">{event.baseline_fmt ?? event.baseline}</td>
                        <td>{event.returned_fmt ?? event.returned}</td>
                        <td>
                          {#if event.outcome !== 'ok'}
                            <span class="chip warn" title={activityTitle(event)}
                              >{event.outcome}</span
                            >
                          {:else}
                            <span class="muted">ok</span>
                          {/if}
                        </td>
                      </tr>
                    {:else}
                      <tr>
                        <td colspan="6" class="muted">No recent MCP events.</td>
                      </tr>
                    {/each}
                  </tbody>
                </table>
              </div>
            </CollapsibleSection>
            <CollapsibleSection id="dash-signals" titleAttr={CHART_HELP.signals}>
              {#snippet heading()}
                <svg viewBox="0 0 24 24" aria-hidden="true"
                  ><path
                    d="M12 3l8 4v6c0 5-3.5 8.5-8 10-4.5-1.5-8-5-8-10V7l8-4z"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="2"
                    stroke-linejoin="round"
                  /></svg
                >
                Health signals
              {/snippet}
              <div class="table-wrap">
                <table class="dash-table">
                  <thead>
                    <tr>
                      <th class="static" title="Signal category">Signal</th>
                      <th class="static" title="Primary value">Value</th>
                      <th class="static" title="Related tool or note">Detail</th>
                    </tr>
                  </thead>
                  <tbody>
                    {#each dashboard.health?.signals || [] as signal}
                      <tr
                        class={`signal-${signal.kind}`}
                        title={`${signal.label}: ${signal.value ?? '—'}${signal.detail ? ` (${signal.detail})` : ''}`}
                      >
                        <td class="muted">{signal.label}</td>
                        <td class="signal-value">{signal.value ?? '—'}</td>
                        <td>
                          {#if signal.detail}
                            <code>{signal.detail}</code>
                          {:else}
                            <span class="muted">—</span>
                          {/if}
                        </td>
                      </tr>
                    {:else}
                      <tr>
                        <td colspan="3" class="muted">No health signals yet.</td>
                      </tr>
                    {/each}
                  </tbody>
                </table>
              </div>
            </CollapsibleSection>
          </div>

          <CollapsibleSection id="dash-tools" titleAttr={CHART_HELP.tools}>
            {#snippet heading()}
              <svg viewBox="0 0 24 24" aria-hidden="true"
                ><path
                  d="M4 6h16M4 12h16M4 18h10"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="2"
                  stroke-linecap="round"
                /></svg
              >
              Tools
            {/snippet}
          <div class="table-wrap">
            <table class="dash-table">
              <thead>
                <tr>
                  <th>
                    <button
                      type="button"
                      class="sort-btn"
                      title={COLUMN_HELP.name}
                      onclick={() => toggleSort('name')}
                      >Tool{sortMark('name')}</button
                    >
                  </th>
                  <th>
                    <button
                      type="button"
                      class="sort-btn"
                      title={COLUMN_HELP.calls}
                      onclick={() => toggleSort('calls')}
                      >Calls{sortMark('calls')}</button
                    >
                  </th>
                  <th>
                    <button
                      type="button"
                      class="sort-btn"
                      title={COLUMN_HELP.avg_ms}
                      onclick={() => toggleSort('avg_ms')}
                      >Avg{sortMark('avg_ms')}</button
                    >
                  </th>
                  <th>
                    <button
                      type="button"
                      class="sort-btn"
                      title={COLUMN_HELP.trunc}
                      onclick={() => toggleSort('trunc_count')}
                      >Trunc{sortMark('trunc_count')}</button
                    >
                  </th>
                  <th>
                    <button
                      type="button"
                      class="sort-btn"
                      title={COLUMN_HELP.err}
                      onclick={() => toggleSort('error_count')}
                      >Err{sortMark('error_count')}</button
                    >
                  </th>
                  <th>
                    <button
                      type="button"
                      class="sort-btn"
                      title={COLUMN_HELP.inv}
                      onclick={() => toggleSort('invalid_count')}
                      >Inv{sortMark('invalid_count')}</button
                    >
                  </th>
                  <th>
                    <button
                      type="button"
                      class="sort-btn"
                      title={COLUMN_HELP.distill}
                      onclick={() => toggleSort('baseline_tokens')}
                      >Distill{sortMark('baseline_tokens')}</button
                    >
                  </th>
                  <th>
                    <button
                      type="button"
                      class="sort-btn"
                      title={COLUMN_HELP.return}
                      onclick={() => toggleSort('returned_tokens')}
                      >Return{sortMark('returned_tokens')}</button
                    >
                  </th>
                  <th>
                    <button
                      type="button"
                      class="sort-btn"
                      title={COLUMN_HELP.saved}
                      onclick={() => toggleSort('saved')}
                      >Saved{sortMark('saved')}</button
                    >
                  </th>
                  <th>
                    <button
                      type="button"
                      class="sort-btn"
                      title={COLUMN_HELP.pct}
                      onclick={() => toggleSort('reduction_pct')}
                      >%{sortMark('reduction_pct')}</button
                    >
                  </th>
                  <th>
                    <button
                      type="button"
                      class="sort-btn"
                      title={COLUMN_HELP.peak}
                      onclick={() => toggleSort('peak_saved')}
                      >Peak{sortMark('peak_saved')}</button
                    >
                  </th>
                  <th>
                    <button
                      type="button"
                      class="sort-btn"
                      title={COLUMN_HELP.last}
                      onclick={() => toggleSort('last_ts')}
                      >Last{sortMark('last_ts')}</button
                    >
                  </th>
                </tr>
              </thead>
              <tbody>
                {#each sortedTools as tool}
                  <tr class:inverted={tool.inverted} title={toolHelp(tool.name)}>
                    <td class="tool-name"
                      ><code title={toolHelp(tool.name)}>{tool.name}</code></td
                    >
                    <td title={COLUMN_HELP.calls}>{tool.calls_fmt ?? tool.calls}</td>
                    <td title={COLUMN_HELP.avg_ms}>{tool.avg_ms}ms</td>
                    <td title={COLUMN_HELP.trunc}>{tool.trunc_count || '—'}</td>
                    <td title={COLUMN_HELP.err}>{tool.error_count || '—'}</td>
                    <td title={COLUMN_HELP.inv}>{tool.invalid_count || '—'}</td>
                    <td title={COLUMN_HELP.distill}
                      >{tool.baseline_fmt ?? tool.baseline_tokens}</td
                    >
                    <td title={COLUMN_HELP.return}
                      >{tool.returned_fmt ?? tool.returned_tokens}</td
                    >
                    <td class="good" title={COLUMN_HELP.saved}
                      >{tool.saved_fmt ?? tool.saved}</td
                    >
                    <td
                      ><span class="bar" title={COLUMN_HELP.pct}
                        >{tool.bar || ''} {tool.reduction_pct}%</span
                      ></td
                    >
                    <td title={COLUMN_HELP.peak}
                      >{tool.peak_saved_fmt ?? tool.peak_saved ?? '—'}</td
                    >
                    <td title={COLUMN_HELP.last}>{tool.last_ago ?? '—'}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
          </CollapsibleSection>
        {/if}

        <footer class="panel-footer muted">
          <div class="cmd-row">
            <span title="Same savings.json as this page, rendered in a terminal TUI."
              >Terminal:</span
            >
            <code class="cmd" title={terminalCmd}>{terminalCmd}</code>
            <button
              class="icon-btn copy-btn"
              type="button"
              title="Copy terminal dashboard command"
              aria-label="Copy terminal dashboard command"
              onclick={() => copyText(terminalCmd, 'Copied terminal dashboard command')}
            >
              <svg viewBox="0 0 24 24" aria-hidden="true"
                ><rect
                  x="8"
                  y="8"
                  width="12"
                  height="12"
                  rx="2"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="2"
                /><path
                  d="M4 16V6a2 2 0 0 1 2-2h10"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="2"
                  stroke-linecap="round"
                /></svg
              >
            </button>
          </div>
          <div class="cmd-row">
            <span title="Serve this wiki UI locally (Meilisearch must be up).">Wiki serve:</span>
            <code class="cmd" title={WIKI_SERVE_CMD}>{WIKI_SERVE_CMD}</code>
            <button
              class="icon-btn copy-btn"
              type="button"
              title="Copy wiki serve command"
              aria-label="Copy wiki serve command"
              onclick={() => copyText(WIKI_SERVE_CMD, 'Copied wiki serve command')}
            >
              <svg viewBox="0 0 24 24" aria-hidden="true"
                ><rect
                  x="8"
                  y="8"
                  width="12"
                  height="12"
                  rx="2"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="2"
                /><path
                  d="M4 16V6a2 2 0 0 1 2-2h10"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="2"
                  stroke-linecap="round"
                /></svg
              >
            </button>
          </div>
          {#if copyFlash}
            <p class="copy-flash">{copyFlash}</p>
          {/if}
          <p title="Polling only runs while the Dashboard tab is active."
            >Auto-refreshes every 2s while this tab is open.</p
          >
        </footer>
      </section>
    {:else}
      <div class="health-wrap">
        <div class="subviews" role="tablist" aria-label="Health subviews">
          <button
            type="button"
            class="subview-btn"
            class:active={healthView === 'runtime'}
            onclick={() => (healthView = 'runtime')}>Runtime</button
          >
          <button
            type="button"
            class="subview-btn"
            class:active={healthView === 'knowledge'}
            onclick={() => (healthView = 'knowledge')}>Knowledge</button
          >
        </div>
        {#if healthView === 'runtime'}
          <RuntimeHealth />
        {:else}
      <section class="health panel enter">
        <header class="panel-head">
          <h2
            class="section-title"
            title="Wiki index health: Meilisearch reachability, search quality metrics, and link-garden scan."
          >
            <svg viewBox="0 0 24 24" aria-hidden="true"
              ><path
                d="M3 12h4l2-5 4 10 2-5h6"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
                stroke-linejoin="round"
              /></svg
            >
            Knowledge health
          </h2>
          <button
            class="icon-btn"
            onclick={refreshHealth}
            title="Re-fetch /api/health and /api/garden"
            aria-label="Refresh health"
          >
            <svg viewBox="0 0 24 24" aria-hidden="true"
              ><path
                d="M20 12a8 8 0 1 1-2.3-5.6"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
              /><path
                d="M20 4v5h-5"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
                stroke-linejoin="round"
              /></svg
            >
          </button>
        </header>
        <p title="ok = Meilisearch reachable and wiki index present"
          >Index / Meilisearch status: <code class:ok={health === 'ok'}>{health}</code></p
        >
        {#if healthDetail}
          <p class="muted" title="Files and chunks currently in the wiki manifest"
            ><code>{healthDetail}</code></p
          >
        {/if}

        <div class="cards">
          <article
            class="card"
            title="Wiki search quality: latency, empty-result rate, and how deep users click into hit lists."
          >
            <CollapsibleSection
              id="health-search"
              titleAttr="Local wiki search stats (not MCP tool telemetry)"
            >
              {#snippet heading()}Search telemetry{/snippet}
              {#if searchTelemetry?.available}
                <ul>
                  <li title="Total wiki searches since serve started / telemetry file began"
                    >Searches: <code>{searchTelemetry.searches ?? 0}</code></li
                  >
                  <li title="Share of searches that returned zero hits"
                    >No-result rate:
                    <code>{((searchTelemetry.no_result_rate ?? 0) * 100).toFixed(1)}%</code></li
                  >
                  <li title="Average end-to-end search latency in milliseconds"
                    >Avg latency: <code>{searchTelemetry.avg_latency_ms ?? 0} ms</code></li
                  >
                  <li title="How many search hits were opened"
                    >Clicks: <code>{searchTelemetry.clicks ?? 0}</code></li
                  >
                  <li title="Mean rank of clicked hits (1 = top result)"
                    >Avg click rank: <code>{searchTelemetry.avg_click_rank ?? '—'}</code></li
                  >
                </ul>
              {:else}
                <p class="muted">No wiki searches recorded yet — try a query.</p>
              {/if}
            </CollapsibleSection>
          </article>
          <article
            class="card"
            title="Static scan of Markdown links: broken targets, orphans with no inbound links, duplicate heading anchors."
          >
            <CollapsibleSection
              id="health-garden"
              titleAttr="Link-graph hygiene for the knowledge garden"
            >
              {#snippet heading()}Link garden{/snippet}
              <ul>
                <li title="Outbound links whose target file cannot be resolved"
                  >Broken links: <code>{garden?.broken_count ?? 0}</code></li
                >
                <li title="Pages with no inbound links from other garden pages"
                  >Orphan pages: <code>{garden?.orphan_count ?? 0}</code></li
                >
                <li title="Same heading/anchor colliding within a file"
                  >Duplicate headings: <code>{garden?.duplicate_heading_count ?? 0}</code></li
                >
              </ul>
            </CollapsibleSection>
          </article>
        </div>

        {#if garden?.broken_links?.length}
          <CollapsibleSection id="health-broken" titleAttr="Broken outbound Markdown links">
            {#snippet heading()}Broken links{/snippet}
            <ul class="plain">
              {#each garden.broken_links.slice(0, 40) as link}
                <li>
                  <button class="linkish" onclick={() => openPath(link.source)}
                    >{link.source}</button
                  >
                  → <code>{link.target}</code>
                </li>
              {/each}
            </ul>
          </CollapsibleSection>
        {/if}

        {#if garden?.orphans?.length}
          <CollapsibleSection id="health-orphans" titleAttr="Pages with no inbound garden links">
            {#snippet heading()}Orphan pages{/snippet}
            <ul class="plain">
              {#each garden.orphans.slice(0, 40) as orphan}
                <li>
                  <button class="linkish" onclick={() => openPath(orphan.path)}>{orphan.path}</button>
                </li>
              {/each}
            </ul>
          </CollapsibleSection>
        {/if}

        {#if garden?.duplicate_headings?.length}
          <CollapsibleSection
            id="health-dupes"
            titleAttr="Heading/anchor collisions within a file"
          >
            {#snippet heading()}Duplicate headings{/snippet}
            <ul class="plain">
              {#each garden.duplicate_headings.slice(0, 40) as dup}
                <li>
                  <button
                    class="linkish"
                    onclick={() => openPath(dup.path, undefined, dup.anchor)}
                    >{dup.path}</button
                  >
                  · {dup.heading}
                </li>
              {/each}
            </ul>
          </CollapsibleSection>
        {/if}

        <footer class="panel-footer muted">
          MCP token telemetry lives on the Dashboard tab (or the terminal
          <code>wordkeep dashboard</code>).
          {#if retainSearchQueries}
            · raw <code>q</code> retention on
          {:else}
            · opt-in raw <code>q</code>: <code>WIKI_RETAIN_QUERIES=1</code> ·
            <code>wiki.retain_search_queries</code>
          {/if}
        </footer>
      </section>
        {/if}
      </div>
    {/if}
    </div>
  </main>
</div>

<style>
  .shell {
    --sidebar-width: 20rem;
    --sidebar-transition: 0.36s var(--ease-snap);
    --header-h: 3.35rem;
    display: grid;
    grid-template-columns: var(--sidebar-width) 6px 1fr;
    grid-template-rows: auto 1fr;
    height: 100dvh;
    min-height: 100vh;
    overflow: hidden;
    transition: grid-template-columns var(--sidebar-transition);
  }
  /* Keep main on column 3 (the 1fr track). Column 1 is width 0 when collapsed —
     assigning main there made the whole reader disappear. */
  .shell.sidebar-collapsed {
    grid-template-columns: 0 0 1fr;
  }
  .subviews {
    display: flex;
    gap: 0.4rem;
    margin: 0 0 0.75rem;
  }
  .subview-btn {
    border: 1px solid var(--border, #333);
    background: transparent;
    color: inherit;
    border-radius: 999px;
    padding: 0.2rem 0.75rem;
    cursor: pointer;
  }
  .subview-btn.active {
    border-color: var(--accent, #6bcf8e);
  }
  .shell.resizing {
    cursor: col-resize;
  }
  .shell.resizing aside,
  .shell.resizing .sidebar-expand-tab {
    transition: none;
  }
  .sidebar-expand-tab {
    position: fixed;
    left: 0;
    top: calc(var(--header-h) + 1.25rem);
    z-index: 26;
    display: grid;
    place-items: center;
    width: 1.65rem;
    height: 2.75rem;
    margin: 0;
    padding: 0;
    border: 1px solid var(--border);
    border-left: 0;
    border-radius: 0 10px 10px 0;
    background: var(--bg-elevated);
    color: var(--accent-strong);
    cursor: pointer;
    box-shadow: 4px 0 18px rgba(0, 0, 0, 0.35);
    animation: sidebar-tab-in 0.38s var(--ease-snap) both;
    transition:
      width 0.2s var(--ease-snap),
      background-color 0.16s var(--ease-snap),
      border-color 0.16s var(--ease-snap),
      box-shadow 0.2s var(--ease-snap),
      color 0.16s var(--ease-snap);
  }
  .sidebar-expand-tab svg {
    width: 1rem;
    height: 1rem;
    transition: transform 0.2s var(--ease-snap);
  }
  .sidebar-expand-tab:hover {
    width: 1.9rem;
    background: var(--bg-hover);
    border-color: rgba(177, 83, 184, 0.55);
    box-shadow: 6px 0 22px rgba(0, 0, 0, 0.42), 0 0 0 1px var(--accent-soft);
  }
  .sidebar-expand-tab:hover svg {
    transform: translateX(2px);
  }
  @keyframes sidebar-tab-in {
    from {
      opacity: 0;
      transform: translateX(-110%);
    }
    to {
      opacity: 1;
      transform: translateX(0);
    }
  }
  .sidebar-chrome {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
    margin: -0.15rem 0 0.65rem;
    padding-bottom: 0.55rem;
    border-bottom: 1px solid rgba(177, 83, 184, 0.16);
  }
  .sidebar-chrome-label {
    font-size: 0.72rem;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .sidebar-chrome-actions {
    display: flex;
    gap: 0.3rem;
  }
  .sidebar-chrome-btn {
    width: 1.85rem;
    height: 1.85rem;
  }
  .sidebar-chrome-btn svg {
    width: 1rem;
    height: 1rem;
  }
  .sidebar-resizer {
    grid-row: 2;
    grid-column: 2;
    width: 100%;
    height: 100%;
    margin: 0;
    padding: 0;
    border: 0;
    cursor: col-resize;
    background: transparent;
    position: relative;
    z-index: 2;
    opacity: 1;
    transition: opacity 0.24s var(--ease-snap);
  }
  .shell.sidebar-collapsed .sidebar-resizer {
    opacity: 0;
    pointer-events: none;
  }
  .sidebar-resizer::after {
    content: '';
    position: absolute;
    inset: 0 1px;
    background: var(--border);
    transition: background 0.15s ease, box-shadow 0.15s ease;
  }
  .sidebar-resizer:hover::after,
  .shell.resizing .sidebar-resizer::after,
  .sidebar-resizer:focus-visible::after {
    background: var(--accent);
    box-shadow: 0 0 0 1px var(--accent-soft);
  }
  .top {
    grid-column: 1 / -1;
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 1rem;
    padding: 0.75rem 1rem;
    border-bottom: 1px solid var(--border);
    background: rgba(26, 20, 28, 0.92);
    backdrop-filter: blur(8px);
    position: sticky;
    top: 0;
    /* Above sidebar combobox menus (z-index 5–9) so sticky header is never covered. */
    z-index: 30;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    flex-wrap: wrap;
  }
  .panel-footer {
    margin-top: 1.5rem;
    padding-top: 0.75rem;
    border-top: 1px solid var(--border);
    font-size: 0.78rem;
    line-height: 1.45;
  }
  .panel-footer code {
    font-size: 0.92em;
  }
  .save-chip code {
    border: 0;
    background: transparent;
    padding: 0;
    color: inherit;
    box-shadow: none;
  }
  .save-chip code:hover {
    transform: none;
    background: transparent;
    box-shadow: none;
  }
  .status code {
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    vertical-align: bottom;
  }
  nav {
    display: flex;
    gap: 0.3rem;
  }
  .icon-btn,
  .hits button,
  .backlinks button,
  .toc button {
    border: 1px solid var(--border);
    background: var(--bg-elevated);
    border-radius: 8px;
    padding: 0.45rem 0.8rem;
    cursor: pointer;
  }
  .icon-btn {
    display: inline-grid;
    place-items: center;
    width: 2.15rem;
    height: 2.15rem;
    padding: 0;
    flex: 0 0 auto;
  }
  .icon-btn svg {
    width: 1.1rem;
    height: 1.1rem;
  }
  .icon-btn:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
  .icon-btn.active,
  .icon-btn.primary,
  .chip.active {
    border-color: var(--accent);
    color: var(--accent-strong);
    background: var(--accent-soft);
  }
  .chip.warn {
    border-color: var(--warn);
    color: var(--warn);
    background: rgba(212, 162, 74, 0.12);
  }
  .icon-btn:hover:not(:disabled),
  .hits button:hover,
  .backlinks button:hover,
  .toc button:hover {
    border-color: rgba(177, 83, 184, 0.45);
    background: var(--bg-hover);
    transform: translateY(-1px);
    box-shadow: 0 4px 14px rgba(0, 0, 0, 0.28);
  }
  .icon-btn:active:not(:disabled),
  .hits button:active,
  .backlinks button:active,
  .toc button:active {
    transform: translateY(0.15em);
    box-shadow: none;
  }
  .section-title {
    display: inline-flex;
    align-items: center;
    gap: 0.45rem;
    margin: 0.35rem 0 0.55rem;
    font-size: 0.95rem;
    color: var(--text);
  }
  .section-title svg {
    width: 1rem;
    height: 1rem;
    color: var(--accent-strong);
    flex: 0 0 auto;
  }
  .section-title span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .panel.enter {
    animation: panel-enter 0.28s var(--ease-snap);
  }
  @keyframes panel-enter {
    from {
      opacity: 0;
      transform: translateY(6px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }
  .title-block {
    min-width: 0;
  }
  .title-block .section-title {
    margin: 0;
    font-size: 1.05rem;
    max-width: min(52rem, 70vw);
  }
  .brand strong {
    font-size: 0.95rem;
  }
  .save-chip {
    margin-top: 0.35rem;
  }
  .search input:focus,
  .filters input:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 2px var(--accent-soft);
  }
  aside {
    grid-column: 1;
    grid-row: 2;
    position: fixed;
    left: 0;
    top: var(--header-h);
    bottom: 0;
    border-right: 0;
    background: var(--bg-elevated);
    padding: 1rem;
    overflow: auto;
    min-width: 0;
    width: var(--sidebar-width);
    /* Keep filter/combo z-index local so they cannot paint over the navbar. */
    isolation: isolate;
    z-index: 5;
    transition:
      transform var(--sidebar-transition),
      opacity 0.28s var(--ease-snap),
      box-shadow var(--sidebar-transition),
      border-radius var(--sidebar-transition);
    will-change: transform, opacity;
  }
  .shell.sidebar-collapsed aside {
    opacity: 0;
    transform: translateX(calc(-1 * var(--sidebar-width) - 12px));
    pointer-events: none;
  }
  .search {
    display: flex;
    gap: 0.4rem;
  }
  .search input,
  .filters input {
    width: 100%;
    border: 1px solid var(--border);
    background: var(--bg);
    border-radius: 8px;
    padding: 0.55rem 0.7rem;
    min-width: 0;
  }
  .filters {
    display: grid;
    grid-template-columns: 1fr;
    gap: 0.4rem;
    margin-top: 0.5rem;
  }
  .filter-field {
    display: grid;
    grid-template-columns: 1.6rem 1fr;
    align-items: center;
    gap: 0.35rem;
    min-width: 0;
  }
  .filter-icon {
    display: inline-grid;
    place-items: center;
    width: 1.6rem;
    height: 1.6rem;
    color: var(--accent-strong);
    opacity: 0.9;
  }
  .filter-icon svg {
    width: 1rem;
    height: 1rem;
  }
  .root-combobox {
    position: relative;
    z-index: 6;
  }
  .combo-field {
    position: relative;
    z-index: 5;
  }
  .combo-field:focus-within {
    z-index: 9;
  }
  .recent-combo {
    margin: 0.55rem 0 0.75rem;
  }
  .recent-combo .section-title {
    margin: 0 0 0.4rem;
  }
  .recent-combo .combo {
    z-index: 7;
  }
  .combo-item {
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }
  .combo-swatch {
    width: 0.7rem;
    height: 0.7rem;
    border-radius: 999px;
    flex: 0 0 auto;
    border: 1px solid rgba(255, 255, 255, 0.25);
  }
  .combo {
    position: relative;
    min-width: 0;
  }
  .combo > input {
    width: 100%;
    border: 1px solid var(--border);
    background: var(--bg);
    border-radius: 8px;
    padding: 0.55rem 0.7rem;
    min-width: 0;
  }
  .combo > input:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 2px var(--accent-soft);
  }
  .combo-menu {
    position: absolute;
    left: 0;
    right: 0;
    top: calc(100% + 0.25rem);
    z-index: 8;
    margin: 0;
    padding: 0.25rem;
    list-style: none;
    max-height: 14rem;
    overflow: auto;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg-elevated);
    box-shadow: 0 10px 28px rgba(0, 0, 0, 0.35);
  }
  .combo-item {
    width: 100%;
    text-align: left;
    border: 0;
    background: transparent;
    color: var(--text);
    border-radius: 6px;
    padding: 0.35rem 0.5rem;
    cursor: pointer;
  }
  .combo-item:hover,
  .combo-item.active {
    background: var(--accent-soft);
    color: var(--accent-strong);
  }
  .combo-item code {
    border: 0;
    background: transparent;
    padding: 0;
    box-shadow: none;
  }
  .combo-empty {
    padding: 0.45rem 0.55rem;
    font-size: 0.85rem;
  }
  .tag-editor {
    margin-top: 0.55rem;
    display: flex;
    flex-direction: column;
    gap: 0.45rem;
  }
  .tag-row {
    display: flex;
    flex-wrap: wrap;
    gap: 0.35rem;
    align-items: center;
  }
  .tag-chip {
    gap: 0.25rem;
    padding-right: 0.25rem;
    border-color: color-mix(in srgb, var(--tag-color, var(--accent)) 55%, var(--border));
    background: color-mix(in srgb, var(--tag-color, var(--accent)) 18%, var(--bg-elevated));
  }
  .tag-swatch {
    position: relative;
    width: 1rem;
    height: 1rem;
    border-radius: 999px;
    overflow: hidden;
    border: 1px solid color-mix(in srgb, var(--tag-color, var(--accent)) 70%, #fff);
    background: var(--tag-color, var(--accent));
    flex: 0 0 auto;
    cursor: pointer;
  }
  .tag-swatch input[type='color'] {
    position: absolute;
    inset: -0.35rem;
    width: 2rem;
    height: 2rem;
    border: 0;
    padding: 0;
    cursor: pointer;
    opacity: 0;
  }
  .tag-hex {
    width: 5.6rem;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--text);
    padding: 0.08rem 0.28rem;
    font-family: "IBM Plex Mono", ui-monospace, monospace;
    font-size: 0.78rem;
  }
  .tag-x svg {
    width: 0.75rem;
    height: 0.75rem;
    display: block;
  }
  .tag-name,
  .tag-x {
    border: 0;
    background: transparent;
    color: inherit;
    padding: 0.05rem 0.2rem;
    cursor: pointer;
    font: inherit;
  }
  .tag-name:hover {
    color: var(--accent-strong);
  }
  .tag-x {
    border-radius: 4px;
    line-height: 1;
    opacity: 0.7;
  }
  .tag-x:hover {
    background: var(--accent-soft);
    opacity: 1;
  }
  .tag-rename {
    width: 7rem;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--text);
    padding: 0.1rem 0.3rem;
    font: inherit;
  }
  .tag-add {
    display: flex;
    gap: 0.35rem;
    max-width: 18rem;
  }
  .tag-add input {
    flex: 1;
    min-width: 0;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg);
    padding: 0.35rem 0.55rem;
  }
  .tag-empty {
    font-size: 0.85rem;
  }
  .tag-error {
    margin: 0;
    font-size: 0.8rem;
  }
  .status {
    min-height: 1.4rem;
    font-size: 0.9rem;
  }
  .tree {
    display: flex;
    flex-direction: column;
    gap: 0.05rem;
    max-height: 36vh;
    overflow-x: hidden;
    overflow-y: auto;
    margin-bottom: 0.65rem;
    min-width: 0;
  }
  .tree-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.35rem;
    margin: 0.2rem 0 0.25rem;
  }
  .tree-head .section-title {
    margin: 0;
    min-width: 0;
    font-size: 0.85rem;
    gap: 0.35rem;
  }
  .tree-actions {
    display: flex;
    gap: 0.2rem;
    flex: 0 0 auto;
  }
  .tree-action {
    width: 1.45rem;
    height: 1.45rem;
  }
  .tree-action svg {
    width: 0.85rem;
    height: 0.85rem;
  }
  .tree-item,
  .tree-dir {
    display: flex;
    align-items: center;
    gap: 0.28rem;
    text-align: left;
    font-size: 0.78rem;
    line-height: 1.25;
    width: 100%;
    min-width: 0;
    max-width: 100%;
    box-sizing: border-box;
    padding: 0.18rem 0.4rem;
    border-radius: 5px;
  }
  .tree-indent {
    flex: 0 0 calc(var(--depth, 0) * 0.55rem);
    width: calc(var(--depth, 0) * 0.55rem);
    height: 1px;
  }
  .tree-label {
    min-width: 0;
    flex: 1 1 auto;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tree-item .leaf,
  .tree-dir .leaf,
  .tree-dir .chevron {
    width: 0.78rem;
    height: 0.78rem;
    flex: 0 0 auto;
    opacity: 0.75;
    color: var(--accent-strong);
  }
  .tree-dir .chevron {
    transition: transform 0.15s var(--ease-snap);
    transform: rotate(90deg);
  }
  .tree-dir.collapsed .chevron {
    transform: rotate(0deg);
  }
  .tree-item {
    border: 1px solid var(--border);
    border-left: 2px solid transparent;
    background: var(--bg-elevated);
    cursor: pointer;
  }
  .tree-item.active {
    border-left-color: var(--accent);
    background: var(--accent-soft);
    color: var(--accent-strong);
  }
  .tree-item:hover {
    background: var(--bg-hover);
    border-left-color: var(--accent);
  }
  .tree-item:active {
    transform: none;
    box-shadow: none;
  }
  .tree-dir {
    color: var(--muted);
    border: 0;
    background: transparent;
    cursor: pointer;
  }
  .tree-dir:hover {
    color: var(--text);
    background: var(--bg-hover);
  }
  .hits button {
    transition:
      border-color 0.16s var(--ease-snap),
      background-color 0.16s var(--ease-snap),
      transform 0.12s var(--ease-snap),
      box-shadow 0.16s var(--ease-snap);
  }
  .card {
    transition:
      border-color 0.18s var(--ease-snap),
      transform 0.18s var(--ease-snap),
      box-shadow 0.18s var(--ease-snap);
  }
  .card:hover {
    border-color: rgba(177, 83, 184, 0.45);
    transform: translateY(-2px);
    box-shadow: 0 8px 22px rgba(0, 0, 0, 0.28);
  }
  .toc {
    transition: border-color 0.18s var(--ease-snap), box-shadow 0.18s var(--ease-snap);
  }
  .toc:hover {
    border-color: rgba(177, 83, 184, 0.4);
    box-shadow: 0 0 0 1px var(--accent-soft);
  }
  main {
    grid-column: 3;
    grid-row: 2;
    padding: 0;
    overflow: auto;
    overscroll-behavior: contain;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    zoom: var(--content-zoom, 1);
  }
  .main-body {
    flex: 1 1 auto;
    padding: 1.25rem clamp(1rem, 3vw, 2.5rem);
    min-width: 0;
  }
  .main-chrome {
    position: sticky;
    top: 0;
    z-index: 4;
    flex: 0 0 auto;
    border-bottom: 1px solid var(--border);
    background: rgba(18, 13, 20, 0.94);
    backdrop-filter: blur(8px);
  }
  .editor-tabs-stack {
    display: flex;
    flex-direction: column;
    gap: 0;
  }
  .editor-tabs {
    display: flex;
    flex-wrap: nowrap;
    gap: 0.15rem;
    overflow-x: auto;
    padding: 0.3rem 0.45rem 0.35rem;
    min-height: 2.15rem;
  }
  .editor-tabs.pinned-row {
    border-bottom: 1px solid rgba(177, 83, 184, 0.22);
    background: rgba(177, 83, 184, 0.08);
    padding-top: 0.35rem;
  }
  .editor-tab {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    max-width: 14rem;
    padding: 0.28rem 0.28rem 0.28rem 0.45rem;
    border: 1px solid transparent;
    border-radius: 8px 8px 0 0;
    background: transparent;
    color: var(--muted);
    cursor: pointer;
    flex: 0 0 auto;
    user-select: none;
  }
  .editor-tab:hover {
    color: var(--text);
    background: var(--bg-hover);
    border-color: var(--border);
  }
  .editor-tab.active {
    color: var(--text);
    background: var(--bg-elevated);
    border-color: var(--border);
    border-bottom-color: var(--accent);
    box-shadow: inset 0 -2px 0 var(--accent);
  }
  .editor-tab.pinned .tab-label {
    font-weight: 600;
  }
  .tab-file {
    width: 0.85rem;
    height: 0.85rem;
    flex: 0 0 auto;
    opacity: 0.75;
    color: var(--accent-strong);
  }
  .tab-label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 0.82rem;
  }
  .tab-action {
    display: inline-grid;
    place-items: center;
    width: 1.2rem;
    height: 1.2rem;
    padding: 0;
    border: 0;
    border-radius: 4px;
    background: transparent;
    color: inherit;
    opacity: 0.45;
    cursor: pointer;
    flex: 0 0 auto;
  }
  .editor-tab:hover .tab-action,
  .editor-tab.active .tab-action,
  .tab-action.active-pin {
    opacity: 0.9;
  }
  .tab-action:hover {
    background: var(--accent-soft);
    color: var(--accent-strong);
    opacity: 1;
  }
  .tab-action svg {
    width: 0.75rem;
    height: 0.75rem;
  }
  .editor {
    width: 100%;
    min-height: min(70vh, 48rem);
    resize: vertical;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: #0a070b;
    color: var(--text);
    padding: 1rem 1.1rem;
    line-height: 1.55;
    font-family: "IBM Plex Mono", ui-monospace, monospace;
    font-size: 0.92rem;
  }
  .editor:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 2px var(--accent-soft);
  }
  .panel-head {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 0.75rem;
    margin-bottom: 1rem;
  }
  .reader-head {
    padding-right: 5.5rem;
  }
  .page-actions-float {
    position: fixed;
    top: var(--page-actions-top, calc(var(--header-h) + 1.25rem));
    right: clamp(1rem, 3vw, 2.5rem);
    z-index: 18;
    opacity: 0.7;
    pointer-events: none;
    transition: opacity 0.2s var(--ease-snap);
  }
  .page-actions-float .actions {
    pointer-events: auto;
  }
  .page-actions-float:hover {
    opacity: 1;
  }
  .actions {
    display: flex;
    gap: 0.3rem;
    flex-shrink: 0;
  }
  .hits {
    list-style: none;
    padding: 0;
    margin: 0;
    display: grid;
    gap: 0.65rem;
  }
  .hits button {
    width: 100%;
    text-align: left;
    display: grid;
    gap: 0.25rem;
  }
  .hits p {
    margin: 0.2rem 0 0;
    color: var(--muted);
    font-size: 0.92rem;
  }
  .reader article {
    line-height: 1.7;
    max-width: 52rem;
  }
  .toc {
    margin: 0 0 1.25rem;
    padding: 0.75rem 1rem;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-elevated);
    max-width: 52rem;
  }
  .toc ul,
  .plain {
    list-style: none;
    padding: 0;
    margin: 0.35rem 0 0;
  }
  .toc button,
  .linkish {
    border: 0;
    background: transparent;
    color: var(--accent-strong);
    padding: 0.15rem 0;
    text-align: left;
  }
  .backlinks ul {
    list-style: none;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
  }
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(16rem, 1fr));
    gap: 0.75rem;
    margin: 1rem 0;
  }
  .card {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 0.85rem 1rem;
    background: var(--bg-elevated);
  }
  .card ul {
    margin: 0.4rem 0 0;
    padding-left: 1.1rem;
  }
  .dash-charts {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 0.75rem;
    margin: 0 0 0.75rem;
  }
  .dash-charts .chart-card {
    cursor: help;
  }
  .dash-charts .chart-card h3 {
    margin: 0 0 0.55rem;
    font-size: 0.72rem;
    font-weight: 600;
    letter-spacing: 0.02em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .dash-overview {
    grid-template-columns: repeat(auto-fit, minmax(5.5rem, 1fr));
    gap: 0.4rem;
    margin: 0.5rem 0 0.75rem;
  }
  .dash-overview .card {
    padding: 0.35rem 0.45rem;
  }
  .dash-overview .card h3 {
    margin: 0;
    font-size: 0.68rem;
    font-weight: 600;
    letter-spacing: 0.02em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .dash-overview .metric {
    margin: 0.15rem 0 0;
    font-size: 0.82rem;
    line-height: 1.2;
  }
  .dash-overview .metric code {
    font-size: 0.95em;
  }
  .metric.good code,
  .dash-table .good {
    color: #7dce9a;
  }
  .table-wrap {
    overflow: auto;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    margin-bottom: 1rem;
  }
  .dash-table {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.82rem;
    white-space: nowrap;
  }
  .dash-table th,
  .dash-table td {
    padding: 0.4rem 0.55rem;
    border-bottom: 1px solid var(--border);
    text-align: right;
  }
  .dash-table th:first-child,
  .dash-table td:first-child {
    text-align: left;
    position: sticky;
    left: 0;
    background: var(--bg-elevated);
  }
  .dash-table thead th {
    color: var(--muted);
    font-weight: 600;
    background: rgba(0, 0, 0, 0.18);
    padding: 0;
  }
  .dash-table thead th.static {
    padding: 0.4rem 0.55rem;
  }
  .sort-btn {
    width: 100%;
    border: 0;
    background: transparent;
    color: inherit;
    font: inherit;
    font-weight: 600;
    padding: 0.4rem 0.55rem;
    cursor: pointer;
    text-align: inherit;
  }
  .sort-btn:hover {
    color: var(--accent-strong);
    background: var(--accent-soft);
  }
  .dash-table tr.inverted td {
    color: #e87b7b;
  }
  .dash-table .bar {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    letter-spacing: 0.02em;
  }
  .dash-table .tool-name {
    text-align: left;
  }
  .dash-table .tool-name code {
    font-size: 0.9em;
  }
  .dash-split {
    display: grid;
    grid-template-columns: 1.4fr 1fr;
    gap: 1rem;
  }
  .dash-table tr.signal-ok .signal-value {
    color: #7dce9a;
  }
  .dash-table tr.signal-warn .signal-value,
  .dash-table tr.signal-danger .signal-value {
    color: #e87b7b;
  }
  .cmd-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.45rem;
    margin: 0.35rem 0;
  }
  .cmd-row .cmd {
    flex: 1 1 16rem;
    min-width: 0;
    overflow-x: auto;
    white-space: nowrap;
  }
  .copy-btn {
    width: 1.75rem;
    height: 1.75rem;
  }
  .copy-btn svg {
    width: 0.95rem;
    height: 0.95rem;
  }
  .copy-flash {
    margin: 0.25rem 0 0;
    color: #7dce9a;
    font-size: 0.78rem;
  }
  @media (max-width: 860px) {
    .shell,
    .shell.sidebar-collapsed {
      grid-template-columns: 1fr;
      grid-template-rows: auto auto 0 1fr;
    }
    .shell.sidebar-collapsed {
      grid-template-rows: auto 0 0 1fr;
    }
    .dash-split {
      grid-template-columns: 1fr;
    }
    .dash-charts {
      grid-template-columns: 1fr;
    }
    aside {
      grid-column: 1;
      grid-row: 2;
      border-bottom: 1px solid var(--border);
      max-height: 40vh;
      position: relative;
      top: auto;
      bottom: auto;
      width: auto;
      transform: none;
    }
    .shell.sidebar-collapsed aside {
      display: none;
      max-height: 0;
      border: 0;
      padding: 0;
      margin: 0;
      overflow: hidden;
      transform: none;
      pointer-events: none;
    }
    .sidebar-resizer {
      display: none;
    }
    main {
      grid-column: 1;
      grid-row: 4;
      min-height: 0;
    }
    .shell.sidebar-collapsed main {
      grid-row: 4;
    }
    .sidebar-expand-tab {
      top: calc(var(--header-h, 3.25rem) + 0.35rem);
      left: 0.35rem;
    }
    .sidebar-chrome-label {
      cursor: pointer;
      user-select: none;
      -webkit-user-select: none;
    }
  }
</style>
