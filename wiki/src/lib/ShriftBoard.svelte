<script lang="ts">
  import { onMount } from 'svelte';
  import {
    createShriftIdea,
    fetchGarden,
    fetchShrift,
    postShrift,
    type ShriftIdea,
    type ShriftPayload,
  } from './api';
  import { matchesQuery } from './boardFilter';
  import CollapsibleSection from './CollapsibleSection.svelte';
  import { sortRows, toggleColumn, type ColumnSort } from './columnSort';
  import BarChart from './charts/BarChart.svelte';
  import DonutChart from './charts/DonutChart.svelte';
  import type { BarDatum, DonutDatum } from './charts/utils';
  import { comboNav } from './combo';
  import { normalizeTag, normalizeTags } from './frontmatter';
  import {
    defaultTagColor,
    loadTagColors,
    resolveTagColor,
    saveTagColors,
    type TagColorMap,
  } from './tagColors';

  type TagOption = { value: string; label: string; tip: string };

  let {
    query = '',
    onOpenPath,
  }: {
    query?: string;
    onOpenPath?: (path: string) => void;
  } = $props();

  let board = $state<ShriftPayload | null>(null);
  let error = $state('');
  let busy = $state('');
  let statusFilter = $state('all');
  let freshnessFilter = $state('all');
  let semantic = $state(false);
  let sort = $state<ColumnSort>({ key: 'touched', dir: 'desc' });
  let newTitle = $state('');
  let newBody = $state('');
  let archiveSlug = $state('');
  let archiveReason = $state('');
  let tagColors = $state<TagColorMap>({});
  let gardenTags = $state<string[]>([]);
  let tagDraftBySlug = $state<Record<string, string>>({});
  let tagBusySlug = $state('');
  let tagMenuSlug = $state('');
  let tagHighlight = $state(0);

  const filtering = $derived(query.trim().length > 0);

  const STATUS_OPTS = ['all', 'seed', 'sprouted', 'planned', 'implemented', 'archived'] as const;
  const FRESH_OPTS = ['all', 'fresh', 'due', 'stale', 'dormant'] as const;

  const STATUS_COLORS: Record<string, string> = {
    seed: 'var(--accent-strong)',
    sprouted: 'var(--ok)',
    planned: '#5b8def',
    implemented: '#7ec8e3',
    archived: 'var(--muted)',
  };
  const FRESH_COLORS: Record<string, string> = {
    fresh: 'var(--ok)',
    due: '#5b8def',
    stale: 'var(--warn)',
    dormant: 'var(--danger)',
  };

  const ideas = $derived.by(() => {
    let rows = board?.ideas ?? [];
    if (statusFilter !== 'all') {
      rows = rows.filter((i) => i.status === statusFilter);
    }
    if (freshnessFilter !== 'all') {
      rows = rows.filter((i) => i.freshness === freshnessFilter);
    }
    if (filtering) {
      rows = rows.filter((i) =>
        matchesQuery(query, [
          i.slug,
          i.title,
          i.status,
          i.freshness,
          i.body_preview,
          ...(i.tags ?? []),
          ...(i.links ?? []),
        ]),
      );
    }
    return sortRows(rows, sort, (row, key) => {
      const idea = row as ShriftIdea;
      switch (key) {
        case 'slug':
          return idea.slug;
        case 'title':
          return idea.title;
        case 'status':
          return idea.status;
        case 'tags':
          return (idea.tags ?? []).join(',');
        case 'freshness':
          return idea.freshness ?? '';
        case 'age_days':
          return idea.age_days ?? 0;
        case 'touched':
          return idea.touched ?? '';
        case 'bookmarked':
          return idea.bookmarked ? 1 : 0;
        default:
          return '';
      }
    });
  });

  const metrics = $derived(board?.metrics ?? {});
  const tags = $derived(
    Object.entries(metrics.tag_histogram ?? {}).sort((a, b) => b[1] - a[1]),
  );

  /** Union of garden tags + shrift histogram for typeahead suggestions. */
  const knownTags = $derived.by(() => {
    const set = new Map<string, string>();
    for (const t of gardenTags) {
      const n = normalizeTag(t);
      if (n) set.set(n.toLowerCase(), n);
    }
    for (const [t] of tags) {
      const n = normalizeTag(t);
      if (n) set.set(n.toLowerCase(), n);
    }
    return [...set.values()].sort((a, b) => a.localeCompare(b));
  });

  const statusSlices = $derived.by((): DonutDatum[] => {
    const by = metrics.by_status ?? {};
    return ['seed', 'sprouted', 'planned', 'implemented', 'archived']
      .filter((k) => (by[k] ?? 0) > 0)
      .map((k) => ({
        label: k,
        value: by[k] ?? 0,
        color: STATUS_COLORS[k] ?? 'var(--muted)',
        title: `${by[k] ?? 0} idea(s) in ${k}`,
      }));
  });

  const freshSlices = $derived.by((): DonutDatum[] => {
    const by = metrics.by_freshness ?? {};
    return ['fresh', 'due', 'stale', 'dormant']
      .filter((k) => (by[k] ?? 0) > 0)
      .map((k) => ({
        label: k,
        value: by[k] ?? 0,
        color: FRESH_COLORS[k] ?? 'var(--muted)',
        title: `${by[k] ?? 0} idea(s) marked ${k}`,
      }));
  });

  const tagBars = $derived.by((): BarDatum[] =>
    tags.slice(0, 8).map(([label, value]) => ({
      label,
      value,
      title: `${value} idea(s) tagged ${label}`,
    })),
  );

  const ageBars = $derived.by((): BarDatum[] => {
    const buckets = [
      { label: '0d', min: 0, max: 0 },
      { label: '1–6d', min: 1, max: 6 },
      { label: '7–13d', min: 7, max: 13 },
      { label: '14–29d', min: 14, max: 29 },
      { label: '30d+', min: 30, max: 1e9 },
    ];
    return buckets.map((b) => {
      const value = (board?.ideas ?? []).filter((i) => {
        const age = i.age_days ?? 0;
        return age >= b.min && age <= b.max;
      }).length;
      return {
        label: b.label,
        value,
        title: `${value} idea(s) last touched in ${b.label}`,
      };
    });
  });

  function tagColor(tag: string): string {
    return resolveTagColor(tagColors, tag);
  }

  function tagOptionsFor(idea: ShriftIdea): TagOption[] {
    const draftRaw = tagDraftBySlug[idea.slug] ?? '';
    const draft = normalizeTag(draftRaw);
    const q = draftRaw.trim().toLowerCase();
    const have = new Set((idea.tags ?? []).map((t) => t.toLowerCase()));
    const matched = knownTags
      .filter((t) => !have.has(t.toLowerCase()))
      .filter((t) => !q || t.toLowerCase().includes(q))
      .slice(0, 40)
      .map((t) => ({
        value: t,
        label: t,
        tip: `Assign “${t}”`,
      }));
    if (
      draft &&
      !have.has(draft.toLowerCase()) &&
      !matched.some((o) => o.value.toLowerCase() === draft.toLowerCase())
    ) {
      return [{ value: draft, label: draft, tip: `Create tag “${draft}”` }, ...matched];
    }
    return matched;
  }

  function openTagMenu(slug: string) {
    tagMenuSlug = slug;
    tagHighlight = 0;
  }

  function closeTagMenu(slug?: string) {
    if (slug && tagMenuSlug !== slug) return;
    tagMenuSlug = '';
    tagHighlight = 0;
  }

  function pickTag(idea: ShriftIdea, tag: string) {
    closeTagMenu(idea.slug);
    tagDraftBySlug = { ...tagDraftBySlug, [idea.slug]: '' };
    void addTag(idea, tag);
  }

  function onTagKeydown(idea: ShriftIdea, event: KeyboardEvent) {
    const options = tagOptionsFor(idea);
    const nav = comboNav(event, {
      open: tagMenuSlug === idea.slug,
      highlight: tagHighlight,
      count: options.length,
    });
    if (!nav) {
      if (event.key === 'Enter') {
        event.preventDefault();
        const draft = tagDraftBySlug[idea.slug] ?? '';
        if (draft.trim()) void addTag(idea, draft);
      }
      return;
    }
    tagMenuSlug = nav.open ? idea.slug : '';
    tagHighlight = nav.highlight;
    if (nav.choose != null) {
      const option = options[nav.choose];
      if (option) pickTag(idea, option.value);
    }
    if (nav.close) {
      tagDraftBySlug = { ...tagDraftBySlug, [idea.slug]: '' };
    }
  }

  async function refresh() {
    try {
      board = await fetchShrift();
      error = board?.error || '';
    } catch (e) {
      error = String(e);
      board = null;
    }
  }

  async function refreshGardenTags() {
    try {
      const garden = await fetchGarden();
      gardenTags = Object.keys(garden.tags ?? {}).sort((a, b) => a.localeCompare(b));
    } catch {
      gardenTags = [];
    }
  }

  onMount(() => {
    tagColors = loadTagColors();
    void refresh();
    void refreshGardenTags();
    const id = setInterval(() => void refresh(), 8000);
    return () => clearInterval(id);
  });

  async function act(label: string, fn: () => Promise<unknown>) {
    busy = label;
    error = '';
    try {
      await fn();
      await refresh();
    } catch (e) {
      error = String(e);
    } finally {
      busy = '';
    }
  }

  async function persistTags(idea: ShriftIdea, next: string[]) {
    const tags = normalizeTags(next);
    tagBusySlug = idea.slug;
    error = '';
    try {
      await postShrift('tags', { slug: idea.slug, tags });
      // Ensure newly created tags get a stable color entry in the shared store.
      let colorsChanged = false;
      const nextColors = { ...tagColors };
      for (const tag of tags) {
        if (!nextColors[tag]) {
          nextColors[tag] = defaultTagColor(tag);
          colorsChanged = true;
        }
      }
      if (colorsChanged) {
        tagColors = nextColors;
        saveTagColors(tagColors);
      }
      await refresh();
      await refreshGardenTags();
    } catch (e) {
      error = String(e);
    } finally {
      tagBusySlug = '';
    }
  }

  async function addTag(idea: ShriftIdea, raw: string) {
    const tag = normalizeTag(raw);
    if (!tag) return;
    const current = idea.tags ?? [];
    if (current.some((t) => t.toLowerCase() === tag.toLowerCase())) {
      tagDraftBySlug = { ...tagDraftBySlug, [idea.slug]: '' };
      return;
    }
    tagDraftBySlug = { ...tagDraftBySlug, [idea.slug]: '' };
    await persistTags(idea, [...current, tag]);
  }

  async function removeTag(idea: ShriftIdea, tag: string) {
    await persistTags(
      idea,
      (idea.tags ?? []).filter((t) => t.toLowerCase() !== tag.toLowerCase()),
    );
  }

  function openIdea(idea: ShriftIdea) {
    onOpenPath?.(idea.path);
  }

  function setDraft(slug: string, value: string) {
    tagDraftBySlug = { ...tagDraftBySlug, [slug]: value };
  }
