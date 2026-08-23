/**
 * The four modulation effects' frequency responses, computed in the webview.
 *
 * Same standing as {@link ./eq.ts}: **a drawing of the filter the engine is set
 * to run, not a measurement of the audio.** The engine publishes parameter
 * values and (for the auto-filter) the cutoff its LFO and envelope have arrived
 * at; it does not publish a curve. So the formulas here are transcriptions of
 * the ones in `plugins/effects/src/{flanger,phaser,autofilter}.rs`, and every
 * surface that draws them says so.
 *
 * All three use the same trick to stay honest about a *digital* filter: the
 * bilinear transform maps the analogue frequency axis onto `tan(πf/fs)`, so a
 * response evaluated at the warped ratio `tan(πf/fs) / tan(πfc/fs)` is the
 * response the running filter actually has, including the way its features
 * crowd together as they approach Nyquist.
 *
 * No DOM, no backend, no Solid: the maths is testable on its own, and
 * `modulation.test.ts` checks it against the numbers the Rust tests assert.
 */

/** The rate a curve is drawn at when no device has opened one. */
const FALLBACK_SAMPLE_RATE = 48_000;

function rateOf(sampleRate: number): number {
  return Number.isFinite(sampleRate) && sampleRate > 1 ? sampleRate : FALLBACK_SAMPLE_RATE;
}

/** `20·log10`, floored so silence is a number rather than `-Infinity`. */
export function toDb(magnitude: number): number {
  return 20 * Math.log10(Math.max(1e-6, magnitude));
}

/** One sample of the engine's LFO: a sine of the phase, in turns. */
export function lfoSine(phase: number): number {
  return Math.sin(2 * Math.PI * (phase - Math.floor(phase)));
}

// ---------------------------------------------------------------------------
// Flanger: a delay comb, with feedback
// ---------------------------------------------------------------------------

/**
 * The flanger's magnitude at `hz`, in dB.
 *
 * `H = (1−mix) + mix · z⁻ᵈ/(1 − fb·z⁻ᵈ)` with `z⁻ᵈ = e^{−j2πfd}`: the dry path
 * plus the delayed one, whose feedback loop turns the FIR comb's gentle notches
 * into a resonant one. `delayMs` is the delay *in force*, which is the manual
 * setting plus wherever the LFO has swept to.
 */
export function flangerMagDb(
  hz: number,
  delayMs: number,
  feedback: number,
  mix: number,
): number {
  const theta = 2 * Math.PI * hz * (Math.max(0.01, delayMs) / 1000);
  const cos = Math.cos(theta);
  const sin = Math.sin(theta);
  // z⁻ᵈ = cos − j·sin
  const fb = Math.max(-0.99, Math.min(0.99, feedback));
  const denReal = 1 - fb * cos;
  const denImag = fb * sin;
  const denSq = denReal * denReal + denImag * denImag;
  // (cos − j sin) / (denReal + j denImag)
  const wetReal = (cos * denReal - sin * denImag) / denSq;
  const wetImag = (-sin * denReal - cos * denImag) / denSq;
  const dry = 1 - mix;
  const real = dry + mix * wetReal;
  const imag = mix * wetImag;
  return toDb(Math.hypot(real, imag));
}

/** The delay the flanger is reading at this LFO phase, in milliseconds. */
export function flangerDelayMs(manualMs: number, depthMs: number, phase: number): number {
  return manualMs + depthMs * 0.5 * (1 + lfoSine(phase));
}

// ---------------------------------------------------------------------------
// Phaser: a cascade of first-order all-pass stages, with feedback
// ---------------------------------------------------------------------------

/**
 * The phaser's magnitude at `hz`, in dB.
 *
 * Each stage contributes `(1 − jr)/(1 + jr)` with `r` the warped frequency
 * ratio: magnitude one, phase `−2·atan r`. `N` of them in a feedback loop, then
 * summed with the dry path, which is where the notches come from.
 */
export function phaserMagDb(
  hz: number,
  centreHz: number,
  stages: number,
  feedback: number,
  mix: number,
  sampleRate: number,
): number {
  const rate = rateOf(sampleRate);
  const warp = (f: number) => Math.tan((Math.PI * Math.min(rate * 0.49, Math.max(1, f))) / rate);
  const r = warp(hz) / warp(centreHz);
  // (1 − jr)/(1 + jr) in polar form: unit magnitude, angle −2·atan r.
  const angle = -2 * Math.atan(r) * Math.max(1, Math.round(stages));
  const apReal = Math.cos(angle);
  const apImag = Math.sin(angle);
  const fb = Math.max(0, Math.min(0.95, feedback));
  const denReal = 1 - fb * apReal;
  const denImag = -fb * apImag;
  const denSq = denReal * denReal + denImag * denImag;
  const wetReal = (apReal * denReal + apImag * denImag) / denSq;
  const wetImag = (apImag * denReal - apReal * denImag) / denSq;
  const real = 1 - mix + mix * wetReal;
  const imag = mix * wetImag;
  return toDb(Math.hypot(real, imag));
}

