// A melody from the project as the vocal's reference (crates/engine/src/melody.rs): which notes the
// playlist plays over an audio clip, on the clip's source time axis. The engine is the source of truth
// (`melody_reference`); this mirror serves the browser mock and the Piano roll's "vocal reference"
// button (which vocals a pattern plays under, and the source seconds a note selection covers).
import type { Clip, Id, MelodyNote, PitchAnalysis, PitchTarget, Project, RefTune } from '../api/types';
import { STRETCH_MIN } from '../api/types';
import { swingOffset } from './pattern';
import { curveCenter } from './pitch';

const SPB_STEPS = 4;
/** Timing slack between voice and melody, seconds (melody.rs `TOLERANCE`). */
export const MELODY_TOLERANCE = 0.15;

/** Where pattern notes land on an audio clip: each (pattern clip, repeat, note index) the playlist plays over it. */
function placements(p: Project, clip: Clip, channel: Id, only?: { pattern: Id; idx: Set<number> }) {
  const spb = 60 / Math.max(1, p.bpm), st = Math.max(STRETCH_MIN, clip.stretch ?? 1);
  const r0 = clip.offset * spb / st, r1 = (clip.offset + clip.len) * spb / st;
  const toSrc = (beat: number) => (beat - clip.start + clip.offset) * spb / st;
  const root = p.channels?.find(c => c.id === channel)?.root;
  const out: MelodyNote[] = [];
  if (root == null) return out;
  for (const pc of p.clips) {
    if (pc.kind !== 'pat' || pc.muted) continue;
    if (only && pc.ref !== only.pattern) continue;
    const lo = Math.max(pc.start, clip.start), hi = Math.min(pc.start + pc.len, clip.start + clip.len);
    if (hi <= lo) continue;
    const pat = p.patterns.find(x => x.id === pc.ref);
    if (!pat || pat.len <= 0) continue;
    const base = pc.start - pc.offset, endSteps = pat.len * SPB_STEPS;
    for (let k = Math.floor((lo - base) / pat.len) - 1; k <= Math.floor((hi - base) / pat.len); k++) {
      pat.notes.forEach((n, i) => {
        if (n.ch !== channel || n.step >= endSteps - 1e-4) return;
        if (only && !only.idx.has(i)) return;
        const nb = base + k * pat.len + (n.step + swingOffset(n.step, pat.swing ?? 0)) / SPB_STEPS;
        if (nb < pc.start || nb >= pc.start + pc.len) return;
        const ne = Math.min(nb + n.len / SPB_STEPS, pc.start + pc.len);
        const a = Math.max(toSrc(nb), r0), b = Math.min(toSrc(ne), r1);
        if (b - a < 0.01) return;
        out.push({ start: a, end: b, key: n.p ?? root });
      });
    }
  }
  out.sort((x, y) => x.start - y.start || x.key - y.key);
  return out.filter((n, i) => i === 0 || !(Math.abs(n.start - out[i - 1].start) < 1e-6 && n.key === out[i - 1].key));
}

/** melody.rs `collect`: the notes of `channel` over `clip`, seconds of its source; `span` keeps those overlapping it. */
export function collectMelody(p: Project, clip: Clip, channel: Id, span?: [number, number] | null): MelodyNote[] {
  const all = placements(p, clip, channel);
  return span ? all.filter(n => n.end > span[0] && n.start < span[1]) : all;
}

/** The source seconds a Piano roll selection (note indices of `pattern`) covers on `clip`, or null if none of it plays there. */
export function selectionSpan(p: Project, clip: Clip, channel: Id, pattern: Id, idx: Iterable<number>): [number, number] | null {
  const ns = placements(p, clip, channel, { pattern, idx: new Set(idx) });
  if (!ns.length) return null;
  return [Math.min(...ns.map(n => n.start)), Math.max(...ns.map(n => n.end))];
}

