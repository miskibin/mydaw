import { t as tl } from '../lib/i18n';
// The lyrics notebook ("Notatnik"): its window, view preferences and the edits.
//
// The text lives in the project (`project.notebook`) and changes only through `putNotebook`,
// so it is saved, undone and seen by the CLI like everything else. Typing is coalesced: the
// view keeps the live text as a pending draft and sends it as ONE undo step after a pause in
// typing (or on blur, page switch, undo, save). Until then a copy of the draft sits in
// localStorage, keyed by project, so text typed into a never-saved project survives a crash.
//
// The window is project-level and is NOT part of the effect-window layer: F5 / a click on
// the playlist never hide it (lyrics stay readable over the playlist while recording).
import { createStore } from 'solid-js/store';
import type { Id, LyricPage, Notebook } from '../api/types';
import { clamp } from '../lib/format';
import { emptyNotebook, movePage, notebookHash, withPage } from '../lib/notebook';
import { commit } from './edit';
import { allocId, S } from './store';
import { say } from './ui';

export interface NbGeom { x: number; y: number; w: number; h: number }
interface NbState {
  open: boolean;
  /** Above the effect windows (last clicked) or below them. */
  front: boolean;
  geom: NbGeom;
  /** Selected page (falls back to the first one). */
  page: Id | null;
  font: number;
  rhymes: boolean;
  /** soft-wrap long lines (a wrapped line is still one bar) */
  wrap: boolean;
}

const GEOM_KEY = 'wave.notebook.geom', PREFS_KEY = 'wave.notebook.prefs', BACKUP_KEY = 'wave.notebook.backup:';
export const FONT_MIN = 11, FONT_MAX = 30;

function load<T>(k: string): Partial<T> {
  try { const v = JSON.parse(localStorage.getItem(k) ?? 'null'); return v && typeof v === 'object' ? v : {}; } catch { return {}; }
}
function store(k: string, v: unknown) { try { localStorage.setItem(k, JSON.stringify(v)); } catch { /* storage unavailable */ } }
const num = (v: unknown, d: number) => (typeof v === 'number' && isFinite(v) ? v : d);

function initialGeom(): NbGeom {
  const g = load<NbGeom>(GEOM_KEY);
  const w = num(g.w, 460), h = num(g.h, 540);
  return { w, h, x: num(g.x, (typeof window !== 'undefined' ? window.innerWidth : 1400) - w - 24), y: num(g.y, 64) };
}
const prefs = load<{ font: number; rhymes: boolean; wrap: boolean }>(PREFS_KEY);

export const [NB, setNB] = createStore<NbState>({
  open: false, front: true, geom: initialGeom(), page: null,
  font: clamp(num(prefs.font, 16), FONT_MIN, FONT_MAX), rhymes: prefs.rhymes !== false, wrap: prefs.wrap === true,
});

export const savePrefs = () => store(PREFS_KEY, { font: NB.font, rhymes: NB.rhymes, wrap: NB.wrap });
export const saveGeom = () => store(GEOM_KEY, NB.geom);

export const notebook = (): Notebook => S.project.notebook ?? emptyNotebook();
export const pages = (): LyricPage[] => S.project.notebook?.pages ?? [];
export const curPage = (): LyricPage | undefined => pages().find(p => p.id === NB.page) ?? pages()[0];

// ---------------------------------------------------------------- window

/** Keep the window reachable after the screen (or the app window) got smaller. */
export function fitGeom() {
  const g = NB.geom, W = window.innerWidth, H = window.innerHeight;
  const w = clamp(g.w, 300, Math.max(300, W - 20)), h = clamp(g.h, 240, Math.max(240, H - 20));
  setNB('geom', { w, h, x: clamp(g.x, 80 - w, W - 80), y: clamp(g.y, 0, H - 40) });
}
export function openNotebook() { fitGeom(); setNB({ open: true, front: true }); }
export function closeNotebook() { flushNotebook(); setNB('open', false); }
/** F8 / the toolbar button. */
export const toggleNotebook = () => (NB.open ? closeNotebook() : openNotebook());

// ---------------------------------------------------------------- typing (coalesced)

let pending: { id: Id; text: string } | null = null;
let timer = 0, backupTimer = 0;
/** Pause in typing after which the text becomes one undo step. */
export const IDLE_MS = 900;

/** Live (not yet committed) text of a page, or null. */
export const pendingText = (id: Id) => (pending && pending.id === id ? pending.text : null);

/** The view reports every change of the textarea here. */
export function typeText(id: Id, text: string) {
  if (pending && pending.id !== id) flushNotebook();
  pending = { id, text };
  clearTimeout(timer);
  timer = window.setTimeout(flushNotebook, IDLE_MS);
  clearTimeout(backupTimer);
  backupTimer = window.setTimeout(writeBackup, 300);
}

