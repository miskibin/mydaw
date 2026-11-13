// What the reverb display draws, from the same numbers the DSP uses (crates/engine/src/dsp/reverb_v2.rs):
// the space's delay lines, scaled by the decay, as an equivalent room; its early reflections; the
// decay per band. `api/reverbSpaces.json` is generated from the engine (tests/reverb_spaces.rs) and
// reverbSpace.test.ts checks this file reproduces the engine's sample values.
import data from '../api/reverbSpaces.json';

export interface SpaceInfo { name: string; lineMs: number[]; tRef: number; erMs: number; erGain: number; lateGain: number }
export const SPACES: SpaceInfo[] = data.spaces;
const [MIN_SCALE, MAX_SCALE] = data.scale;
export const PLATE = 0;
const SOUND_M_PER_S = 343;
export const MIN_DECAY = 0.2, MAX_DECAY = 6;

type Params = Record<string, number | undefined>;

/** reverb_v2.rs `derive`: decay per band and the network scale. */
export function derive(p: Params) {
  const space = Math.min(SPACES.length - 1, Math.max(0, Math.round(p.space ?? 0)));
  const sp = SPACES[space];
  const tMid = Math.min(MAX_DECAY, Math.max(MIN_DECAY, p.size ?? 1));
  const tone = Math.min(1, Math.max(-1, (p.tone ?? 0) / 100));
  const hf = tone < 0 ? 0.5 + 0.25 * tone : 0.5 + 0.35 * tone;
  const bass = Math.min(1.5, Math.max(0.5, p.bass ?? 1));
  const scale = Math.min(MAX_SCALE, Math.max(MIN_SCALE, Math.pow(tMid / sp.tRef, 0.4)));
  return { space, sp, tMid, tLow: tMid * bass, tHigh: tMid * hf, scale };
}

/** reverb_v2.rs `equivalent_room_m`: a cube whose mean free path (2L/3) is the mean line delay. */
export function roomM(p: Params): number {
  const d = derive(p);
  const mean = d.sp.lineMs.reduce((a, b) => a + b, 0) / d.sp.lineMs.length;
  return 1.5 * mean * d.scale / 1000 * SOUND_M_PER_S;
}

/** Length × width × height (m) of a room of that equivalent size: the cube reshaped to the space's typical proportions. */
const SHAPE: Record<number, [number, number, number]> = { 1: [1.35, 1, 0.6], 2: [1.7, 1.05, 0.62], 3: [1.15, 1, 0.65] };
export function roomDims(p: Params): [number, number, number] {
  const d = derive(p), side = roomM(p), s = SHAPE[d.space] ?? [1, 1, 1];
  const k = side / Math.cbrt(s[0] * s[1] * s[2]);
  return [s[0] * k, s[1] * k, s[2] * k];
}

/**
 * The impulse response's outline in time (s) and level (dB, the early and late parts on one scale):
 * the late tail starts when the early reflections have mostly passed and falls 60 dB in each band's
 * decay time. `early` pulses sit in the space's real span with the engine's −20 dB-over-the-span
 * slope (their exact placement is random per channel in the DSP; here it is evenly jittered).
 */
export function envelope(p: Params) {
  const d = derive(p), pre = Math.min(150, Math.max(0, p.pre ?? 0)) / 1000;
  const erS = d.sp.erMs / 1000;
  const erLvl = d.sp.erGain * Math.min(100, Math.max(0, p.early ?? 50)) / 50;
  const onset = pre + erS * 0.3;
  const lateDb = 20 * Math.log10(d.sp.lateGain);
  const early: { t: number; db: number }[] = [];
  if (erS > 0 && erLvl > 0) {
    const n = 24;
    for (let i = 0; i < n; i++) {
      const pos = (i + 0.5 + 0.4 * Math.sin(i * 2.399)) / n;
      early.push({ t: pre + pos * erS, db: 20 * Math.log10(erLvl * Math.exp(-2.3 * pos)) });
    }
  }
  return { ...d, pre, onset, lateDb, early, end: onset + Math.max(d.tMid, d.tLow) };
}

/** The decay a drag of the tail end to time `t` (s) means: the mid band reaching −60 dB there. */
export const decayAt = (p: Params, t: number) => Math.min(MAX_DECAY, Math.max(MIN_DECAY, t - envelope(p).onset));

/** A round time span the envelope axis shows: the longest band's decay plus some room, at least 0.5 s. */
export function timeSpan(p: Params): number {
  const e = envelope(p), need = Math.max(0.5, e.end * 1.12);
  return [0.5, 0.75, 1, 1.5, 2, 3, 4, 5, 6, 8].find(s => s >= need) ?? need;
}
