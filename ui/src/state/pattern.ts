// Pattern commands: the current pattern (FL's pattern selector), the Channel Rack and Piano roll
// windows, pattern and channel edits, auditions and the pattern loop preview.
// Every edit is an op (putPattern / putChannel / removeChannel) and one undo step.
import { createEffect, createRoot, createSignal, on } from 'solid-js';
import { t, tName } from '../lib/i18n';
import { newSynthChannel } from '../lib/synth';
import { prefs } from './prefs';
import { be } from '../api/backend';
import { KEYS_DYNAMICS, KEYS_RELEASE, SAMPLE_ROOT, TOTAL_BEATS, type Channel, type Id, type Inst, type Note, type Op, type Pattern } from '../api/types';
import { baseName, clamp, mod } from '../lib/format';
import { BAR, barsText, builtinChannel, cloneNotes, INST_NAME, gridW, notesText, stepsOf } from '../lib/pattern';
import { commit } from './edit';
import { unwrap } from 'solid-js/store';
import { allocId, plain, plainNotes, S } from './store';
import { curBeat, tele } from './telemetry';
import { say, setU, U } from './ui';

/** Channel Rack geometry shared with the view (sizes the window to its content). */
export const RACK_LEFT = 212, RACK_HEAD = 34, RACK_ROW = 26;
/** Step button width in the Channel Rack (px): compact like FL's, wider only when the window has room. */
export const STEP_MIN = 15, STEP_MAX = 22;

/** The pattern the engine loops on its own (PAT mode / preview). */
let previewId: Id | null = null;

const PCOLORS = ['#4f8ef7', '#e0a44a', '#7c8cf8', '#47b8a8', '#d9678f', '#5fbf8a', '#9a8cf0', '#69c3d6'];

export const patById = (id: Id | undefined | null) => (id == null ? undefined : S.project.patterns.find(p => p.id === id));
export const chanById = (id: Id | undefined | null) => (id == null ? undefined : S.project.channels?.find(c => c.id === id));
export const channels = () => S.project.channels ?? [];
/** The current pattern (the toolbar's pattern selector); the first one when it is gone. */
export const curPat = () => patById(U.pat) ?? S.project.patterns[0];
/** The selected channel (Piano roll, graph editor, typing keyboard); the first one when it is gone. */
export const selChan = () => chanById(U.selCh) ?? channels()[0];

// ---------------------------------------------------------------- current pattern

/** Make `id` the current pattern (FL's pattern selector): the rack, the roll and PAT mode follow, and the playlist paints it. */
export function setCurPattern(id: Id, quiet = false) {
  const pt = patById(id); if (!pt) return;
  // keep the selected channel; with none yet, the first one this pattern plays
  const selCh = chanById(U.selCh) ? U.selCh : channels().find(c => pt.notes.some(n => n.ch === c.id))?.id ?? channels()[0]?.id ?? null;
  setU({ pat: id, selCh, cur: { kind: 'pat', ref: id } });
  if (!quiet) say(t('b.st.curPat', { n: S.project.patterns.indexOf(pt) + 1, name: pt.name }));
}
createRoot(() => {
  // a pattern picked in the playlist or the browser (the paint brush) becomes the current one
  createEffect(on(() => U.cur, cur => { if (cur?.kind === 'pat' && cur.ref !== U.pat && patById(cur.ref)) setU('pat', cur.ref); }));
  // PAT mode keeps looping the current pattern when it changes (FL)
  createEffect(on(() => curPat()?.id, id => { if (id != null && U.patPlaying && previewId !== id) void togglePreview(id); }, { defer: true }));
});
/** Numpad + / −: the next / previous pattern (wraps). */
export function stepPattern(d: number) {
  const ps = S.project.patterns; if (!ps.length) return;
  const i = Math.max(0, ps.findIndex(p => p.id === curPat()?.id));
  setCurPattern(ps[mod(i + d, ps.length)].id);
}
/** Numpad 1–9: the n-th pattern (1-based). */
export function pickPatternNum(n: number) {
  const pt = S.project.patterns[n - 1];
  if (pt) setCurPattern(pt.id); else say(t('b.st.noPatN', { n }));
}
/** F4: the first empty pattern after the current one, or a new one (FL "next empty pattern"). */
export function nextEmptyPattern() {
  const ps = S.project.patterns, i = ps.findIndex(p => p.id === curPat()?.id);
  const empty = [...ps.slice(i + 1), ...ps.slice(0, Math.max(0, i))].find(p => !p.notes.length);
  if (empty) setCurPattern(empty.id); else newPattern();
}

