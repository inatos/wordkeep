function escapeHtml(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

export function stripFrontmatter(text: string): string {
  if (!text.startsWith('---')) return text;
  const end = text.indexOf('\n---', 3);
  if (end < 0) return text;
  return text.slice(end + 4).replace(/^\r?\n/, '');
}

export function frontmatterTags(text: string): string[] {
  if (!text.startsWith('---')) return [];
  const end = text.indexOf('\n---', 3);
  if (end < 0) return [];
  const tags: string[] = [];
  let inTags = false;
  for (const line of text.slice(0, end).split('\n')) {
    const trimmed = line.trim();
    if (/^tags\s*:/i.test(trimmed)) {
      inTags = true;
      continue;
    }
    if (inTags) {
      if (trimmed.startsWith('- ')) tags.push(trimmed.slice(2).replace(/^["']|["']$/g, ''));
      else if (trimmed) inTags = false;
    }
  }
  return tags.filter(Boolean);
}

function inline(text: string): string {
  return text
    .replace(/`([^`]+)`/g, '<code>$1</code>')
    .replace(/\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g, '<a href="$2" target="_blank" rel="noreferrer">$1</a>')
    .replace(/~~(.+?)~~/g, '<del>$1</del>')
    .replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>')
    .replace(/(^|[^\*])\*([^*\n]+)\*/g, '$1<em>$2</em>')
    .replace(
      /(^|\s)(@vault|@lorekeep|@continuity|@peer|@thread|@page)\b/g,
      '$1<span class="chip md-ref">$2</span>',
    );
}

function table(rows: string[]): string {
  const cells = rows.map((row) =>
    row
      .trim()
      .replace(/^\|/, '')
      .replace(/\|$/, '')
      .split('|')
      .map((cell) => cell.trim()),
  );
  const body = cells.filter((row) => !row.every((cell) => /^:?-+:?$/.test(cell)));
  if (!body.length) return '';
  const head = body[0].map((cell) => `<th>${inline(cell)}</th>`).join('');
  const rest = body
    .slice(1)
    .map((row) => `<tr>${row.map((cell) => `<td>${inline(cell)}</td>`).join('')}</tr>`)
    .join('');
  return `<div class="table-wrap"><table class="dash-table"><thead><tr>${head}</tr></thead><tbody>${rest}</tbody></table></div>`;
}

export function renderStudioMarkdown(source: string): string {
  const text = escapeHtml(stripFrontmatter(source));
  const fences: string[] = [];
  const parked = text.replace(/```([a-zA-Z0-9_-]*)\n([\s\S]*?)```/g, (_all, lang: string, code: string) => {
    const index = fences.length;
    fences.push(`<pre class="md-code"><code data-lang="${lang}">${code.replace(/\n$/, '')}</code></pre>`);
    return `\n\u0000F${index}\u0000\n`;
  });
  const lines = parked.split('\n');
  const out: string[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i] ?? '';
    const fence = line.match(/^\u0000F(\d+)\u0000$/);
    if (fence) {
      out.push(fences[Number(fence[1])] ?? '');
      i += 1;
      continue;
    }
    if (/^\s*\|.+\|\s*$/.test(line)) {
      const rows = [line];
      i += 1;
      while (i < lines.length && /^\s*\|.+\|\s*$/.test(lines[i] ?? '')) {
        rows.push(lines[i] ?? '');
        i += 1;
      }
      out.push(table(rows));
      continue;
    }
    const heading = line.match(/^(#{1,4})\s+(.+)$/);
    if (heading) {
      const level = heading[1].length;
      out.push(`<h${level}>${inline(heading[2])}</h${level}>`);
      i += 1;
      continue;
    }
    if (/^---+$/.test(line.trim())) {
      out.push('<hr/>');
      i += 1;
      continue;
    }
    if (line.startsWith('&gt; ') || line === '&gt;') {
      const quote: string[] = [];
      while (i < lines.length && ((lines[i] ?? '').startsWith('&gt; ') || lines[i] === '&gt;')) {
        quote.push((lines[i] ?? '').replace(/^&gt; ?/, ''));
        i += 1;
      }
      out.push(`<blockquote>${quote.map((row) => `<p>${inline(row)}</p>`).join('')}</blockquote>`);
      continue;
    }
    if (/^\s*[-*]\s+/.test(line)) {
      const items: string[] = [];
      while (i < lines.length && /^\s*[-*]\s+/.test(lines[i] ?? '')) {
        const raw = (lines[i] ?? '').replace(/^\s*[-*]\s+/, '');
        const task = raw.match(/^\[( |x|X)\]\s+(.*)$/);
        items.push(
          task
            ? `<li class="task"><input type="checkbox" disabled ${task[1].toLowerCase() === 'x' ? 'checked' : ''}/> ${inline(task[2])}</li>`
            : `<li>${inline(raw)}</li>`,
        );
        i += 1;
      }
      out.push(`<ul>${items.join('')}</ul>`);
      continue;
    }
    if (/^\s*\d+\.\s+/.test(line)) {
      const items: string[] = [];
      while (i < lines.length && /^\s*\d+\.\s+/.test(lines[i] ?? '')) {
        items.push(`<li>${inline((lines[i] ?? '').replace(/^\s*\d+\.\s+/, ''))}</li>`);
        i += 1;
      }
      out.push(`<ol>${items.join('')}</ol>`);
      continue;
    }
    if (!line.trim()) {
      i += 1;
      continue;
    }
    const para: string[] = [line];
    i += 1;
    while (
      i < lines.length &&
      (lines[i] ?? '').trim() &&
      !/^(#{1,4}\s|\s*[-*]\s|\s*\d+\.\s|&gt; |---|\s*\|)/.test(lines[i] ?? '') &&
      !/^\u0000F/.test(lines[i] ?? '')
    ) {
      para.push(lines[i] ?? '');
      i += 1;
    }
    out.push(`<p>${inline(para.join(' '))}</p>`);
  }
  return out.join('');
}

export function applyWrap(
  value: string,
  start: number,
  end: number,
  before: string,
  after: string,
): { value: string; start: number; end: number } {
  const selected = value.slice(start, end) || 'text';
  const next = `${value.slice(0, start)}${before}${selected}${after}${value.slice(end)}`;
  const caret = start + before.length;
  return { value: next, start: caret, end: caret + selected.length };
}
