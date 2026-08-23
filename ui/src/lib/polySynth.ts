/**
 * Pictures of the poly synth, computed in the webview.
 *
 * The engine has the real oscillators, TPT SVF and linear ADSR in
 * `plugins/instruments/src/poly_synth.rs`. None of that is on the command
 * surface as a curve, so this file is a second implementation of the same
 * formulas: enough to draw what the knobs are set to do. The live overlay in
 * the editor is a measurement; these traces are a prediction.
 *
 * No DOM, no backend, no Solid.
 */

import { EQ_MAX_HZ, EQ_MIN_HZ, eqDbFromY, eqHzFromX } from './eq';

/** How far `envAmount` at ±100 % moves the cutoff, matching `ENV_OCTAVES`. */
export const ENV_OCTAVES = 4;

/** Display box for the filter curve; a Q of 12 is about +21.5 dB at the corner. */
export const FILTER_RANGE_DB = 24;

/** Engine clamp: `tan` blows up at Nyquist, so the SVF never aims above this. */
const SVF_NYQUIST_FRACTION = 0.2;
const SVF_CUTOFF_FLOOR_HZ = 10;
const SVF_Q_FLOOR = 0.4;
const SVF_Q_CEIL = 20;
const FALLBACK_SAMPLE_RATE = 48_000;

/** Log-time constant so a 1 ms attack still owns pixels next to a 4 s release. */
const TIME_TAU_MS = 80;
/** Added to every stage so a 1 ms attack is not a hairline against a 4 s release. */
const STAGE_PAD = 0.45;
/** Visual sustain plateau; the engine holds until note-off, which is not a knob. */
const HOLD_MS = 220;
const ATTACK_MS_RANGE: readonly [number, number] = [1, 2_000];
const DECAY_MS_RANGE: readonly [number, number] = [1, 4_000];
const RELEASE_MS_RANGE: readonly [number, number] = [1, 4_000];

export type AdsrHandle = 'attack' | 'decay' | 'sustain' | 'release';

export interface Adsr {
  attackMs: number;
  decayMs: number;
  sustain: number;
  releaseMs: number;
}

export interface AdsrPoint {
  x: number;
  y: number;
}

export interface AdsrLayout {
  points: Record<AdsrHandle, AdsrPoint>;
  noteOffX: number;
  path: string;
}

export interface OscSlot {
  wave: number;
  coarse: number;
  fine: number;
  mix: number;
}

export interface FilterPointer {
  cutoffHz: number;
  resonance: number;
}

/** Dedicated editor only when this channel is the poly synth *and* it published knobs. */
export function usesPolySynthEditor(sourceId: string | undefined, paramCount: number): boolean {
  return sourceId === 'poly-synth' && paramCount > 0;
}

/** Cutoff at full envelope, `cutoff * 2^((envAmount/100)*4)`. */
export function envCutoffHz(cutoffHz: number, envAmountPct: number): number {
  const cutoff = Number.isFinite(cutoffHz) ? cutoffHz : 0;
  const amount = Number.isFinite(envAmountPct) ? envAmountPct : 0;
  return cutoff * 2 ** ((amount / 100) * ENV_OCTAVES);
}

/** Whether the ghost sweep should be drawn. Zero is a real setting: the env does not move the filter. */
export function envSweepActive(envAmountPct: number): boolean {
  return Number.isFinite(envAmountPct) && Math.abs(envAmountPct) >= 0.5;
}

function sampleRateOrFallback(sampleRate: number): number {
  return Number.isFinite(sampleRate) && sampleRate > 1 ? sampleRate : FALLBACK_SAMPLE_RATE;
}

/**
 * TPT SVF lowpass magnitude in dB, analog prototype with bilinear prewarp.
 *
 * Matches `Svf::set`: `g = tan(π f / fs)`, `k = 1/Q`, then
 * `|H| = 1 / hypot(1 - (w/g)², k w/g)`.
 */
export function svfLowpassMagDb(
  hz: number,
  cutoffHz: number,
  q: number,
  sampleRate: number,
): number {
  const rate = sampleRateOrFallback(sampleRate);
  const nyquist = rate * 0.5;
  const nyquistCap = rate * SVF_NYQUIST_FRACTION;
  const cutoff = Math.min(nyquistCap, Math.max(SVF_CUTOFF_FLOOR_HZ, Number.isFinite(cutoffHz) ? cutoffHz : 0));
  const resonance = Math.min(SVF_Q_CEIL, Math.max(SVF_Q_FLOOR, Number.isFinite(q) && q > 0 ? q : 1));
  const freq = Math.min(nyquist * 0.99, Math.max(1, Number.isFinite(hz) ? hz : 1));
  const w = Math.tan((Math.PI * freq) / rate);
  const g = Math.tan((Math.PI * cutoff) / rate);
  if (g <= 0) return 0;
  const k = 1 / resonance;
  const ratio = w / g;
  const mag = 1 / Math.hypot(1 - ratio * ratio, k * ratio);
  return 20 * Math.log10(Math.max(1e-12, mag));
}

