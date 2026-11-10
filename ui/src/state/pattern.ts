// Pattern editor commands: the window, channels, auditions and the pattern loop preview.
// Every edit is an op (putPattern / putChannel / removeChannel) and one undo step.
import { createSignal } from 'solid-js';
import { newSynthChannel } from '../lib/synth';
import { prefs } from './prefs';
import { be } from '../api/backend';
import { KEYS_DYNAMICS, KEYS_RELEASE, SAMPLE_ROOT, type Channel, type Id, type Inst, type Note, type Op, type Pattern } from '../api/types';
import { baseName, clamp, mod } from '../lib/format';
import { builtinChannel, cloneNotes, INST_NAME, stepsOf } from '../lib/pattern';
import { commit } from './edit';
import { allocId, plain, S } from './store';
import { curBeat, tele } from './telemetry';
import { say, setU, U } from './ui';

const PCOLORS = ['#4f8ef7', '#e0a44a', '#7c8cf8', '#47b8a8', '#d9678f', '#5fbf8a', '#9a8cf0', '#69c3d6'];

export const patById = (id: Id | undefined | null) => (id == null ? undefined : S.project.patterns.find(p => p.id === id));
export const chanById = (id: Id | undefined | null) => (id == null ? undefined : S.project.channels?.find(c => c.id === id));
export const channels = () => S.project.channels ?? [];
/** The pattern shown in the editor. */
export const curPat = () => patById(U.patWin?.pat);

// ---------------------------------------------------------------- window

/** Open the pattern editor on `id` (or bring it to the front). It shares the effect-window layer. */
export function openPattern(id?: Id, view?: 'rack' | 'roll', ch?: Id) {
  const pat = patById(id) ?? patById(U.cur?.kind === 'pat' ? U.cur.ref : undefined) ?? S.project.patterns[0];
  if (!pat) { say('Brak patternów — utwórz nowy'); return; }
  if (U.plugsHidden) setU('plugsHidden', false);
  const z = U.zTop + 1;
  // sized to the content: the rack's controls, ~34 px per step, a row per channel (+ room for one more)
  const rows = Math.max(5, channels().length + 1);
  const w = clamp(252 + stepsOf(pat) * 34 + 4, 700, window.innerWidth - 40), h = clamp(30 + rows * 28 + 76 + 24, 320, window.innerHeight - 90);
  const cur = U.patWin;
  // keep the selected channel if it plays in this pattern, else the first one that does
  const used = (id: Id | null | undefined) => id != null && pat.notes.some(n => n.ch === id);
  const firstCh = ch ?? (used(cur?.ch) ? cur!.ch : channels().find(c => used(c.id))?.id ?? cur?.ch ?? channels()[0]?.id ?? null);
  setU({
    zTop: z,
    patWin: cur ? { ...cur, pat: pat.id, z, ch: firstCh, view: view ?? cur.view }
      : { pat: pat.id, ch: firstCh, view: view ?? 'rack', x: Math.max(20, (window.innerWidth - w) / 2), y: 56, w, h, z },
    cur: { kind: 'pat', ref: pat.id },
  });
}
export function closePattern() {
  if (U.patPlaying) void stopPreview();
  setU('patWin', null);
}
/** F6: show the editor, or close it when it is already the visible top window. */
export function togglePattern() {
  const w = U.patWin;
  const top = w && !U.plugsHidden && U.plugs.every(p => p.z < w.z);
  if (top) closePattern(); else openPattern(w?.pat);
}
export function patFront() {
  const w = U.patWin; if (!w) return;
  if (U.plugsHidden) setU('plugsHidden', false);
  if (w.z === U.zTop) return;
  const z = U.zTop + 1;
  setU('zTop', z); setU('patWin', 'z', z);
}
export function selectChannel(id: Id) { if (U.patWin && U.patWin.ch !== id) setU('patWin', 'ch', id); }
export function setView(view: 'rack' | 'roll', ch?: Id) {
  const w = U.patWin; if (!w) return;
  // the piano roll needs height for a couple of octaves; the rack window may be short
  const h = view === 'roll' ? Math.max(w.h, Math.min(520, window.innerHeight - w.y - 20)) : w.h;
  setU('patWin', { view, h, ...(ch != null ? { ch } : {}) });
}

