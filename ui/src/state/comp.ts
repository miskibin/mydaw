// Take lanes & comping: which comp a track shows, and the edits the playlist, its menus and
// the keyboard make. Every edit is one `putComp` (or `removeComp`) undo step.
import { be } from '../api/backend';
import { lang, t as tl } from '../lib/i18n';
import type { Clip, Comp, CompRegion, Id, Take } from '../api/types';
import { removeTake, swapRange, toTimeline, unusedTakes, usedBeats, writeRange } from '../lib/comp';
import { fmtPos } from '../lib/format';
import { commit, enqueue, resync } from './edit';
import { plain, S } from './store';
import { say, setU, U } from './ui';

/** Height of one take lane, px. */
export const LANE_H = 30;

export interface TakeRow { clip: Clip; comp: Comp }

export const compById = (id: Id) => S.project.comps?.find(c => c.id === id);

/** Comp clips on a track, earliest first. */
export function compClipsOn(track: number): Clip[] {
  return S.project.clips.filter(c => c.track === track && c.kind === 'comp' && compById(c.ref)).sort((a, b) => a.start - b.start);
}

/** The comp a track's take lanes show: the selected comp clip on it, else its first one. */
export function activeComp(track: number): TakeRow | null {
  const cs = compClipsOn(track); if (!cs.length) return null;
  const clip = cs.find(c => U.sel.includes(c.id)) ?? cs[0];
  return { clip, comp: compById(clip.ref)! };
}

/** Take lanes shown under a track (null = none / collapsed). */
export function takeRow(track: number): TakeRow | null {
  const t = S.project.tracks[track]; if (!t || !U.takesOpen[t.id]) return null;
  return activeComp(track);
}

export function toggleTakes(track: number, open?: boolean) {
  const t = S.project.tracks[track]; if (!t) return;
  const v = open ?? !U.takesOpen[t.id];
  setU('takesOpen', t.id, v);
  if (v && !activeComp(track)) say(tl('a.cp.noTakes', { name: t.name }));
}

/** Polish count: 1 take, 2–4 take'i, 5+ take'ów. */
export function takesLabel(n: number): string {
  const m10 = n % 10, m100 = n % 100;
  const few = lang() === 'pl' && m10 >= 2 && m10 <= 4 && (m100 < 12 || m100 > 14);
  return tl(n === 1 ? 'a.cp.takeOne' : few ? 'a.cp.takeFew' : 'a.cp.takeMany', { n });
}

export function putComp(comp: Comp, label: string) {
  commit([{ op: 'putComp', comp: plain(comp) }], label);
}
const withRegions = (comp: Comp, regions: CompRegion[]): Comp => ({ ...plain(comp), regions });

/** Clicking a take's header: the whole take becomes the comp (within the clip). */
export function useWholeTake(row: TakeRow, take: Take) {
  const { clip, comp } = row;
  const lo = Math.max(take.start, clip.offset), hi = Math.min(take.start + take.len, clip.offset + clip.len);
  putComp(withRegions(comp, writeRange({ ...comp, regions: [] }, take.id, lo, hi)), tl('a.cp.wholeTake'));
  setU('compSel', null);
  say('Comp = ' + take.name);
}

export function renameTake(comp: Comp, id: Id, name: string) {
  putComp({ ...plain(comp), takes: comp.takes.map(t => (t.id === id ? { ...t, name } : t)) }, tl('a.cp.takeName'));
}
export function renameComp(comp: Comp, name: string) { putComp({ ...plain(comp), name }, tl('a.cp.compName')); }

export function deleteTake(comp: Comp, id: Id) {
  const t = comp.takes.find(x => x.id === id); if (!t) return;
  if (comp.takes.length === 1) { commit([{ op: 'removeComp', id: comp.id }], tl('a.pl.deleteTake')); say(tl('a.cp.lastGone')); return; }
  putComp(removeTake(plain(comp), id), tl('a.pl.deleteTake'));
  if (S.audition[comp.id] === id) void audition(comp.id, null);
  say(tl('a.cp.deleted', { what: t.name }));
}
export function removeUnused(comp: Comp) {
  const gone = unusedTakes(comp);
  if (!gone.length) return say(tl('a.cp.allUsed'));
  if (gone.length === comp.takes.length) return say(tl('a.cp.noneUsed'));
  putComp({ ...plain(comp), takes: comp.takes.filter(t => !gone.some(g => g.id === t.id)) }, tl('a.pl.removeUnused'));
  say(tl('a.cp.deleted', { what: takesLabel(gone.length) }));
}
export const isUsed = (comp: Comp, id: Id) => usedBeats(comp, id) > 0;

export function setXfade(comp: Comp, secs: number) {
  putComp({ ...plain(comp), xfade: secs }, tl('a.cp.xfade'));
  say(tl('a.cp.xfadeMs', { ms: Math.round(secs * 1000) }));
}

export async function audition(compId: Id, take: Id | null) {
  try { await enqueue(() => be().compAudition(compId, take)); }
  catch (e) { say(tl('a.err', { e: String(e) })); return; }
  const cp = compById(compId), t = cp?.takes.find(x => x.id === take);
  say(t ? tl('a.cp.soloTake', { name: t.name }) : tl('a.cp.soloComp'));
}

export function flatten(clipId: Id) {
  enqueue(() => be().flattenComp(clipId))
    .then(() => { say(tl('a.cp.flattened')); setU('compSel', null); })
    .catch(e => { say(tl('a.cp.notFlattened', { e: String(e) })); void resync(); });
}

/** Where the pointer last was over a comp (clip body or a take lane), in comp beats. */
export const compHover = { clipId: null as Id | null, cb: 0 };

/** Ctrl+↑ / Ctrl+↓: the time selection (else the hovered region) plays the previous / next take. */
export function swapTake(dir: -1 | 1): boolean {
  const sel = U.compSel;
  const clipId = sel?.clipId ?? compHover.clipId; if (clipId == null) return false;
  const clip = S.project.clips.find(c => c.id === clipId && c.kind === 'comp'); if (!clip) return false;
  const comp = compById(clip.ref); if (!comp) return false;
  let from: number, to: number;
  if (sel) ({ from, to } = sel);
  else {
    const r = comp.regions.find(x => compHover.cb >= x.from && compHover.cb < x.to);
    if (!r) { say(tl('a.cp.hover')); return true; }
    ({ from, to } = r);
  }
  const res = swapRange(comp, from, to, dir);
  if (!res) { say(dir < 0 ? tl('a.cp.firstTake') : tl('a.cp.lastTake')); return true; }
  putComp(withRegions(comp, res.regions), tl('a.cp.swap'));
  if (sel) setU('compSel', { clipId, from: Math.max(from, res.take.start), to: Math.min(to, res.take.start + res.take.len) });
  say(fmtPos(toTimeline(clip, from)) + '–' + fmtPos(toTimeline(clip, to)) + ' → ' + res.take.name);
  return true;
}
