// Piano-roll editing maths (pure): quantize, legato, selection helpers, the note properties
// popover's parsing, velocity wheel steps and view navigation (zoom to a box, follow the
// playhead page-wise). Units as in `pattern.ts`: note `step`/`len` in 16ths, `p` MIDI key,
// `v` velocity 0..1.
import { TOTAL_BEATS, type Channel, type Id, type Note } from '../api/types';
import { keyOf, velOf } from './pattern';
import { NOTE_NAMES } from './pitch';
import { clampLen, clampPos, type ZLimits, type ZRange } from './zoomBar';

const EPS = 1e-3;
/** Notes must end within the song (the engine rejects a pattern otherwise). */
export const MAX_STEP = TOTAL_BEATS * 4;
const r6 = (x: number) => Math.round(x * 1e6) / 1e6;

/**
 * Ctrl+Q: note starts `idx` rounded to the nearest multiple of `unit` steps; lengths stay. A note
 * that would then end past the song moves back by whole units. `moved` = notes that changed.
 */
export function quantizeStarts(notes: readonly Note[], idx: readonly number[], unit: number): { notes: Note[]; moved: number } {
  const out = notes.slice();
  let moved = 0;
  if (!(unit > 0)) return { notes: out, moved };
  for (const i of idx) {
    const n = notes[i]; if (!n) continue;
    let s = Math.max(0, r6(Math.round(n.step / unit) * unit));
    if (s + n.len > MAX_STEP + 1e-9) s = Math.max(0, r6(Math.floor((MAX_STEP - n.len) / unit + 1e-9) * unit));
    if (Math.abs(s - n.step) > 1e-6) { out[i] = { ...n, step: s }; moved++; }
  }
  return { notes: out, moved };
}

/**
 * Ctrl+L: each note of `idx` lasts until the next later start among `idx` (notes starting
 * together, a chord, are one start and all reach the next one); the notes of the last start keep
 * their length. `changed` = notes whose length changed.
 */
export function legato(notes: readonly Note[], idx: readonly number[]): { notes: Note[]; changed: number } {
  const out = notes.slice();
  const starts = [...new Set(idx.filter(i => notes[i]).map(i => notes[i].step))].sort((a, b) => a - b);
  // starts closer than EPS are the same start (a chord drawn off the grid)
  const uniq: number[] = [];
  for (const s of starts) if (!uniq.length || s - uniq[uniq.length - 1] > EPS) uniq.push(s);
  let changed = 0;
  for (const i of idx) {
    const n = notes[i]; if (!n) continue;
    const next = uniq.find(s => s > n.step + EPS);
    if (next == null) continue;
    const len = r6(next - n.step);
    if (n.trig || Math.abs(len - n.len) > 1e-6) { out[i] = { ...n, len, trig: false }; changed++; }
  }
  return { notes: out, changed };
}

/** Shift+I: the channel's notes (`chanIdx`) that are not selected. */
export function invertSelection(chanIdx: readonly number[], sel: ReadonlySet<number>): Set<number> {
  return new Set(chanIdx.filter(i => !sel.has(i)));
}

/** Ctrl+click on the keyboard: every note of channel `ch` on `key`. */
export function notesOfKey(notes: readonly Note[], ch: Id, chan: Channel | undefined, key: number): number[] {
  const out: number[] = [];
  notes.forEach((n, i) => { if (n.ch === ch && keyOf(n, chan) === key) out.push(i); });
  return out;
}

/** Alt+wheel: velocities of `idx` changed by `d` (−1..1), rounded to 1 %, kept within 0..1. */
export function nudgeVelocity(notes: readonly Note[], idx: readonly number[], d: number): Note[] {
  const out = notes.slice();
  for (const i of idx) {
    const n = notes[i]; if (!n) continue;
    out[i] = { ...n, v: Math.min(1, Math.max(0, Math.round((velOf(n) + d) * 100) / 100)) };
  }
  return out;
}

/**
 * Wheel notches in one wheel event: a mouse wheel sends one event per notch (|delta| about 100 px,
 * or 1–3 lines), a touchpad many small ones — those add up in `acc` until they make a notch.
 * Returns the signed notch count (positive = wheel up = more) and the new remainder.
 */
export function wheelNotches(acc: number, delta: number, lines: boolean): { n: number; acc: number } {
  if (lines || Math.abs(delta) >= 50) return { n: delta === 0 ? 0 : -Math.sign(delta) * (lines ? 1 : Math.max(1, Math.round(Math.abs(delta) / 100))), acc: 0 };
  acc += delta;
  if (Math.abs(acc) < 100) return { n: 0, acc };
  return { n: -Math.sign(acc), acc: acc - Math.sign(acc) * 100 };
}

// ---------------------------------------------------------------- view navigation

/** The view showing [a, b] (content units), centred on it when the zoom limits keep it wider or narrower. */
export function fitRange(a: number, b: number, lim: ZLimits): ZRange {
  const lo = Math.min(a, b), hi = Math.max(a, b);
  const len = clampLen(Math.max(1e-6, hi - lo), lim);
  return clampPos({ pos: 0, len }, lim.total, (lo + hi) / 2 - len / 2);
}

/**
 * Follow playback page-wise (FL): when the playhead at `x` has left the visible range, the view
 * flips so the playhead sits `lead` units after its left edge; otherwise the view stays (null).
 */