</script>

<section class="shrift-board">
  {#if error}
    <p class="err">{error}</p>
  {/if}

  <div class="toolbar">
    <div class="pill-group" role="group" aria-label="Status filter">
      <span class="pill-label">Status</span>
      {#each STATUS_OPTS as opt}
        <button
          type="button"
          class="pill"
          class:active={statusFilter === opt}
          onclick={() => (statusFilter = opt)}>{opt}</button
        >
      {/each}
    </div>
    <div class="pill-group" role="group" aria-label="Freshness filter">
      <span class="pill-label">Freshness</span>
      {#each FRESH_OPTS as opt}
        <button
          type="button"
          class="pill"
          class:active={freshnessFilter === opt}
          class:warn={opt === 'stale' || opt === 'dormant'}
          onclick={() => (freshnessFilter = opt)}>{opt}</button
        >
      {/each}
    </div>
    <label class="check">
      <input type="checkbox" bind:checked={semantic} />
      Hybrid rank hint
    </label>
    <button
      type="button"
      class="icon-btn"
      title={busy || 'Refresh board'}
      aria-label="Refresh"
      disabled={!!busy}
      onclick={() => void refresh()}
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
  </div>

  <CollapsibleSection
    id="shrift-results"
    titleAttr="Filtered idea table — collapse to focus on charts or capture"
    defaultOpen={true}
  >
    {#snippet heading()}
      Results
      <span class="muted tiny count">{ideas.length}/{metrics.total ?? 0}</span>
    {/snippet}
    <div
      class="table-wrap results"
      class:menu-open={!!tagMenuSlug}
      aria-label="Shrift search results"
    >
      <table class="dash-table">
        <thead>
          <tr>
            {#each [
              ['bookmarked', '★', true],
              ['status', 'Status', false],
              ['tags', 'Tags', false],
              ['freshness', 'Fresh', false],
              ['slug', 'Slug', false],
              ['title', 'Title', false],
              ['age_days', 'Age', true],
              ['touched', 'Touched', false],
            ] as [key, label, numeric]}
              <th>
                <button
                  type="button"
                  class="th"
                  onclick={() => (sort = toggleColumn(sort, key, numeric))}
                >
                  {label}
                  {#if sort.key === key}{sort.dir === 'asc' ? ' ↑' : ' ↓'}{/if}
                </button>
              </th>
            {/each}
            <th>Actions</th>
          </tr>
        </thead>
        <tbody>
          {#if ideas.length === 0}
            <tr>
              <td colspan="9" class="muted">
                {filtering || statusFilter !== 'all' || freshnessFilter !== 'all'
                  ? 'No shrifts match this filter.'
                  : 'No shrifts yet — capture one below.'}
              </td>
            </tr>
          {:else}
            {#each ideas as idea}
              <tr
                class:dormant={idea.freshness === 'dormant'}
                class:stale={idea.freshness === 'stale'}
                class:tag-row-open={tagMenuSlug === idea.slug}
              >
                <td class="star-cell">{idea.bookmarked ? '★' : ''}</td>
                <td><span class="badge status-{idea.status}">{idea.status}</span></td>
                <td class="tags-cell">
                  <div class="tag-row">
                    {#each idea.tags ?? [] as tag}
                      <span
                        class="chip tag-chip"
                        style={`--tag-color:${tagColor(tag)}`}
                        title={`tag:${tag}`}
                      >
                        <span class="tag-swatch" style={`background:${tagColor(tag)}`}></span>
                        <span class="tag-name">{tag}</span>
                        <button
                          type="button"
                          class="tag-x"
                          title={`Remove tag “${tag}”`}
                          aria-label={`Remove tag ${tag}`}
                          disabled={tagBusySlug === idea.slug || !!busy}
                          onclick={() => void removeTag(idea, tag)}>×</button
                        >
                      </span>
                    {/each}
                    <div
                      class="filter-field combo-field tag-add"
                      title="Assign a frontmatter tag from the garden store or create one"
                    >
                      <span
                        class="filter-icon"
                        title="Tag — type to search"
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
                          value={tagDraftBySlug[idea.slug] ?? ''}
                          type="text"
                          role="combobox"
                          aria-expanded={tagMenuSlug === idea.slug}
                          aria-controls={`shrift-tags-${idea.slug}`}
                          aria-autocomplete="list"
                          placeholder="Tag — type to search…"
                          aria-label={`Add tag to ${idea.slug}`}
                          title="Tag assign — typeahead over garden + shrift tags"
                          disabled={tagBusySlug === idea.slug || !!busy}
                          onfocus={() => openTagMenu(idea.slug)}
                          oninput={(e) => {
                            setDraft(idea.slug, (e.currentTarget as HTMLInputElement).value);
                            openTagMenu(idea.slug);
                          }}
                          onkeydown={(e) => onTagKeydown(idea, e)}
                          onblur={() => {
                            setTimeout(() => closeTagMenu(idea.slug), 120);
                          }}
                        />
                        {#if tagMenuSlug === idea.slug}
                          {@const options = tagOptionsFor(idea)}
                          <ul
                            id={`shrift-tags-${idea.slug}`}
                            class="combo-menu"
                            role="listbox"
                            title="All tags"
                          >
                            <li class="combo-heading" aria-hidden="true">All tags</li>
                            {#each options as option, index}
                              <li role="option" aria-selected={index === tagHighlight}>
                                <button
                                  type="button"
                                  class="combo-item"
                                  class:active={index === tagHighlight}
                                  title={option.tip}
                                  onmousedown={(e) => e.preventDefault()}
                                  onmouseenter={() => (tagHighlight = index)}
                                  onclick={() => pickTag(idea, option.value)}
                                >
                                  <span
                                    class="combo-swatch"
                                    style={`background:${tagColor(option.value)}`}
                                    aria-hidden="true"
                                  ></span>
                                  <code>{option.label}</code>
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
                </td>
                <td><span class="badge fresh-{idea.freshness}">{idea.freshness}</span></td>
                <td>
                  <button type="button" class="linkish" onclick={() => openIdea(idea)}
                    >{idea.slug}</button
                  >
                </td>
                <td>{idea.title}</td>
                <td>{idea.age_days ?? 0}d</td>
                <td>{idea.touched}</td>
                <td class="actions">
                  <button
                    type="button"
                    class="icon-btn"
                    title="Touch (refresh freshness)"
                    aria-label="Touch"
                    disabled={!!busy}
                    onclick={() => void act('touch', () => postShrift('touch', { slug: idea.slug }))}
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
                  <button
                    type="button"
                    class="icon-btn"
                    title="Promote to next status"
                    aria-label="Promote"
                    disabled={!!busy || idea.status === 'archived'}
                    onclick={() =>
                      void act('promote', () => postShrift('promote', { slug: idea.slug }))}
                  >
                    <svg viewBox="0 0 24 24" aria-hidden="true"
                      ><path
                        d="M12 19V5M5 12l7-7 7 7"
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
                    class="icon-btn"
                    class:active={idea.bookmarked}
                    title={idea.bookmarked ? 'Remove bookmark' : 'Bookmark'}
                    aria-label={idea.bookmarked ? 'Unstar' : 'Star'}
                    disabled={!!busy}
                    onclick={() =>
                      void act('bookmark', () => postShrift('bookmark', { slug: idea.slug }))}
                  >
                    <svg viewBox="0 0 24 24" aria-hidden="true"
                      ><path
                        d="M12 3.5l2.6 5.3 5.9.9-4.2 4.1 1 5.8L12 16.9 6.7 19.6l1-5.8L3.5 9.7l5.9-.9z"
                        fill={idea.bookmarked ? 'currentColor' : 'none'}
                        stroke="currentColor"
                        stroke-width="1.6"
                        stroke-linejoin="round"
                      /></svg
                    >
                  </button>
                  {#if idea.status !== 'archived'}
                    <button
                      type="button"
                      class="icon-btn"
                      title="Archive"
                      aria-label="Archive"
                      disabled={!!busy}
                      onclick={() => {
                        archiveSlug = idea.slug;
                        archiveReason = idea.freshness === 'dormant' ? '' : 'archived from wiki';
                      }}
                    >
                      <svg viewBox="0 0 24 24" aria-hidden="true"
                        ><path
                          d="M4 7h16v12H4zM3 4h18v3H3zM10 11h4"
                          fill="none"
                          stroke="currentColor"
                          stroke-width="2"
                          stroke-linejoin="round"
                        /></svg
                      >
                    </button>
                  {/if}
                </td>
              </tr>
            {/each}
          {/if}
        </tbody>
      </table>
    </div>
    {#if filtering}
      <p class="muted tiny">Showing {ideas.length} of {metrics.total ?? 0} shrifts for “{query.trim()}”.</p>
    {/if}
  </CollapsibleSection>

  <CollapsibleSection id="shrift-overview" titleAttr="Idea counts, freshness, and tag mix" defaultOpen={true}>
    {#snippet heading()}Overview{/snippet}
    <div class="cards">
      <article class="card">
        <h3>Total</h3>
        <p class="metric"><code>{metrics.total ?? 0}</code></p>
      </article>
      <article class="card">
        <h3>Bookmarked</h3>
        <p class="metric"><code>{metrics.bookmarked ?? 0}</code></p>
      </article>
      <article class="card">
        <h3>Fresh</h3>
        <p class="metric"><code>{metrics.by_freshness?.fresh ?? 0}</code></p>
      </article>
      <article class="card">
        <h3>Due</h3>
        <p class="metric"><code>{metrics.by_freshness?.due ?? 0}</code></p>
      </article>
      <article class="card">
        <h3>Stale</h3>
        <p class="metric warn"><code>{metrics.by_freshness?.stale ?? 0}</code></p>
      </article>
      <article class="card">
        <h3>Dormant</h3>
        <p class="metric warn"><code>{metrics.by_freshness?.dormant ?? 0}</code></p>
      </article>
    </div>

    <div class="charts">
      <article class="card chart-card" title="Lifecycle mix across all ideas">
        <h3>By status</h3>
        <DonutChart
          data={statusSlices}
          empty="No ideas yet."
          ariaLabel="Ideas by status"
          chartTitle="Lifecycle mix"
          centerLabel="ideas"
          centerTitle="Total ideas"
          valueLabel="ideas"
        />
      </article>
      <article class="card chart-card" title="Freshness mix from touched date">
        <h3>By freshness</h3>
        <DonutChart
          data={freshSlices}
          empty="No freshness data."
          ariaLabel="Ideas by freshness"
          chartTitle="TTL freshness mix"
          centerLabel="ideas"
          centerTitle="Total ideas"
          valueLabel="ideas"
        />
      </article>
      <article class="card chart-card" title="Most-used idea tags">
        <h3>Tags</h3>
        <BarChart
          data={tagBars}
          empty="No tags yet."
          ariaLabel="Ideas by tag"
          chartTitle="Tag frequency"
        />
      </article>
      <article class="card chart-card" title="Age since last touch">
        <h3>Age buckets</h3>
        <BarChart
          data={ageBars}
          empty="No age data."
          ariaLabel="Ideas by age since touch"
          chartTitle="Days since touched"
        />
      </article>
    </div>
  </CollapsibleSection>

  <CollapsibleSection id="shrift-capture" titleAttr="Capture a new seed idea" defaultOpen={false}>
    {#snippet heading()}Capture{/snippet}
    <form
      class="capture"
      onsubmit={(e) => {
        e.preventDefault();
        if (!newTitle.trim()) return;
        void act('capture', () =>
          createShriftIdea({
            title: newTitle.trim(),
            body: newBody.trim(),
            source: 'wiki',
          }).then(() => {
            newTitle = '';
            newBody = '';
          }),
        );
      }}
    >
      <input bind:value={newTitle} placeholder="Idea title" required />
      <textarea bind:value={newBody} placeholder="Optional body / future-impl notes" rows="3"
      ></textarea>
      <button type="submit" class="icon-btn primary" disabled={!!busy} title="Capture seed" aria-label="Capture seed">
        <svg viewBox="0 0 24 24" aria-hidden="true"
          ><path
            d="M12 5v14M5 12h14"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
          /></svg
        >
        <span>Capture</span>
      </button>
    </form>
  </CollapsibleSection>

  {#if (board?.stalest?.length ?? 0) > 0}
    <CollapsibleSection id="shrift-stalest" titleAttr="Stalest seed/sprouted ideas for handoff" defaultOpen={false}>
      {#snippet heading()}Stalest (handoff surface){/snippet}
      <ul class="compact">
        {#each board?.stalest ?? [] as idea}
          <li>
            <button type="button" class="linkish" onclick={() => openIdea(idea)}>
              <span class="badge status-{idea.status}">{idea.status}</span>
              <span class="badge fresh-{idea.freshness}">{idea.freshness}</span>
              {idea.slug} — {idea.title}
            </button>
          </li>
        {/each}
      </ul>
    </CollapsibleSection>
  {/if}

  {#if (board?.dormant?.length ?? 0) > 0}
    <CollapsibleSection id="shrift-dormant" titleAttr="Dormant ideas need confirm-gated archive" defaultOpen={true}>
      {#snippet heading()}Dormant (confirm-gated archive){/snippet}
      <ul class="compact">
        {#each board?.dormant ?? [] as idea}
          <li class="dormant-row">
            <button type="button" class="linkish" onclick={() => openIdea(idea)}>
              {idea.slug} — {idea.age_days}d · {idea.title}
            </button>
            <button
              type="button"
              class="icon-btn"
              title="Archive…"
              aria-label="Archive"
              onclick={() => {
                archiveSlug = idea.slug;
                archiveReason = '';
              }}
            >
              <svg viewBox="0 0 24 24" aria-hidden="true"
                ><path
                  d="M4 7h16v12H4zM3 4h18v3H3zM10 11h4"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="2"
                  stroke-linejoin="round"
                /></svg
              >
            </button>
          </li>
        {/each}
      </ul>
    </CollapsibleSection>
  {/if}

  {#if archiveSlug}
    <form
      class="archive-form card"
      onsubmit={(e) => {
        e.preventDefault();
        const slug = archiveSlug;
        const reason = archiveReason.trim();
        if (!reason) return;
        void act('archive', () =>
          postShrift('status', {
            slug,
            status: 'archived',
            archive_reason: reason,
          }).then(() => {
            archiveSlug = '';
            archiveReason = '';
          }),
        );
      }}
    >
      <h3>Archive <code>{archiveSlug}</code></h3>
      <input bind:value={archiveReason} placeholder="Reason (required)" required />
      <div class="archive-actions">
        <button type="submit" class="icon-btn primary" title="Confirm archive">Confirm</button>
        <button type="button" class="icon-btn" onclick={() => (archiveSlug = '')}>Cancel</button>
      </div>
    </form>
  {/if}
</section>

<style>
  .shrift-board {
    display: flex;
    flex-direction: column;
    gap: 1rem;
    min-width: 0;
    max-width: 100%;
    width: 100%;
  }
  .shrift-board :global(.collapsible),
  .shrift-board :global(.collapsible-body) {
    min-width: 0;
    max-width: 100%;
  }
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(8.5rem, 1fr));
    gap: 0.6rem;
    margin-bottom: 0.75rem;
  }
  .charts {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 0.75rem;
  }
  .chart-card h3 {
    margin-bottom: 0.55rem;
  }
  .card {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-elevated);
    padding: 0.65rem 0.75rem;
  }
  .card h3 {
    margin: 0 0 0.35rem;
    font-size: 0.78rem;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .metric {
    margin: 0;
    font-size: 1.35rem;
    font-variant-numeric: tabular-nums;
  }
  .metric code {
    font-family: inherit;
  }
  .metric.warn {
    color: var(--warn);
  }
  .toolbar {
    display: flex;
    flex-wrap: wrap;
    gap: 0.55rem 0.75rem;
    align-items: center;
  }
  .pill-group {
    display: inline-flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.25rem;
  }
  .pill-label {
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--muted);
    margin-right: 0.2rem;
  }
  .pill {
    border: 1px solid var(--border);
    background: var(--bg-elevated);
    color: var(--muted);
    border-radius: 999px;
    padding: 0.18rem 0.55rem;
    font-size: 0.78rem;
    cursor: pointer;
  }
  .pill:hover {
    border-color: color-mix(in srgb, var(--accent) 45%, var(--border));
    color: var(--text);
  }
  .pill.active {
    color: var(--accent-strong);
    border-color: color-mix(in srgb, var(--accent) 55%, var(--border));
    background: color-mix(in srgb, var(--accent) 14%, transparent);
  }
  .pill.active.warn {
    color: var(--warn);
    border-color: color-mix(in srgb, var(--warn) 55%, var(--border));
    background: rgba(212, 162, 74, 0.12);
  }
  .check {
    display: inline-flex;
    gap: 0.35rem;
    align-items: center;
    font-size: 0.8rem;
    color: var(--muted);
  }
  .icon-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 0.35rem;
    width: 2rem;
    height: 2rem;
    padding: 0;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg-elevated);
    color: var(--muted);
    cursor: pointer;
  }
  .icon-btn svg {
    width: 1rem;
    height: 1rem;
  }
  .icon-btn:hover:not(:disabled) {
    color: var(--text);
    border-color: color-mix(in srgb, var(--accent) 45%, var(--border));
  }
  .icon-btn:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
  .icon-btn.active {
    color: var(--warn);
    border-color: color-mix(in srgb, var(--warn) 45%, var(--border));
  }
  .icon-btn.primary {
    width: auto;
    padding: 0 0.7rem;
    color: var(--accent-strong);
    border-color: color-mix(in srgb, var(--accent) 55%, var(--border));
  }
  .table-wrap {
    overflow: auto;
  }
  .table-wrap.menu-open {
    overflow: visible;
  }
  .results {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-elevated);
  }
  .count {
    margin-left: 0.35rem;
    font-weight: 500;
  }
  .dash-table {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.85rem;
  }
  .dash-table th,
  .dash-table td {
    text-align: left;
    padding: 0.4rem 0.5rem;
    border-bottom: 1px solid var(--border);
    vertical-align: middle;
  }
  .dash-table th {
    color: var(--muted);
    font-weight: 600;
    font-size: 0.75rem;
    text-transform: uppercase;
    letter-spacing: 0.03em;
  }
  button.th {
    background: none;
    border: 0;
    color: inherit;
    font: inherit;
    cursor: pointer;
    padding: 0;
  }
  .star-cell {
    color: var(--warn);
    width: 1.5rem;
  }
  tr.tag-row-open {
    position: relative;
    z-index: 6;
  }
  .tags-cell {
    min-width: 14rem;
    max-width: 24rem;
    overflow: visible;
  }
  .tag-row {
    display: flex;
    flex-wrap: wrap;
    gap: 0.3rem;
    align-items: center;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
    border-radius: 999px;
    border: 1px solid var(--border);
    padding: 0.05rem 0.25rem 0.05rem 0.35rem;
    font-size: 0.75rem;
    background: var(--bg);
  }
  .tag-chip {
    border-color: color-mix(in srgb, var(--tag-color, var(--accent)) 55%, var(--border));
    background: color-mix(in srgb, var(--tag-color, var(--accent)) 18%, var(--bg-elevated));
  }
  .tag-swatch {
    width: 0.65rem;
    height: 0.65rem;
    border-radius: 999px;
    flex: 0 0 auto;
  }
  .tag-name {
    max-width: 7rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tag-x {
    border: 0;
    background: transparent;
    color: var(--muted);
    cursor: pointer;
    padding: 0 0.2rem;
    font-size: 0.9rem;
    line-height: 1;
  }
  .tag-x:hover:not(:disabled) {
    color: var(--text);
  }
  .tag-add {
    display: grid;
    grid-template-columns: 1.35rem minmax(7.5rem, 11rem);
    align-items: center;
    gap: 0.3rem;
    min-width: 0;
  }
  .filter-field {
    min-width: 0;
  }
  .filter-icon {
    display: inline-grid;
    place-items: center;
    width: 1.35rem;
    height: 1.35rem;
    color: var(--accent-strong);
    opacity: 0.9;
  }
  .filter-icon svg {
    width: 0.95rem;
    height: 0.95rem;
  }
  .combo-field {
    position: relative;
    z-index: 5;
  }
  .combo-field:focus-within {
    z-index: 9;
  }
  .combo {
    position: relative;
    min-width: 0;
  }
  .combo > input {
    width: 100%;
    border: 1px solid var(--border);
    background: var(--bg);
    color: inherit;
    border-radius: 8px;
    padding: 0.35rem 0.55rem;
    min-width: 0;
    font: inherit;
    font-size: 0.78rem;
  }
  .combo > input:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 2px var(--accent-soft);
  }
  .combo > input:disabled {
    opacity: 0.5;
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
  .combo-heading {
    padding: 0.35rem 0.5rem 0.25rem;
    font-size: 0.72rem;
    font-weight: 600;
    letter-spacing: 0.02em;
    color: var(--accent-strong);
  }
  .combo-item {
    display: flex;
    align-items: center;
    gap: 0.4rem;
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
    font-size: 0.8rem;
    color: inherit;
  }
  .combo-swatch {
    width: 0.7rem;
    height: 0.7rem;
    border-radius: 999px;
    flex: 0 0 auto;
    border: 1px solid rgba(255, 255, 255, 0.25);
  }
  .combo-empty {
    padding: 0.45rem 0.55rem;
    font-size: 0.85rem;
  }
  .badge {
    display: inline-block;
    border-radius: 999px;
    padding: 0.05rem 0.45rem;
    border: 1px solid var(--border);
    font-size: 0.75rem;
  }
  .badge.status-seed {
    color: var(--accent-strong);
    border-color: color-mix(in srgb, var(--accent) 55%, var(--border));
  }
  .badge.status-sprouted {
    color: var(--ok);
    border-color: color-mix(in srgb, var(--ok) 55%, var(--border));
  }
  .badge.status-planned {
    color: #5b8def;
    border-color: color-mix(in srgb, #5b8def 55%, var(--border));
  }
  .badge.status-implemented {
    color: #7ec8e3;
    border-color: color-mix(in srgb, #7ec8e3 55%, var(--border));
  }
  .badge.status-archived {
    color: var(--muted);
  }
  .badge.fresh-fresh {
    color: var(--ok);
    border-color: color-mix(in srgb, var(--ok) 55%, var(--border));
  }
  .badge.fresh-due {
    color: #5b8def;
    border-color: color-mix(in srgb, #5b8def 55%, var(--border));
  }
  .badge.fresh-stale,
  .badge.fresh-dormant {
    color: var(--warn);
    border-color: color-mix(in srgb, var(--warn) 55%, var(--border));
    background: rgba(212, 162, 74, 0.12);
  }
  .actions {
    display: flex;
    flex-wrap: nowrap;
    gap: 0.25rem;
  }
  .actions .icon-btn {
    width: 1.85rem;
    height: 1.85rem;
  }
  .linkish {
    background: none;
    border: 0;
    color: #8ec7ff;
    cursor: pointer;
    padding: 0;
    font: inherit;
    text-align: left;
    display: inline-flex;
    flex-wrap: wrap;
    gap: 0.3rem;
    align-items: center;
  }
  .muted {
    opacity: 0.65;
  }
  .tiny {
    font-size: 0.78rem;
    margin: 0;
  }
  .err {
    color: var(--danger, #ff8e8e);
    margin: 0;
  }
  tr.dormant,
  tr.stale {
    background: rgba(212, 162, 74, 0.06);
  }
  .compact {
    margin: 0;
    padding-left: 1.1rem;
  }
  .dormant-row {
    display: flex;
    gap: 0.5rem;
    align-items: center;
    flex-wrap: wrap;
  }
  .capture,
  .archive-form {
    display: flex;
    flex-direction: column;
    gap: 0.45rem;
  }
  .capture input,
  .capture textarea,
  .archive-form input {
    background: var(--bg);
    border: 1px solid var(--border);
    color: inherit;
    border-radius: 8px;
    padding: 0.45rem 0.6rem;
    font: inherit;
  }
  .archive-actions {
    display: flex;
    gap: 0.4rem;
  }
  @media (max-width: 800px) {
    .charts {
      grid-template-columns: 1fr;
    }
  }
</style>
