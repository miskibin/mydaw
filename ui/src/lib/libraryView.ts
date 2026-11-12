// What the browser's Library tab shows of a list of files: its groups, the files a search
// keeps, and how many rows of a long group are drawn.
import type { LibFile } from '../api/types';

/** Groups of this many files or fewer start open; a longer list starts as closed group rows. */
export const OPEN_UP_TO = 40;
/** Rows drawn per group and per list: a folder of thousands of files must not build thousands of rows. */
export const GROUP_ROWS = 300;
export const LIST_ROWS = 600;

export interface LibGroup { name: string; files: LibFile[]; /** files of the group before the row cap */ total: number }

const fold = (s: string) => s.normalize('NFD').replace(/[̀-ͯ]/g, '').toLowerCase();
/** The words of a search (lower case, accents dropped); empty = no search. */
export const searchTerms = (q: string): string[] => fold(q).split(/\s+/).filter(Boolean);

/**
 * Does the file match every word? A word matches the file's name, its group as stored or its
 * group as shown (`shown`, the translated name), so "werble" and "snares" both find snares.
 */
export function matches(f: LibFile, terms: string[], shown: (group: string) => string = g => g): boolean {
  if (!terms.length) return true;
  const hay = fold(`${f.name} ${f.group} ${shown(f.group)}`);
  return terms.every(w => hay.includes(w));
}

/**
 * The files a search keeps, grouped in the order the list gives them. A group holds at most
 * `rows` of its files and the whole list at most `listRows`; `total` is the uncut count.
 */
export function groupFiles(files: readonly LibFile[], terms: string[] = [], shown?: (group: string) => string, rows = GROUP_ROWS, listRows = LIST_ROWS): LibGroup[] {
  const m = new Map<string, LibFile[]>();
  for (const f of files) {
    if (!matches(f, terms, shown)) continue;
    const g = m.get(f.group);
    if (g) g.push(f); else m.set(f.group, [f]);
  }
  let left = listRows;
  return [...m.entries()].map(([name, all]) => {
    const n = Math.min(all.length, rows, left);
    left -= n;
    return { name, files: n < all.length ? all.slice(0, n) : all, total: all.length };
  });
}

/** Is a group open? The user's choice (made in this view: browsing or this search), else open in a search and by the list's size otherwise. */
export const groupOpen = (choice: boolean | undefined, searching: boolean, listSize: number): boolean =>
  choice ?? (searching || listSize <= OPEN_UP_TO);

/** The row `step` after (+1) or before (−1) the current one, staying on the ends; no current = the first (or last going up). */
export function stepIndex(count: number, current: number, step: 1 | -1): number {
  if (count <= 0) return -1;
  if (current < 0) return step > 0 ? 0 : count - 1;
  return Math.min(count - 1, Math.max(0, current + step));
}
