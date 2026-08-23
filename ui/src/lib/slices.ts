/**
 * The sample window's chop: markers, the slices between them, and the keys
 * that play them.
 *
 * The engine stores a chop as ascending frame positions, each the **start** of
 * a slice; the last slice runs to the region's end. Key `rootKey + n` plays
 * slice `n` and nothing else answers, which is the Fruity Slicer / Simpler
 * convention and the reason these helpers talk in slice *indices* as often as
 * in frames.
 *
 * Marker editing lives here rather than in the view for the usual reason: the
 * three gestures (nudge, delete, add) each have to keep the list sorted,
 * deduped and inside the region, and three copies of that would be three
 * chances to leave the engine a table it has to repair behind the user's back.
 */

import { noteName } from './pattern';

/** `MAX_SLICES` in `plugins/sequencer/src/oneshot.rs`. */
export const MAX_SLICES = 128;

/**
 * The closest two markers may sit, in frames at the file's own rate.
 *
 * The engine's detector will not place onsets closer than 30 ms and its
 * playback declick is 2 ms at each end, so a hand-placed slice shorter than
 * one declick would be nothing but the fade. 3 ms is generous enough to chop a
 * click and tight enough that the rule is never in the way.
 */
export const MIN_SLICE_MS = 3;

/** One playable slice, resolved against the region. */
export interface Slice {
  index: number;
  start: number;
  end: number;
  /** The MIDI key that plays it, or `null` past 127. */
  key: number | null;
}

/** How close, in frames, two markers may be at `rate`. */
export function minSliceFrames(rate: number): number {
  if (!(rate > 0)) return 1;
  return Math.max(1, Math.round((MIN_SLICE_MS / 1_000) * rate));
}

/**
 * The markers as playable slices, each with its end and its key.
 *
 * `rangeEnd` is where the last slice stops -- the region's end, not the file's,
 * because the region is what the engine plays. Markers at or past it are
 * dropped: they have no slice.
 */
export function resolveSlices(
  frames: readonly number[],
  rangeEnd: number,
  rootKey: number,
): Slice[] {
  const out: Slice[] = [];
  for (let index = 0; index < frames.length; index += 1) {
    const start = frames[index]!;
    if (start >= rangeEnd) break;
    const next = frames[index + 1];
    const end = next === undefined ? rangeEnd : Math.min(next, rangeEnd);
    if (end <= start) continue;
    const key = rootKey + out.length;
    out.push({ index: out.length, start, end, key: key <= 127 ? key : null });
  }
  return out;
}

/** Which slice `frame` falls inside, or `-1` when it is past the last one. */
export function sliceAt(frames: readonly number[], frame: number): number {
  let found = -1;
  for (let index = 0; index < frames.length; index += 1) {
    if (frames[index]! <= frame) found = index;
    else break;
  }
  return found;
}

/** The nearest marker to `frame` within `tolerance`, or `-1`. */
export function markerNear(
  frames: readonly number[],
  frame: number,
  tolerance: number,
): number {
  let best = -1;
  let distance = tolerance;
  frames.forEach((marker, index) => {
    const gap = Math.abs(marker - frame);
    if (gap <= distance) {
      distance = gap;
      best = index;
    }
  });
  return best;
}

/** Sorted, deduped, inside `[rangeStart, rangeEnd)` and capped. Never fails. */
export function normalizeSlices(
  frames: readonly number[],
  rangeStart: number,
  rangeEnd: number,
): number[] {
  const clean = new Set<number>();
  for (const frame of frames) {
    if (!Number.isFinite(frame)) continue;
    const rounded = Math.round(frame);
    if (rounded < rangeStart || rounded >= rangeEnd) continue;
    clean.add(rounded);
  }
  return [...clean].sort((a, b) => a - b).slice(0, MAX_SLICES);
}

/**
 * Move marker `index` to `frame`.
 *
 * The first marker is the head of the chop and is pinned to the region start:
 * dragging it would leave the beginning of the sample in no slice at all, and
 * the region handle already exists for moving that edge. Every other marker is
 * held one [`MIN_SLICE_MS`] clear of its neighbours rather than being allowed
 * to swap past them, so a long drag parks against the neighbour instead of
 * renumbering every pad under the hand.
 */
export function withMarkerMoved(
  frames: readonly number[],
  index: number,
  frame: number,
  rangeStart: number,
  rangeEnd: number,
  rate: number,
): number[] {
  if (index <= 0 || index >= frames.length) return frames as number[];
  const gap = minSliceFrames(rate);
  const low = (frames[index - 1] ?? rangeStart) + gap;
  const high = (frames[index + 1] ?? rangeEnd) - gap;
  if (high < low) return frames as number[];
  const next = Math.min(high, Math.max(low, Math.round(frame)));
  if (next === frames[index]) return frames as number[];
  const out = [...frames];
  out[index] = next;
  return out;
}

/**
 * Add a marker at `frame`, splitting the slice it lands in.
 *
 * Refuses (by answering with the same array) when it would sit inside the
 * minimum gap of an existing marker, or when the table is full -- the caller
 * says so in the readout rather than the list silently not growing.
 */
export function withMarkerAdded(
  frames: readonly number[],
  frame: number,
  rangeStart: number,
  rangeEnd: number,
  rate: number,
): number[] {
  const rounded = Math.round(frame);
  if (rounded <= rangeStart || rounded >= rangeEnd) return frames as number[];
  if (frames.length >= MAX_SLICES) return frames as number[];
  const gap = minSliceFrames(rate);
  if (frames.some((marker) => Math.abs(marker - rounded) < gap)) return frames as number[];
  return [...frames, rounded].sort((a, b) => a - b);
}

/**
 * Delete marker `index`, merging its slice into the one before it.
 *
 * The first marker cannot go: it is the start of the chop, and a table whose
 * first slice began somewhere in the middle of the region would leave the head
 * of the sample unreachable from any key.
 */
export function withMarkerRemoved(frames: readonly number[], index: number): number[] {
  if (index <= 0 || index >= frames.length) return frames as number[];
  return frames.filter((_, at) => at !== index);
}

/** `1`-based, the way a slicer numbers pads. */
export function sliceLabel(index: number): string {
  return String(index + 1);
}

/** What a slice's marker says on hover: its number, its key and its length. */
export function sliceTitle(slice: Slice, rate: number): string {
  const seconds = rate > 0 ? (slice.end - slice.start) / rate : 0;
  const length = rate > 0 ? `${(seconds * 1_000).toFixed(0)} ms` : `${slice.end - slice.start} fr`;
  const key = slice.key === null ? 'past the keyboard — not playable' : noteName(slice.key);
  return `Slice ${sliceLabel(slice.index)} · ${key} · ${length}\nDrag to nudge, double-click to remove`;
}
