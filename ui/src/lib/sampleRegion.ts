/**
 * The sample window's arithmetic: the played region of a file, and pitch.
 *
 * The engine stores a region as a pair of frame counts with one piece of
 * shorthand -- `end == 0` means "the last frame", so a region that has never
 * been touched survives the file being reloaded at a different length. Every
 * conversion in and out of that shorthand lives here rather than being spelled
 * out at each of the four places the window edits a region.
 */

/** A resolved region: real frame numbers, `start < end`, both inside the file. */
export interface Region {
  start: number;
  end: number;
}

/**
 * The shortest region a drag may leave.
 *
 * A region of two frames is a click, not a sound, and a zero-length one would
 * silence the channel with no visible handle left to grab and undo it.
 */
export const MIN_REGION_FRAMES = 64;

/** Resolve the engine's pair against the file's real length. */
export function resolveRegion(startFrames: number, endFrames: number, total: number): Region {
  if (total <= 0) return { start: 0, end: 0 };
  const start = clampFrames(startFrames, total);
  const end = endFrames <= 0 ? total : clampFrames(endFrames, total);
  if (end <= start) return { start: 0, end: total };
  return { start, end };
}

/**
 * A dragged pair of frame positions as a legal region.
 *
 * `storedEnd` is what the engine is told: `0` whenever the region runs to the
 * end of the file, so trimming only the front does not pin the tail to today's
 * frame count.
 */
export function dragRegion(
  a: number,
  b: number,
  total: number,
): Region & { storedEnd: number } {
  if (total <= 0) return { start: 0, end: 0, storedEnd: 0 };
  let start = clampFrames(Math.min(a, b), total);
  let end = clampFrames(Math.max(a, b), total);
  if (end - start < MIN_REGION_FRAMES) {
    const span = Math.min(MIN_REGION_FRAMES, total);
    if (start + span <= total) end = start + span;
    else {
      end = total;
      start = Math.max(0, total - span);
    }
  }
  return { start, end, storedEnd: end >= total ? 0 : end };
}

/** Where a frame sits along the file, as a percentage. */
export function framePercent(frames: number, total: number): number {
  if (total <= 0) return 0;
  return Math.min(100, Math.max(0, (frames / total) * 100));
}

/** The frame a pointer at `fraction` (`0..1`) of the waveform is over. */
export function frameAt(fraction: number, total: number): number {
  return clampFrames(Math.round(fraction * total), total);
}

/** `frames` at `rate`, as `1.234 s` -- or `m:ss.mmm` once it is worth minutes. */
export function formatFrames(frames: number, rate: number): string {
  if (rate <= 0) return `${Math.round(frames)}`;
  const seconds = frames / rate;
  if (seconds < 60) return `${seconds.toFixed(3)} s`;
  const minutes = Math.floor(seconds / 60);
  const rest = seconds - minutes * 60;
  return `${minutes}:${rest.toFixed(3).padStart(6, '0')}`;
}

/**
 * Cents as a musician reads them: `+2 st -8 ct`, not `+192`.
 *
 * The knob's own unit stays cents -- that is what the engine takes and what a
 * fine drag moves by -- but ±1200 cents is ±an octave, and "how far is this
 * sample off" is a question about semitones.
 */
export function pitchLabel(cents: number): string {
  const rounded = Math.round(cents);
  if (rounded === 0) return 'root';
  const sign = rounded < 0 ? -1 : 1;
  const magnitude = Math.abs(rounded);
  const semitones = Math.floor(magnitude / 100);
  const rest = magnitude - semitones * 100;
  const parts: string[] = [];
  if (semitones > 0) parts.push(`${sign < 0 ? '-' : '+'}${semitones} st`);
  if (rest > 0 || semitones === 0) parts.push(`${sign < 0 ? '-' : '+'}${rest} ct`);
  return parts.join(' ');
}

/**
 * Step the pitch by whole semitones, landing on the grid.
 *
 * From +214 cents, "one semitone up" is +300 and not +314: the buttons are for
 * transposing, and a transpose that carried a stray 14 cents with it would make
 * the detune impossible to get rid of by ear.
 */
export function stepSemitone(cents: number, delta: number, limit = 1_200): number {
  const grid = delta > 0 ? Math.floor(cents / 100) : Math.ceil(cents / 100);
  const next = (grid + delta) * 100;
  return Math.min(limit, Math.max(-limit, next));
}

function clampFrames(frames: number, total: number): number {
  if (!Number.isFinite(frames)) return 0;
  return Math.min(total, Math.max(0, Math.round(frames)));
}
