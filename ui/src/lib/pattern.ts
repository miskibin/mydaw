/**
 * Step-grid arithmetic and editing, kept out of the component.
 *
 * Two things live here, both pure functions of their arguments:
 *
 * - **What a click does.** `toggleStep` is the whole interaction model of the
 *   grid, and it is small enough to read and specific enough to get wrong.
 * - **Which step is playing.** `currentStep` turns the transport position (in
 *   frames, from the meter stream, interpolated locally by `Playhead`) into a
 *   step index, using the same arithmetic the Rust scheduler uses.
 *
 * Keeping them here rather than in `SequencerPanel.tsx` means the panel is
 * markup and the behaviour is testable.
 */

import type { PatternDto, StepDto } from '../api/types';

/** Velocity a plain click gives a step. Mirrors `DEFAULT_STEP_VELOCITY`. */
export const STEP_VELOCITY = 0.78;

/** Velocity a shift-click gives a step. Mirrors `ACCENT_VELOCITY`. */
export const ACCENT_VELOCITY = 1.0;

/** At or above this, a step draws as an accent. Mirrors `ACCENT_THRESHOLD`. */
export const ACCENT_THRESHOLD = 0.95;

/** Steps per beat, so 16 steps is one bar of 4/4. Mirrors `STEPS_PER_BEAT`. */
export const STEPS_PER_BEAT = 4;

/** Sample rate assumed before the backend reports the device's. */
export const FALLBACK_SAMPLE_RATE = 48_000;

/** Whether a step is loud enough to draw as an accent. */
export function isAccent(step: StepDto): boolean {
  return step.on && step.velocity >= ACCENT_THRESHOLD;
}

/**
 * What one click does to one cell.
 *
 * | Cell was | Click | Shift-click |
 * | --- | --- | --- |
 * | off | on, normal | on, accented |
 * | on, normal | off | accented |
 * | on, accented | off | off |
 *
 * Shift-click *promotes* rather than toggling, because the alternative -- two
 * clicks to accent an existing step, one of which briefly deletes it -- is the
 * thing people complain about in step sequencers that get this wrong.
 */
export function toggleStep(step: StepDto, accent: boolean): StepDto {
  if (!accent) {
    return step.on
      ? { on: false, velocity: step.velocity }
      : { on: true, velocity: STEP_VELOCITY };
  }
  if (step.on && isAccent(step)) return { on: false, velocity: step.velocity };
  return { on: true, velocity: ACCENT_VELOCITY };
}

/**
 * A copy of `pattern` with one cell toggled.
 *
 * Structurally shared: only the touched row and cell are new objects, so Solid's
 * reference equality still short-circuits the untouched rows. Out-of-range
 * indices return the pattern unchanged rather than throwing -- a click can only
 * come from a rendered cell, and defending the invariant is cheaper than
 * proving it.
 */
export function withToggledStep(
  pattern: PatternDto,
  rowIndex: number,
  stepIndex: number,
  accent: boolean,
): PatternDto {
  const row = pattern.rows[rowIndex];
  const cell = row?.steps[stepIndex];
  if (!row || !cell) return pattern;

  const steps = row.steps.slice();
  steps[stepIndex] = toggleStep(cell, accent);
  const rows = pattern.rows.slice();
  rows[rowIndex] = { ...row, steps };
  return { ...pattern, rows };
}

/** A copy of `pattern` with every cell switched off. */
export function clearedPattern(pattern: PatternDto): PatternDto {
  return {
    ...pattern,
    rows: pattern.rows.map((row) => ({
      ...row,
      steps: row.steps.map((step) => ({ on: false, velocity: step.velocity })),
    })),
  };
}

/** Total cells switched on. */
export function hitCount(pattern: PatternDto): number {
  return pattern.rows.reduce(
    (total, row) => total + row.steps.filter((step) => step.on).length,
    0,
  );
}

/** Samples per step, the same formula the Rust scheduler uses. */
export function stepLengthSamples(sampleRate: number, bpm: number): number {
  return (sampleRate * 60) / bpm / STEPS_PER_BEAT;
}

/**
 * Which step of the loop the playhead is in, or `null` when it cannot be known.
 *
 * `samplePos` is the interpolated transport position in frames. The pattern
 * loops every `steps` steps, so this is a floor and a modulo -- computed from
 * the absolute position, exactly as the scheduler computes its boundaries, so
 * the highlight cannot drift away from what is being heard.
 *
 * `null` for a nonsense tempo or rate, or a pattern with no steps: the panel
 * then draws no highlight rather than one on step 0.
 */
export function currentStep(
  samplePos: number,
  sampleRate: number,
  bpm: number,
  steps: number,
): number | null {
  if (!Number.isFinite(samplePos) || samplePos < 0) return null;
  if (!Number.isFinite(sampleRate) || sampleRate <= 0) return null;
  if (!Number.isFinite(bpm) || bpm <= 0) return null;
  if (!Number.isInteger(steps) || steps <= 0) return null;

  const length = stepLengthSamples(sampleRate, bpm);
  if (!Number.isFinite(length) || length <= 0) return null;
  return Math.floor(samplePos / length) % steps;
}
