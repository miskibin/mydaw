// Channels, step sequencer and piano roll: pure logic shared by the pattern editor, the
// optimistic store, and the in-browser mock (which must behave like crates/engine/src/ops.rs).
//
// Units: pattern `len` in beats; note `step`/`len` in 16ths ("steps"); `p` MIDI key
// (absent = the channel's root); `v` velocity 0..1 (absent = 1).
import { t, type TranslationKey } from './i18n';
import { migrateSatParams } from './saturator';
import { KEYS_RELEASE, SAMPLE_ROOT, SAMPLER, TOTAL_BEATS, type Channel, type Id, type Note, type Op, type Pattern, type Project, type SamplerState } from '../api/types';
import { addStarterKit } from './factory';
import { SYNTH_CATALOG } from '../api/synthCatalog';
import { synthError } from './synth';

/** Beats per bar (4/4). */
export const BAR = 4;
const EPS = 1e-3;

/** Why sampler settings cannot be used, or null (model.rs `SamplerState::check`). */
export function samplerError(s: SamplerState | undefined): string | null {
  if (!s) return null; // absent = the defaults
  const within = (v: number, lo: number, hi: number) => Number.isFinite(v) && v >= lo && v <= hi;
  if (!(within(s.start, 0, 1) && within(s.end, 0, 1) && s.end - s.start >= SAMPLER.minSpan - 1e-6)) return 'sampler start and end are fractions 0..1 of the file, start before end';
  if (!(Number.isInteger(s.semi) && within(s.semi, SAMPLER.semi.min, SAMPLER.semi.max) && within(s.fine, SAMPLER.fine.min, SAMPLER.fine.max))) return 'sampler pitch is -24..24 semitones and -100..100 cents';
  if (!(within(s.attack, SAMPLER.attack.min, SAMPLER.attack.max) && within(s.decay, SAMPLER.decay.min, SAMPLER.decay.max) && within(s.sustain, 0, 1) && within(s.release, SAMPLER.release.min, SAMPLER.release.max))) return 'sampler envelope: attack 0..2000 ms, decay 0..5000 ms, sustain 0..1, release 1..5000 ms';
  if (!(within(s.fadeInMs ?? 0, 0, SAMPLER.fadeMaxMs) && within(s.fadeOutMs ?? 0, 0, SAMPLER.fadeMaxMs))) return 'sampler fades must be 0..5000 ms';
  if (!(Number.isInteger(s.cut) && within(s.cut, 0, SAMPLER.cutGroups))) return 'sampler cut group is 0 (none) or 1..8';
  return null;
}
export const keyOf = (n: Note, c: Channel | undefined) => n.p ?? c?.root ?? SAMPLE_ROOT;
export const velOf = (n: Note) => n.v ?? 1;
/** Number of 16ths in the pattern. */
export const stepsOf = (p: Pattern) => Math.max(1, Math.round(p.len * 4));
/** Channel Rack step grid: extra px between groups of four steps (beats), like FL's. */
export const BEAT_GAP = 5;
/** Left edge (px) of a step in the Channel Rack grid, `cw` px per step; fractional steps interpolate inside their cell. */
export const stepX = (step: number, cw: number) => step * cw + Math.floor(step / 4 + 1e-6) * BEAT_GAP;
/** Width (px) of `steps` steps including the beat gaps between them. */
export const gridW = (steps: number, cw: number) => steps * cw + Math.max(0, Math.ceil(steps / 4) - 1) * BEAT_GAP;
/** The step under grid x (px); a point in a beat gap belongs to the step on its left. */
export function stepAt(x: number, cw: number): number {
  const gw = 4 * cw + BEAT_GAP, b = Math.floor(x / gw);
  return b * 4 + Math.min(3, Math.floor((x - b * gw) / cw));
}
/** Widest cell (px) at which `steps` steps and their beat gaps fit into `w` px. */
export const fitCw = (w: number, steps: number) => Math.floor((w - (Math.ceil(steps / 4) - 1) * BEAT_GAP) / steps);
/** Beats the notes need: the end of the last one rounded up to a bar, ≥ 1 bar, ≤ the song (model.rs `content_len`). */
export function contentLen(notes: readonly Note[]): number {
  let end = 0;
  for (const n of notes) end = Math.max(end, (n.step + n.len) / 4);
  return Math.min(TOTAL_BEATS, Math.max(1, Math.ceil(end / BAR - 1e-6)) * BAR);
}
/** The pattern's `minLen` in whole bars (beats), ≥ 1 bar. */
export const minLenOf = (p: Pick<Pattern, 'minLen'>) => Math.min(TOTAL_BEATS, Math.max(1, Math.ceil((p.minLen ?? BAR) / BAR - 1e-6)) * BAR);
/** Sets `len` (and normalises `minLen`) like the engine's `Pattern::fit_len`. */
export function fitLen(p: Pattern): Pattern {
  p.minLen = minLenOf(p);
  p.len = Math.max(contentLen(p.notes), p.minLen);
  return p;
}

