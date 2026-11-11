// The sampler's knob scales and its envelope's shape (pure; the channel window draws them).
import { SAMPLER, type SamplerState } from '../api/types';

const clamp01 = (v: number) => Math.min(1, Math.max(0, v));
/** Attack and decay: cubic, so the first third of the travel covers the short times that matter (0..74 ms of 2 s). */
const cubicTo = (ms: number, max: number) => Math.cbrt(clamp01(ms / max));
const cubicFrom = (t: number, max: number) => { const ms = max * clamp01(t) ** 3; return ms < 100 ? Math.round(ms * 10) / 10 : Math.round(ms); };
export const attackToT = (ms: number) => cubicTo(ms, SAMPLER.attack.max);
export const attackFromT = (t: number) => cubicFrom(t, SAMPLER.attack.max);
export const decayToT = (ms: number) => cubicTo(ms, SAMPLER.decay.max);
export const decayFromT = (t: number) => cubicFrom(t, SAMPLER.decay.max);
/** Release: logarithmic, 1 ms .. 5 s. */
export const releaseToT = (ms: number) => clamp01(Math.log(Math.max(SAMPLER.release.min, ms) / SAMPLER.release.min) / Math.log(SAMPLER.release.max / SAMPLER.release.min));
export const releaseFromT = (t: number) => { const ms = SAMPLER.release.min * (SAMPLER.release.max / SAMPLER.release.min) ** clamp01(t); return ms < 100 ? Math.round(ms * 10) / 10 : Math.round(ms); };

/** "0 ms", "12.5 ms", "350 ms", "1.20 s". */
export function msText(ms: number): string {
  if (ms >= 1000) return (ms / 1000).toFixed(2) + ' s';
  return (ms < 100 && ms % 1 ? ms.toFixed(1) : String(Math.round(ms))) + ' ms';
}

/**
 * The volume envelope over a part `seconds` long, as [position 0..1, level 0..1] points (the
 * shape `crates/engine/src/synth.rs` plays: linear attack, linear decay to the sustain level).
 * Empty when the envelope does nothing (no attack, no decay, full sustain) or the part has no length.
 */
export function envelopePoints(s: Pick<SamplerState, 'attack' | 'decay' | 'sustain'>, seconds: number): [number, number][] {
  if (!(seconds > 0) || (s.attack <= 0 && (s.decay <= 0 || s.sustain >= 1) && s.sustain >= 1)) return [];
  const a = s.attack / 1000 / seconds, d = s.decay / 1000 / seconds;
  /** Level at position u (0..1 of the part). */
  const at = (u: number) => (u < a ? u / a : u < a + d ? 1 - (1 - s.sustain) * (u - a) / d : s.sustain);
  const pts: [number, number][] = [[0, a > 0 ? 0 : 1]];
  if (a > 0) pts.push([Math.min(1, a), at(Math.min(1, a) - 1e-9)]);
  if (a < 1 && d > 0) pts.push([Math.min(1, a + d), a + d <= 1 ? s.sustain : at(1)]);
  else if (a < 1 && s.sustain < 1) pts.push([a, s.sustain]);
  // with no sustain the sound is over when the decay ends
  if (a + d < 1 && s.sustain > 0) pts.push([1, s.sustain]);
  return pts;
}
