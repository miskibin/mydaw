// Pitch helpers shared by the Pitch Correct window and the pitch editor: note names and scales
// (a port of crates/engine/src/pitch.rs `scale_mask` / `snap`), the scrolling pitch-graph
// mapping, and the clip `tune` edit operations (mirroring crates/engine/src/tune_render.rs).
import type { ClipTune, NoteEdit, PitchAnalysis, PitchTarget, RefTune, TuneEdit } from '../api/types';

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

/** Median voiced value of `curve` inside [start, end) — tune_render.rs `note_center`. */
export function curveCenter(curve: ArrayLike<number | null>, rate: number, start: number, end: number): number | null {
  const i0 = Math.floor(start * rate), i1 = Math.min(Math.floor(end * rate), curve.length);
  const v: number[] = [];
  for (let i = Math.max(0, i0); i < i1; i++) { const x = curve[i]; if (x != null && !Number.isNaN(x)) v.push(x); }
  if (!v.length) return null;
  v.sort((a, b) => a - b);
  return v[v.length >> 1];
}
/** Median voiced pitch inside [start, end) of the analysis. */
export const noteCenter = (an: PitchAnalysis, start: number, end: number) => curveCenter(an.curve, an.rate, start, end);

/** Target pitch at analysis frame `f`, if the target covers it (reference.rs `PitchTarget::at`). */
export function targetAt(t: PitchTarget | null | undefined, f: number): number | null {
  if (!t) return null;
  const v = t.targetF0[f - t.start];
  return v == null || Number.isNaN(v) ? null : v;
}

/** The pitch edits start from: the Tune to Reference target where there is one, else the sung pitch. */
export function baseCurve(an: PitchAnalysis, target?: PitchTarget | null): Float32Array {
  const out = new Float32Array(an.curve.length);
  for (let f = 0; f < out.length; f++) {
    const o = an.curve[f];
    out[f] = o == null || Number.isNaN(o) ? NaN : targetAt(target, f) ?? o;
  }
  return out;
}

/** Note centres over the base pitch (what Main/Drift edits move). */
export const noteCenters = (an: PitchAnalysis, notes: NoteEdit[], target?: PitchTarget | null) => {
  const base = target ? baseCurve(an, target) : an.curve;
  return notes.map(n => curveCenter(base, an.rate, n.start, n.end));
};

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

/** The pitch the engine will render, per analysis frame (NaN = unvoiced; left untouched) — tune_render.rs
 *  `edited_curve`: from the target (Tune to Reference) or the sung pitch, then notes, then pencil. */