// ---------------------------------------------------------------- windows

type WinKey = 'rackWin' | 'rollWin';
/** Bring a pattern window to the front of the window layer (showing the layer if it was tucked away). */
function front(k: WinKey) {
  const w = U[k]; if (!w) return;
  if (U.plugsHidden) setU('plugsHidden', false);
  if (w.z === U.zTop) return;
  const z = U.zTop + 1;
  setU('zTop', z); setU(k, 'z', z);
}
export const rackFront = () => front('rackWin');
export const rollFront = () => front('rollWin');
/** The window is open and visible on top of every other floating window. */
function onTop(k: WinKey) {
  const w = U[k];
  if (!w || U.plugsHidden) return false;
  const other = U[k === 'rackWin' ? 'rollWin' : 'rackWin'];
  return U.plugs.every(p => p.z < w.z) && (!other || other.z < w.z);
}

/** F6: open the Channel Rack (or bring it to the front). */
export function openRack() {
  if (!S.project.patterns.length) { say(t('b.st.noPatterns')); return; }
  if (U.rackWin) { rackFront(); return; }
  const pt = curPat()!, rows = Math.max(4, channels().length + 1);
  const w = clamp(RACK_LEFT + gridW(stepsOf(pt) + 16, STEP_MAX) + 8, 480, window.innerWidth - 40);
  const h = clamp(RACK_HEAD + rows * RACK_ROW + 12, 220, window.innerHeight - 120);
  const z = U.zTop + 1;
  setU({ zTop: z, plugsHidden: false, rackWin: { x: Math.max(20, Math.round((window.innerWidth - w) / 2)), y: 64, w, h, z } });
}
export function closeRack() { setU('rackWin', null); }
export function toggleRack() { if (onTop('rackWin')) closeRack(); else openRack(); }

/** F7: open the Piano roll on `ch` (default: the selected channel), or bring it to the front. */
export function openRoll(ch?: Id) {
  if (!S.project.patterns.length) { say(t('b.st.noPatterns')); return; }
  if (ch != null) selectChannel(ch);
  if (!selChan()) { say(t('b.st.noChannels')); return; }
  // the roll takes the keyboard (tools, arrows, Ctrl+C/V) as soon as it is shown
  requestAnimationFrame(() => (document.querySelector('.rollwin') as HTMLElement | null)?.focus({ preventScroll: true }));
  if (U.rollWin) { rollFront(); return; }
  const w = clamp(Math.round(window.innerWidth * 0.62), 640, window.innerWidth - 40), h = clamp(Math.round(window.innerHeight * 0.58), 360, window.innerHeight - 100);
  const z = U.zTop + 1;
  setU({ zTop: z, plugsHidden: false, rollWin: { x: Math.max(20, Math.round((window.innerWidth - w) / 2) + 40), y: 96, w, h, z } });
}
export function closeRoll() { setU('rollWin', null); }
export function toggleRoll() { if (onTop('rollWin')) closeRoll(); else openRoll(); }
/** Escape: close the top pattern window when it is above every effect window. True when one closed. */
export function closeTopPatWin(): boolean {
  if (onTop('rollWin')) { closeRoll(); return true; }
  if (onTop('rackWin')) { closeRack(); return true; }
  return false;
}
/** Open a pattern from the playlist / browser: make it current and show the rack (or the roll when one channel plays it, FL-style). */
export function openPattern(id: Id) {
  setCurPattern(id, true);
  const pt = patById(id); if (!pt) return;
  const used = [...new Set(pt.notes.map(n => n.ch))];
  if (used.length === 1) openRoll(used[0]); else openRack();
}
export function selectChannel(id: Id) { if (U.selCh !== id) setU('selCh', id); }

