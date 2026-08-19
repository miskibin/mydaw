/**
 * Pattern arithmetic and editing, kept out of the components.
 *
 * Everything here is a pure function of its arguments, in three families:
 *
 * - **What a click does.** `toggleStep` is the whole interaction model of the
 *   step grid, and it is small enough to read and specific enough to get wrong.
 * - **Where the playhead is.** `currentStep` and `currentTick` turn the
 *   transport position (in frames, from the meter stream, interpolated locally
 *   by `Playhead`) into a grid position, using the same arithmetic the Rust
 *   scheduler uses.
 * - **What an edit produces.** The `with*` functions return a new `PatternDto`
 *   with one thing changed and everything else structurally shared, so Solid's
 *   reference equality still short-circuits the untouched parts.
 *
 * Keeping them here rather than in the panels means the panels are markup and
 * the behaviour is testable.
 */

import type { MelodicNoteDto, PatternDto, StepDto } from '../api/types';

/** Velocity a plain click gives a step. Mirrors `DEFAULT_STEP_VELOCITY`. */
export const STEP_VELOCITY = 0.78;

/** Velocity a shift-click gives a step. Mirrors `ACCENT_VELOCITY`. */
export const ACCENT_VELOCITY = 1.0;

/** At or above this, a step draws as an accent. Mirrors `ACCENT_THRESHOLD`. */
export const ACCENT_THRESHOLD = 0.95;

/** Steps per beat, so 16 steps is one bar of 4/4. Mirrors `STEPS_PER_BEAT`. */
export const STEPS_PER_BEAT = 4;

/** Ticks per quarter note. Mirrors `PPQ` in `plugins/sequencer/src/pattern.rs`. */
export const PPQ = 960;

/** Ticks in one sixteenth-note step. Mirrors `TICKS_PER_STEP`. */
export const TICKS_PER_STEP = PPQ / STEPS_PER_BEAT;

/** The lengths a pattern may be, in steps. Mirrors `PATTERN_LENGTHS`. */
export const PATTERN_LENGTHS = [16, 32, 64] as const;

/** Most notes a pattern may hold. Mirrors `MAX_NOTES`. */
export const MAX_NOTES = 1024;

/** Lowest and highest MIDI key a note may address. Mirrors `MAX_KEY`. */
export const MIN_KEY = 0;
export const MAX_KEY = 127;

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

// ---------------------------------------------------------------- piano roll

/** Ticks in one time round the loop. */
export function patternTicks(pattern: PatternDto): number {
  return pattern.steps * TICKS_PER_STEP;
}

/**
 * Where in the loop the playhead is, in ticks -- fractional, because this is
 * what draws a line rather than what fires a note.
 *
 * `null` for the same reasons `currentStep` returns it. Deliberately the same
 * `floor(pos / stepLength)` scaled to ticks, so the roll's playhead and the
 * sequencer's highlight can never disagree about which step is playing.
 */
export function currentTick(
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
  const loopTicks = steps * TICKS_PER_STEP;
  const ticks = (samplePos / length) * TICKS_PER_STEP;
  // `%` on a float is exact enough here: the position is a display value and
  // the modulus is a whole number of ticks.
  return ((ticks % loopTicks) + loopTicks) % loopTicks;
}

/**
 * Round `ticks` down to the nearest multiple of `snap`.
 *
 * `snap` of 0 (or anything that is not a positive integer) means "no snap", and
 * the tick is returned as an integer -- the engine's positions are integers
 * whether or not the user asked for a grid.
 */
export function snapTicks(ticks: number, snap: number): number {
  const floored = Math.max(0, Math.floor(ticks));
  if (!Number.isFinite(snap) || snap < 1) return floored;
  return Math.floor(floored / snap) * snap;
}

/**
 * A note clamped into what the engine will accept, or `null` if it cannot be.
 *
 * The one place the UI's rules about a note live. Every editing helper below
 * runs its result through this, so an edit can be written as arithmetic and the
 * bounds are applied once: a note is at least one tick long, starts at or after
 * zero, ends at or before the loop point, and addresses a real MIDI key.
 *
 * `null` when there is nowhere legal to put it -- a note dragged past the end of
 * a pattern with no room left for even one tick. The caller drops the edit
 * rather than storing something Rust would refuse.
 */
export function clampNote(note: MelodicNoteDto, pattern: PatternDto): MelodicNoteDto | null {
  const loop = patternTicks(pattern);
  const key = Math.round(Math.min(MAX_KEY, Math.max(MIN_KEY, note.key)));
  const start = Math.min(Math.max(0, Math.round(note.start)), loop - 1);
  if (start < 0 || loop < 1) return null;
  const length = Math.min(Math.max(1, Math.round(note.length)), loop - start);
  if (length < 1) return null;
  return {
    key,
    channel: Math.round(Math.min(15, Math.max(0, note.channel))),
    start,
    length,
    velocity: Math.min(1, Math.max(0, note.velocity)),
  };
}

