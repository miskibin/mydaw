// Delay math shared by the view, the mock backend and tests (mirrors crates/engine/src/dsp/delay.rs).
import { clamp } from './format';

export interface Tap { ms: number; side: 0 | 1; amp: number }

/** Mirrors dsp::delay: resonance → SVF damping. */
const resK = (res: number) => Math.max(0.005, Math.SQRT2 * Math.pow(1 - clamp(res / 100, 0, 1), 1.5));

/** The delay's time in ms at `bpm` (dsp::delay::delay_seconds). */
export function delayMs(p: Record<string, number>, bpm: number): number {
  const s = (p.sync ?? 0) < 0.5 ? (p.beats ?? 0.75) * 60 / (bpm > 1 ? bpm : 120) : (p.time ?? 375) / 1000;
  return clamp(s, 0.0005, 4) * 1000;
}

/**
 * The echoes of a click, for drawing: time, side (0 left, 1 right) and level relative to the
 * click. Follows the model routing of dsp::delay; levels use the feedback and the threshold of
 * the loop drive (the loop filter's broadband loss is not modelled — it is drawn as fading tint).
 */
export function echoTaps(p: Record<string, number>, bpm: number, maxMs: number, maxN = 48): Tap[] {
  const model = Math.round(p.model ?? 1), T0 = delayMs(p, bpm);
  if (model === 3) return [];
  const fb = (p.fb ?? 35) / 100, wet = (p.mix ?? 25) / 100, inp = (p.input ?? 100) / 100;
  const th = Math.pow(10, (p.level ?? 0) / 20), off = (p.offset ?? 0) / 100, pan = (p.pan ?? -60) / 100;
  const out: Tap[] = [];
  const amp = (k: number, g: number) => wet * Math.min(th, inp * g * Math.pow(fb, k));
  for (let k = 0; k < maxN; k++) {
    if (model === 2) {
      // ping-pong: a source panned left starts on the right; balance pan on the input
      const gl = pan > 0 ? 1 - pan : 1, gr = pan < 0 ? 1 + pan : 1;
      const ms = T0 * (k + 1);
      if (ms > maxMs) break;
      const fromL: 0 | 1 = k % 2 === 0 ? 1 : 0, fromR: 0 | 1 = k % 2 === 0 ? 0 : 1;
      out.push({ ms, side: fromL, amp: amp(k, gl * 0.5) }, { ms, side: fromR, amp: amp(k, gr * 0.5) });
    } else {
      const lm = model === 0 ? 1 + Math.max(0, -off) : 1, rm = model === 0 ? 1 + Math.max(0, off) : 1;
      const lp = model === 1 ? Math.max(0, -off) * T0 : 0, rp = model === 1 ? Math.max(0, off) * T0 : 0;
      const l = Math.min(4000, T0 * lm) * (k + 1) + lp, r = Math.min(4000, T0 * rm) * (k + 1) + rp;
      if (l > maxMs && r > maxMs) break;
      if (l <= maxMs) out.push({ ms: l, side: 0, amp: amp(k, 1) });
      if (r <= maxMs) out.push({ ms: r, side: 1, amp: amp(k, 1) });
    }
  }
  return out.filter(t => t.amp > 0.004);
}

/** Loop gain bound (feedback × filter resonance peak), as the engine's tail estimate. */
export function loopGain(p: Record<string, number>): number {
  const ft = Math.round(p.ftype ?? 1), peak = ft === 0 ? 1 : Math.max(1, 1 / resK(p.res ?? 0));
  return (p.fb ?? 35) / 100 * peak;
}

/** dsp::delay::shape */
export function shapeCurve(x: number, dist: number, th: number, knee: number, sym: number): number {
  if (dist === 0) return clamp(x, -th, th);
  const t = x < 0 ? th * (1 + 3 * sym) : th, w = 0.02 + 0.88 * knee, u = Math.abs(x) / t, lin = 1 - w;
  const y = u <= lin ? u : lin + w * Math.tanh((u - lin) / w);
  return Math.sign(x) * y * t;
}
