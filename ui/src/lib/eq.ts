// EQ response math — a port of crates/engine/src/dsp.rs (`Biquad::set`, `Eq::design`) so the
// window draws exactly what the engine does: RBJ cookbook biquads, cut filters as cascades
// (user Q on the first stage, 0.707 on the rest), notch = bell at −60 dB with Q ≥ 2,
// tilt = high shelf (Q 0.5, gain g) followed by −g/2 dB.
import type { BandType, EqBand, Placement } from '../api/types';

export const fx2x = (f: number, W: number) => Math.log(f / 20) / Math.log(1000) * W;
export const x2f = (x: number, W: number) => 20 * Math.pow(1000, x / W);

export const SR = 48000;
const clampN = (v: number, a: number, b: number) => Math.min(b, Math.max(a, v));

export const isCut = (t: BandType) => t === 'lowcut' || t === 'highcut';
/** Bands whose gain means something (bell, shelves, tilt). */
export const hasGain = (t: BandType) => t === 'peak' || t === 'lowshelf' || t === 'highshelf' || t === 'tilt';
/** Bands that can be dynamic (catalog.rs: "any bell/shelf/tilt"). */
export const canBeDynamic = hasGain;
export const placeOf = (b: EqBand): Placement => b.place ?? 'stereo';
export const slopeOf = (b: EqBand) => b.slope ?? 12;
/** Cascade length of a band (catalog.rs `param_block`: slope/12 clamped to 1..4; only cuts cascade). */
export const stagesOf = (b: EqBand) => isCut(b.type) ? clampN(Math.floor(slopeOf(b) / 12), 1, 4) : 1;

type Kind = 'lp' | 'hp' | 'bp' | 'peak' | 'ls' | 'hs';

/** RBJ biquad, normalised [b0,b1,b2,a1,a2] (a0 = 1), as `Biquad::set`. */
export function rbj(t: Kind, f: number, q: number, gainDb: number, sr = SR): [number, number, number, number, number] {
  f = clampN(f, 5, sr * 0.49);
  const w = 2 * Math.PI * f / sr, sw = Math.sin(w), cw = Math.cos(w);
  q = Math.max(0.05, q);
  const alpha = sw / (2 * q), a = Math.pow(10, gainDb / 40);
  let b0: number, b1: number, b2: number, a0: number, a1: number, a2: number;
  switch (t) {
    case 'lp': [b0, b1, b2, a0, a1, a2] = [(1 - cw) / 2, 1 - cw, (1 - cw) / 2, 1 + alpha, -2 * cw, 1 - alpha]; break;
    case 'hp': [b0, b1, b2, a0, a1, a2] = [(1 + cw) / 2, -(1 + cw), (1 + cw) / 2, 1 + alpha, -2 * cw, 1 - alpha]; break;
    case 'bp': [b0, b1, b2, a0, a1, a2] = [alpha, 0, -alpha, 1 + alpha, -2 * cw, 1 - alpha]; break;
    case 'peak': [b0, b1, b2, a0, a1, a2] = [1 + alpha * a, -2 * cw, 1 - alpha * a, 1 + alpha / a, -2 * cw, 1 - alpha / a]; break;
    case 'ls': {
      const s = 2 * Math.sqrt(a) * alpha;
      [b0, b1, b2, a0, a1, a2] = [a * ((a + 1) - (a - 1) * cw + s), 2 * a * ((a - 1) - (a + 1) * cw), a * ((a + 1) - (a - 1) * cw - s),
        (a + 1) + (a - 1) * cw + s, -2 * ((a - 1) + (a + 1) * cw), (a + 1) + (a - 1) * cw - s];
      break;
    }
    case 'hs': {
      const s = 2 * Math.sqrt(a) * alpha;
      [b0, b1, b2, a0, a1, a2] = [a * ((a + 1) + (a - 1) * cw + s), -2 * a * ((a - 1) + (a + 1) * cw), a * ((a + 1) + (a - 1) * cw - s),
        (a + 1) - (a - 1) * cw + s, 2 * ((a - 1) - (a + 1) * cw), (a + 1) - (a - 1) * cw - s];
      break;
    }
  }
  return [b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0];
}

/** A band as the engine designs it: biquad stages and a flat output offset (tilt). */
export interface Design { stages: [number, number, number, number, number][]; offsetDb: number }

/** `Eq::design` for one band, with an extra (dynamic) gain in dB. */
export function design(b: EqBand, extraDb = 0, sr = SR): Design {
  const f = clampN(b.f, 10, 22000), q = clampN(b.q, 0.025, 40), g = clampN(b.g, -30, 30) + extraDb;
  const n = stagesOf(b);
  const cascade = (k: Kind) => Array.from({ length: n }, (_, i) => rbj(k, f, i === 0 ? q : 0.707, 0, sr));
  switch (b.type) {
    case 'lowcut': return { stages: cascade('hp'), offsetDb: 0 };
    case 'highcut': return { stages: cascade('lp'), offsetDb: 0 };
    case 'lowshelf': return { stages: [rbj('ls', f, q, g, sr)], offsetDb: 0 };
    case 'highshelf': return { stages: [rbj('hs', f, q, g, sr)], offsetDb: 0 };
    case 'notch': return { stages: [rbj('peak', f, Math.max(q, 2), -60, sr)], offsetDb: 0 };
    case 'bandpass': return { stages: [rbj('bp', f, q, 0, sr)], offsetDb: 0 };
    case 'tilt': return { stages: [rbj('hs', f, 0.5, g, sr)], offsetDb: -g / 2 };
    default: return { stages: [rbj('peak', f, q, g, sr)], offsetDb: 0 };
  }
}

