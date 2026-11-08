// The lyrics notebook ("Notatnik"): pure text logic shared by the view, the store and the mock
// backend. Mirrors crates/engine/src/model.rs (`Notebook`, `LyricPage`) and ops.rs
// (`putNotebook`, `validate_notebook`).
//
// One line = one bar. Bars are 1-based like the playlist ruler and a bar is 4 beats (the
// project is 4/4: `fmtPos` and the ruler count bars the same way). A line whose trimmed text is
// wrapped in square brackets ("[Refren]") is a header and takes no bar; an empty line is a bar
// of rest and still counts.
import type { Id, LyricPage, Notebook, Project } from '../api/types';

export const BEATS_PER_BAR = 4;
export const MAX_PAGES = 64;
export const MAX_TEXT = 100_000;
export const MAX_BAR = 9_999;

export const emptyNotebook = (): Notebook => ({ pages: [] });

/** `[Refren]`, `  [Zwrotka 2] ` — a section header, not a bar. */
export function isHeader(line: string): boolean {
  const t = line.trim();
  return t.length >= 2 && t[0] === '[' && t[t.length - 1] === ']';
}

/** Bar number of every line of `text` (null for headers), counting from `startBar`. */
export function lineBars(text: string, startBar: number): (number | null)[] {
  let bar = startBar;
  return text.split('\n').map(l => (isHeader(l) ? null : bar++));
}

/** The line that holds `bar` (−1 if the page does not reach it). */
export function lineOfBar(text: string, startBar: number, bar: number): number {
  if (bar < startBar) return -1;
  const bars = lineBars(text, startBar);
  return bars.indexOf(bar);
}

/** First and last bar a page covers ([start, start − 1] when it has no bar lines). */
export function pageBarRange(p: Pick<LyricPage, 'text' | 'startBar'>): [number, number] {
  const n = lineBars(p.text, p.startBar).filter(b => b != null).length;
  return [p.startBar, p.startBar + n - 1];
}

/** 1-based bar under a playhead position (beats); null during the count-in. */
export const barOfBeat = (beat: number): number | null => (beat < 0 ? null : Math.floor(beat / BEATS_PER_BAR + 1e-9) + 1);
/** Beat where a 1-based bar starts. */
export const beatOfBar = (bar: number) => (Math.max(1, bar) - 1) * BEATS_PER_BAR;

// ------------------------------------------------------------------ syllables (Polish)

const VOWELS = 'aąeęioóuy';
const isVowel = (c: string) => VOWELS.includes(c);
/** Letters of a word, lowercased (digits, punctuation, apostrophes dropped). */
const letters = (w: string) => w.toLowerCase().replace(/[^a-ząćęłńóśźżäöüéèáàíìúùâêîôû]/g, '');

/**
 * Indices of the syllable nuclei of one word. Every Polish vowel letter is a nucleus, except
 * an `i` followed by another vowel, which only softens the consonant before it (nie, się,
 * miasto, dzień). Word-initial `au`/`eu` (auto, Europa) is one diphthong. Loan words like
 * "Maria" (ri-a) are undercounted by one: a pragmatic trade-off, the counter is a guide.
 */
function nuclei(w: string): number[] {
  const out: number[] = [];
  for (let i = 0; i < w.length; i++) {
    const c = w[i];
    if (!isVowel(c)) continue;
    if (c === 'i' && i + 1 < w.length && isVowel(w[i + 1])) continue;
    if (i === 1 && c === 'u' && (w[0] === 'a' || w[0] === 'e')) continue;
    out.push(i);
  }
  return out;
}

/** Syllables in one word. */
export const wordSyllables = (w: string) => nuclei(letters(w)).length;

/** Syllables in a line (0 for headers and empty lines). */
export function syllables(line: string): number {
  if (isHeader(line)) return 0;
  let n = 0;
  for (const w of line.split(/[\s\-–—/]+/)) if (w) n += wordSyllables(w);
  return n;
}

// ------------------------------------------------------------------ rhymes

