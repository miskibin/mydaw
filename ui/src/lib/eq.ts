// EQ curve math (RBJ cookbook biquads), so the window can draw the response without WebAudio.
import type { EqBand } from '../api/types';

export const fx2x = (f: number, W: number) => Math.log(f / 20) / Math.log(1000) * W;
export const x2f = (x: number, W: number) => 20 * Math.pow(1000, x / W);

const SR = 48000;

/** Biquad coefficients [b0,b1,b2,a0,a1,a2] for a band. */
export function coeffs(b: EqBand, sr = SR): number[] {
  const w0 = 2 * Math.PI * Math.min(b.f, sr * 0.49) / sr, cw = Math.cos(w0), sw = Math.sin(w0);
  const q = Math.max(0.05, b.q), alpha = sw / (2 * q), A = Math.pow(10, b.g / 40);
  switch (b.type) {
    case 'lowcut': return [(1 + cw) / 2, -(1 + cw), (1 + cw) / 2, 1 + alpha, -2 * cw, 1 - alpha];
    case 'highcut': return [(1 - cw) / 2, 1 - cw, (1 - cw) / 2, 1 + alpha, -2 * cw, 1 - alpha];
    case 'peak': return [1 + alpha * A, -2 * cw, 1 - alpha * A, 1 + alpha / A, -2 * cw, 1 - alpha / A];
    case 'lowshelf': {
      const s = 2 * Math.sqrt(A) * alpha;
      return [A * ((A + 1) - (A - 1) * cw + s), 2 * A * ((A - 1) - (A + 1) * cw), A * ((A + 1) - (A - 1) * cw - s),
        (A + 1) + (A - 1) * cw + s, -2 * ((A - 1) + (A + 1) * cw), (A + 1) + (A - 1) * cw - s];
    }
    case 'highshelf': {
      const s = 2 * Math.sqrt(A) * alpha;
      return [A * ((A + 1) + (A - 1) * cw + s), -2 * A * ((A - 1) + (A + 1) * cw), A * ((A + 1) + (A - 1) * cw - s),
        (A + 1) - (A - 1) * cw + s, 2 * ((A - 1) - (A + 1) * cw), (A + 1) - (A - 1) * cw - s];
    }
  }
}

/** Magnitude response in dB of one biquad at frequency f. */
export function magDb(c: number[], f: number, sr = SR): number {
  const w = 2 * Math.PI * f / sr, cw = Math.cos(w), sw = Math.sin(w), c2 = Math.cos(2 * w), s2 = Math.sin(2 * w);
  const nr = c[0] + c[1] * cw + c[2] * c2, ni = -(c[1] * sw + c[2] * s2);
  const dr = c[3] + c[4] * cw + c[5] * c2, di = -(c[4] * sw + c[5] * s2);
  const m = Math.sqrt((nr * nr + ni * ni) / (dr * dr + di * di));
  return 20 * Math.log10(m || 1e-9);
}

/** Response of a single band at the given frequencies (0 dB when off). */
export function bandCurve(b: EqBand, freqs: Float32Array, out: Float32Array): Float32Array {
  if (!b.on) { out.fill(0); return out; }
  const c = coeffs(b);
  for (let i = 0; i < freqs.length; i++) out[i] = magDb(c, freqs[i]);
  return out;
}

/** Sum of all enabled bands + output gain. */
export function eqCurve(bands: EqBand[], out: number, freqs: Float32Array, dst: Float32Array): Float32Array {
  dst.fill(out);
  const tmp = new Float32Array(freqs.length);
  for (const b of bands) {
    if (!b.on) continue;
    bandCurve(b, freqs, tmp);
    for (let i = 0; i < dst.length; i++) dst[i] += tmp[i];
  }
  return dst;
}
