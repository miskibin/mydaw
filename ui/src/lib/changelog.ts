// CHANGELOG.md (bundled into the app) and the small Markdown subset release notes use.
// Release notes in the update dialog come from the release body; the "What's new" dialog
// reads the bundled file, so it works offline and covers every version since the last one seen.

export interface Release { version: string; date: string | null; notes: string }

/** `## 0.9.3 — 2026-11-10` sections, in file order (newest first). Text before the first one is ignored. */
export function parseChangelog(md: string): Release[] {
  const out: Release[] = [];
  let cur: { version: string; date: string | null; lines: string[] } | null = null;
  const flush = () => { if (cur) out.push({ version: cur.version, date: cur.date, notes: cur.lines.join('\n').trim() }); };
  for (const line of md.replace(/\r\n/g, '\n').split('\n')) {
    const m = /^##\s+v?(\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.+-]*)?)(?:\s+[—–-]+\s+(\S.*?))?\s*$/.exec(line);
    if (m) { flush(); cur = { version: m[1], date: m[2] ?? null, lines: [] }; } else if (cur) cur.lines.push(line);
  }
  flush();
  return out;
}

/** Compares `major.minor.patch` (a pre-release such as `-beta.1` sorts before the release). */
export function cmpVersion(a: string, b: string): number {
  const split = (v: string) => { const [core, pre] = v.replace(/^v/, '').split('+')[0].split(/-(.*)/s); return { n: core.split('.').map(x => parseInt(x, 10) || 0), pre: pre ?? '' }; };
  const x = split(a), y = split(b);
  for (let i = 0; i < 3; i++) { const d = (x.n[i] ?? 0) - (y.n[i] ?? 0); if (d) return d < 0 ? -1 : 1; }
  if (x.pre === y.pre) return 0;
  if (!x.pre) return 1;
  if (!y.pre) return -1;
  return x.pre < y.pre ? -1 : 1;
}

/** The releases newer than `seen` up to and including `current`, newest first. Empty entries are dropped. */
export function releasesSince(all: Release[], seen: string, current: string): Release[] {
  return all
    .filter(r => r.notes && cmpVersion(r.version, seen) > 0 && cmpVersion(r.version, current) <= 0)
    .sort((a, b) => cmpVersion(b.version, a.version));
}

/** The newest release that is not newer than `current` (for reading the notes again from Settings). */
export function releaseOf(all: Release[], current: string): Release | null {
  return all.filter(r => r.notes && cmpVersion(r.version, current) <= 0).sort((a, b) => cmpVersion(b.version, a.version))[0] ?? null;
}

export type Inline = { t: 'text' | 'b' | 'code'; s: string };
export type Block = { t: 'h'; s: string } | { t: 'li'; inl: Inline[] } | { t: 'p'; inl: Inline[] };

/** `**bold**` and `` `code` ``; a link `[label](url)` keeps its label (notes never open arbitrary addresses). */
export function parseInline(s: string): Inline[] {
  const out: Inline[] = [];
  const re = /\*\*(.+?)\*\*|`([^`]+)`|\[([^\]]+)\]\([^)]*\)/g;
  let at = 0;
  for (let m = re.exec(s); m; m = re.exec(s)) {
    if (m.index > at) out.push({ t: 'text', s: s.slice(at, m.index) });
    if (m[1] != null) out.push({ t: 'b', s: m[1] }); else if (m[2] != null) out.push({ t: 'code', s: m[2] }); else out.push({ t: 'text', s: m[3] });
    at = m.index + m[0].length;
  }
  if (at < s.length) out.push({ t: 'text', s: s.slice(at) });
  return out;
}

/** Headings (`#`..`####`), bullets (`-`, `*`) and paragraphs; a wrapped bullet continues on an indented line. */
export function parseNotes(md: string): Block[] {
  const out: Block[] = [];
  let open: { t: 'li' | 'p'; text: string } | null = null;
  const close = () => { if (open) out.push({ t: open.t, inl: parseInline(open.text) }); open = null; };
  for (const raw of md.replace(/\r\n/g, '\n').split('\n')) {
    const line = raw.trimEnd();
    if (!line.trim()) { close(); continue; }
    const h = /^#{1,4}\s+(.*)$/.exec(line);
    if (h) { close(); out.push({ t: 'h', s: h[1] }); continue; }
    const li = /^\s*[-*]\s+(.*)$/.exec(line);
    if (li) { close(); open = { t: 'li', text: li[1] }; continue; }
    if (open && /^\s/.test(line)) open.text += ' ' + line.trim();
    else if (open?.t === 'p') open.text += ' ' + line.trim();
    else { close(); open = { t: 'p', text: line.trim() }; }
  }
  close();
  return out;
}
