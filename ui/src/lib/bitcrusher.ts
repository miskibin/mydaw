/**
 * The bitcrusher's damage, drawn as the waveform it produces.
 *
 * A transcription of `plugins/effects/src/bitcrusher.rs`: the same mid-tread
 * quantiser and the same phase-accumulator sample-and-hold, run over one cycle
 * of a sine so the editor can show what the two knobs do to a wave rather than
 * printing the numbers back at the user. Like every other picture in the shell
 * this is a drawing of the settings, not a measurement of the audio -- but it is
 * the *same arithmetic*, so a sample it draws on a step is a sample the engine
 * puts there too.
 */

/** `bitcrusher::MAX_BITS`: the top of the knob, where the quantiser is off. */
export const MAX_BITS = 24;

/** `bitcrusher::MAX_TONE_HZ`: the top of the filter knob, where it is off. */
export const MAX_TONE_HZ = 20_000;

/** `dsp::quantise` -- round onto a grid of `2^bits` levels spanning −1…1. */
export function quantise(x: number, bits: number): number {
  const levels = 2 ** (Math.min(32, Math.max(1, bits)) - 1);
  return Math.round(x * levels) / levels;
}

/** How many quantisation levels are worth drawing before the grid is a fill. */
export const MAX_DRAWN_LEVELS = 33;

/**
 * The quantiser's step size in the −1…1 range, or `null` when the knob is at
 * the top and the quantiser is switched out.
 */
export function quantiseStep(bits: number): number | null {
  return bits >= MAX_BITS ? null : 2 ** (1 - Math.min(MAX_BITS, Math.max(1, bits)));
}

/**
 * One cycle of a sine through the crusher, sampled at `points`.
 *
 * `points` stands in for the sample rate of the picture: the hold accumulator
 * steps by `1/downsample` per drawn sample exactly as the engine's does per
 * audio sample, so the staircase's tread count is the ratio the knob says and
 * not an artist's impression of it.
 */
export function crushedCycle(
  bits: number,
  downsample: number,
  points = 256,
): { clean: number[]; crushed: number[] } {
  const count = Math.max(8, Math.trunc(points));
  const divisor = Math.max(1, downsample);
  const clean: number[] = [];
  const crushed: number[] = [];
  let phase = 1;
  let held = 0;
  for (let i = 0; i < count; i += 1) {
    const x = Math.sin((2 * Math.PI * i) / count);
    phase += 1 / divisor;
    if (phase >= 1) {
      phase -= 1;
      held = bits >= MAX_BITS ? x : quantise(x, bits);
    }
    clean.push(x);
    crushed.push(held);
  }
  return { clean, crushed };
}

/**
 * The staircase as an SVG path: horizontal treads and vertical risers.
 *
 * Drawn as steps rather than as a polyline through the sample points, because
 * the sample-and-hold *is* a step: joining the held values with diagonals would
 * draw a smoother wave than the one leaving the effect.
 */
export function stepPath(samples: readonly number[], width: number, height: number): string {
  if (samples.length === 0) return '';
  const dx = width / samples.length;
  const y = (value: number) => (height / 2) * (1 - Math.max(-1, Math.min(1, value)));
  let d = `M0 ${y(samples[0]!).toFixed(2)}`;
  for (let i = 0; i < samples.length; i += 1) {
    const x1 = ((i + 1) * dx).toFixed(2);
    d += ` H${x1}`;
    const next = samples[i + 1];
    if (next !== undefined) d += ` V${y(next).toFixed(2)}`;
  }
  return d;
}

/** A smooth polyline through `samples`, for the reference wave behind. */
export function linePath(samples: readonly number[], width: number, height: number): string {
  if (samples.length < 2) return '';
  const dx = width / (samples.length - 1);
  return samples
    .map((value, i) => {
      const y = (height / 2) * (1 - Math.max(-1, Math.min(1, value)));
      return `${i === 0 ? 'M' : 'L'}${(i * dx).toFixed(2)} ${y.toFixed(2)}`;
    })
    .join(' ');
}

/** The rate the held samples come out at, in Hz, for a readout. */
export function holdRateHz(sampleRate: number, downsample: number): number {
  const rate = Number.isFinite(sampleRate) && sampleRate > 1 ? sampleRate : 48_000;
  return rate / Math.max(1, downsample);
}
