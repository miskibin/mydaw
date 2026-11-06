// A deterministic synthetic sung vocal for the in-browser mock: a melody in A minor with
// scoops, drift, vibrato, legato glides, breaths and sibilants. It feeds `pitch_analyze`,
// the waveform peaks of "vocal" sources and the live meters of the vocal processors.
import type { NoteBlob, PitchAnalysis } from './types';

export const VOCAL_RATE = 200;

function rng(seed: number) {
  let s = (seed % 2147483646) + 1;
  return () => (s = (s * 16807) % 2147483647) / 2147483647;
}

export interface Vocal extends PitchAnalysis {
  dur: number;
  /** [start, end, kind] of unvoiced stretches with audible noise: 's' sibilant, 'b' breath. */
  noise: [number, number, 's' | 'b'][];
}

const cache = new Map<string, Vocal>();

/** The vocal for `seed`, `dur` seconds long (cached). */
export function synthVocal(seed: number, dur: number): Vocal {
  const key = seed + ':' + dur.toFixed(3);
  const hit = cache.get(key); if (hit) return hit;
  const r = rng(seed);
  const n = Math.max(1, Math.round(dur * VOCAL_RATE));
  const curve: (number | null)[] = new Array(n).fill(null);
  const levels = new Array<number>(n).fill(0);
  const notes: NoteBlob[] = [];
  const noise: Vocal['noise'] = [];
  // A natural minor around A3..E4
  const pool = [57, 59, 60, 62, 64, 65, 67, 69];
  let t = 0.35, idx = 3, phrase = 0;
  while (t < dur - 0.4) {
    // a phrase of 3..6 notes, some legato
    const len = 3 + Math.floor(r() * 4);
    let prevEnd = -1, prevPitch = 0;
    for (let k = 0; k < len && t < dur - 0.4; k++) {
      idx = Math.max(0, Math.min(pool.length - 1, idx + Math.round((r() - 0.5) * 3.4)));
      const target = pool[idx];
      const nd = 0.22 + r() * 0.7;
      const legato = k > 0 && r() < 0.45;
      const start = legato ? prevEnd : t;
      const end = Math.min(dur - 0.05, start + nd);
      const detune = (r() - 0.5) * 0.7;         // ± 35 cents off
      const scoop = legato ? 0 : -(0.5 + r() * 1.0); // semitones below at the onset
      const vib = 0.18 + r() * 0.3, vibHz = 5.2 + r() * 0.9;
      const drift = (r() - 0.5) * 0.4;
      const peak = 0.55 + r() * 0.4;
      const f0 = Math.floor(start * VOCAL_RATE), f1 = Math.min(n, Math.floor(end * VOCAL_RATE));
      for (let f = f0; f < f1; f++) {
        const u = f / VOCAL_RATE - start, L = end - start;
        let m = target + detune + drift * (u / L);
        if (legato) m += (prevPitch - target - detune) * Math.exp(-u / 0.045);
        else m += scoop * Math.exp(-u / 0.06);
        if (u > 0.22) m += vib * Math.min(1, (u - 0.22) / 0.25) * Math.sin(2 * Math.PI * vibHz * u);
        curve[f] = m;
        const env = Math.min(1, u / 0.035) * Math.min(1, (L - u) / 0.06) * (0.82 + 0.18 * Math.cos(u * 3.1));
        levels[f] = peak * Math.max(0, env);
      }
      const v = curve.slice(f0, f1).filter((x): x is number => x != null).sort((a, b) => a - b);
      if (v.length > 4) notes.push({ start: f0 / VOCAL_RATE, end: f1 / VOCAL_RATE, pitch: v[v.length >> 1], level: peak });
      prevEnd = end; prevPitch = target + detune;
      t = end;
      if (!legato || k === len - 1) {
        // consonant / breath gap after the note
        const g = 0.06 + r() * 0.16;
        if (r() < 0.55) noise.push([t + 0.01, t + g - 0.01, 's']);
        t += g;
      }
    }
    // breath between phrases
    const br = 0.3 + r() * 0.4;
    noise.push([t + 0.05, t + br - 0.05, 'b']);
    t += br;
    phrase++;
  }
  for (const [a, b, kind] of noise) {
    for (let f = Math.floor(a * VOCAL_RATE); f < Math.min(n, b * VOCAL_RATE); f++) if (curve[f] == null) levels[f] = kind === 's' ? 0.22 : 0.07;
  }
  void phrase;
  const out: Vocal = { rate: VOCAL_RATE, curve, levels, notes, dur, noise };
  cache.set(key, out);
  return out;
}

/** Waveform peaks (100 [min,max] pairs per second) of the vocal. */
export function vocalPeaks(v: Vocal): Float32Array {
  const n = Math.max(1, Math.round(v.dur * 100)), out = new Float32Array(n * 2), r = rng(17);
  for (let i = 0; i < n; i++) {
    const f = Math.min(v.levels.length - 1, Math.floor(i / 100 * VOCAL_RATE));
    const voiced = v.curve[f] != null;
    const a = Math.min(1, v.levels[f] * (voiced ? 0.85 + 0.15 * r() : 0.5 + r()));
    out[2 * i] = -a * (0.88 + 0.12 * r()); out[2 * i + 1] = a;
  }
  return out;
}

export interface VocalSample { midi: number | null; level: number; sib: number }
/** The vocal at time `t` (s): pitch (null = unvoiced), level 0..1, sibilance 0..1. */
export function vocalAt(v: Vocal, t: number): VocalSample {
  const tt = ((t % v.dur) + v.dur) % v.dur;
  const f = Math.min(v.curve.length - 1, Math.floor(tt * VOCAL_RATE));
  let sib = 0;
  for (const [a, b, k] of v.noise) if (k === 's' && tt >= a && tt < b) { sib = Math.min(1, Math.min(tt - a, b - tt) / 0.015); break; }
  return { midi: v.curve[f], level: v.levels[f], sib };
}