// ---------------------------------------------------------------- patterns

/** The pattern with other notes. Copies only the new notes (once per frame of a drag: the old ones are dropped anyway). */
export const putNotes = (pt: Pattern, notes: Note[]): Op => {
  const { notes: _old, ...rest } = unwrap(pt);
  return { op: 'putPattern', pattern: { ...plain(rest), notes: plainNotes(notes) } };
};
export const patOp = (pt: Pattern, patch: Partial<Pattern>): Op => ({ op: 'putPattern', pattern: { ...plain(pt), ...plain(patch) } });

const freshName = (base: string) => {
  const names = new Set(S.project.patterns.map(p => p.name));
  if (!names.has(base)) return base;
  for (let k = 2; ; k++) if (!names.has(base + ' ' + k)) return base + ' ' + k;
};
export function newPattern() {
  const n = S.project.patterns.length;
  const pt: Pattern = { id: allocId(), name: freshName('Pattern ' + (n + 1)), color: PCOLORS[n % PCOLORS.length], len: BAR, minLen: BAR, notes: [] };
  commit([{ op: 'putPattern', pattern: pt }], t('b.pat.new'));
  setCurPattern(pt.id, true);
  say(t('b.st.newSay', { name: pt.name }));
}
/** Shift+Ctrl+C (FL "Clone"): a copy of the pattern right after it, made current. */
export function clonePattern(pt = curPat()) {
  if (!pt) return;
  const np: Pattern = { ...plain(pt), id: allocId(), name: freshName(pt.name) };
  commit([{ op: 'putPattern', pattern: np }], t('b.st.clonePat'));
  setCurPattern(np.id, true);
  say(t('b.st.clonedPat', { name: pt.name, to: np.name }));
}
/** Delete a pattern and every playlist clip that plays it (one undo step). */
export function deletePattern(pt = curPat()) {
  if (!pt) return;
  const clips = S.project.clips.filter(c => c.kind === 'pat' && c.ref === pt.id).map(c => c.id);
  const i = S.project.patterns.findIndex(p => p.id === pt.id);
  commit([...(clips.length ? [{ op: 'removeClips', ids: clips } as Op] : []), { op: 'removePattern', id: pt.id }], t('b.st.delPat'));
  const next = S.project.patterns[Math.min(i, S.project.patterns.length - 1)];
  if (next) setCurPattern(next.id, true);
  say(t('b.st.deletedPat', { name: pt.name, clips: clips.length }));
}
export function renamePattern(pt: Pattern, name: string) {
  const v = name.trim(); if (!v || v === pt.name) return;
  commit([patOp(pt, { name: v })], t('b.st.renamePat'));
}
export function recolorPattern(pt: Pattern, color: string) { commit([patOp(pt, { color })], t('b.st.colorPat')); }
/**
 * FL "Split by channel": one new pattern per channel the pattern plays (named after the
 * channel), its clips replaced by one clip per channel on consecutive tracks.
 */