/** Swing in steps added to a note on an odd 16th (crates/engine/src/model.rs `swing_offset`). */
export function swingOffset(step: number, swing: number): number {
  const r = Math.round(step);
  if (swing <= 0 || Math.abs(step - r) > EPS || ((r % 2) + 2) % 2 !== 1) return 0;
  return 0.5 * Math.min(1, swing);
}

// ---------------------------------------------------------------- engine mirror

export class PatternOpError extends Error {}

const bump = (p: Project, id: Id) => { if (id >= p.nextId) p.nextId = id + 1; };

/** Project.migrate: derived pattern lengths, the starter kit when the Channel Rack is empty. */
export function migrateProject(p: Project) {
  for (const pt of p.patterns) fitLen(pt);
  if (!p.channels?.length) addStarterKit(p);
  // saturator parameters from before the character saturator
  for (const ins of p.inserts) for (const fx of ins.fx) if (fx.type === 'sat' && migrateSatParams(fx.params)) fx.preset = null;
}

/** Clips showing exactly one whole pass of the pattern (offset 0, old length) follow a length change. */
export function followPatternLength(p: Project, id: Id, oldLen: number, len: number) {
  for (const c of p.clips) {
    if (c.kind === 'pat' && c.ref === id && Math.abs(c.offset) < 1e-9 && Math.abs(c.len - oldLen) < 1e-6) c.len = Math.max(1 / 16, Math.min(len, TOTAL_BEATS - c.start));
  }
}

/**
 * Applies a pattern/channel op in place (`putPattern`, `putChannel`, `removeChannel`, and the
 * channel side of `removeSource`). `strict` validates like the engine and throws; the
 * optimistic store applies without checks. Returns false for ops it does not handle.
 * `adopt` is given the pattern (a private copy) before it is put in the project: the store marks
 * it as plain data so that storing it does not walk all its notes.
 */