/** Audio clips the playlist plays while `pattern` plays (the vocals a melody in it can be a reference for), by start. */
export function vocalsUnder(p: Project, pattern: Id): Clip[] {
  const pcs = p.clips.filter(c => c.kind === 'pat' && c.ref === pattern && !c.muted);
  return p.clips.filter(c => c.kind === 'audio' && pcs.some(pc => pc.start < c.start + c.len && pc.start + pc.len > c.start)).sort((a, b) => a.start - b.start || a.track - b.track);
}

/** melody.rs `fit`: share of sung frames whose pitch class is within a semitone of a note sounding there. */
export function melodyFit(take: PitchAnalysis, region: [number, number], notes: MelodyNote[]): number {
  let sung = 0, hit = 0;
  for (let f = Math.floor(region[0] * take.rate); f < Math.min(take.curve.length, Math.floor(region[1] * take.rate)); f++) {
    const u = take.curve[f]; if (u == null || Number.isNaN(u)) continue;
    sung++;
    const t = f / take.rate;
    if (notes.some(n => n.start <= t && n.end > t && (() => { const d = (((u - n.key) % 12) + 12) % 12; return Math.min(d, 12 - d) <= 1; })())) hit++;
  }
  return sung ? hit / sung : 0;
}

/**
 * A simplified melody target for the browser mock (the engine also moves a melisma's step to where the
 * voice moves): each sung note takes the played note overlapping it most (in a chord, the one nearest the
 * voice), in the octave nearest the singing; notes nothing is played under stay as sung.
 */
export function melodyTarget(take: PitchAnalysis, spans: { start: number; end: number }[], notes: MelodyNote[], p: RefTune): PitchTarget {
  const n = take.curve.length;
  const num = (x: number | null | undefined) => (x == null || Number.isNaN(x) ? NaN : x);
  const pick = spans.map(s => {
    const c = curveCenter(take.curve, take.rate, s.start, s.end);
    const over = notes.map(m => ({ m, ov: Math.min(s.end, m.end) - Math.max(s.start, m.start) })).filter(x => x.ov > 0);
    if (c == null || !over.length) return null;
    const best = Math.max(...over.map(x => x.ov));
    return { s, c, keys: over.filter(x => x.ov >= best - 1e-6).map(x => x.m.key) };
  });
  const cost = (o: number) => pick.reduce((acc, x) => acc + (x ? Math.min(...x.keys.map(k => Math.abs(x.c - (k + o)))) * (x.s.end - x.s.start) : 0), 0);
  const octave = p.octave ?? [0, -12, 12, -24, 24, -36, 36].reduce((b, o) => (cost(o) < cost(b) - 1e-9 ? o : b), 0);
  const ref = new Array<number | null>(n).fill(null), target = take.curve.map(x => (x == null || Number.isNaN(x) ? null : x));
  const conf = new Array<number>(n).fill(0);
  for (const x of pick) {
    if (!x) continue;
    const key = x.keys.reduce((b, k) => (Math.abs(x.c - (k + octave)) < Math.abs(x.c - (b + octave)) ? k : b)) + octave;
    for (let f = Math.floor(x.s.start * take.rate); f < Math.min(n, Math.floor(x.s.end * take.rate)); f++) {
      const u = num(take.curve[f]); if (Number.isNaN(u)) continue;
      ref[f] = key; conf[f] = 1;
      target[f] = u + p.correction * (key + p.vibrato * (u - x.c) - u);
    }
  }
  const voiced = take.curve.filter(x => x != null && !Number.isNaN(x)).length, covered = conf.filter(c => c > 0).length;
  const reason = !notes.length ? 'the melody has no notes over this clip' : !covered ? 'no melody note plays while this take is sung' : null;
  return {
    rate: take.rate, start: 0, sourceF0: take.curve.slice(), referenceF0: ref, targetF0: target,
    alignmentConfidence: conf, refTime: Array.from({ length: n }, (_, f) => f / take.rate),
    octave, notes: notes.map(m => ({ start: m.start, end: m.end, pitch: m.key + octave, level: 0.8 })),
    score: voiced ? covered / voiced : 0, reason,
  };
}