export function splitByChannel(pt = curPat()) {
  if (!pt) return;
  const used = channels().filter(c => pt.notes.some(n => n.ch === c.id));
  if (used.length < 2) { say(t('b.st.splitOne')); return; }
  const ops: Op[] = [], made: Pattern[] = [];
  for (const c of used) {
    const np: Pattern = { id: allocId(), name: freshName(pt.name + ' – ' + c.name), color: c.color, len: pt.len, minLen: pt.minLen ?? BAR, swing: pt.swing, notes: plain(pt.notes.filter(n => n.ch === c.id)) };
    made.push(np); ops.push({ op: 'putPattern', pattern: np });
  }
  const old = S.project.clips.filter(c => c.kind === 'pat' && c.ref === pt.id);
  const nt = S.project.tracks.length, clips = [];
  for (const c of old) for (let k = 0; k < made.length; k++) {
    const track = Math.min(nt - 1, c.track + k);
    clips.push({ ...plain(c), id: allocId(), track, ref: made[k].id });
  }
  if (old.length) ops.push({ op: 'removeClips', ids: old.map(c => c.id) }, { op: 'putClips', clips });
  ops.push({ op: 'removePattern', id: pt.id });
  commit(ops, t('b.st.splitPat'));
  setCurPattern(made[0].id, true);
  say(t('b.st.splitSay', { name: pt.name, n: made.length }));
}
/** "+ bar": keep one more (empty) bar at the end of the pattern. */
export function addBar(pt = curPat()) {
  if (!pt) return;
  if (pt.len + BAR > TOTAL_BEATS) return;
  commit([patOp(pt, { minLen: pt.len + BAR })], t('b.st.patLen'));
  say(t('b.st.barsSay', { name: pt.name, bars: barsText((pt.len + BAR) / BAR) }));
}
/** "− bar": drop the last bar, with the notes that start in it (notes running into it end at the new end). */
export function removeBar(pt = curPat()) {
  if (!pt || pt.len <= BAR) return;
  const end = (pt.len - BAR) * 4;
  const gone = pt.notes.filter(n => n.step >= end - 1e-4).length;
  const notes = pt.notes.filter(n => n.step < end - 1e-4).map(n => (n.step + n.len > end ? { ...n, len: end - n.step } : n));
  commit([patOp(pt, { minLen: pt.len - BAR, notes })], t('b.st.patLen'));
  say(t('b.st.barsSay', { name: pt.name, bars: barsText((pt.len - BAR) / BAR) }) + (gone ? t('b.st.barGone', { x: notesText(gone) }) : ''));
}

// ---------------------------------------------------------------- channels

export function chanOp(c: Channel, patch: Partial<Channel> = {}, index?: number): Op {
  return { op: 'putChannel', channel: { ...plain(c), ...patch } as Channel, ...(index != null ? { index } : {}) };
}
export function setChannel(c: Channel, patch: Partial<Channel>, label: string) { commit([chanOp(c, patch)], label); }
export function toggleChanMute(c: Channel) { setChannel(c, c.mute ? { mute: false } : { mute: true, solo: false }, t('b.st.muteCh')); say(c.name + t(c.mute ? 'b.st.unmuted' : 'b.st.muted')); }
export function toggleChanSolo(c: Channel) { setChannel(c, c.solo ? { solo: false } : { solo: true, mute: false }, t('b.st.soloCh')); say(c.name + t(c.solo ? 'b.st.soloOff' : 'b.st.soloOn')); }

export function addBuiltin(inst: Inst) {
  const c = builtinChannel(allocId(), inst);
  commit([chanOp(c)], t('b.st.addCh'));
  selectChannel(c.id);
  say(t('b.st.addedCh', { name: INST_NAME[inst] }));
}
/** A new channel of the built-in synth playing factory preset `name` (or the blank patch). */
export function addSynth(name: string | null) {
  const cat = S.catalog?.synth; if (!cat) { say(t('b.st.synthNA')); return; }
  const c = newSynthChannel(cat, allocId(), name);
  commit([chanOp(c)], t('b.st.addSynth'));
  selectChannel(c.id);
  say(t('b.st.synthSay', { name: name ? tName(name) : t('b.st.emptySound') }));
}
const keysChannel = (id: Id): Channel => ({ id, name: 'WAVE Keys', color: '#e0a44a', kind: 'keys', keys: { release: KEYS_RELEASE.def, dynamics: KEYS_DYNAMICS.def }, vol: 0.7937, pan: 0, mute: false, solo: false, insert: null, root: 60 });
const samplerChannel = (id: Id, s: { id: Id; name: string; color: string }): Channel => ({ id, name: s.name, color: s.color, kind: 'sample', source: s.id, vol: 0.7937, pan: 0, mute: false, solo: false, insert: null, root: SAMPLE_ROOT });