export function applyPatternOp(p: Project, op: Op, strict: boolean, adopt: <T extends object>(v: T) => T = v => v): boolean {
  const fail = (m: string) => { if (strict) throw new PatternOpError(m); };
  switch (op.op) {
    case 'putPattern': {
      const pt: Pattern = structuredClone(op.pattern);
      if (!((pt.swing ?? 0) >= 0 && (pt.swing ?? 0) <= 1)) fail('swing must be 0..1');
      for (const n of pt.notes) {
        if (!(n.step >= 0 && n.len > 0 && isFinite(n.step) && isFinite(n.len))) fail(`pattern ${pt.id}: a note has an invalid step/len`);
        if ((n.step + n.len) > TOTAL_BEATS * 4) fail(`pattern ${pt.id}: notes must end within ${TOTAL_BEATS} beats`);
        if (n.v != null && !(n.v >= 0 && n.v <= 1)) fail('note velocity `v` must be 0..1');
        if (n.trig != null && typeof n.trig !== 'boolean') fail('note `trig` must be a boolean');
        if (!n.ch) fail('every note needs `ch`');
      }
      if (strict) { const bad = pt.notes.find(n => !(p.channels ?? []).some(c => c.id === n.ch)); if (bad) fail(`no channel with id ${bad.ch}`); }
      if (!pt.swing) delete pt.swing;
      for (const n of pt.notes) if (n.v === 1) delete n.v;
      fitLen(pt);
      bump(p, pt.id);
      const i = p.patterns.findIndex(x => x.id === pt.id);
      if (i >= 0) {
        const old = p.patterns[i].len;
        p.patterns[i] = adopt(pt);
        if (Math.abs(old - pt.len) > 1e-9) followPatternLength(p, pt.id, old, pt.len);
      } else p.patterns.push(adopt(pt));
      return true;
    }
    case 'putChannel': {
      const c = structuredClone(op.channel);
      if (!(c.vol >= 0 && c.vol <= 1)) fail('channel vol is a fader position 0..1');
      if (!(c.pan >= -1 && c.pan <= 1)) fail('channel pan must be -1..1');
      if (c.kind === 'sample') {
        if (!p.sources.some(s => s.id === c.source)) fail(`no source with id ${c.source}`);
        const e = samplerError(c.sampler); if (e) fail(e);
      }
      if (c.kind === 'synth') { const e = synthError(SYNTH_CATALOG, c.synth ?? { preset: null, params: {} }); if (e) fail(e); }
      if (c.kind === 'keys') {
        if (!(c.keys?.release >= KEYS_RELEASE.min && c.keys.release <= KEYS_RELEASE.max)) fail(`keys release must be ${KEYS_RELEASE.min}..${KEYS_RELEASE.max} ms`);
        if (!(c.keys.dynamics >= 0 && c.keys.dynamics <= 100)) fail('keys dynamics must be 0..100 %');
      }
      if (c.insert != null && !p.inserts.some(m => m.id === c.insert)) fail(`no insert with id ${c.insert}`);
      const chans = p.channels ?? (p.channels = []);
      bump(p, c.id);
      const i = chans.findIndex(x => x.id === c.id);
      if (i >= 0) {
        chans[i] = c;
        if (op.index != null) { const [x] = chans.splice(i, 1); chans.splice(Math.min(op.index, chans.length), 0, x); }
      } else chans.splice(Math.min(op.index ?? chans.length, chans.length), 0, c);
      return true;
    }
    case 'removeChannel': {
      const chans = p.channels ?? [];
      if (!chans.some(c => c.id === op.id)) { fail(`no channel with id ${op.id}`); return true; }
      p.channels = chans.filter(c => c.id !== op.id);
      for (const pt of p.patterns) pt.notes = pt.notes.filter(n => n.ch !== op.id);
      return true;
    }
    case 'removeSource': {
      // sampler channels playing it go, with their notes (the caller removes the source)
      const gone = new Set((p.channels ?? []).filter(c => c.kind === 'sample' && c.source === op.id).map(c => c.id));
      if (gone.size) {
        p.channels = (p.channels ?? []).filter(c => !gone.has(c.id));
        for (const pt of p.patterns) pt.notes = pt.notes.filter(n => !gone.has(n.ch));
      }
      return false;
    }
  }
  return false;
}

// ---------------------------------------------------------------- step sequencer

const onStep = (n: Note, ch: Id, s: number) => n.ch === ch && Math.abs(n.step - s) < EPS;
export const isStepOn = (notes: readonly Note[], ch: Id, s: number) => notes.some(n => onStep(n, ch, s));

/** Notes with step `s` of channel `ch` set on (a trigger at the root) or off (every note starting there). */
export function setStep(notes: readonly Note[], ch: Id, s: number, on: boolean): Note[] {
  const has = isStepOn(notes, ch, s);
  if (on === has) return notes as Note[];
  return on ? [...notes, { ch, step: s, len: 1, trig: true }] : notes.filter(n => !onStep(n, ch, s));
}

/** Every grid cell the pointer passes going from (r0, s0) to (r1, s1), both ends included, in order. */
export function cellsBetween(r0: number, s0: number, r1: number, s1: number): [number, number][] {
  const n = Math.max(Math.abs(r1 - r0), Math.abs(s1 - s0));
  const out: [number, number][] = [];
  for (let k = 0; k <= n; k++) {
    const t = n ? k / n : 0;
    const c: [number, number] = [Math.round(r0 + (r1 - r0) * t), Math.round(s0 + (s1 - s0) * t)];
    const last = out[out.length - 1];
    if (!last || last[0] !== c[0] || last[1] !== c[1]) out.push(c);
  }
  return out;
}

/** "Fill each N steps": the channel's steps become 0, N, 2N … up to `steps` (its other notes go). */
export function fillEvery(notes: readonly Note[], ch: Id, steps: number, n: number): Note[] {
  const keep = notes.filter(x => x.ch !== ch);
  for (let s = 0; s < steps; s += n) keep.push({ ch, step: s, len: 1, trig: true });
  return keep;
}