// ---------------------------------------------------------------- patterns

export const putNotes = (pt: Pattern, notes: Note[]): Op => ({ op: 'putPattern', pattern: { ...plain(pt), notes: plain(notes) } });
export const patOp = (pt: Pattern, patch: Partial<Pattern>): Op => ({ op: 'putPattern', pattern: { ...plain(pt), ...plain(patch) } });

export function newPattern() {
  const n = S.project.patterns.length;
  const pt: Pattern = { id: allocId(), name: 'Pattern ' + (n + 1), color: PCOLORS[n % PCOLORS.length], len: 4, notes: [] };
  commit([{ op: 'putPattern', pattern: pt }], 'Nowy pattern');
  openPattern(pt.id);
  say('Nowy pattern: ' + pt.name + ' — maluj kroki, przeciągnij na playlistę');
}
export function setBars(pt: Pattern, bars: number) {
  const len = clamp(Math.round(bars), 1, 8) * 4;
  if (len === pt.len) return;
  const hidden = pt.notes.filter(n => n.step >= len * 4 - 1e-4).length;
  commit([patOp(pt, { len })], 'Długość patternu');
  say(pt.name + ': ' + len / 4 + (len === 4 ? ' takt' : len < 20 ? ' takty' : ' taktów') + (hidden ? ' · ' + hidden + ' nut poza końcem (ukryte, nie usunięte)' : ''));
}

// ---------------------------------------------------------------- channels

export function chanOp(c: Channel, patch: Partial<Channel> = {}, index?: number): Op {
  return { op: 'putChannel', channel: { ...plain(c), ...patch } as Channel, ...(index != null ? { index } : {}) };
}
export function setChannel(c: Channel, patch: Partial<Channel>, label: string) { commit([chanOp(c, patch)], label); }
export function toggleChanMute(c: Channel) { setChannel(c, c.mute ? { mute: false } : { mute: true, solo: false }, 'Mute kanału'); say(c.name + (c.mute ? ' · włączony' : ' · wyciszony')); }
export function toggleChanSolo(c: Channel) { setChannel(c, c.solo ? { solo: false } : { solo: true, mute: false }, 'Solo kanału'); say(c.name + (c.solo ? ' · solo wył.' : ' · solo')); }

