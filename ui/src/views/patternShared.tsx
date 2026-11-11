// What the Channel Rack and the Piano roll windows share: sizing hooks, the window frame
// (move, resize, focus, keys that must not reach the playlist), channel and pattern menus.
import { createEffect, createSignal, onCleanup, onMount, type JSX } from 'solid-js';
import { t, tName } from '../lib/i18n';
import type { Channel, LibFile, Note, Pattern } from '../api/types';
import { clamp } from '../lib/format';
import { FACTORY_KIT, LIB_SCHEME } from '../lib/factory';
import { barsText, clearChannel, fillEvery, notesText, rotate, stepsOf } from '../lib/pattern';
import { libGroup, loadStdLib, stdLibGroups } from '../state/library';
import { isTypingKey } from '../state/typingKeys';
import { commit } from '../state/edit';
import {
  addBar, addKeys, addSampler, addSamplerFiles, addSynth, audition, chanKindLabel, channels, clonePattern, cloneChannel, curPat, deleteChannel,
  deletePattern, type InstrumentPick, newPattern, nextEmptyPattern, openPattern, openRoll, putNotes, recolorPattern, removeBar, renamePattern, replaceChannel, replaceWithFile, setChannel, setCurPattern, splitByChannel,
} from '../state/pattern';
import { arrVersion, S } from '../state/store';
import { openMenu, openRename, say, setU, type MenuItem, type PatWin } from '../state/ui';
import { track } from '../ui/drag';
import { libGlyph, PICK_GLYPH, synthCategoryGlyph } from '../ui/menuIcons';
import { theme } from '../ui/theme';
import { moveWindow } from './FxWindow';

/** Size of an element, tracked. */
export function useSize(el: () => HTMLElement | undefined) {
  const [size, setSize] = createSignal({ w: 0, h: 0 });
  onMount(() => {
    const ro = new ResizeObserver(() => { const e = el(); if (e) setSize({ w: e.clientWidth, h: e.clientHeight }); });
    const e = el(); if (e) ro.observe(e);
    onCleanup(() => ro.disconnect());
  });
  return size;
}

/** Bumped when anything the canvases show changes (pattern notes, channels, theme). */
export function useDataVersion() {
  const [v, setV] = createSignal(0);
  // arrVersion bumps on every arrangement change: no deep JSON walk of a 2000-note pattern per edit
  createEffect(() => { arrVersion(); curPat(); channels(); theme.epoch; setV(x => x + 1); });
  return v;
}

/**
 * Keys typed while a pattern window has the focus never edit the playlist behind it: selection,
 * delete, clipboard and nudge shortcuts stop here (Space, Ctrl+Z, F-keys and Esc stay global).
 * `own` handles the window's own keys first.
 */
export function guardKeys(e: KeyboardEvent, own?: (e: KeyboardEvent) => void) {
  if (isTypingKey(e)) return; // typing keyboard on: these keys play notes (App handles them)
  own?.(e);
  if (e.cancelBubble) return;
  const m = e.ctrlKey || e.metaKey, k = e.key.toLowerCase();
  if (e.key === 'Delete' || e.key === 'Backspace' || e.key.startsWith('Arrow') || (m && !e.shiftKey && 'abcdxvm'.includes(k) && k.length === 1)) { e.preventDefault(); e.stopPropagation(); }
}