/** What a channel can play, for "add" and "replace": a built-in, a synth preset, WAVE Keys, a sampler of a source. */
export type InstrumentPick = { kind: 'inst'; inst: Inst } | { kind: 'synth'; preset: string | null } | { kind: 'keys' } | { kind: 'sample'; source: Id };
function makeChannel(id: Id, pick: InstrumentPick): Channel | null {
  switch (pick.kind) {
    case 'inst': return builtinChannel(id, pick.inst);
    case 'synth': { const cat = S.catalog?.synth; if (!cat) { say(t('b.st.synthNA')); return null; } return newSynthChannel(cat, id, pick.preset); }
    case 'keys': return keysChannel(id);
    case 'sample': { const src = S.project.sources.find(x => x.id === pick.source); return src ? samplerChannel(id, src) : null; }
  }
}
/**
 * FL "Replace": the channel plays another instrument. Its notes, level, pan, mute/solo, mixer
 * routing and colour stay; the name and the root key come from the new instrument. One undo step.
 */
export function replaceChannel(c: Channel, pick: InstrumentPick) {
  const fresh = makeChannel(c.id, pick); if (!fresh) return;
  const keep = { id: c.id, color: c.color, vol: c.vol, pan: c.pan, mute: c.mute, solo: c.solo, insert: c.insert };
  commit([{ op: 'putChannel', channel: { ...fresh, ...keep } }], t('b.st.replaceCh'));
  say(t('b.st.replacedCh', { from: c.name, to: fresh.name }));
}
/** Import audio files and make `c` a sampler of the first one. */
export async function replaceWithFile(c: Channel) {
  try {
    const ps = await be().pickAudioFiles();
    if (!ps?.length) return;
    const r = await be().importAudio({ paths: ps.slice(0, 1) });
    for (let k = 0; k < 50 && !r.sourceIds.every(id => S.project.sources.some(s => s.id === id)); k++) await new Promise(res => setTimeout(res, 20));
    if (r.sourceIds[0] != null) replaceChannel(c, { kind: 'sample', source: r.sourceIds[0] });
  } catch (e) { say(t('b.st.samplerErr', { err: String(e) })); }
}

/** A new WAVE Keys channel: the sampled grand piano. */
export function addKeys() {
  const c = keysChannel(allocId());
  commit([chanOp(c)], t('b.st.addKeys'));
  selectChannel(c.id);
  say(t('b.st.keysSay'));
}
export function addSampler(sourceId: Id) {
  const s = S.project.sources.find(x => x.id === sourceId); if (!s) return;
  const c = samplerChannel(allocId(), s);
  commit([chanOp(c)], t('b.st.addSampler'));
  selectChannel(c.id);
  say(t('b.st.samplerSay', { name: s.name }));
}
/** Import audio files (dialog, library) and add one sampler channel per file. */
export async function addSamplerFiles(paths?: string[] | null) {
  try {
    const ps = paths ?? await be().pickAudioFiles();
    if (!ps?.length) return;
    const r = await be().importAudio({ paths: ps });
    // the import's state event may still be in flight: wait until the sources are in the store
    for (let k = 0; k < 50 && !r.sourceIds.every(id => S.project.sources.some(s => s.id === id)); k++) await new Promise(res => setTimeout(res, 20));
    for (const id of r.sourceIds) addSampler(id);
  } catch (e) { say(t('b.st.samplerErr', { err: String(e) })); }
}
export function cloneChannel(c: Channel) {
  const id = allocId(), idx = channels().findIndex(x => x.id === c.id) + 1;
  const ops: Op[] = [chanOp({ ...c, id, name: c.name + ' 2', mute: false, solo: false }, {}, idx)];
  // FL clones the channel's steps too: in every pattern
  for (const pt of S.project.patterns) if (pt.notes.some(n => n.ch === c.id)) ops.push(putNotes(pt, cloneNotes(pt.notes, c.id, id)));
  commit(ops, t('b.st.cloneCh'));
  selectChannel(id);
  say(t('b.st.cloned', { name: c.name }));
}
export function deleteChannel(c: Channel) {
  const used = S.project.patterns.filter(pt => pt.notes.some(n => n.ch === c.id)).length;
  commit([{ op: 'removeChannel', id: c.id }], t('b.st.delCh'));
  if (U.selCh === c.id) setU('selCh', channels()[0]?.id ?? null);
  say(t('b.st.deletedCh', { name: c.name, used: used ? t(used === 1 ? 'b.st.usedIn1' : 'b.st.usedInN', { n: used }) : '' }));
}
export const chanSource = (c: Channel) => (c.kind === 'sample' ? S.project.sources.find(s => s.id === c.source) : undefined);
export const chanKindLabel = (c: Channel) => (c.kind === 'inst' ? t('b.st.kindInst', { name: INST_NAME[c.inst] }) : c.kind === 'synth' ? t('b.st.kindSynth', { name: c.synth.preset ? tName(c.synth.preset) : t('b.st.kindCustom') }) : c.kind === 'keys' ? t('b.st.kindKeys') : t('b.st.kindSample', { name: chanSource(c) ? baseName(chanSource(c)!.path) : t('b.cw.noFile') }));