/** Send the pending draft (if any) as one undo step. Safe to call any time. */
export function flushNotebook() {
  clearTimeout(timer);
  if (!pending) return;
  const { id, text } = pending; pending = null;
  const pg = pages().find(p => p.id === id);
  if (!pg || pg.text === text) return;
  commit([{ op: 'putNotebook', notebook: withPage(notebook(), id, { text }) }], tl('a.nb.text'));
  writeBackup();
}

function put(nb: Notebook, label: string) {
  flushNotebook();
  commit([{ op: 'putNotebook', notebook: nb }], label);
  writeBackup();
}

// ---------------------------------------------------------------- pages

export function addPage(): Id {
  flushNotebook();
  const ps = pages();
  let n = ps.length + 1;
  while (ps.some(p => p.name === tl('a.nb.pageName', { n }))) n++;
  const pg: LyricPage = { id: allocId(), name: tl('a.nb.pageName', { n }), startBar: 1, text: '' };
  put({ pages: [...notebook().pages.map(p => ({ ...p })), pg] }, tl('a.nb.newPage'));
  setNB('page', pg.id);
  return pg.id;
}
export function renamePage(id: Id, name: string) {
  const v = name.trim().slice(0, 80);
  const pg = pages().find(p => p.id === id);
  if (!pg || !v || v === pg.name) return;
  put(withPage(notebook(), id, { name: v }), tl('a.nb.pageNameLabel'));
}
export function setStartBar(id: Id, bar: number) {
  const pg = pages().find(p => p.id === id);
  const b = clamp(Math.round(bar), 1, 9999);
  if (!pg || !isFinite(bar) || b === pg.startBar) return;
  put(withPage(notebook(), id, { startBar: b }), tl('a.nb.pageStart'));
  say(tl('a.nb.pageFrom', { name: pg.name, bar: b }));
}
export function deletePage(id: Id) {
  flushNotebook();
  const ps = pages(), i = ps.findIndex(p => p.id === id);
  if (i < 0) return;
  const name = ps[i].name;
  put({ pages: ps.filter(p => p.id !== id).map(p => ({ ...p })) }, tl('a.nb.deletePage'));
  if (NB.page === id) setNB('page', ps[i + 1]?.id ?? ps[i - 1]?.id ?? null);
  say(tl('a.nb.pageDeleted', { name }));
}
export function reorderPage(id: Id, to: number) {
  const ps = pages(), i = ps.findIndex(p => p.id === id);
  if (i < 0 || i === to) return;
  put(movePage(notebook(), id, to), tl('a.nb.pageOrder'));
}
export function selectPage(id: Id) {
  if (NB.page === id) return;
  flushNotebook();
  setNB('page', id);
}

// ---------------------------------------------------------------- draft backup

interface Backup { base: string; nb: Notebook; t: number }
const backupKey = () => BACKUP_KEY + (S.path ?? '(bez nazwy)');
/** Hash of the notebook as it is in the file (last open / save). */
let savedHash = notebookHash(undefined);

/** The notebook with the pending draft applied. */
function liveNotebook(): Notebook {
  const nb = notebook();
  return pending ? withPage(nb, pending.id, { text: pending.text }) : nb;
}

/** Keep (or drop) the local copy of text that is not in the saved file. */
export function writeBackup() {
  clearTimeout(backupTimer);
  const nb = liveNotebook(), h = notebookHash(nb);
  try {
    if (h === savedHash) localStorage.removeItem(backupKey());
    else localStorage.setItem(backupKey(), JSON.stringify({ base: savedHash, nb, t: Date.now() } satisfies Backup));
  } catch { /* storage unavailable */ }
}

/** The project is clean (just opened or saved): what it holds is the new baseline. */
export function markSaved() {
  savedHash = notebookHash(S.project.notebook);
  if (!pending) writeBackup();
  // "Save as" of an untitled project: its backup is now in a file
  if (S.path != null) {
    try {
      const k = BACKUP_KEY + '(bez nazwy)', b = JSON.parse(localStorage.getItem(k) ?? 'null') as Backup | null;
      if (b?.nb && notebookHash(b.nb) === savedHash) localStorage.removeItem(k);
    } catch { /* storage unavailable */ }
  }
}

/**
 * After a project is loaded: if this browser holds text typed into it that never reached the
 * file, and the file's notebook is still exactly what that text was based on (nothing newer
 * was saved since), put the text back as one undoable edit.
 */
export function restoreBackup(): boolean {
  let b: Backup | null = null;
  try { b = JSON.parse(localStorage.getItem(backupKey()) ?? 'null'); } catch { b = null; }
  if (!b || !b.nb || !Array.isArray(b.nb.pages)) return false;
  const cur = notebookHash(S.project.notebook);
  if (b.base !== cur || notebookHash(b.nb) === cur) return false;
  savedHash = cur;
  commit([{ op: 'putNotebook', notebook: b.nb }], tl('a.nb.restore'));
  say(tl('a.nb.restored'));
  return true;
}
