/** Space-separated substring match. Empty query matches everything. */
export function matchesQuery(
  query: string,
  parts: Array<string | number | null | undefined>,
): boolean {
  const tokens = query
    .trim()
    .toLowerCase()
    .split(/\s+/)
    .filter(Boolean);
  if (!tokens.length) return true;
  const hay = parts
    .filter((part) => part != null && String(part) !== '')
    .join(' ')
    .toLowerCase();
  return tokens.every((token) => hay.includes(token));
}