/** Log-spaced magnitudes from 20 Hz to 20 kHz, same abscissa as the EQ curve. */
export function svfLowpassCurveDb(
  cutoffHz: number,
  q: number,
  sampleRate: number,
  points: number,
): number[] {
  const count = Math.max(2, Math.trunc(points));
  const ratio = EQ_MAX_HZ / EQ_MIN_HZ;
  return Array.from({ length: count }, (_, index) =>
    svfLowpassMagDb(EQ_MIN_HZ * ratio ** (index / (count - 1)), cutoffHz, q, sampleRate),
  );
}

export const WAVE_SAW = 0;
export const WAVE_SQUARE = 1;
export const WAVE_SINE = 2;
export const WAVE_TRIANGLE = 3;
export const WAVE_NOISE = 4;
export const OSC_COUNT = 3;
export const MIX_AUDIBLE = 0.02;

/** Engine `pitch_ratio`: `2^(coarse/12 + fine/1200)`. */
export function pitchRatio(coarse: number, fine: number): number {
  const st = Number.isFinite(coarse) ? Math.min(24, Math.max(-24, coarse)) : 0;
  const ct = Number.isFinite(fine) ? Math.min(100, Math.max(-100, fine)) : 0;
  return 2 ** (st / 12 + ct / 1_200);
}

/** One sample of a classic analog shape. Phase is wrapped to 0..1. */
export function oscSample(wave: number, phase: number): number {
  const t = phase - Math.floor(phase);
  const shape = Number.isFinite(wave) ? Math.round(wave) : 0;
  if (shape === WAVE_SQUARE) return t < 0.5 ? -1 : 1;
  if (shape === WAVE_SINE) return Math.sin(t * Math.PI * 2);
  if (shape === WAVE_TRIANGLE) return t < 0.5 ? 4 * t - 1 : 3 - 4 * t;
  if (shape === WAVE_NOISE) {
    const x = Math.sin(t * 12.9898) * 43758.5453;
    return (x - Math.floor(x)) * 2 - 1;
  }
  return 2 * t - 1;
}

export interface OscillatorStack {
  layers: { id: number; samples: number[]; mix: number }[];
  mix: number[];
}

/**
 * One cycle of oscillator 1, with the other two drawn at their pitch ratios
 * so a -12 sub completes two cycles in the same box.
 */
export function oscillatorStack(oscs: readonly OscSlot[], samples = 96): OscillatorStack {
  const count = Math.max(2, Math.trunc(samples));
  const mix = new Array<number>(count).fill(0);
  const layers: OscillatorStack['layers'] = [];
  oscs.slice(0, OSC_COUNT).forEach((osc, id) => {
    const gain = Number.isFinite(osc.mix) ? Math.min(1, Math.max(0, osc.mix)) : 0;
    if (id > 0 && gain < MIX_AUDIBLE) return;
    const ratio = pitchRatio(osc.coarse, osc.fine);
    const trace: number[] = [];
    for (let index = 0; index < count; index += 1) {
      const value = oscSample(osc.wave, (index / count) * ratio) * (id === 0 && gain < MIX_AUDIBLE ? 1 : gain);
      trace.push(value);
      mix[index] = (mix[index] ?? 0) + value;
    }
    layers.push({ id, samples: trace, mix: id === 0 ? 1 : gain });
  });
  return { layers, mix };
}

/** Polyline for a cycle, ±2 mapped into a box of `width`×`height`. */
export function oscillatorPath(samples: readonly number[], width: number, height: number): string {
  return signalPath(samples, width, height, 0, 2);
}

/**
 * Peak-normalise a scope hop so a quiet note still draws, instead of vanishing
 * under a noise gate. A single sample cannot make a polyline.
 */
export function normalisedScope(samples: readonly number[] | null | undefined): number[] | null {
  if (!samples || samples.length < 2) return null;
  let peak = 0;
  for (const value of samples) peak = Math.max(peak, Math.abs(value));
  if (peak < 1e-9) return samples.map(() => 0);
  return samples.map((value) => value / peak);
}

/**
 * A time-domain trace. `offset` shifts the midline as a fraction of height so
 * stacked layers don't sit on top of each other.
 */
export function signalPath(
  samples: readonly number[],
  width: number,
  height: number,
  offset = 0,
  range = 1.6,
): string {
  if (samples.length < 2 || width <= 0 || height <= 0) return '';
  const mid = height / 2 + offset * height;
  const amp = Math.max(6, (height / 2) * 0.78);
  const span = range > 0 ? range : 1;
  const step = width / (samples.length - 1);
  return samples
    .map((value, index) => {
      const y = mid - (Math.min(span, Math.max(-span, value)) / span) * amp;
      return `${index === 0 ? 'M' : 'L'}${(index * step).toFixed(2)} ${y.toFixed(2)}`;
    })
    .join(' ');
}

export function filterFromPointer(
  x: number,
  y: number,
  width: number,
  height: number,
  rangeDb: number,
): FilterPointer {
  const cutoffHz = eqHzFromX(x, width);
  const db = eqDbFromY(y, height, rangeDb);
  const resonance = 10 ** (db / 20);
  return { cutoffHz, resonance };
}

