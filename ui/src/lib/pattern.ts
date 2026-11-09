// Channels, step sequencer and piano roll: pure logic shared by the pattern editor, the
// optimistic store, and the in-browser mock (which must behave like crates/engine/src/ops.rs).
//
// Units: pattern `len` in beats; note `step`/`len` in 16ths ("steps"); `p` MIDI key
// (absent = the channel's root); `v` velocity 0..1 (absent = 1).
import { SAMPLE_ROOT, TOTAL_BEATS, VOL_UNITY, type Channel, type Id, type Inst, type Note, type Op, type Pattern, type Project } from '../api/types';
import { SYNTH_CATALOG } from '../api/synthCatalog';
import { synthError } from './synth';

export const INSTS: Inst[] = ['kick', 'clap', 'hat', 'bass'];
export const INST_NAME: Record<Inst, string> = { kick: 'Kick', clap: 'Clap', hat: 'Hat', bass: 'Bass' };
export const INST_COLOR: Record<Inst, string> = { kick: '#4f8ef7', clap: '#d9678f', hat: '#e0a44a', bass: '#7c8cf8' };
export const instRoot = (i: Inst) => (i === 'bass' ? 36 : SAMPLE_ROOT);
/** Longest pattern in the editor (8 bars). */
export const MAX_BARS = 8;
const EPS = 1e-3;

export function builtinChannel(id: Id, inst: Inst): Channel {
  return { id, name: INST_NAME[inst], color: INST_COLOR[inst], kind: 'inst', inst, vol: VOL_UNITY, pan: 0, mute: false, solo: false, insert: null, root: instRoot(inst) };
}
/** Channels whose notes have a pitch worth editing (piano roll). */
export const isPitched = (c: Channel) => c.kind === 'sample' || c.kind === 'synth' || c.inst === 'bass';
export const keyOf = (n: Note, c: Channel | undefined) => n.p ?? c?.root ?? SAMPLE_ROOT;
export const velOf = (n: Note) => n.v ?? 1;
/** Number of 16ths in the pattern. */
export const stepsOf = (p: Pattern) => Math.max(1, Math.round(p.len * 4));
/** Notes the editor shows and the engine plays (the rest are past the end: kept, hidden). */
export const visible = (n: Note, p: Pattern) => n.step < p.len * 4 - 1e-4;

/** Swing in steps added to a note on an odd 16th (crates/engine/src/model.rs `swing_offset`). */
export function swingOffset(step: number, swing: number): number {
  const r = Math.round(step);
  if (swing <= 0 || Math.abs(step - r) > EPS || ((r % 2) + 2) % 2 !== 1) return 0;
  return 0.5 * Math.min(1, swing);
}

// ---------------------------------------------------------------- engine mirror

export class PatternOpError extends Error {}

const bump = (p: Project, id: Id) => { if (id >= p.nextId) p.nextId = id + 1; };

export function channelForInst(p: Project, inst: Inst): Id {
  const chans = p.channels ?? (p.channels = []);
  const c = chans.find(x => x.kind === 'inst' && x.inst === inst);
  if (c) return c.id;
  const id = p.nextId++;
  chans.push(builtinChannel(id, inst));
  return id;
}

/** Old notes say `inst`: give them that instrument's channel (created if missing). */
export function resolveNotes(p: Project, notes: Note[]) {
  for (const n of notes) {
    if (!n.ch && n.inst) n.ch = channelForInst(p, n.inst);
    delete n.inst;
  }
}

