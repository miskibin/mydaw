// Pitch helpers shared by the Pitch Correct window and the pitch editor: note names and scales
// (a port of crates/engine/src/pitch.rs `scale_mask` / `snap`), the scrolling pitch-graph
// mapping, and the clip `tune` edit operations (mirroring crates/engine/src/tune_render.rs).
import type { ClipTune, NoteEdit, PitchAnalysis } from '../api/types';

export const hzToMidi = (hz: number) => 69 + 12 * Math.log2(hz / 440);
export const midiToHz = (m: number) => 440 * Math.pow(2, (m - 69) / 12);

export const NOTE_NAMES = ['C', 'C#', 'D', 'D#', 'E', 'F', 'F#', 'G', 'G#', 'A', 'A#', 'B'];
const pc = (n: number) => ((Math.round(n) % 12) + 12) % 12;
/** "A3" for MIDI 57 (C4 = 60). */
export const noteName = (m: number) => NOTE_NAMES[pc(m)] + (Math.floor(Math.round(m) / 12) - 1);
export const isBlack = (m: number) => [1, 3, 6, 8, 10].includes(pc(m));

/** Bit k set = pitch class k above the key is in the scale (pitch.rs `scale_mask`). */
export function scaleMask(scale: number): number {
  switch (Math.round(scale)) {
    case 1: return 0b1010_1011_0101; // major
    case 2: return 0b0101_1010_1101; // natural minor
    case 3: return 0b0010_1001_0101; // major pentatonic
    case 4: return 0b0100_1010_1001; // minor pentatonic
    default: return 0b1111_1111_1111;
  }
}
export const inScale = (m: number, key: number, mask: number) => (mask & (1 << ((((Math.round(m) - Math.round(key)) % 12) + 12) % 12))) !== 0;

/** Nearest allowed MIDI note to `m` (pitch.rs `snap`). */
export function snapNote(m: number, key: number, mask: number): number {
  const base = Math.round(m);
  let best = base, bd = Infinity;
  for (let d = -6; d <= 6; d++) {
    const n = base + d;
    if (!inScale(n, key, mask)) continue;
    const dist = Math.abs(n - m);
    if (dist < bd) { bd = dist; best = n; }
  }
  return best;
}

// ---------------------------------------------------------------- scrolling pitch graph

/** x of a sample taken at `t` (s) when the right edge is `now` and the window shows `secs`. */
export const graphX = (t: number, now: number, W: number, secs: number) => W - (now - t) / secs * W;
/** y of a MIDI pitch when the view is centred on `center` and shows `span` semitones. */
export const graphY = (m: number, center: number, span: number, H: number) => H / 2 - (m - center) / span * H;
export const graphMidi = (y: number, center: number, span: number, H: number) => center + (H / 2 - y) / H * span;

/**
 * Critically damped follower: the view centre glides toward `target` without overshoot.
 * Time-based (dt in s) so motion is the same at any frame rate. Returns [pos, vel].
 */
export function follow(pos: number, vel: number, target: number, dt: number, omega = 3.2): [number, number] {
  // exact solution of x'' = -2ωx' - ω²(x - target) over dt
  const x = pos - target, e = Math.exp(-omega * dt);
  const nx = (x + (vel + omega * x) * dt) * e;
  const nv = (vel - omega * (vel + omega * x) * dt) * e;
  return [target + nx, nv];
}

// ---------------------------------------------------------------- clip tune edits

export const MIN_NOTE = 0.02;

/** `tune.notes` as the analysis segmented them (no edits). */
export function initTune(an: PitchAnalysis): ClipTune {
  return { notes: an.notes.map(n => ({ start: n.start, end: n.end, shift: 0, drift: 0, formant: 0, gain: 0 })), pencil: [] };
}

/** Median voiced pitch inside [start, end) — tune_render.rs `note_center`. */
export function noteCenter(an: PitchAnalysis, start: number, end: number): number | null {
  const i0 = Math.floor(start * an.rate), i1 = Math.min(Math.floor(end * an.rate), an.curve.length);
  const v: number[] = [];
  for (let i = Math.max(0, i0); i < i1; i++) { const x = an.curve[i]; if (x != null && !Number.isNaN(x)) v.push(x); }
  if (!v.length) return null;
  v.sort((a, b) => a - b);
  return v[v.length >> 1];
}

/** Pencil value at `t` if `t` is inside a drawn run (gaps > 30 ms end a run) — tune_render.rs `pencil_at`. */
export function pencilAt(p: [number, number][] | undefined, t: number): number | null {
  if (!p || p.length < 2) return null;
  let lo = 0, hi = p.length; // partition_point(q[0] <= t)
  while (lo < hi) { const m = (lo + hi) >> 1; if (p[m][0] <= t) lo = m + 1; else hi = m; }
  const i = lo;
  if (i === 0 || i >= p.length) return null;
  const a = p[i - 1], b = p[i];
  if (b[0] - a[0] > 0.03) return null;
  return a[1] + (b[1] - a[1]) * (t - a[0]) / Math.max(1e-6, b[0] - a[0]);
}

/** Index of the note containing `t`, or -1. Notes are sorted and do not overlap. */
export function noteAt(notes: NoteEdit[], t: number): number {
  let lo = 0, hi = notes.length - 1;
  while (lo <= hi) {
    const m = (lo + hi) >> 1, n = notes[m];
    if (t < n.start) hi = m - 1; else if (t >= n.end) lo = m + 1; else return m;
  }
  return -1;
}

