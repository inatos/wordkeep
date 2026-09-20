export type SortDir = 'asc' | 'desc';

export type ColumnSort = { key: string; dir: SortDir };

/** First press on a text column sorts ascending; numbers start descending. Press again flips. */
export function toggleColumn(current: ColumnSort, key: string, numeric = false): ColumnSort {
  if (current.key === key) {
    return { key, dir: current.dir === 'asc' ? 'desc' : 'asc' };
  }
  return { key, dir: numeric ? 'desc' : 'asc' };
}

export function sortRows<T>(
  rows: readonly T[],
  sort: ColumnSort,
  valueOf: (row: T, key: string) => string | number | null | undefined,
): T[] {
  if (!sort.key) return [...rows];
  const sign = sort.dir === 'asc' ? 1 : -1;
  return [...rows].sort((a, b) => compare(valueOf(a, sort.key), valueOf(b, sort.key), sign));
}

function compare(
  a: string | number | null | undefined,
  b: string | number | null | undefined,
  sign: number,
): number {
  const aNil = a == null || a === '';
  const bNil = b == null || b === '';
  if (aNil && bNil) return 0;
  if (aNil) return 1;
  if (bNil) return -1;
  if (typeof a === 'number' && typeof b === 'number') {
    if (Number.isNaN(a) && Number.isNaN(b)) return 0;
    if (Number.isNaN(a)) return 1;
    if (Number.isNaN(b)) return -1;
    return (a - b) * sign;
  }
  return (
    String(a).localeCompare(String(b), undefined, { numeric: true, sensitivity: 'base' }) * sign
  );
}
