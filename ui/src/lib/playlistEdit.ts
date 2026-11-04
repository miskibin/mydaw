/**
 * Playlist selection, clipboard and audio-clip edit arithmetic.
 *
 * Solid-free: the view owns the pointer and the engine round trip; this file
 * answers "which clips did that marquee hit" and "where do copies land".
 *
 * Duplicate-to-the-right is FL's Ctrl+B: the group is copied as a block so the
 * leftmost copy sits on the selection's right edge. Paste-at-playhead is Ctrl+V:
 * the same relative layout, slid so the earliest clip starts at the playhead.
 * Trimming an audio clip's head and slicing one keep the audio anchored in
 * time; see the section further down.
 */

import type { ClipDto } from '../api/types';
import { TICKS_PER_STEP, samplesPerTick, snapTicks } from './pattern';

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

// ---------------------------------------------------------------------------
// Audio clip edits: trimming the head, slicing, and per-clip volume
// ---------------------------------------------------------------------------
//
// An audio clip is a window onto a recording: `start` and `length` are song
// ticks, `offsetFrames` is where in the file the window opens, in frames of the
// **file's own** rate. Every edit here keeps the audio anchored in time -- the
// syllable that was on beat 3 is still on beat 3 after the clip around it has
// been trimmed or cut -- by moving the offset by exactly the ticks the clip
// edge moved, converted at the file's rate.

/** The part of an audio clip these edits change. */
export interface AudioClipWindow {
  start: number;
  length: number;
  offsetFrames: number;
}

/** What the edits need to know about the file. `frames` 0 = length unknown. */
export interface AudioFileTiming {
  frames: number;
  sampleRate: number;
}

/**
 * The shortest clip a trim or a slice will leave: a 1/64 note.
 *
 * Long enough to grab and to see at any zoom; short enough that it never gets
 * in the way of a real edit.
 */
export const MIN_CLIP_TICKS = TICKS_PER_STEP / 4;

/** Frames of a file at `sampleRate` that one song tick covers at `bpm`. */
export function fileFramesPerTick(sampleRate: number, bpm: number): number {
  return samplesPerTick(sampleRate, bpm);
}

/**
 * A new start for an audio clip's left edge, with the audio held in place.
 *
 * `rawStart` is the pointer's tick; it is snapped to `snap` unless `free`. The
 * right edge does not move. The result is clamped so the clip never opens
 * before the file's first frame (nor before song tick 0), and never trims past
 * the clip's end or the end of the audio -- at least {@link MIN_CLIP_TICKS} of
 * clip, and of sound, stays. A clamp can land off the grid; that is the edge of
 * the recording, which is where the user was dragging to.
 */
export function trimClipStart(
  clip: AudioClipWindow,
  file: AudioFileTiming,
  bpm: number,
  rawStart: number,
  snap: number,
  free = false,
): AudioClipWindow {
  const perTick = fileFramesPerTick(file.sampleRate, bpm);
  const offset = Math.max(0, clip.offsetFrames);
  // Where the file's frame 0 sits on the timeline, in fractional ticks.
  const anchor = clip.start - offset / perTick;
  const end = clip.start + clip.length;
  const audioEnd = file.frames > 0 ? anchor + file.frames / perTick : Number.POSITIVE_INFINITY;
  // Ceil, so the earliest start never asks for a frame before the file starts;
  // the < 1 tick of offset it can leave keeps the audio exactly where it was.
  // One frame of slack first: an offset rounded to whole frames by an earlier
  // trim puts the anchor a hair past its tick, and dragging back out should
  // land on that tick at offset 0, not one tick later.
  const lo = Math.min(clip.start, Math.max(0, Math.ceil(anchor - 1 / perTick - 1e-9)));
  const hi = Math.max(clip.start, Math.floor(Math.min(end, audioEnd)) - MIN_CLIP_TICKS);
  const wanted = free ? Math.round(rawStart) : snapTicks(rawStart, snap);
  const start = Math.min(hi, Math.max(lo, wanted));
  return {
    start,
    length: end - start,
    offsetFrames: Math.max(0, Math.round(offset + (start - clip.start) * perTick)),
  };
}

/**
 * Where a click at `rawTick` cuts a clip, or `null` when it cannot.
 *
 * Snapped to the grid unless `free`; a pointer nearest the clip's own edge is
 * no cut. A clip with **no** grid line inside it (a short clip under a coarse
 * snap) is cut where the pointer is instead: the user clicked inside the clip
 * and there is no grid line it could have meant. Either side keeping less than
 * {@link MIN_CLIP_TICKS} is no cut.
 */