function displayMs(ms: number): number {
  return Math.log1p(Math.max(0, ms) / TIME_TAU_MS) + STAGE_PAD;
}

function fromDisplay(units: number): number {
  return TIME_TAU_MS * Math.expm1(Math.max(0, units - STAGE_PAD));
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

function stageY(level: number, height: number): number {
  const pad = height * 0.1;
  const usable = Math.max(1, height - 2 * pad);
  return pad + (1 - clamp(level, 0, 1)) * usable;
}

function levelFromY(y: number, height: number): number {
  const pad = height * 0.1;
  const usable = Math.max(1, height - 2 * pad);
  return clamp(1 - (y - pad) / usable, 0, 1);
}

function stagesOf(adsr: Adsr): { attack: number; decay: number; hold: number; release: number; total: number } {
  const attack = displayMs(adsr.attackMs);
  const decay = displayMs(adsr.decayMs);
  const hold = displayMs(HOLD_MS);
  const release = displayMs(adsr.releaseMs);
  return { attack, decay, hold, release, total: attack + decay + hold + release };
}

function xAt(prefix: number, total: number, width: number): number {
  if (total <= 0 || width <= 0) return 0;
  return (prefix / total) * width;
}

/** ADSR as a path that fills the box, with log-compressed stages so short times stay visible. */
export function adsrLayout(adsr: Adsr, width: number, height: number): AdsrLayout {
  const stages = stagesOf(adsr);
  const xAttack = xAt(stages.attack, stages.total, width);
  const xDecay = xAt(stages.attack + stages.decay, stages.total, width);
  const xOff = xAt(stages.attack + stages.decay + stages.hold, stages.total, width);
  const yPeak = stageY(1, height);
  const ySustain = stageY(adsr.sustain, height);
  const yZero = stageY(0, height);
  const xSustain = (xDecay + xOff) / 2;
  const points: Record<AdsrHandle, AdsrPoint> = {
    attack: { x: xAttack, y: yPeak },
    decay: { x: xDecay, y: ySustain },
    sustain: { x: xSustain, y: ySustain },
    release: { x: xOff, y: ySustain },
  };
  const path = [
    `M0 ${yZero.toFixed(2)}`,
    `L${xAttack.toFixed(2)} ${yPeak.toFixed(2)}`,
    `L${xDecay.toFixed(2)} ${ySustain.toFixed(2)}`,
    `L${xOff.toFixed(2)} ${ySustain.toFixed(2)}`,
    `L${width.toFixed(2)} ${yZero.toFixed(2)}`,
  ].join(' ');
  return { points, noteOffX: xOff, path };
}

export function hitAdsrHandle(layout: AdsrLayout, x: number, y: number, radius: number): AdsrHandle | null {
  let best: { handle: AdsrHandle; d: number } | null = null;
  for (const handle of ['attack', 'decay', 'sustain', 'release'] as const) {
    const point = layout.points[handle];
    const d = Math.hypot(point.x - x, point.y - y);
    if (d <= radius && (!best || d < best.d)) best = { handle, d };
  }
  return best?.handle ?? null;
}

function displayFromX(
  x: number,
  width: number,
  prefix: number,
  suffix: number,
): number {
  const t = clamp(width > 0 ? x / width : 0, 0.02, 0.98);
  // prefix / (prefix + d + suffix) is not this handle; this handle's own display
  // duration is the unknown in (prefix + d) / (prefix + d + suffix) = t
  const rest = suffix;
  const self = (t * rest) / (1 - t) - prefix;
  return Math.max(0.001, self);
}

export function adsrFromDrag(
  adsr: Adsr,
  handle: AdsrHandle,
  x: number,
  y: number,
  width: number,
  height: number,
): Adsr {
  const stages = stagesOf(adsr);
  const sustain = handle === 'attack' ? adsr.sustain : levelFromY(y, height);
  if (handle === 'attack') {
    const rest = stages.decay + stages.hold + stages.release;
    const t = clamp(width > 0 ? x / width : 0, 0.02, 0.85);
    const attack = fromDisplay((t * rest) / (1 - t));
    return { ...adsr, attackMs: clamp(attack, ATTACK_MS_RANGE[0], ATTACK_MS_RANGE[1]) };
  }
  if (handle === 'decay') {
    const prefix = stages.attack;
    const suffix = stages.hold + stages.release;
    const decay = fromDisplay(displayFromX(x, width, prefix, suffix));
    return {
      ...adsr,
      decayMs: clamp(decay, DECAY_MS_RANGE[0], DECAY_MS_RANGE[1]),
      sustain,
    };
  }
  if (handle === 'sustain') {
    return { ...adsr, sustain };
  }
  // Note-off handle: (A+D+H) / total = t  →  R = prefix * (1-t)/t
  const prefix = stages.attack + stages.decay + stages.hold;
  const t = clamp(width > 0 ? x / width : 0, 0.08, 0.92);
  const release = fromDisplay((prefix * (1 - t)) / t);
  return { ...adsr, releaseMs: clamp(release, RELEASE_MS_RANGE[0], RELEASE_MS_RANGE[1]) };
}
