#!/usr/bin/env bun
/**
 * Capture README screenshots for wordkeep wiki + terminal dashboard.
 * Requires wordkeep-wiki serving on :8787.
 */
import { chromium } from 'playwright';
import { mkdir } from 'node:fs/promises';
import path from 'node:path';

const ROOT = path.resolve(import.meta.dir, '../..');
const DOCS = path.join(ROOT, 'docs');
const BASE = process.env.WIKI_URL || 'http://127.0.0.1:8787';

await mkdir(DOCS, { recursive: true });

const browser = await chromium.launch({ headless: true });
const page = await browser.newPage({
  viewport: { width: 1440, height: 900 },
  deviceScaleFactor: 2,
  colorScheme: 'dark',
});

async function shot(name: string) {
  const dest = path.join(DOCS, name);
  await page.screenshot({ path: dest, fullPage: false });
  console.log('wrote', dest);
}

async function waitReady() {
  await page.waitForSelector('.shell', { timeout: 15000 });
  await page.waitForTimeout(400);
}

// --- Wiki: searchable filters / recent ---
await page.goto(`${BASE}/?tab=search`, { waitUntil: 'networkidle' });
await waitReady();
await page.evaluate(() => localStorage.setItem('wordkeep-wiki-sidebar-width', '360'));
await page.reload({ waitUntil: 'networkidle' });
await waitReady();

await page.fill('input[type="search"][aria-label="Search"]', 'flecs');
await page.waitForTimeout(500);
await page.locator('input[aria-label="Path root"]').click();
await page.waitForTimeout(350);
await shot('wiki-search.png');
await page.keyboard.press('Escape');
await page.waitForTimeout(150);

// --- Wiki: reader + tags + editor tabs ---
await page.goto(`${BASE}/?tab=search`, { waitUntil: 'networkidle' });
await waitReady();

// Prefer a known indexed path from Recent / Tree
const combat = page.locator('aside .tree-item', { hasText: 'betwixt-combat' }).first();
const anyLeaf = page.locator('aside button.tree-item').first();
if (await combat.count()) await combat.click();
else if (await anyLeaf.count()) await anyLeaf.click();
await waitReady();
await page.waitForTimeout(500);

const camera = page.locator('aside .tree-item', { hasText: 'betwixt-camera' }).first();
if (await camera.count()) {
  await camera.click();
  await page.waitForTimeout(400);
}
if (await combat.count()) {
  await combat.click();
  await page.waitForTimeout(400);
}

const pinBtn = page.locator('.editor-tab .tab-action.pin').first();
if (await pinBtn.count()) {
  await pinBtn.click();
  await page.waitForTimeout(200);
}

const tagInput = page.locator('.tag-add input');
const pageOk = !(await page.locator('text=Failed to load page').count());
if (pageOk && (await tagInput.count())) {
  await tagInput.fill('combat');
  await page.locator('.tag-add button[type="submit"]').click();
  await page.waitForTimeout(900);
}
await shot('wiki-reader.png');

// --- Wiki: GUI dashboard ---
await page.goto(`${BASE}/?tab=dashboard`, { waitUntil: 'networkidle' });
await waitReady();
await page.waitForSelector('.dashboard', { timeout: 10000 });
await page.waitForTimeout(800);
await shot('wiki-dashboard.png');

// --- Wiki: health ---
await page.goto(`${BASE}/?tab=health`, { waitUntil: 'networkidle' });
await waitReady();
await page.waitForTimeout(500);
await shot('wiki-health.png');

// --- Terminal-style dashboard render from live API ---
const dash = await page.evaluate(async (base) => {
  const res = await fetch(`${base}/api/dashboard`);
  return res.json();
}, BASE);

const tools = (dash.tools || []).slice(0, 18);
const overview = dash.overview || {};
const activity = (dash.activity || []).slice(0, 10);
const signals = dash.health?.signals || [];

