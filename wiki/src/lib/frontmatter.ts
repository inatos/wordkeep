/** Rewrite YAML frontmatter `tags` while preserving other keys. */

export function normalizeTag(raw: string): string {
  return raw.trim().replace(/\s+/g, ' ');
}

export function normalizeTags(tags: string[]): string[] {
  const out: string[] = [];
  for (const raw of tags) {
    const tag = normalizeTag(raw);
    if (!tag) continue;
    if (!out.some((existing) => existing.toLowerCase() === tag.toLowerCase())) {
      out.push(tag);
    }
  }
  return out;
}

function yamlQuote(value: string): string {
  if (/^[\w./:@+-]+$/.test(value)) return value;
  return `"${value.replace(/\\/g, '\\\\').replace(/"/g, '\\"')}"`;
}

function stripTagsFromFrontmatter(fmLines: string[]): string[] {
  const out: string[] = [];
  let inTagsList = false;
  for (const line of fmLines) {
    const trimmed = line.trim();
    if (/^tags\s*:/i.test(trimmed)) {
      const after = trimmed.replace(/^tags\s*:/i, '').trim();
      inTagsList = after === '';
      continue;
    }
    if (inTagsList) {
      if (trimmed.startsWith('- ') || trimmed === '-') continue;
      inTagsList = false;
    }
    out.push(line);
  }
  return out;
}

/** Set page tags in Markdown frontmatter. Empty list removes the tags key. */
export function setMarkdownTags(markdown: string, tags: string[]): string {
  const unique = normalizeTags(tags);
  const lines = markdown.replace(/^\uFEFF/, '').split(/\r?\n/);
  let first = lines.findIndex((line) => line.trim().length > 0);
  if (first < 0) first = 0;

  const opens =
    first < lines.length && lines[first]!.trimStart().replace(/^\uFEFF/, '').trim() === '---';

  if (!opens) {
    if (unique.length === 0) return markdown;
    const block = ['---', 'tags:', ...unique.map((tag) => `  - ${yamlQuote(tag)}`), '---', ''];
    return [...block, ...lines].join('\n');
  }

  let end = -1;
  for (let i = first + 1; i < lines.length; i++) {
    if (lines[i]!.trim() === '---') {
      end = i;
      break;
    }
  }
  const closed = end >= 0;
  if (!closed) end = lines.length;

  const body = closed ? lines.slice(end + 1) : [];
  const cleaned = stripTagsFromFrontmatter(lines.slice(first + 1, end));
  const tagLines =
    unique.length === 0 ? [] : ['tags:', ...unique.map((tag) => `  - ${yamlQuote(tag)}`)];
  const newFm = [...cleaned, ...tagLines];

  if (newFm.length === 0) {
    const rest = body.join('\n').replace(/^\n+/, '');
    return rest;
  }

  return ['---', ...newFm, '---', ...body].join('\n');
}