/** Where the phaser's `k`-th notch sits: `fc·tan((2k+1)π/2N)`, warped. */
export function phaserNotchHz(
  centreHz: number,
  stages: number,
  index: number,
  sampleRate: number,
): number {
  const rate = rateOf(sampleRate);
  const n = Math.max(1, Math.round(stages));
  const ratio = Math.tan(((2 * index + 1) * Math.PI) / (2 * n));
  if (ratio <= 0) return Number.NaN;
  const g = Math.tan((Math.PI * Math.min(rate * 0.49, Math.max(1, centreHz))) / rate);
  return (rate / Math.PI) * Math.atan(g * ratio);
}

/** The corner the phaser's cascade sits at for this LFO phase, in Hz. */
export function phaserCentreHz(
  centreHz: number,
  depthPercent: number,
  phase: number,
  maxOctaves = 2,
): number {
  return centreHz * 2 ** ((depthPercent / 100) * maxOctaves * lfoSine(phase));
}

// ---------------------------------------------------------------------------
// Auto-filter: a state-variable filter
// ---------------------------------------------------------------------------

/** Filter modes, in the order the engine's `mode` switch declares them. */
export type FilterMode = 'lowpass' | 'highpass' | 'bandpass';

export function filterMode(choice: number): FilterMode {
  if (Math.round(choice) === 1) return 'highpass';
  if (Math.round(choice) === 2) return 'bandpass';
  return 'lowpass';
}

/** `autofilter::MIN_Q` and `MAX_Q`, and the exponential map between them. */
export const MIN_Q = 0.7;
export const MAX_Q = 12;

export function resonanceQ(percent: number): number {
  const t = Math.min(1, Math.max(0, percent / 100));
  return MIN_Q * (MAX_Q / MIN_Q) ** t;
}

/** Inverse of {@link resonanceQ}: a Q back to a knob percentage. */
export function resonancePercent(q: number): number {
  const clamped = Math.min(MAX_Q, Math.max(MIN_Q, q));
  return (Math.log(clamped / MIN_Q) / Math.log(MAX_Q / MIN_Q)) * 100;
}

/**
 * The state-variable filter's magnitude at `hz`, in dB.
 *
 * `D = (1 − r²) + j·r/Q` with `r` the warped frequency ratio; the lowpass is
 * `1/D`, the highpass `r²/D`, and the bandpass `(r/Q)/D` -- normalised, like the
 * engine's, so its peak is unity instead of `Q`.
 */
export function svfMagDb(
  hz: number,
  cutoffHz: number,
  q: number,
  mode: FilterMode,
  sampleRate: number,
  mix = 1,
): number {
  const rate = rateOf(sampleRate);
  const warp = (f: number) => Math.tan((Math.PI * Math.min(rate * 0.49, Math.max(1, f))) / rate);
  const r = warp(hz) / warp(cutoffHz);
  const k = 1 / Math.max(0.3, q);
  const denReal = 1 - r * r;
  const denImag = k * r;
  const denSq = denReal * denReal + denImag * denImag;
  const numerator = mode === 'highpass' ? r * r : mode === 'bandpass' ? k * r : 1;
  // The numerator of each mode is real up to a factor of j, which the magnitude
  // does not see, so only the modulus is needed for the wet path...
  const wet = numerator / Math.sqrt(denSq);
  if (mix >= 1) return toDb(wet);
  // ...but a dry blend adds two complex numbers, so the phase matters again.
  const scale = numerator / denSq;
  const wetReal = mode === 'bandpass' ? scale * denImag : scale * denReal;
  const wetImag = mode === 'bandpass' ? scale * denReal : -scale * denImag;
  return toDb(Math.hypot(1 - mix + mix * wetReal, mix * wetImag));
}

/**
 * The cutoff in force for an LFO phase and an envelope, in Hz.
 *
 * The same `cutoff · 2^(lfo + env)` the engine computes, with the same octave
 * ceilings -- so the shaded sweep band an editor draws is the travel the audio
 * really has.
 */
export function modulatedCutoffHz(options: {
  cutoffHz: number;
  lfoAmount: number;
  phase: number;
  envAmount?: number;
  envelope?: number;
  maxLfoOctaves?: number;
  maxEnvOctaves?: number;
}): number {
  const lfo = ((options.lfoAmount / 100) * (options.maxLfoOctaves ?? 3)) * lfoSine(options.phase);
  const env =
    ((options.envAmount ?? 0) / 100) *
    (options.maxEnvOctaves ?? 4) *
    Math.min(1, Math.max(0, options.envelope ?? 0));
  return options.cutoffHz * 2 ** (lfo + env);
}

/** `autofilter::sync_rate_hz`: one LFO cycle per division. */
export const LFO_DIVISION_BEATS = [4, 2, 1, 0.5, 0.25, 0.125, 0.0625] as const;
export const LFO_DIVISION_LABELS = ['1/1', '1/2', '1/4', '1/8', '1/16', '1/32', '1/64'] as const;

export function syncRateHz(division: number, bpm: number): number {
  const index = Math.min(
    LFO_DIVISION_BEATS.length - 1,
    Math.max(0, Math.round(division)),
  );
  const beats = LFO_DIVISION_BEATS[index] ?? 1;
  if (!Number.isFinite(bpm) || bpm <= 0) return 0;
  return bpm / 60 / beats;
}