/** The pitch the engine will render, per analysis frame (NaN = unvoiced; left untouched). */
export function editedCurve(an: PitchAnalysis, tune: ClipTune | null | undefined, centers?: (number | null)[]): Float32Array {
  const out = new Float32Array(an.curve.length);
  const notes = tune?.notes ?? [];
  const cs = centers ?? notes.map(n => noteCenter(an, n.start, n.end));
  let ni = 0;
  for (let f = 0; f < out.length; f++) {
    const o = an.curve[f];
    if (o == null || Number.isNaN(o)) { out[f] = NaN; continue; }
    const t = f / an.rate;
    while (ni < notes.length && notes[ni].end <= t) ni++;
    let target = o;
    const n = notes[ni];
    if (n && n.start <= t) {
      const c = cs[ni];
      target = c != null ? c + (o - c) * (1 - Math.min(1, Math.max(0, n.drift))) + n.shift / 100 : o + n.shift / 100;
    }
    const pv = pencilAt(tune?.pencil, t);
    out[f] = pv ?? target;
  }
  return out;
}

const cloneTune = (t: ClipTune): ClipTune => ({ notes: t.notes.map(n => ({ ...n })), pencil: (t.pencil ?? []).map(p => [p[0], p[1]] as [number, number]) });

/** Splits note `i` at `t`; both halves keep the note's edits. Null if either half would be < 20 ms. */
export function splitNote(tune: ClipTune, i: number, t: number): ClipTune | null {
  const n = tune.notes[i];
  if (!n || t - n.start < MIN_NOTE || n.end - t < MIN_NOTE) return null;
  const out = cloneTune(tune);
  out.notes.splice(i, 1, { ...n, end: t }, { ...n, start: t });
  return out;
}

/** Index i such that note i ends exactly where note i+1 starts, nearest to `t` within `tol` s; else -1. */
export function splitNear(notes: NoteEdit[], t: number, tol: number): number {
  let best = -1, bd = tol;
  for (let i = 0; i + 1 < notes.length; i++) {
    if (Math.abs(notes[i].end - notes[i + 1].start) > 1e-6) continue;
    const d = Math.abs(notes[i].end - t);
    if (d <= bd) { bd = d; best = i; }
  }
  return best;
}

/** Merges note i with i+1 (they must touch). The merged note keeps note i's edits. */
export function mergeNotes(tune: ClipTune, i: number): ClipTune | null {
  const a = tune.notes[i], b = tune.notes[i + 1];
  if (!a || !b || Math.abs(a.end - b.start) > 1e-6) return null;
  const out = cloneTune(tune);
  out.notes.splice(i, 2, { ...a, end: b.end });
  return out;
}

/** Applies `patch(note)` to the notes in `idx`. */
export function editNotes(tune: ClipTune, idx: Iterable<number>, patch: (n: NoteEdit, i: number) => Partial<NoteEdit>): ClipTune {
  const out = cloneTune(tune);
  for (const i of idx) { const n = out.notes[i]; if (n) Object.assign(n, patch(n, i)); }
  return out;
}

/** Main tool: a note centred at `center` dragged by `rawCents` → shift snapped to the scale (free with `free`). */
export function dragShift(center: number, startShift: number, rawCents: number, key: number, mask: number, free: boolean): number {
  const want = startShift + rawCents;
  if (free) return Math.round(want);
  return Math.round((snapNote(center + want / 100, key, mask) - center) * 100);
}

/**
 * "Popraw wysokość" macro: move each note's centre `centerPct` of the way to the nearest scale
 * note and flatten `driftPct` of its drift. Returns the new tune.
 */
export function correctPitch(tune: ClipTune, an: PitchAnalysis, idx: Iterable<number>, centerPct: number, driftPct: number, key: number, mask: number): ClipTune {
  return editNotes(tune, idx, n => {
    const c = noteCenter(an, n.start, n.end);
    if (c == null) return {};
    const target = snapNote(c, key, mask);
    return { shift: Math.round((target - c) * 100 * centerPct / 100), drift: Math.min(1, Math.max(0, driftPct / 100)) };
  });
}

/**
 * Inserts a pencil stroke (points sorted by time, spaced < 30 ms) replacing whatever was drawn
 * in its time span.
 */
export function addPencil(pencil: [number, number][] | undefined, stroke: [number, number][]): [number, number][] {
  if (stroke.length < 2) return (pencil ?? []).slice();
  const s = stroke.slice().sort((a, b) => a[0] - b[0]);
  const t0 = s[0][0], t1 = s[s.length - 1][0];
  const kept = (pencil ?? []).filter(p => p[0] < t0 - 1e-4 || p[0] > t1 + 1e-4);
  return [...kept, ...s].sort((a, b) => a[0] - b[0]);
}
/** Removes pencil points within [t0, t1]. */
export const erasePencil = (pencil: [number, number][] | undefined, t0: number, t1: number) => (pencil ?? []).filter(p => p[0] < t0 || p[0] > t1);

/** True when a tune has no audible edit (so the clip plays the plain source). */
export function tuneIsNeutral(t: ClipTune | null | undefined): boolean {
  if (!t) return true;
  return !(t.pencil?.length) && t.notes.every(n => !n.shift && !n.drift && !n.formant && !n.gain);
}