/** A pattern window: frame, title bar (drag to move), bottom-right resize grip. */
export function PatFrame(props: {
  win: PatWin; key: 'rackWin' | 'rollWin'; z: number; class: string; minW: number; minH: number;
  front: () => void; onKey?: (e: KeyboardEvent) => void; head: JSX.Element; children: JSX.Element;
}) {
  let root!: HTMLDivElement;
  const W = () => props.win;
  const resize = (e: PointerEvent) => {
    if (e.button !== 0) return; e.preventDefault(); e.stopPropagation();
    const sx = e.clientX, sy = e.clientY, o = { ...W() };
    track(ev => setU(props.key, w => w && { ...w, w: clamp(o.w + ev.clientX - sx, props.minW, window.innerWidth - 20), h: clamp(o.h + ev.clientY - sy, props.minH, window.innerHeight - 20) }), null, 'nwse-resize');
  };
  return (
    <div ref={root} tabIndex={0} class={'win patwin ' + props.class} style={{ left: W().x + 'px', top: W().y + 'px', width: W().w + 'px', height: W().h + 'px', 'z-index': props.z }}
      onPointerDown={e => { props.front(); const tg = (e.target as HTMLElement).tagName; if (tg !== 'SELECT' && tg !== 'INPUT') root.focus({ preventScroll: true }); }}
      onKeyDown={e => guardKeys(e, props.onKey)}>
      <div class="winhd" onPointerDown={e => moveWindow(e, () => W(), (x, y) => setU(props.key, w => w && { ...w, x, y }))}>{props.head}</div>
      {props.children}
      <div class="resizer" onPointerDown={resize} title={t('b.pe.resize')} />
    </div>
  );
}

// ---------------------------------------------------------------- channel menus

export function insLabel(c: Channel): string {
  if (c.insert == null) return '—';
  const i = S.project.inserts.findIndex(m => m.id === c.insert);
  return i <= 0 ? 'M' : String(i);
}
export function insTitle(c: Channel): string {
  if (c.insert == null) return t('b.pe.insAuto');
  const m = S.project.inserts.find(x => x.id === c.insert);
  return t('b.pe.insTo', { name: m ? (m.id === 0 ? 'Master' : m.name) : '?' });
}
export function insertMenu(c: Channel, x: number, y: number) {
  const items: MenuItem[] = [{ t: 'title', label: t('b.pe.mixChName', { name: c.name }) },
    { t: 'item', label: t('b.pe.clipTrack'), kbd: c.insert == null ? '✓' : '', title: t('b.pe.clipTrackTitle'), act: () => setChannel(c, { insert: null }, t('b.pe.mixCh')) }, { t: 'sep' }];
  S.project.inserts.forEach((m, i) => items.push({ t: 'item', label: i === 0 ? 'Master' : i + ' · ' + m.name, kbd: c.insert === m.id ? '✓' : '', act: () => { setChannel(c, { insert: m.id }, t('b.pe.mixCh')); say(c.name + ' → ' + (i === 0 ? 'Master' : t('b.pe.insWord', { n: i }))); } }));
  openMenu(x, y, items, Math.min(window.innerHeight - 20, 60 + items.length * 26));
}