export function splitPoint(
  clip: Pick<AudioClipWindow, 'start' | 'length'>,
  rawTick: number,
  snap: number,
  free = false,
): number | null {
  const end = clip.start + clip.length;
  let at = free ? Math.round(rawTick) : snapTicks(rawTick, snap);
  if (!free && snap > 0 && (at <= clip.start || at >= end)) {
    const firstInside = (Math.floor(clip.start / snap) + 1) * snap;
    if (firstInside < end) return null;
    at = Math.round(rawTick);
  }
  if (at - clip.start < MIN_CLIP_TICKS || end - at < MIN_CLIP_TICKS) return null;
  return at;
}

/**
 * The two halves of an audio clip cut at song tick `at`.
 *
 * The left half is the original clip, shortened; the right half is a new clip
 * whose offset has advanced by the ticks it starts later, so it plays the audio
 * that was already there. `at` must come from {@link splitPoint}.
 */
export function splitAudioClip(
  clip: AudioClipWindow,
  at: number,
  file: Pick<AudioFileTiming, 'sampleRate'>,
  bpm: number,
): { left: AudioClipWindow; right: AudioClipWindow } {
  const perTick = fileFramesPerTick(file.sampleRate, bpm);
  const offset = Math.max(0, clip.offsetFrames);
  const end = clip.start + clip.length;
  return {
    left: { start: clip.start, length: at - clip.start, offsetFrames: offset },
    right: {
      start: at,
      length: end - at,
      offsetFrames: Math.max(0, Math.round(offset + (at - clip.start) * perTick)),
    },
  };
}

/** The per-clip volume range the playlist offers, in dB. */
export const CLIP_GAIN_MIN_DB = -48;
export const CLIP_GAIN_MAX_DB = 12;

/** A clip volume clamped to the offered range and to 0.1 dB. `NaN` is unity. */
export function clampClipGain(db: number): number {
  if (!Number.isFinite(db)) return 0;
  const clamped = Math.min(CLIP_GAIN_MAX_DB, Math.max(CLIP_GAIN_MIN_DB, db));
  const rounded = Math.round(clamped * 10) / 10;
  return Object.is(rounded, -0) ? 0 : rounded;
}

/** `+3.0 dB` / `−6.5 dB`, or `''` for unity, which is not worth a label. */
export function formatClipGain(db: number | undefined): string {
  const value = clampClipGain(db ?? 0);
  if (Math.abs(value) < 0.05) return '';
  return `${value > 0 ? '+' : '−'}${Math.abs(value).toFixed(1)} dB`;
}

/**
 * Waveform peaks as the clip's volume makes them sound, clamped to full scale.
 *
 * So a clip turned down looks quieter than its neighbour on the same lane --
 * the volume reads off the arrangement without opening anything.
 */
export function gainPeaks(
  peaks: readonly (readonly [number, number])[],
  db: number | undefined,
): (readonly [number, number])[] {
  const value = clampClipGain(db ?? 0);
  if (value === 0) return peaks.slice();
  const k = 10 ** (value / 20);
  const clip = (x: number) => Math.min(1, Math.max(-1, x * k));
  return peaks.map(([lo, hi]) => [clip(lo), clip(hi)] as const);
}

/**
 * A sender that only ever sends the newest value, one at a time.
 *
 * A volume drag produces a value per pointer move; sending them all would
 * queue a backlog of engine commands that keeps changing the sound after the
 * hand has stopped. This keeps at most one in flight and, when it lands, sends
 * whatever is newest. `settled()` resolves once nothing is in flight, which is
 * when the gesture can be committed to undo against the engine's real state.
 */
export function latestOnly<T>(send: (value: T) => Promise<unknown>): {
  push(value: T): void;
  settled(): Promise<void>;
} {
  let wanted: { value: T } | null = null;
  let running: Promise<void> | null = null;
  const pump = async (): Promise<void> => {
    while (wanted) {
      const { value } = wanted;
      wanted = null;
      try {
        await send(value);
      } catch {
        // The caller reports its own failures; a failed send must not wedge
        // the queue for the next value.
      }
    }
    running = null;
  };
  return {
    push(value: T): void {
      wanted = { value };
      if (!running) running = pump();
    },
    async settled(): Promise<void> {
      while (running) await running;
    },
  };
}