/** Frequencies with their precomputed trig terms (so per-frame curves are cheap). */
export class FreqGrid {
  readonly c1: Float64Array; readonly s1: Float64Array; readonly c2: Float64Array; readonly s2: Float64Array;
  constructor(readonly f: ArrayLike<number>, sr = SR) {
    const n = f.length;
    this.c1 = new Float64Array(n); this.s1 = new Float64Array(n); this.c2 = new Float64Array(n); this.s2 = new Float64Array(n);
    for (let i = 0; i < n; i++) {
      const w = 2 * Math.PI * f[i] / sr;
      this.c1[i] = Math.cos(w); this.s1[i] = Math.sin(w); this.c2[i] = Math.cos(2 * w); this.s2[i] = Math.sin(2 * w);
    }
  }
  get length() { return this.f.length; }
}
const gridCache = new WeakMap<object, FreqGrid>();
function gridOf(freqs: ArrayLike<number> | FreqGrid): FreqGrid {
  if (freqs instanceof FreqGrid) return freqs;
  let g = gridCache.get(freqs as object);
  if (!g || g.length !== freqs.length) { g = new FreqGrid(freqs); gridCache.set(freqs as object, g); }
  return g;
}

/** Adds the response (dB) of a design to `out` (`Biquad::response_db`). */
export function addDesign(d: Design, grid: FreqGrid, out: Float32Array | Float64Array, scale = 1) {
  for (const [b0, b1, b2, a1, a2] of d.stages) {
    for (let i = 0; i < grid.length; i++) {
      const c1 = grid.c1[i], s1 = grid.s1[i], c2 = grid.c2[i], s2 = grid.s2[i];
      const nr = b0 + b1 * c1 + b2 * c2, ni = -(b1 * s1 + b2 * s2);
      const dr = 1 + a1 * c1 + a2 * c2, di = -(a1 * s1 + a2 * s2);
      out[i] += scale * 10 * Math.log10(Math.max(1e-30, (nr * nr + ni * ni) / (dr * dr + di * di)));
    }
  }
  if (d.offsetDb) for (let i = 0; i < grid.length; i++) out[i] += scale * d.offsetDb;
}

/** Response of a single band at the given frequencies (0 dB when off). `extraDb` = live dynamic gain. */
export function bandCurve(b: EqBand, freqs: ArrayLike<number> | FreqGrid, out: Float32Array, extraDb = 0): Float32Array {
  out.fill(0);
  if (!b.on) return out;
  addDesign(design(b, extraDb), gridOf(freqs), out);
  return out;
}

/** The signal path a curve describes: stereo bands affect every channel; L/R/M/S bands only their own. */
export type Channel = Placement;
export const bandInChannel = (b: EqBand, ch: Channel) => { const p = placeOf(b); return p === 'stereo' || p === ch; };

/**
 * Total response of the EQ (dB) + output gain on one channel (default: what stereo bands do).
 * `dyn[k]`, if given, is band k's live dynamic gain offset (telemetry `meters[k]`).
 */
export function eqCurve(bands: EqBand[], out: number, freqs: ArrayLike<number> | FreqGrid, dst: Float32Array, ch: Channel = 'stereo', dyn?: ArrayLike<number>): Float32Array {
  dst.fill(out);
  const grid = gridOf(freqs);
  bands.forEach((b, k) => {
    if (!b.on || !bandInChannel(b, ch)) return;
    addDesign(design(b, b.dynamic && dyn ? dyn[k] ?? 0 : 0), grid, dst);
  });
  return dst;
}

/** Placements other than stereo that have at least one enabled band (each gets its own dashed curve). */
export function usedPlacements(bands: EqBand[]): Placement[] {
  const s = new Set<Placement>();
  for (const b of bands) if (b.on && placeOf(b) !== 'stereo') s.add(placeOf(b));
  return (['left', 'right', 'mid', 'side'] as Placement[]).filter(p => s.has(p));
}

/** Where a band's handle sits vertically (dB): its gain for bell/shelf/tilt, the curve level for the rest. */
export function handleDb(b: EqBand): number {
  if (hasGain(b.type)) return b.type === 'tilt' ? b.g / 2 : b.g;
  return 0;
}

/** A sensible fresh band for a click at (f, g). */
export function newBand(type: BandType, f: number, g: number): EqBand {
  return { on: true, type, f, g: hasGain(type) ? g : 0, q: type === 'peak' ? 1 : type === 'notch' ? 6 : type === 'bandpass' ? 2 : 0.71, slope: 12, place: 'stereo' };
}
