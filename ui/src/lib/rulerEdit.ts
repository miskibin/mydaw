// The time ruler's loop editing (Playlist, Piano roll, Pitch editor): which part of a loop range
// the pointer grabs and where a drag moves it. Pure: beats in, beats out; `bw` is px per beat.
import { clamp } from './format';
import { snapRound } from './snap';
import type { BeatRange } from './patLoop';

/** How near (px) a loop edge must be to grab it. */
export const EDGE_PX = 8;

/** The loop's start or end edge under `beat` (within 8 px); inside it with Shift = its body. */
export function loopEdgeAt(loop: BeatRange | null, beat: number, bw: number, shift = false): 'a' | 'b' | 'body' | null {
  if (!loop || !(bw > 0)) return null;
  const x = beat * bw;
  if (Math.abs(x - loop.a * bw) <= EDGE_PX) return 'a';
  if (Math.abs(x - loop.b * bw) <= EDGE_PX) return 'b';
  return shift && beat > loop.a && beat < loop.b ? 'body' : null;
}

/**
 * The loop with one edge dragged to `raw` (beats, unsnapped), or its body moved by
 * `raw − raw0`; snapped to `unit` (0 = free), at least one unit long, inside `[0, max]`.
 */
export function dragLoop(loop: BeatRange, edge: 'a' | 'b' | 'body', raw: number, raw0: number, unit: number, max: number): BeatRange {
  const u = unit || 1 / 64;
  if (edge === 'a') return { a: clamp(snapRound(raw, unit), 0, loop.b - u), b: loop.b };
  if (edge === 'b') return { a: loop.a, b: clamp(snapRound(raw, unit), loop.a + u, max) };
  const len = loop.b - loop.a, a = clamp(snapRound(loop.a + raw - raw0, unit), 0, Math.max(0, max - len));
  return { a, b: a + len };
}

/** A loop drawn from `b0` to `b1` (snapped beats, either order); none while they are the same. */
export function drawnLoop(b0: number, b1: number): BeatRange | null {
  return b1 === b0 ? null : { a: Math.min(b0, b1), b: Math.max(b0, b1) };
}

export const sameRange = (x: BeatRange | null | undefined, y: BeatRange | null | undefined) => (!x && !y) || (!!x && !!y && x.a === y.a && x.b === y.b);