/** Right-click on a channel: piano roll, step fills, rename / colour / clone / delete. */
export function chanMenu(c: Channel, x: number, y: number) {
  const pt = curPat(); if (!pt) return;
  const steps = stepsOf(pt);
  const edit = (notes: Note[], label: string) => commit([putNotes(pt, notes)], label);
  openMenu(x, y, [
    { t: 'title', label: c.name + ' · ' + chanKindLabel(c) },
    { t: 'item', label: 'Piano roll', kbd: 'F7', act: () => openRoll(c.id) },
    { t: 'sub', label: t('b.pe.replace'), items: instrumentItems(p => replaceChannel(c, p), f => void replaceWithFile(c, f)) },
    { t: 'sep' },
    { t: 'item', label: t('b.pe.fill2'), act: () => edit(fillEvery(pt.notes, c.id, steps, 2), t('b.pe.fill')) },
    { t: 'item', label: t('b.pe.fill4'), act: () => edit(fillEvery(pt.notes, c.id, steps, 4), t('b.pe.fill')) },
    { t: 'item', label: t('b.pe.fill8'), act: () => edit(fillEvery(pt.notes, c.id, steps, 8), t('b.pe.fill')) },
    { t: 'item', label: t('b.pe.shiftL'), kbd: 'Shift+Alt+←', act: () => edit(rotate(pt.notes, c.id, steps, -1), t('b.pe.shiftSteps')) },
    { t: 'item', label: t('b.pe.shiftR'), kbd: 'Shift+Alt+→', act: () => edit(rotate(pt.notes, c.id, steps, 1), t('b.pe.shiftSteps')) },
    { t: 'item', label: t('b.pe.clear'), disabled: !pt.notes.some(n => n.ch === c.id), act: () => edit(clearChannel(pt.notes, c.id), t('b.pe.clearCh')) },
    { t: 'sep' },
    { t: 'item', label: t('b.pe.preview'), kbd: t('b.pe.ctrlClick'), act: () => audition(c.id, undefined, 0.8, true) },
    { t: 'item', label: t('b.pe.rename'), act: () => openRename(x, y, c.name, v => v.trim() && setChannel(c, { name: v.trim() }, t('b.pe.chName'))) },
    { t: 'swatches', current: c.color, pick: col => setChannel(c, { color: col }, t('b.pe.chColor')) },
    { t: 'item', label: t('b.pe.clone'), act: () => cloneChannel(c) },
    { t: 'sep' },
    { t: 'item', label: t('b.pe.delete'), danger: true, act: () => deleteChannel(c) },
  ], 470);
}

/**
 * The instruments a channel can play, as menu items: `pick` adds a channel or replaces one;
 * `file` does it with a library file, or with nothing given a file picked from disk.
 */
function instrumentItems(pick: (p: InstrumentPick) => void, file: (f?: LibFile) => void): MenuItem[] {
  // samplers first: the samples that ship with the program, the project's own audio, a file
  const items: MenuItem[] = [{ t: 'title', label: t('b.pe.menuStdLib') }];
  void loadStdLib();
  for (const [g, fs] of stdLibGroups()) items.push({ t: 'sub', label: libGroup(g), glyph: libGlyph(g), items: fs.map(f => ({ t: 'item', label: f.name, glyph: libGlyph(g), act: () => file(f) }) as MenuItem) });
  // the project's audio that is not a standard-library sample (those are in the groups above)
  const own = S.project.sources.filter(s => !s.path.startsWith(LIB_SCHEME + FACTORY_KIT + '/'));
  if (own.length) {
    items.push({ t: 'sub', label: t('b.pe.menuSampler'), glyph: PICK_GLYPH.source, items: own.slice(0, 24).map(s => ({ t: 'item', label: s.name, glyph: PICK_GLYPH.source, kbd: s.dur.toFixed(1) + ' s', act: () => pick({ kind: 'sample', source: s.id }) }) as MenuItem) });
  }
  items.push({ t: 'item', label: t('b.pe.menuFile'), act: () => file() });
  const syn = S.catalog?.synth;
  if (syn) {
    items.push({ t: 'sep' }, { t: 'title', label: t('b.pe.menuSynth') });
    for (const g of syn.categories) {
      const ps = syn.presets.filter(p => p.category === g);
      const glyph = synthCategoryGlyph(g);
      if (ps.length) items.push({ t: 'sub', label: tName(g), glyph, items: ps.map(p => ({ t: 'item', label: tName(p.name), glyph, act: () => pick({ kind: 'synth', preset: p.name }) }) as MenuItem) });
    }
    items.push({ t: 'item', label: t('b.pe.menuSynthEmpty'), glyph: PICK_GLYPH.emptySynth, act: () => pick({ kind: 'synth', preset: null }) });
  }
  items.push({ t: 'sep' }, { t: 'title', label: t('b.pe.menuSampled') }, { t: 'item', label: t('b.pe.menuKeys'), glyph: PICK_GLYPH.keys, act: () => pick({ kind: 'keys' }) });
  return items;
}

