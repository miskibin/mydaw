/**
 * Playlist selection and clipboard arithmetic.
 *
 * Solid-free: the view owns the pointer and the engine round trip; this file
 * answers "which clips did that marquee hit" and "where do copies land".
 *
 * Duplicate-to-the-right is FL's Ctrl+B: the group is copied as a block so the
 * leftmost copy sits on the selection's right edge. Paste-at-playhead is Ctrl+V:
 * the same relative layout, slid so the earliest clip starts at the playhead.
 */

import type { ClipDto } from '../api/types';

/** A clip's arrangement data, without the engine's id. */
export interface PlaylistClipScore {
  kind: ClipDto['kind'];
  track: number;
  start: number;
  length: number;
  patternId?: number;
  sourceId?: number;
  offsetFrames?: number;
  gainDb?: number;
}

/** Snapshot of a live clip for the session clipboard. */
export function clipScore(clip: ClipDto): PlaylistClipScore {
  return {
    kind: clip.kind,
    track: clip.track,
    start: clip.start,
    length: clip.length,
    ...(clip.patternId !== undefined ? { patternId: clip.patternId } : {}),
    ...(clip.sourceId !== undefined ? { sourceId: clip.sourceId } : {}),
    ...(clip.offsetFrames !== undefined ? { offsetFrames: clip.offsetFrames } : {}),
    ...(clip.gainDb !== undefined ? { gainDb: clip.gainDb } : {}),
  };
}

export interface MarqueeRange {
  track0: number;
  track1: number;
  start: number;
  end: number;
}

/**
 * Clips whose lane is inside the marquee's track span and whose time overlaps
 * the marquee. A zero-width time span is empty — a click, not a box.
 */
export function clipsIntersecting(
  clips: readonly Pick<ClipDto, 'id' | 'track' | 'start' | 'length'>[],
  range: MarqueeRange,
): number[] {
  const loTrack = Math.min(range.track0, range.track1);
  const hiTrack = Math.max(range.track0, range.track1);
  const lo = Math.min(range.start, range.end);
  const hi = Math.max(range.start, range.end);
  if (!(hi > lo)) return [];
  return clips
    .filter(
      (clip) =>
        clip.track >= loTrack &&
        clip.track <= hiTrack &&
        clip.start < hi &&
        clip.start + clip.length > lo,
    )
    .map((clip) => clip.id);
}

/** Add `id` if missing, drop it if it is already in the selection. */
export function toggleClipId(ids: readonly number[], id: number): number[] {
  return ids.includes(id) ? ids.filter((entry) => entry !== id) : [...ids, id];
}

/** Append `added` without duplicating ids already in `ids`. */
export function unionClipIds(ids: readonly number[], added: readonly number[]): number[] {
  const have = new Set(ids);
  const next = [...ids];
  for (const id of added) {
    if (have.has(id)) continue;
    have.add(id);
    next.push(id);
  }
  return next;
}

/**
 * Copies of `clips` translated so the block sits immediately after itself.
 *
 * The span is the selection's bounding box in time, not each clip's own length:
 * a short clip that starts late still moves with the group.
 */
export function duplicateToRight<T extends { start: number; length: number }>(
  clips: readonly T[],
): T[] {
  if (clips.length === 0) return [];
  const origin = Math.min(...clips.map((clip) => clip.start));
  const end = Math.max(...clips.map((clip) => clip.start + clip.length));
  const span = Math.max(0, end - origin);
  return clips.map((clip) => ({ ...clip, start: clip.start + span }));
}

/** Copies of `clips` with the earliest start aligned to `playhead`. */
export function pasteAtPlayhead<T extends { start: number }>(
  clips: readonly T[],
  playhead: number,
): T[] {
  if (clips.length === 0) return [];
  const origin = Math.min(...clips.map((clip) => clip.start));
  const dest = Math.max(0, playhead);
  return clips.map((clip) => ({ ...clip, start: Math.max(0, dest + (clip.start - origin)) }));
}

/**
 * How far a group can move without running off tick 0 or off the lane list.
 *
 * `dTrack` / `dStart` are the requested deltas; the result is what is legal.
 */
export function clampGroupMove(
  clips: readonly { track: number; start: number }[],
  dTrack: number,
  dStart: number,
  laneCount: number,
): { dTrack: number; dStart: number } {
  if (clips.length === 0) return { dTrack: 0, dStart: 0 };
  const minStart = Math.min(...clips.map((clip) => clip.start));
  const minTrack = Math.min(...clips.map((clip) => clip.track));
  const maxTrack = Math.max(...clips.map((clip) => clip.track));
  const lastLane = Math.max(0, laneCount - 1);
  const start = Math.max(-minStart, dStart);
  return {
    dStart: start === 0 ? 0 : start,
    dTrack: Math.min(lastLane - maxTrack, Math.max(-minTrack, Math.round(dTrack))),
  };
}