/** Shift the channel's notes by `d` steps, wrapping around the pattern length. */
export function rotate(notes: readonly Note[], ch: Id, steps: number, d: number): Note[] {
  return notes.map(n => n.ch === ch ? { ...n, step: ((((n.step + d) % steps) + steps) % steps) } : n);
}

export const clearChannel = (notes: readonly Note[], ch: Id) => notes.filter(n => n.ch !== ch);

/** Copies of `from`'s notes for channel `to`. */
export const cloneNotes = (notes: readonly Note[], from: Id, to: Id): Note[] => [...notes, ...notes.filter(n => n.ch === from).map(n => ({ ...n, ch: to }))];

/** A channel's notes without their channel: what the Channel Rack's clipboard holds (Ctrl+C / X). */
export type ChannelClip = Omit<Note, 'ch'>[];
export const copyChannelNotes = (notes: readonly Note[], ch: Id): ChannelClip =>
  notes.filter(n => n.ch === ch).map(({ ch: _ch, ...n }) => ({ ...n })).sort((a, b) => a.step - b.step);
/**
 * Ctrl+V in the Channel Rack: the channel's notes in this pattern become the clipboard's (its
 * other notes go, every other channel keeps its own). The pattern's length follows the notes.
 * Notes that would end past the song are dropped.
 */
export function pasteChannelNotes(notes: readonly Note[], ch: Id, clip: ChannelClip): Note[] {
  return [...notes.filter(n => n.ch !== ch), ...clip.filter(n => n.step + n.len <= TOTAL_BEATS * 4 + EPS).map(n => ({ ...n, ch }))];
}

/**
 * A row with held notes, off-grid events or chords needs the mini piano roll.
 * Legacy rows keep their previous step-grid layout and playback until edited.
 */
export function isRollRow(notes: readonly Note[], c: Channel): boolean {
  const seen = new Set<number>();
  for (const n of notes) {
    if (n.ch !== c.id) continue;
    const r = Math.round(n.step);
    const isStep = n.trig ?? Math.abs(n.len - 1) <= EPS;
    if (!isStep || Math.abs(n.step - r) > EPS || seen.has(r)) return true;
    seen.add(r);
  }
  return false;
}

/** Set the key of the channel's notes that start in cell [s, s+1) (`root` = no key of its own). */
export function setKeyAt(notes: readonly Note[], c: Channel, s: number, key: number): Note[] {
  const k = Math.round(Math.min(127, Math.max(0, key)));
  return notes.map(n => {
    if (n.ch !== c.id || n.step < s - EPS || n.step >= s + 1 - EPS) return n;
    const { p: _p, ...rest } = n;
    return k === c.root ? rest : { ...rest, p: k };
  });
}

/** Set the velocity of the channel's notes that start in cell [s, s+1). */
export function setVelAt(notes: readonly Note[], ch: Id, s: number, v: number): Note[] {
  const vv = Math.round(Math.min(1, Math.max(0, v)) * 100) / 100;
  return notes.map(n => n.ch === ch && n.step >= s - EPS && n.step < s + 1 - EPS ? { ...n, v: vv } : n);
}

// ---------------------------------------------------------------- piano roll

/** Piano-roll snap choices, in steps (16ths). */
export const ROLL_SNAPS: [string, number][] = [['1/4', 4], ['1/8', 2], ['1/16', 1], ['1/32', 0.5]];
/** Round (or floor) `x` steps to the grid; `unit` 0 = free (1/128 resolution). */
export function snapStep(x: number, unit: number, floor = false): number {
  if (!unit) return Math.round(x * 32) / 32;
  return (floor ? Math.floor(x / unit + 1e-9) : Math.round(x / unit)) * unit;
}

/**
 * Moves notes `idx` by `dStep` steps and `dKey` semitones from their `orig` positions. The time
 * move is snapped so the first note lands on the grid (relative positions stay), clamped at 0;
 * keys stay within 0..127.
 */
export function moveNotes(orig: readonly Note[], idx: readonly number[], dStep: number, dKey: number, unit: number, chan: Channel | undefined): Note[] {
  const sel = idx.map(i => orig[i]);
  const first = Math.min(...sel.map(n => n.step));
  let d = snapStep(first + dStep, unit) - first;
  d = Math.max(d, -first);
  const keys = sel.map(n => keyOf(n, chan));
  const dk = Math.max(-Math.min(...keys), Math.min(127 - Math.max(...keys), dKey));
  const out = orig.slice();
  for (const i of idx) { const n = orig[i]; out[i] = { ...n, step: n.step + d, p: keyOf(n, chan) + dk }; }
  return out;
}