// ---------------------------------------------------------------- sound

export function audition(ch: Id, key?: number, vel = 0.8, force = false) {
  if (!force && !prefs().noteAudition) return;
  be().noteAudition({ ch, key, vel, pattern: curPat()?.id }).catch(e => console.warn(e));
}


// FL's PAT / SONG switch (L): in PAT mode Space loops the current pattern, in SONG the playlist.
const MODE_KEY = 'wave.patMode';
const [patMode, setPatModeS] = createSignal((() => { try { return localStorage.getItem(MODE_KEY) === '1'; } catch { return false; } })());
export { patMode };
export function setPatMode(on: boolean) {
  setPatModeS(on);
  try { localStorage.setItem(MODE_KEY, on ? '1' : '0'); } catch { /* private window */ }
  say(on ? t('b.st.modePat', { name: modePattern()?.name ?? '—' }) : t('b.st.modeSong'));
}
/** The pattern PAT mode plays: the current one. */
export const modePattern = () => curPat();
export const previewPattern = () => (U.patPlaying ? previewId : null);
export async function togglePreview(id: Id) {
  try {
    if (U.patPlaying && previewId === id) { await stopPreview(); say(t('b.st.prevStopped')); return; }
    previewId = id;
    if (U.playing) setU('playing', false);
    await be().patternPlay(id);
    setU('patPlaying', true);
    say(t('b.st.prevSay', { mode: t(patMode() ? 'b.st.modePatWord' : 'b.st.modePrevWord'), name: patById(id)?.name ?? t('b.st.patternWord'), hint: patMode() ? '' : t('b.st.prevHint') }));
  } catch (e) { say(t('b.st.prevErr', { err: String(e) })); }
}
export async function stopPreview() {
  try { await be().patternPlay(null); setU('patPlaying', false); } catch (e) { console.warn(e); }
}

/**
 * Where the playhead is inside pattern `pt` (beats), or −1: the loop preview of this pattern,
 * else the song playhead over a clip that plays it.
 */
export function patPlayhead(pt: Pattern): number {
  if (U.patPlaying && previewId === pt.id && tele.frame.patBeat >= 0) return tele.frame.patBeat;
  if (!U.playing) return -1;
  const b = curBeat();
  for (const c of S.project.clips) {
    if (c.kind !== 'pat' || c.ref !== pt.id || c.muted || b < c.start || b >= c.start + c.len) continue;
    return mod(b - c.start + c.offset, pt.len);
  }
  return -1;
}
export const playStep = (pt: Pattern) => { const b = patPlayhead(pt); return b < 0 ? -1 : Math.floor(b * 4 + 1e-6) % stepsOf(pt); };