/** The last word of a line (with its character range), or null. */
export function lastWord(line: string): { word: string; from: number; to: number } | null {
  const re = /[\p{L}\p{N}'’]+/gu;
  let m: RegExpExecArray | null, last: RegExpExecArray | null = null;
  while ((m = re.exec(line))) last = m;
  if (!last) return null;
  return { word: last[0], from: last.index, to: last.index + last[0].length };
}

/** Spelling → sound, roughly: `ó` = `u`, `rz` = `ż`, `ch` = `h`, final consonants devoice. */
function phonetic(w: string): string {
  let s = w.replace(/ó/g, 'u').replace(/rz/g, 'ż').replace(/ch/g, 'h');
  const dev: [RegExp, string][] = [[/dz$/, 'c'], [/dź$/, 'ć'], [/dż$/, 'cz'], [/b$/, 'p'], [/d$/, 't'], [/g$/, 'k'], [/w$/, 'f'], [/ż$/, 'sz'], [/ź$/, 'ś'], [/z$/, 's']];
  for (const [re, to] of dev) if (re.test(s)) { s = s.replace(re, to); break; }
  return s;
}

/**
 * Rhyme key of a word: its ending from the stressed vowel on. Polish stresses the
 * penultimate syllable, so polysyllables rhyme from the second-last nucleus ("kochanie" →
 * "anie", "miłości" → "ości"); a one-syllable word from its only vowel ("noc" → "oc").
 * Null for words without a vowel.
 */
export function rhymeKey(word: string): string | null {
  const w = phonetic(letters(word));
  const nu = nuclei(w);
  if (!nu.length) return null;
  return w.slice(nu.length >= 2 ? nu[nu.length - 2] : nu[0]);
}

/**
 * Rhyme groups of a page: line index → group number (0, 1, … in order of first appearance).
 * Only bar lines take part (headers and empty lines never rhyme), and an ending needs at
 * least two lines to form a group.
 */
export function rhymeGroups(lines: string[]): Map<number, number> {
  const byKey = new Map<string, number[]>();
  lines.forEach((l, i) => {
    if (isHeader(l)) return;
    const w = lastWord(l); if (!w) return;
    const k = rhymeKey(w.word); if (!k) return;
    const list = byKey.get(k); if (list) list.push(i); else byKey.set(k, [i]);
  });
  const groups = [...byKey.values()].filter(v => v.length >= 2).sort((a, b) => a[0] - b[0]);
  const out = new Map<number, number>();
  groups.forEach((g, gi) => g.forEach(i => out.set(i, gi)));
  return out;
}

// ------------------------------------------------------------------ the op

export class NotebookError extends Error {}

export function validateNotebook(nb: Notebook): void {
  if (nb.pages.length > MAX_PAGES) throw new NotebookError(`a notebook has at most ${MAX_PAGES} pages`);
  const ids = new Set<Id>();
  for (const pg of nb.pages) {
    if (ids.has(pg.id)) throw new NotebookError(`notebook page id ${pg.id} is used twice`);
    ids.add(pg.id);
    if (!(Number.isInteger(pg.startBar) && pg.startBar >= 1 && pg.startBar <= MAX_BAR)) throw new NotebookError(`page ${pg.id}: start bar must be 1..${MAX_BAR}`);
    if (new TextEncoder().encode(pg.text).length > MAX_TEXT) throw new NotebookError(`page ${pg.id}: text is longer than ${MAX_TEXT} bytes`);
    if ([...pg.name].length > 80) throw new NotebookError(`page ${pg.id}: name is longer than 80 characters`);
  }
}

/** `putNotebook`: replaces every page. `validate` = false for optimistic local application. */
export function applyNotebookOp(p: Project, nb: Notebook, validate = true) {
  if (validate) validateNotebook(nb);
  for (const pg of nb.pages) if (pg.id >= p.nextId) p.nextId = pg.id + 1;
  p.notebook = structuredClone(nb);
}

/** The notebook with one page's fields replaced (pages keep their order). */
export function withPage(nb: Notebook, id: Id, patch: Partial<LyricPage>): Notebook {
  return { pages: nb.pages.map(p => (p.id === id ? { ...p, ...patch } : { ...p })) };
}

/** Move page `id` to position `to` (clamped). */
export function movePage(nb: Notebook, id: Id, to: number): Notebook {
  const pages = nb.pages.map(p => ({ ...p }));
  const i = pages.findIndex(p => p.id === id);
  if (i < 0) return { pages };
  const [pg] = pages.splice(i, 1);
  pages.splice(Math.max(0, Math.min(to, pages.length)), 0, pg);
  return { pages };
}

/** A short stable fingerprint of a notebook (for the draft backup). */
export function notebookHash(nb: Notebook | undefined): string {
  const s = JSON.stringify((nb?.pages ?? []).map(p => [p.id, p.name, p.startBar, p.text]));
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) { h ^= s.charCodeAt(i); h = Math.imul(h, 16777619); }
  return (h >>> 0).toString(36) + ':' + s.length;
}
