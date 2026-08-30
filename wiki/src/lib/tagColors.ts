/** Client-side tag → color map (safeStorage). Does not alter Markdown frontmatter. */

import { safeStorage } from './safeStorage';

const STORAGE_KEY = 'wordkeep-wiki-tag-colors';

export type TagColorMap = Record<string, string>;

export function normalizeHex(raw: string): string | null {
  let value = raw.trim();
  if (!value) return null;
  if (!value.startsWith('#')) value = `#${value}`;
  if (/^#[0-9a-fA-F]{3}$/.test(value)) {
    const r = value[1]!;
    const g = value[2]!;
    const b = value[3]!;
    value = `#${r}${r}${g}${g}${b}${b}`;
  }
  if (!/^#[0-9a-fA-F]{6}$/.test(value)) return null;
  return value.toLowerCase();
}

/** Stable fallback when no explicit color is stored. */
export function defaultTagColor(tag: string): string {
  let hash = 2166136261;
  for (let i = 0; i < tag.length; i++) {
    hash ^= tag.charCodeAt(i);
    hash = Math.imul(hash, 16777619);
  }
  const hue = hash % 360;
  return hslToHex(((hue % 360) + 360) % 360, 52, 58);
}

function hslToHex(h: number, s: number, l: number): string {
  const sat = s / 100;
  const light = l / 100;
  const a = sat * Math.min(light, 1 - light);
  const f = (n: number) => {
    const k = (n + h / 30) % 12;
    const color = light - a * Math.max(Math.min(k - 3, 9 - k, 1), -1);
    return Math.round(255 * color)
      .toString(16)
      .padStart(2, '0');
  };
  return `#${f(0)}${f(8)}${f(4)}`;
}

export function loadTagColors(): TagColorMap {
  try {
    const raw = safeStorage.getItem(STORAGE_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as unknown;
    if (!parsed || typeof parsed !== 'object') return {};
    const out: TagColorMap = {};
    for (const [key, value] of Object.entries(parsed as Record<string, unknown>)) {
      if (typeof value !== 'string') continue;
      const hex = normalizeHex(value);
      if (hex) out[key] = hex;
    }
    return out;
  } catch {
    return {};
  }
}

export function saveTagColors(map: TagColorMap): void {
  safeStorage.setItem(STORAGE_KEY, JSON.stringify(map));
}

export function resolveTagColor(map: TagColorMap, tag: string): string {
  return map[tag] || defaultTagColor(tag);
}