export function editedCurve(an: PitchAnalysis, tune: ClipTune | null | undefined, centers?: (number | null)[], target?: PitchTarget | null): Float32Array {
  const out = new Float32Array(an.curve.length);
  const notes = tune?.notes ?? [];
  const base = baseCurve(an, target);
  const cs = centers ?? notes.map(n => curveCenter(base, an.rate, n.start, n.end));
  let ni = 0;
  for (let f = 0; f < out.length; f++) {
    const o = base[f];
    if (Number.isNaN(o)) { out[f] = NaN; continue; }
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
  return retune(an, tune, out);
}

/** Retune speed (ms) at `t`: the note's own, else the reference's; null = immediate (pencil too) — tune_render.rs `speed_at`. */
function speedAt(tune: ClipTune, n: NoteEdit | undefined, t: number): number | null {
  if (pencilAt(tune.pencil, t) != null) return null;
  const s = n && n.start <= t && t < n.end && n.speed != null ? n.speed : tune.reference?.speed ?? null;
  return s != null && s > 0 ? s : null;
}

/** The correction (wanted − sung) follows its target with the note's retune speed: a one-pole lag reaching 95 %
 *  of a change in `speed` ms, starting from none after an unvoiced gap — tune_render.rs `edited_curve`. */
function retune(an: PitchAnalysis, tune: ClipTune | null | undefined, want: Float32Array): Float32Array {
  if (!tune || !(tune.notes.some(n => n.speed != null) || tune.reference?.speed != null)) return want;
  const dt = 1 / an.rate, notes = tune.notes;
  let d = 0, ni = 0;
  for (let f = 0; f < want.length; f++) {
    const o = an.curve[f];
    if (Number.isNaN(want[f]) || o == null) { d = 0; continue; }
    const t = f / an.rate;
    while (ni < notes.length && notes[ni].end <= t) ni++;
    const goal = want[f] - o, ms = speedAt(tune, notes[ni], t);
    d = ms == null ? goal : d + (goal - d) * (1 - Math.exp(-dt * 3000 / ms));
    want[f] = o + d;
  }
  return want;
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

// ---------------------------------------------------------------- editTune (tune_edit.rs)

/** FNV-1a 64 over the note edges in whole microseconds — tune_edit.rs `fingerprint`. */
export function segmentationFingerprint(spans: [number, number][]): string {
  let h = 0xcbf29ce484222325n;
  const eat = (x: number) => {
    let v = BigInt.asUintN(64, BigInt(x));
    for (let k = 0; k < 8; k++) { h ^= v & 0xffn; h = BigInt.asUintN(64, h * 0x100000001b3n); v >>= 8n; }
  };
  eat(spans.length);
  for (const [a, b] of spans) { eat(Math.round(a * 1e6)); eat(Math.round(b * 1e6)); }
  return h.toString(16).padStart(16, '0');
}

const TUNE_EDIT_KEYS = new Set(['base', 'init', 'clear', 'reset', 'notes', 'split', 'merge', 'erasePencil', 'pencil']);
const NOTE_PATCH_KEYS = new Set(['i', 'shift', 'shiftBy', 'drift', 'formant', 'gain', 'speed']);
const blankNote = (start: number, end: number): NoteEdit => ({ start, end, shift: 0, drift: 0, formant: 0, gain: 0 });

function checkValues(k: number, n: NoteEdit) {
  if (!(Number.isFinite(n.shift) && Math.abs(n.shift) <= 2400)) throw new Error(`note ${k}: shift must be within ±2400 cents`);
  if (!(Number.isFinite(n.formant) && Math.abs(n.formant) <= 2400)) throw new Error(`note ${k}: formant must be within ±2400 cents`);
  if (!(n.drift >= 0 && n.drift <= 1)) throw new Error(`note ${k}: drift must be 0..1`);
  if (!(n.gain >= -24 && n.gain <= 12)) throw new Error(`note ${k}: gain must be -24..12 dB`);
  if (n.speed != null && !(n.speed >= 0 && n.speed <= 1000)) throw new Error(`note ${k}: speed must be 0..1000 ms`);
}

/** The `editTune` op on a clip's tune (null = no edits) — a port of tune_edit.rs `apply` for the browser mock.
 *  Returns the new tune (null = nothing left to render); throws on an invalid edit, changing nothing. */
export function applyTuneEdit(tune: ClipTune | null | undefined, e: TuneEdit): ClipTune | null {
  for (const k of Object.keys(e)) if (!TUNE_EDIT_KEYS.has(k)) throw new Error(`unknown field "${k}"`);
  for (const p of e.notes ?? []) for (const k of Object.keys(p)) if (!NOTE_PATCH_KEYS.has(k)) throw new Error(`unknown field "${k}"`);
  const t: ClipTune = { notes: (tune?.notes ?? []).map(n => ({ ...n })), pencil: (tune?.pencil ?? []).map(p => [p[0], p[1]] as [number, number]) };
  if (tune?.reference) t.reference = tune.reference;
  if (!t.notes.length && e.init) {
    t.notes = e.init.map(s => blankNote(s[0], s[1]));
    t.notes.forEach((n, k) => {
      if (!(Number.isFinite(n.start) && Number.isFinite(n.end) && n.start >= 0 && n.end > n.start)) throw new Error(`init: note ${k} has an invalid range`);
      if (k > 0 && n.start < t.notes[k - 1].end - 1e-9) throw new Error(`init: notes ${k - 1} and ${k} overlap or are out of order`);
    });
  }
  const n = t.notes.length;
  if (e.base != null) {
    const now = segmentationFingerprint(t.notes.map(x => [x.start, x.end]));
    if (e.base !== now) throw new Error(`STALE_NOTES: the clip's notes changed since they were read (base ${e.base}, now ${now}); inspect it again`);
  }
  const idx = (i: number, what: string) => {
    if (Number.isInteger(i) && i >= 0 && i < n) return i;
    throw new Error(n === 0 ? `${what}: the clip has no notes yet; pass "init"` : `${what}: no note ${i} (the clip has ${n})`);
  };
  if (e.clear) {
    if ((e.reset?.length ?? 0) || (e.notes?.length ?? 0) || (e.split?.length ?? 0) || (e.merge?.length ?? 0)) throw new Error('"clear" removes every note edit; it cannot be combined with reset, notes, split or merge');
    t.notes = []; t.pencil = [];
  }
  for (const i of e.reset ?? []) { const k = idx(i, 'reset'); t.notes[k] = blankNote(t.notes[k].start, t.notes[k].end); }
  const patched = new Array<boolean>(n).fill(false);
  for (const p of e.notes ?? []) {
    const k = idx(p.i, 'notes');
    if (patched[k]) throw new Error(`notes: note ${k} is listed twice`);
    patched[k] = true;
    const nt = t.notes[k];
    if (p.shift != null) nt.shift = p.shift;
    if (p.shiftBy != null) nt.shift += p.shiftBy;
    if (p.drift != null) nt.drift = p.drift;
    if (p.formant != null) nt.formant = p.formant;
    if (p.gain != null) nt.gain = p.gain;
    if ('speed' in p) { if (p.speed == null) delete nt.speed; else nt.speed = p.speed; }
    checkValues(k, nt);
  }
  const cuts: [number, number][] = [];
  for (const sp of e.split ?? []) {
    const k = idx(sp.i, 'split'), nt = t.notes[k];
    if (!(Number.isFinite(sp.at) && sp.at - nt.start >= MIN_NOTE - 1e-9 && nt.end - sp.at >= MIN_NOTE - 1e-9)) throw new Error(`split: ${sp.at} s is not inside note ${k} with ${MIN_NOTE * 1000} ms to spare`);
    cuts.push([k, sp.at]);
  }
  cuts.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
  for (let j = 1; j < cuts.length; j++) if (cuts[j][0] === cuts[j - 1][0] && cuts[j][1] - cuts[j - 1][1] < MIN_NOTE - 1e-9) throw new Error(`split: two cuts in one note are closer than ${MIN_NOTE * 1000} ms`);
  const merges = [...new Set(e.merge ?? [])].sort((a, b) => a - b);
  for (const i of merges) {
    idx(i, 'merge');
    if (!(i + 1 < n)) throw new Error(`merge: note ${i} is the last one; it has no next note to merge with`);
    if (Math.abs(t.notes[i].end - t.notes[i + 1].start) > 1e-6) throw new Error(`merge: notes ${i} and ${i + 1} do not touch`);
    for (const j of [i, i + 1]) if (cuts.some(c => c[0] === j)) throw new Error(`note ${j} is both split and merged in one edit`);
    if (patched[i + 1]) throw new Error(`merge: note ${i + 1} is merged into note ${i}, its own edits in "notes" would be lost`);
  }
  const out: NoteEdit[] = [];
  for (let k = 0; k < t.notes.length;) {
    const nt = { ...t.notes[k] };
    let j = k;
    while (merges.includes(j)) { j++; nt.end = t.notes[j].end; }
    let from = nt.start;
    for (const c of cuts) if (c[0] === k) { out.push({ ...nt, start: from, end: c[1] }); from = c[1]; }
    out.push({ ...nt, start: from });
    k = j + 1;
  }
  t.notes = out;
  for (const r of e.erasePencil ?? []) {
    if (!(Number.isFinite(r[0]) && Number.isFinite(r[1]) && r[1] >= r[0])) throw new Error('erasePencil: each range is [from, to] seconds with to ≥ from');
    t.pencil = erasePencil(t.pencil, r[0], r[1]);
  }
  if (e.pencil?.length) {
    if (e.pencil.length < 2) throw new Error('pencil: a stroke needs at least 2 points');
    if (e.pencil.some(p => !(Number.isFinite(p[0]) && p[0] >= 0 && p[1] >= 0 && p[1] <= 127))) throw new Error('pencil: points are [seconds ≥ 0, midi 0..127]');
    t.pencil = addPencil(t.pencil, e.pencil);
  }
  return t.notes.length || t.pencil?.length || t.reference ? t : null;
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
export function correctPitch(tune: ClipTune, an: PitchAnalysis, idx: Iterable<number>, centerPct: number, driftPct: number, key: number, mask: number, speed: number | null = null): ClipTune {
  return editNotes(tune, idx, n => {
    const c = noteCenter(an, n.start, n.end);
    if (c == null) return {};
    const target = snapNote(c, key, mask);
    return { shift: Math.round((target - c) * 100 * centerPct / 100), drift: Math.min(1, Math.max(0, driftPct / 100)), speed };
  });
}

/** One-click correction styles (Strength %, Speed ms, Flatten %), the same words as the Pitch Correct effect's presets. */
export const CORRECT_PRESETS: { id: 'natural' | 'pop' | 'tight' | 'robot'; strength: number; speed: number; flatten: number }[] = [
  { id: 'natural', strength: 80, speed: 120, flatten: 20 },
  { id: 'pop', strength: 100, speed: 50, flatten: 50 },
  { id: 'tight', strength: 100, speed: 15, flatten: 80 },
  { id: 'robot', strength: 100, speed: 0, flatten: 100 },
];

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
  return !t.reference && !(t.pencil?.length) && t.notes.every(n => !n.shift && !n.drift && !n.formant && !n.gain);
}

// ---------------------------------------------------------------- Tune to Reference

export const DEFAULT_REF: Omit<RefTune, 'source'> = { correction: 1, vibrato: 1, influence: 1, octave: null, key: 0, scale: 0 };

/**
 * A simplified Tune to Reference for the browser mock: the reference is stretched linearly over
 * the take (the engine aligns with DTW instead), moved by the octave nearest to the take, and the
 * target follows the engine's note model — each note's centre goes `influence` of the way to the
 * reference note centre (the rest to the nearest scale note), the singer's own deviation from
 * the centre is kept by `vibrato`, and the result is mixed in by `correction`.
 */
export function simpleTarget(take: PitchAnalysis, ref: PitchAnalysis, notes: { start: number; end: number }[], p: RefTune): PitchTarget {
  const n = take.curve.length, k = ref.curve.length / Math.max(1, n);
  const num = (x: number | null | undefined) => (x == null || Number.isNaN(x) ? NaN : x);
  const r = Array.from({ length: n }, (_, f) => num(ref.curve[Math.min(ref.curve.length - 1, Math.floor(f * k))]));
  const diffs: number[] = [];
  for (let f = 0; f < n; f++) { const u = num(take.curve[f]); if (!Number.isNaN(u) && !Number.isNaN(r[f])) diffs.push(u - r[f]); }
  diffs.sort((a, b) => a - b);
  const octave = p.octave ?? (diffs.length ? 12 * Math.round(diffs[diffs.length >> 1] / 12) : 0);
  const mask = scaleMask(p.scale ?? 0), target = new Array<number | null>(n).fill(null);
  for (let f = 0; f < n; f++) { const u = num(take.curve[f]); target[f] = Number.isNaN(u) ? null : u; }
  for (const nt of notes) {
    const uc = curveCenter(take.curve, take.rate, nt.start, nt.end);
    const rc = curveCenter(r.map(x => x + octave), take.rate, nt.start, nt.end);
    if (uc == null) continue;
    const tc = rc == null ? uc : p.influence * rc + (1 - p.influence) * snapNote(uc, p.key ?? 0, mask);
    for (let f = Math.floor(nt.start * take.rate); f < Math.min(n, Math.floor(nt.end * take.rate)); f++) {
      const u = num(take.curve[f]); if (Number.isNaN(u)) continue;
      const want = tc + p.vibrato * (u - uc);
      target[f] = u + p.correction * (want - u);
    }
  }
  const voiced = diffs.length / Math.max(1, take.curve.filter(x => x != null).length);
  return {
    rate: take.rate, start: 0,
    sourceF0: take.curve.slice(), referenceF0: r.map(x => (Number.isNaN(x) ? null : x + octave)), targetF0: target,
    alignmentConfidence: r.map(x => (Number.isNaN(x) ? 0 : 1)), refTime: Array.from({ length: n }, (_, f) => f * k / ref.rate),
    octave, notes: ref.notes.map(b => ({ ...b, start: b.start / k, end: b.end / k, pitch: b.pitch + octave })),
    score: Math.min(1, voiced), reason: null,
  };
}

/** Stretches of the target (seconds of the source) where the alignment is too unsure to correct. */
export function lowConfidenceRuns(t: PitchTarget, below = 0.2): [number, number][] {
  const out: [number, number][] = [];
  let s = -1;
  for (let i = 0; i <= t.alignmentConfidence.length; i++) {
    const low = i < t.alignmentConfidence.length && t.alignmentConfidence[i] < below && t.sourceF0[i] != null;
    if (low && s < 0) s = i;
    else if (!low && s >= 0) { out.push([(t.start + s) / t.rate, (t.start + i) / t.rate]); s = -1; }
  }
  return out;
}