/** Project.migrate: channels for legacy notes (Kick, Clap, Hat, Bass order), four built-ins when empty. */
export function migrateProject(p: Project) {
  const used = new Set<Inst>();
  for (const pt of p.patterns) for (const n of pt.notes) if (!n.ch && n.inst) used.add(n.inst);
  for (const i of INSTS) if (used.has(i)) channelForInst(p, i);
  for (const pt of p.patterns) resolveNotes(p, pt.notes);
  if (!p.channels?.length) for (const i of INSTS) channelForInst(p, i);
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
 */
export function applyPatternOp(p: Project, op: Op, strict: boolean): boolean {
  const fail = (m: string) => { if (strict) throw new PatternOpError(m); };
  switch (op.op) {
    case 'putPattern': {
      const pt: Pattern = structuredClone(op.pattern);
      if (!(pt.len > 0 && pt.len <= TOTAL_BEATS)) fail(`pattern length must be within 0..${TOTAL_BEATS} beats`);
      if (!((pt.swing ?? 0) >= 0 && (pt.swing ?? 0) <= 1)) fail('swing must be 0..1');
      for (const n of pt.notes) {
        if (!(n.step >= 0 && n.len > 0 && isFinite(n.step) && isFinite(n.len))) fail(`pattern ${pt.id}: a note has an invalid step/len`);
        if (n.v != null && !(n.v >= 0 && n.v <= 1)) fail('note velocity `v` must be 0..1');
        if (!n.ch && !n.inst) fail('every note needs `ch`');
      }
      resolveNotes(p, pt.notes);
      if (strict) { const bad = pt.notes.find(n => !(p.channels ?? []).some(c => c.id === n.ch)); if (bad) fail(`no channel with id ${bad.ch}`); }
      if (!pt.swing) delete pt.swing;
      for (const n of pt.notes) if (n.v === 1) delete n.v;
      bump(p, pt.id);
      const i = p.patterns.findIndex(x => x.id === pt.id);
      if (i >= 0) {
        const old = p.patterns[i].len;
        p.patterns[i] = pt;
        if (Math.abs(old - pt.len) > 1e-9) followPatternLength(p, pt.id, old, pt.len);
      } else p.patterns.push(pt);
      return true;
    }
    case 'putChannel': {
      const c = structuredClone(op.channel);
      if (!(c.vol >= 0 && c.vol <= 1)) fail('channel vol is a fader position 0..1');
      if (!(c.pan >= -1 && c.pan <= 1)) fail('channel pan must be -1..1');
      if (c.kind === 'sample' && !p.sources.some(s => s.id === c.source)) fail(`no source with id ${c.source}`);
      if (c.kind === 'synth') { const e = synthError(SYNTH_CATALOG, c.synth ?? { preset: null, params: {} }); if (e) fail(e); }
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

/** Notes with step `s` of channel `ch` set on (a 1-step note at the root) or off (every note starting there). */
export function setStep(notes: readonly Note[], ch: Id, s: number, on: boolean): Note[] {
  const has = isStepOn(notes, ch, s);
  if (on === has) return notes as Note[];
  return on ? [...notes, { ch, step: s, len: 1 }] : notes.filter(n => !onStep(n, ch, s));
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

/** "Fill each N steps": the channel's steps become 0, N, 2N … (its other visible notes go). */
export function fillEvery(notes: readonly Note[], ch: Id, steps: number, n: number): Note[] {
  const keep = notes.filter(x => x.ch !== ch || x.step >= steps - 1e-4);
  for (let s = 0; s < steps; s += n) keep.push({ ch, step: s, len: 1 });
  return keep;
}

/** Shift the channel's visible notes by `d` steps, wrapping around the pattern length. */
export function rotate(notes: readonly Note[], ch: Id, steps: number, d: number): Note[] {
  return notes.map(n => n.ch === ch && n.step < steps - 1e-4 ? { ...n, step: ((((n.step + d) % steps) + steps) % steps) } : n);
}

export const clearChannel = (notes: readonly Note[], ch: Id) => notes.filter(n => n.ch !== ch);

/** Copies of `from`'s notes for channel `to`. */
export const cloneNotes = (notes: readonly Note[], from: Id, to: Id): Note[] => [...notes, ...notes.filter(n => n.ch === from).map(n => ({ ...n, ch: to }))];

/** True when the channel's notes in this pattern are more than plain steps (lengths, pitches, off-grid). */
export function isRollRow(notes: readonly Note[], c: Channel): boolean {
  return notes.some(n => n.ch === c.id && (Math.abs(n.len - 1) > EPS || (n.p != null && n.p !== c.root) || Math.abs(n.step - Math.round(n.step)) > EPS));
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
    const end = snapStep(n.step + n.len + dLen, unit);
    out[i] = { ...n, len: Math.max(min, end - n.step) };
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

/** Index of the channel's note under (step, key), the topmost (latest) first. */
export function noteAt(notes: readonly Note[], ch: Id, chan: Channel | undefined, step: number, key: number): number {
  for (let i = notes.length - 1; i >= 0; i--) {
    const n = notes[i];
    if (n.ch === ch && keyOf(n, chan) === key && step >= n.step - EPS && step < n.step + n.len) return i;
  }
  return -1;
}

/** Major / natural minor scale masks (bit k = k semitones above the root). */
export const SCALES: [string, number][] = [['dur', 0b101010110101], ['moll', 0b010110101101], ['chromatyczna', 0xfff]];
export const inScale = (key: number, root: number, mask: number) => (mask & (1 << ((((key - root) % 12) + 12) % 12))) !== 0;