export function addBuiltin(inst: Inst) {
  const c = builtinChannel(allocId(), inst);
  commit([chanOp(c)], 'Dodaj kanał');
  selectChannel(c.id);
  say('Dodano kanał ' + INST_NAME[inst]);
}
/** A new channel of the built-in synth playing factory preset `name` (or the blank patch). */
export function addSynth(name: string | null) {
  const cat = S.catalog?.synth; if (!cat) { say('Syntezator niedostępny'); return; }
  const c = newSynthChannel(cat, allocId(), name);
  commit([chanOp(c)], 'Dodaj syntezator');
  selectChannel(c.id);
  say('Syntezator: ' + (name ?? 'pusty dźwięk') + ' — klik w nazwę kanału = edycja dźwięku');
}
/** A new WAVE Keys channel: the sampled grand piano. */
export function addKeys() {
  const c: Channel = { id: allocId(), name: 'WAVE Keys', color: '#e0a44a', kind: 'keys', keys: { release: KEYS_RELEASE.def, dynamics: KEYS_DYNAMICS.def }, vol: 0.7937, pan: 0, mute: false, solo: false, insert: null, root: 60 };
  commit([chanOp(c)], 'Dodaj WAVE Keys');
  selectChannel(c.id);
  say('WAVE Keys: fortepian — 2× klik w nazwę = piano roll, klik = ustawienia');
}
export function addSampler(sourceId: Id) {
  const s = S.project.sources.find(x => x.id === sourceId); if (!s) return;
  const c: Channel = { id: allocId(), name: s.name, color: s.color, kind: 'sample', source: s.id, vol: 0.7937, pan: 0, mute: false, solo: false, insert: null, root: SAMPLE_ROOT };
  commit([chanOp(c)], 'Dodaj sampler');
  selectChannel(c.id);
  say('Sampler: ' + s.name + ' — klik w krok = odsłuch, piano roll = wysokości');
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
  } catch (e) { say('Nie dodano samplera: ' + String(e)); }
}
export function cloneChannel(c: Channel) {
  const id = allocId(), idx = channels().findIndex(x => x.id === c.id) + 1;
  const ops: Op[] = [chanOp({ ...c, id, name: c.name + ' 2', mute: false, solo: false }, {}, idx)];
  // FL clones the channel's steps too: in every pattern
  for (const pt of S.project.patterns) if (pt.notes.some(n => n.ch === c.id)) ops.push(putNotes(pt, cloneNotes(pt.notes, c.id, id)));
  commit(ops, 'Klonuj kanał');
  selectChannel(id);
  say('Sklonowano ' + c.name);
}
export function deleteChannel(c: Channel) {
  const used = S.project.patterns.filter(pt => pt.notes.some(n => n.ch === c.id)).length;
  commit([{ op: 'removeChannel', id: c.id }], 'Usuń kanał');
  if (U.patWin?.ch === c.id) setU('patWin', { ch: channels()[0]?.id ?? null, view: 'rack' });
  say('Usunięto kanał ' + c.name + (used ? ' (z nutami w ' + used + ' pattern' + (used === 1 ? 'ie' : 'ach') + ') — Ctrl+Z cofa' : ''));
}
export const chanSource = (c: Channel) => (c.kind === 'sample' ? S.project.sources.find(s => s.id === c.source) : undefined);
export const chanKindLabel = (c: Channel) => (c.kind === 'inst' ? 'wbudowany · ' + INST_NAME[c.inst] : c.kind === 'synth' ? 'syntezator · ' + (c.synth.preset ?? 'własny') : c.kind === 'keys' ? 'WAVE Keys · fortepian' : 'sampler · ' + (chanSource(c) ? baseName(chanSource(c)!.path) : 'brak pliku'));

// ---------------------------------------------------------------- sound

export function audition(ch: Id, key?: number, vel = 0.8, force = false) {
  if (!force && !prefs().noteAudition) return;
  be().noteAudition({ ch, key, vel, pattern: U.patWin?.pat }).catch(e => console.warn(e));
}

let previewId: Id | null = null;

// FL's PAT / SONG switch (L): in PAT mode Space loops the current pattern, in SONG the playlist.
const MODE_KEY = 'wave.patMode';
const [patMode, setPatModeS] = createSignal((() => { try { return localStorage.getItem(MODE_KEY) === '1'; } catch { return false; } })());
export { patMode };
export function setPatMode(on: boolean) {
  setPatModeS(on);
  try { localStorage.setItem(MODE_KEY, on ? '1' : '0'); } catch { /* private window */ }
  say(on ? 'Tryb PAT — Spacja gra bieżący pattern w pętli (' + (modePattern()?.name ?? '—') + ')' : 'Tryb SONG — Spacja gra utwór z playlisty');
}
/** The pattern PAT mode plays: the open editor's, else the one selected in the browser / playlist. */
export const modePattern = () => curPat() ?? patById(U.cur?.kind === 'pat' ? U.cur.ref : undefined) ?? S.project.patterns[0];
export const previewPattern = () => (U.patPlaying ? previewId : null);
export async function togglePreview(id: Id) {
  try {
    if (U.patPlaying && previewId === id) { await stopPreview(); say('Podgląd patternu zatrzymany'); return; }
    previewId = id;
    if (U.playing) setU('playing', false);
    await be().patternPlay(id);
    setU('patPlaying', true);
    say((patMode() ? 'PAT · ' : 'Podgląd · ') + (patById(id)?.name ?? 'pattern') + ' w pętli — Spacja zatrzymuje' + (patMode() ? '' : ' · L = tryb PAT/SONG'));
  } catch (e) { say('Podgląd: ' + String(e)); }
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