export function addChannelMenu(x: number, y: number) {
  const add = (p: InstrumentPick) => {
    if (p.kind === 'synth') addSynth(p.preset);
    else if (p.kind === 'keys') addKeys();
    else addSampler(p.source);
  };
  const items = instrumentItems(add, f => void addSamplerFiles(f ? [f] : null));
  const h = Math.min(window.innerHeight - 20, 80 + items.length * 26);
  openMenu(x, Math.min(y, window.innerHeight - h - 8), items, h);
}

// ---------------------------------------------------------------- pattern menu

/** F2: rename the current pattern in a small box at (x, y). */
export function renameCurrent(x: number, y: number) {
  const pt = curPat(); if (!pt) return;
  openRename(x, y, pt.name, v => renamePattern(pt, v));
}


/** What can be done to one pattern: rename, clone, split by channel, length, colour, delete. */
function patItems(p: Pattern, x: number, y: number): MenuItem[] {
  const last = S.project.patterns.length < 2;
  return [
    { t: 'item', label: t('b.pm.rename'), kbd: p.id === curPat()?.id ? 'F2' : undefined, act: () => openRename(x, y, p.name, v => renamePattern(p, v)) },
    { t: 'item', label: t('b.pm.clone'), kbd: p.id === curPat()?.id ? 'Shift+Ctrl+C' : undefined, act: () => clonePattern(p) },
    { t: 'item', label: t('b.pm.split'), disabled: new Set(p.notes.map(n => n.ch)).size < 2, title: t('b.pm.splitTitle'), act: () => splitByChannel(p) },
    { t: 'item', label: t('b.pm.addBar'), kbd: barsText(p.len / 4), act: () => addBar(p) },
    { t: 'item', label: t('b.pm.removeBar'), disabled: p.len <= 4, title: t('b.pm.removeBarTitle'), act: () => removeBar(p) },
    { t: 'swatches', current: p.color, pick: col => recolorPattern(p, col) },
    { t: 'sep' },
    { t: 'item', label: t('b.pm.delete'), danger: true, disabled: last, title: last ? t('b.pm.lastOne') : '', act: () => deletePattern(p) },
  ];
}
/** Right-click on a pattern in the browser (FL's picker panel): open it, then everything in `patItems`. */
export function patternItemMenu(p: Pattern, x: number, y: number) {
  const items: MenuItem[] = [{ t: 'title', label: p.name }, { t: 'item', label: t('b.pm.open'), act: () => openPattern(p.id) }, { t: 'sep' }, ...patItems(p, x, y)];
  openMenu(x, y, items, 40 + items.length * 26);
}

/** The pattern selector's menu (toolbar and Channel Rack title): pick, new, clone, rename, colour, split, delete. */
export function patternMenu(x: number, y: number) {
  const cur = curPat();
  const items: MenuItem[] = [{ t: 'title', label: t('b.pm.title') }];
  S.project.patterns.forEach((p, i) => items.push({
    t: 'item', label: (i < 9 ? i + 1 + ' · ' : '') + p.name, icon: 'M5 5h14v14H5z', iconColor: p.color,
    kbd: p.id === cur?.id ? '✓' : p.notes.length ? barsText(p.len / 4) : t('b.pm.empty'),
    act: () => setCurPattern(p.id),
  }));
  items.push({ t: 'sep' },
    { t: 'item', label: t('b.pat.new'), act: newPattern },
    { t: 'item', label: t('b.pm.nextEmpty'), kbd: 'F4', act: nextEmptyPattern });
  if (cur) items.push({ t: 'sep' }, { t: 'title', label: cur.name }, ...patItems(cur, x, y));
  const h = Math.min(window.innerHeight - 20, 40 + items.length * 26);
  openMenu(x, y, items, h);
}

/** "3 notes · 2 bars" for status lines. */
export const patSummary = () => { const pt = curPat(); return pt ? notesText(pt.notes.length) + ' · ' + barsText(pt.len / 4) : ''; };
