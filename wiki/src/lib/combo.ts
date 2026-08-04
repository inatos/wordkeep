/** Shared keyboard navigation for searchable combobox menus. */

export type ComboNavResult = {
  open: boolean;
  highlight: number;
  /** Index to commit when Enter is pressed. */
  choose?: number;
  close?: boolean;
};

export function comboNav(
  event: KeyboardEvent,
  opts: { open: boolean; highlight: number; count: number },
): ComboNavResult | null {
  const last = Math.max(0, opts.count - 1);
  if (event.key === 'ArrowDown') {
    event.preventDefault();
    if (!opts.open) return { open: true, highlight: 0 };
    return { open: true, highlight: Math.min(opts.highlight + 1, last) };
  }
  if (event.key === 'ArrowUp') {
    event.preventDefault();
    if (!opts.open) return { open: true, highlight: last };
    return { open: true, highlight: Math.max(opts.highlight - 1, 0) };
  }
  if (event.key === 'Enter' && opts.open && opts.count > 0) {
    event.preventDefault();
    const index = Math.min(Math.max(opts.highlight, 0), last);
    return { open: false, highlight: 0, choose: index };
  }
  if (event.key === 'Escape') {
    event.preventDefault();
    return { open: false, highlight: 0, close: true };
  }
  return null;
}