/** A copy of `pattern` with `note` appended, or unchanged when it will not fit. */
export function withNote(pattern: PatternDto, note: MelodicNoteDto): PatternDto {
  if (pattern.notes.length >= MAX_NOTES) return pattern;
  const clamped = clampNote(note, pattern);
  if (!clamped) return pattern;
  return { ...pattern, notes: [...pattern.notes, clamped] };
}

/** A copy of `pattern` with note `index` replaced. Out-of-range indices no-op. */
export function withReplacedNote(
  pattern: PatternDto,
  index: number,
  note: MelodicNoteDto,
): PatternDto {
  if (index < 0 || index >= pattern.notes.length) return pattern;
  const clamped = clampNote(note, pattern);
  if (!clamped) return pattern;
  const notes = pattern.notes.slice();
  notes[index] = clamped;
  return { ...pattern, notes };
}

/** A copy of `pattern` without note `index`. Out-of-range indices no-op. */
export function withoutNote(pattern: PatternDto, index: number): PatternDto {
  if (index < 0 || index >= pattern.notes.length) return pattern;
  return { ...pattern, notes: pattern.notes.filter((_, i) => i !== index) };
}

/** A copy of `pattern` with no melodic notes at all. */
export function withoutNotes(pattern: PatternDto): PatternDto {
  return pattern.notes.length === 0 ? pattern : { ...pattern, notes: [] };
}

/**
 * A copy of `pattern` at a new length, in steps.
 *
 * Growing is free. **Shrinking drops the notes that no longer fit** -- there is
 * nowhere to put a note that starts past the new loop point, and truncating one
 * to fit would silently rewrite music the user wrote. The drum grid is not
 * touched either way: Rust keeps every row's full-length array, so shortening
 * and lengthening again gives the steps back. Notes are the half that cannot
 * work like that, because the wire only carries the notes that exist.
 *
 * An illegal length returns the pattern unchanged rather than sending Rust
 * something it will refuse.
 */
export function withPatternLength(pattern: PatternDto, steps: number): PatternDto {
  if (!(PATTERN_LENGTHS as readonly number[]).includes(steps)) return pattern;
  if (steps === pattern.steps) return pattern;

  const loop = steps * TICKS_PER_STEP;
  const rows = pattern.rows.map((row) => {
    if (steps <= row.steps.length) return { ...row, steps: row.steps.slice(0, steps) };
    const grown = row.steps.slice();
    while (grown.length < steps) grown.push({ on: false, velocity: STEP_VELOCITY });
    return { ...row, steps: grown };
  });
  const notes = pattern.notes
    .filter((note) => note.start < loop)
    .map((note) => ({ ...note, length: Math.min(note.length, loop - note.start) }));
  return { steps, rows, notes };
}

/**
 * The index of the topmost note under `(tick, key)`, or `-1`.
 *
 * Last match wins, so the note drawn on top -- later in the array -- is the one
 * a click selects. That matches what the eye picked, which is the only rule a
 * hit test needs.
 */
export function noteAt(pattern: PatternDto, tick: number, key: number): number {
  for (let index = pattern.notes.length - 1; index >= 0; index -= 1) {
    const note = pattern.notes[index];
    if (!note) continue;
    if (note.key === key && tick >= note.start && tick < note.start + note.length) return index;
  }
  return -1;
}

/** Names for the twelve pitch classes, sharps only. `noteName(60)` is `C4`. */
const PITCH_CLASSES = ['C', 'C#', 'D', 'D#', 'E', 'F', 'F#', 'G', 'G#', 'A', 'A#', 'B'];

/**
 * Scientific pitch name for a MIDI key: 60 is `C4`, which is the convention
 * every DAW in this family uses (and the one MIDI itself declines to specify).
 */
export function noteName(key: number): string {
  const clamped = Math.round(Math.min(MAX_KEY, Math.max(MIN_KEY, key)));
  return `${pitchClass(clamped)}${Math.floor(clamped / 12) - 1}`;
}

/** Whether `key` is a black key, i.e. one of the five accidentals. */
export function isBlackKey(key: number): boolean {
  return pitchClass(key).length === 2;
}

/** The pitch class of `key`, for any integer -- negative ones included. */
function pitchClass(key: number): string {
  return PITCH_CLASSES[((Math.round(key) % 12) + 12) % 12] ?? 'C';
}
