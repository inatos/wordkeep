const STORAGE_KEY = 'wordkeep-wiki-section-collapsed';

function readCollapsed(): Set<string> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return new Set();
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return new Set();
    return new Set(parsed.filter((k): k is string => typeof k === 'string'));
  } catch {
    return new Set();
  }
}

function writeCollapsed(ids: Set<string>): void {
  localStorage.setItem(STORAGE_KEY, JSON.stringify([...ids]));
}

/** Open unless the id is in the persisted collapsed set (first visit uses defaultOpen). */
export function isSectionOpen(id: string, defaultOpen = true): boolean {
  const collapsed = readCollapsed();
  if (collapsed.has(id)) return false;
  return defaultOpen;
}

export function setSectionOpen(id: string, open: boolean): void {
  const collapsed = readCollapsed();
  if (open) collapsed.delete(id);
  else collapsed.add(id);
  writeCollapsed(collapsed);
}