export function followPage(x: number, r: ZRange, lead: number): number | null {
  if (x >= r.pos && x < r.pos + r.len) return null;
  return Math.max(0, x - Math.min(lead, r.len / 4));
}

// ---------------------------------------------------------------- note properties

/** "2:3:1" = bar 2, beat 3, first 16th (all from 1); fractional 16ths keep up to 3 decimals. */
export function fmtStart(step: number): string {
  const bar = Math.floor(step / 16 + 1e-9), beat = Math.floor((step - bar * 16) / 4 + 1e-9), s = r6(step - bar * 16 - beat * 4);
  return `${bar + 1}:${beat + 1}:${+(s + 1).toFixed(3)}`;
}

/** Parses "bar:beat:16th", "bar:beat" or "bar" (from 1) to steps; null when it is not one. */
export function parseStart(text: string): number | null {
  // "2:1:1.5" — only the last part may carry decimals (checked below)
  const p = text.trim().split(':').map(x => x.trim());
  if (p.length > 3 || p.some(x => !/^\d+(\.\d+)?$/.test(x))) return null;
  const [bar, beat = 1, s = 1] = p.map(Number);
  if (!Number.isInteger(bar) || bar < 1 || !Number.isInteger(beat) || beat < 1 || beat > 4 || s < 1 || s >= 5) return null;
  return r6((bar - 1) * 16 + (beat - 1) * 4 + (s - 1));
}

/** Length in steps as text ("2", "0.5"). */
export const fmtLen = (len: number) => String(+len.toFixed(4));

/** Parses a length: 16ths ("2", "0.5") or a note value ("1/8" = 2 steps, "3/16"); null when invalid. */
export function parseLen(text: string): number | null {
  const s = text.trim();
  let v: number;
  const f = /^(\d+(?:\.\d+)?)\s*\/\s*(\d+)$/.exec(s);
  if (f) { const d = Number(f[2]); if (!d) return null; v = (16 * Number(f[1])) / d; }
  else if (/^\d+(\.\d+)?$/.test(s) || /^\.\d+$/.test(s)) v = Number(s);
  else return null;
  v = Math.round(v * 32) / 32; // the roll's free resolution (1/128 note)
  return v > 0 && v <= MAX_STEP ? v : null;
}

/** "C5", "c#4", "Db3", "A-1" or a MIDI number 0..127; null when invalid. */
export function parseKey(text: string): number | null {
  const s = text.trim();
  if (/^\d+$/.test(s)) { const n = Number(s); return n <= 127 ? n : null; }
  const m = /^([a-gA-G])([#♯b♭]?)(-?\d+)$/.exec(s);
  if (!m) return null;
  const pc = NOTE_NAMES.indexOf(m[1].toUpperCase()) + (m[2] === '#' || m[2] === '♯' ? 1 : m[2] ? -1 : 0);
  const k = (Number(m[3]) + 1) * 12 + pc;
  return k >= 0 && k <= 127 ? k : null;
}

/** Parses a velocity in % ("80", "80 %"); 0..100, null when invalid. */
export function parseVel(text: string): number | null {
  const s = text.trim().replace(/\s*%$/, '');
  if (!/^\d+(\.\d+)?$/.test(s)) return null;
  const v = Number(s);
  return v <= 100 ? Math.round(v) : null;
}

/** What the properties popover shows for notes `idx`: velocity (%) and length when they all share one (else null), the earliest start, the lowest key. */
export function readProps(notes: readonly Note[], idx: readonly number[], chan: Channel | undefined): { vel: number | null; len: number | null; start: number; key: number } {
  const ns = idx.map(i => notes[i]).filter(Boolean);
  const vs = new Set(ns.map(n => Math.round(velOf(n) * 100))), ls = new Set(ns.map(n => n.trig ? null : n.len));
  return {
    vel: vs.size === 1 ? [...vs][0] : null,
    len: ls.size === 1 ? [...ls][0] : null,
    start: Math.min(...ns.map(n => n.step)),
    key: Math.min(...ns.map(n => keyOf(n, chan))),
  };
}

/**
 * The popover's values applied to notes `idx`: velocity (%) and length are set on every note;
 * start and key move the whole group (the earliest start / lowest key lands on the value, the
 * notes keep their positions relative to each other). Returns null when a note would leave the
 * song or the keys 0..127 (nothing is changed then).
 */
export function applyProps(notes: readonly Note[], idx: readonly number[], chan: Channel | undefined,
  ch: { vel?: number; len?: number; start?: number; key?: number }): Note[] | null {
  const cur = readProps(notes, idx, chan);
  const dS = ch.start != null ? ch.start - cur.start : 0, dK = ch.key != null ? ch.key - cur.key : 0;
  const out = notes.slice();
  for (const i of idx) {
    const n = notes[i]; if (!n) continue;
    const step = r6(n.step + dS), len = ch.len ?? n.len, key = keyOf(n, chan) + dK;
    if (step < 0 || step + len > MAX_STEP + 1e-9 || key < 0 || key > 127) return null;
    const m: Note = { ...n, step, len };
    if (ch.len != null) m.trig = false;
    if (dK) m.p = key;
    if (ch.vel != null) m.v = ch.vel / 100;
    out[i] = m;
  }
  return out;
}