function esc(s: string) {
  return String(s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
}

const toolRows = tools
  .map((t: any) => {
    const bar = esc(t.bar || '');
    const name = esc((t.name || '').slice(0, 14).padEnd(14));
    return `<tr>
      <td class="name">${name}</td>
      <td>${esc(t.calls_fmt ?? t.calls)}</td>
      <td>${t.avg_ms}ms</td>
      <td>${t.trunc_count || '—'}</td>
      <td>${t.error_count || '—'}</td>
      <td>${esc(t.baseline_fmt ?? t.baseline_tokens)}</td>
      <td>${esc(t.returned_fmt ?? t.returned_tokens)}</td>
      <td class="good">${esc(t.saved_fmt ?? t.saved)}</td>
      <td class="bar">${bar} ${t.reduction_pct}%</td>
      <td>${esc(t.peak_saved_fmt ?? t.peak_saved ?? '—')}</td>
      <td>${esc(t.last_ago ?? '—')}</td>
    </tr>`;
  })
  .join('');

const activityRows = activity
  .map(
    (e: any) =>
      `<div class="act"><span class="dim">${esc(e.ago)}</span> <span class="cyan">${esc(e.tool)}</span> <span class="dim">${e.elapsed_ms}ms</span> <span class="good">+${esc(e.baseline_fmt ?? e.baseline)} → ${esc(e.returned_fmt ?? e.returned)}</span></div>`,
  )
  .join('');

const signalRows = signals
  .map(
    (s: any) =>
      `<div><span class="dim">${esc(s.label)}</span> <strong class="${esc(s.kind)}">${esc(String(s.value ?? '—'))}</strong> ${s.detail ? `<span class="dim">(${esc(s.detail)})</span>` : ''}</div>`,
  )
  .join('');

const html = `<!doctype html>
<html>
<head>
<meta charset="utf-8" />
<style>
  html, body { margin: 0; background: #0c0c0c; color: #d6d6d6;
    font: 13px/1.35 ui-monospace, "Cascadia Mono", "IBM Plex Mono", Menlo, monospace; }
  .frame { padding: 18px 20px 22px; }
  .panel { border: 1px solid #3a3a3a; border-radius: 2px; margin-bottom: 12px; }
  .title { color: #9aa0a6; padding: 4px 10px; border-bottom: 1px solid #2a2a2a; }
  .body { padding: 10px 12px; }
  .overview { display: flex; gap: 1.5rem; flex-wrap: wrap; }
  .overview b { color: #e8e8e8; }
  .good { color: #7dce9a; }
  .cyan { color: #6ec6ff; }
  .dim { color: #7a7a7a; }
  .ok { color: #7dce9a; }
  .warn, .danger { color: #e87b7b; }
  .info { color: #e8e8e8; }
  table { width: 100%; border-collapse: collapse; font-size: 12px; }
  th { text-align: left; color: #8b8b8b; font-weight: 600; padding: 2px 8px 6px 0; }
  td { padding: 2px 10px 2px 0; white-space: nowrap; }
  td.name { color: #f0f0f0; }
  td.bar { letter-spacing: 0.02em; color: #9ad4a8; }
  .split { display: grid; grid-template-columns: 1.35fr 1fr; gap: 12px; }
  .act { margin: 2px 0; }
  h1 { font-size: 14px; margin: 0 0 12px; color: #bdbdbd; font-weight: 600; }
</style>
</head>
<body>
<div class="frame">
  <h1>wordkeep dashboard · cargo run --features dashboard -- dashboard</h1>
  <div class="panel">
    <div class="title"> overview </div>
    <div class="body overview">
      <div>calls <b>${esc(overview.calls_fmt ?? overview.calls ?? 0)}</b></div>
      <div>distilled <b>${esc(overview.baseline_fmt ?? 0)}</b></div>
      <div>returned <b>${esc(overview.returned_fmt ?? 0)}</b></div>
      <div>saved <b class="good">${esc(overview.saved_fmt ?? 0)} · ${overview.reduction_pct ?? 0}%</b></div>
      <div>tracking <b>${esc(overview.since_label ?? '—')}</b></div>
    </div>
  </div>
  <div class="panel">
    <div class="title"> tools (by saved) </div>
    <div class="body">
      <table>
        <thead><tr>
          <th>tool</th><th>calls</th><th>avg</th><th>trunc</th><th>err</th>
          <th>distill</th><th>return</th><th>saved</th><th>reduction</th><th>peak</th><th>last</th>
        </tr></thead>
        <tbody>${toolRows}</tbody>
      </table>
    </div>
  </div>
  <div class="split">
    <div class="panel">
      <div class="title"> recent activity </div>
      <div class="body">${activityRows || '<div class="dim">no recent events</div>'}</div>
    </div>
    <div class="panel">
      <div class="title"> health </div>
      <div class="body">${signalRows}</div>
    </div>
  </div>
</div>
</body>
</html>`;

await page.setContent(html, { waitUntil: 'load' });
await page.setViewportSize({ width: 1400, height: 860 });
await page.waitForTimeout(200);
await shot('dashboard.png');

await browser.close();
console.log('done');