/** Resizes notes `idx` by `dLen` steps (end snapped to the grid); never shorter than one grid unit (1/32 free). */
export function resizeNotes(orig: readonly Note[], idx: readonly number[], dLen: number, unit: number): Note[] {
  const out = orig.slice();
  const min = unit || 0.25;
  for (const i of idx) {
    const n = orig[i];
    const end = snapStep(n.step + (n.trig ? 0 : n.len) + dLen, unit);
    out[i] = { ...n, len: Math.max(min, end - n.step), trig: false };
  }
  return out;
}

/**
 * Ctrl+B: copies of the selection right after it. The offset is the selection's extent
 * rounded up to whole beats (4 steps), so a one-bar phrase repeats on the next bar.
 * Returns the notes and the indices of the copies (the new selection).
 */
export function duplicateRight(notes: readonly Note[], idx: readonly number[]): { notes: Note[]; idx: number[] } {
  if (!idx.length) return { notes: notes as Note[], idx: [] };
  const sel = idx.map(i => notes[i]);
  const a = Math.min(...sel.map(n => n.step)), b = Math.max(...sel.map(n => n.step + n.len));
  const span = Math.max(4, Math.ceil((b - a) / 4 - 1e-9) * 4);
  const copies = sel.map(n => ({ ...n, step: n.step + span }));
  return { notes: [...notes, ...copies], idx: copies.map((_, k) => notes.length + k) };
}

/** Transpose notes `idx` by `semis` (blocked at 0..127 as a group). */
export function transpose(notes: readonly Note[], idx: readonly number[], semis: number, chan: Channel | undefined): Note[] {
  if (!idx.length) return notes as Note[];
  const keys = idx.map(i => keyOf(notes[i], chan));
  const d = Math.max(-Math.min(...keys), Math.min(127 - Math.max(...keys), semis));
  const out = notes.slice();
  for (const i of idx) out[i] = { ...notes[i], p: keyOf(notes[i], chan) + d };
  return out;
}

/** Width of a note in the Piano Roll, including a grab area for a zero-length trigger. */
export const noteWidth = (n: Note, pps: number) => n.trig ? 7 : Math.max(3, n.len * pps - 1);

/** Index of the channel's note under (step, key), the topmost (latest) first. */
export function noteAt(notes: readonly Note[], ch: Id, chan: Channel | undefined, step: number, key: number, pps?: number): number {
  for (let i = notes.length - 1; i >= 0; i--) {
    const n = notes[i];
    const width = pps ? noteWidth(n, pps) / pps : n.len;
    if (n.ch === ch && keyOf(n, chan) === key && step >= n.step - EPS && step < n.step + width) return i;
  }
  return -1;
}

/** Scale masks (bit k = k semitones above the root) keyed by the engine's scale id — the
 * same id space as pitch::scale_mask, the Pitch Correct panel and setKey ops. */
export const SCALES: [TranslationKey, number, number][] = [
  ['b.sc.major', 1, 0b101010110101],
  ['b.sc.minor', 2, 0b010110101101],
  ['b.sc.pentatonicMaj', 3, 0b001010010101],
  ['b.sc.pentatonicMin', 4, 0b010010101001],
  ['b.sc.chromatic', 0, 0xfff],
];
/** Mask for an engine scale id (0 = chromatic, 1 = major, 2 = natural minor, 3/4 = pentatonics). */
export const scaleMask = (id: number): number => SCALES.find(s => s[1] === id)?.[2] ?? 0xfff;
/** "3 bars" in the current language. */
export const barsText = (n: number): string => t(n === 1 ? 'b.pat.bar1' : n < 5 ? 'b.pat.bar2' : 'b.pat.bar5', { n });
/** "3 notes" in the current language. */
export const notesText = (n: number): string => t(n === 1 ? 'b.pat.note1' : 'b.pat.noteN', { n });
export const inScale = (key: number, root: number, mask: number) => (mask & (1 << ((((key - root) % 12) + 12) % 12))) !== 0;
