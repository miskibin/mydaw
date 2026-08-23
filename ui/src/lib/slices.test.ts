import { describe, expect, it } from 'vitest';

import {
  MAX_SLICES,
  markerNear,
  minSliceFrames,
  normalizeSlices,
  resolveSlices,
  sliceAt,
  sliceLabel,
  sliceTitle,
  withMarkerAdded,
  withMarkerMoved,
  withMarkerRemoved,
} from './slices';

const RATE = 48_000;

describe('resolveSlices', () => {
  it('runs each slice to the next marker and the last one to the region end', () => {
    const slices = resolveSlices([0, 1_000, 2_500], 4_000, 60);
    expect(slices.map((slice) => [slice.start, slice.end])).toEqual([
      [0, 1_000],
      [1_000, 2_500],
      [2_500, 4_000],
    ]);
    expect(slices.map((slice) => slice.key)).toEqual([60, 61, 62]);
  });

  it('drops markers the region no longer reaches', () => {
    const slices = resolveSlices([0, 1_000, 9_000], 2_000, 60);
    expect(slices).toHaveLength(2);
    expect(slices.at(-1)!.end).toBe(2_000);
  });

  it('says when a slice is past the keyboard instead of inventing a key', () => {
    const frames = Array.from({ length: 4 }, (_, index) => index * 100);
    const slices = resolveSlices(frames, 1_000, 126);
    expect(slices.map((slice) => slice.key)).toEqual([126, 127, null, null]);
    expect(sliceTitle(slices[2]!, RATE)).toContain('not playable');
  });

  it('names a slice by its number, its key and its length', () => {
    const slices = resolveSlices([0, 4_800], 9_600, 60);
    expect(sliceLabel(slices[0]!.index)).toBe('1');
    expect(sliceTitle(slices[0]!, RATE)).toContain('Slice 1');
    expect(sliceTitle(slices[0]!, RATE)).toContain('C4');
    expect(sliceTitle(slices[0]!, RATE)).toContain('100 ms');
  });
});

describe('finding a marker', () => {
  it('reports the slice a frame falls in, and -1 before the first marker', () => {
    expect(sliceAt([0, 100, 200], 150)).toBe(1);
    expect(sliceAt([0, 100, 200], 200)).toBe(2);
    expect(sliceAt([100, 200], 50)).toBe(-1);
  });

  it('grabs the nearest marker inside the tolerance and nothing outside it', () => {
    expect(markerNear([0, 1_000, 2_000], 1_040, 60)).toBe(1);
    expect(markerNear([0, 1_000, 2_000], 1_500, 60)).toBe(-1);
    expect(markerNear([0, 1_000, 1_020], 1_015, 60)).toBe(2);
  });
});

describe('editing markers', () => {
  it('nudges a marker and keeps it clear of both neighbours', () => {
    const gap = minSliceFrames(RATE);
    const frames = [0, 1_000, 2_000];
    expect(withMarkerMoved(frames, 1, 1_500, 0, 3_000, RATE)[1]).toBe(1_500);
    // Dragged past the neighbour: parked against it, not reordered.
    expect(withMarkerMoved(frames, 1, 9_999, 0, 3_000, RATE)[1]).toBe(2_000 - gap);
    expect(withMarkerMoved(frames, 1, -9_999, 0, 3_000, RATE)[1]).toBe(0 + gap);
  });

  it('pins the head of the chop', () => {
    const frames = [0, 1_000];
    expect(withMarkerMoved(frames, 0, 500, 0, 3_000, RATE)).toBe(frames);
    expect(withMarkerRemoved(frames, 0)).toBe(frames);
  });

  it('splits a slice where the pointer landed', () => {
    const frames = [0, 2_000];
    const next = withMarkerAdded(frames, 900, 0, 4_000, RATE);
    expect(next).toEqual([0, 900, 2_000]);
  });

  it('refuses a split on top of a marker, or past the region, by reference', () => {
    const frames = [0, 2_000];
    expect(withMarkerAdded(frames, 2_001, 0, 4_000, RATE)).toBe(frames);
    expect(withMarkerAdded(frames, 9_000, 0, 4_000, RATE)).toBe(frames);
    expect(withMarkerAdded(frames, 0, 0, 4_000, RATE)).toBe(frames);
  });

  it('refuses a split once the engine table is full', () => {
    const frames = Array.from({ length: MAX_SLICES }, (_, index) => index * 1_000);
    expect(withMarkerAdded(frames, 500, 0, 999_000, RATE)).toBe(frames);
  });

  it('removes a marker, merging its slice into the one before', () => {
    expect(withMarkerRemoved([0, 1_000, 2_000], 1)).toEqual([0, 2_000]);
    expect(resolveSlices(withMarkerRemoved([0, 1_000, 2_000], 1), 3_000, 60)).toHaveLength(2);
  });
});

describe('normalizeSlices', () => {
  it('sorts, dedupes, clamps to the region and caps at the engine table', () => {
    expect(normalizeSlices([900, 100, 500, 100, -5, 99_999], 0, 1_000)).toEqual([100, 500, 900]);
    const many = Array.from({ length: 400 }, (_, index) => index * 10);
    expect(normalizeSlices(many, 0, 999_999)).toHaveLength(MAX_SLICES);
    expect(normalizeSlices([Number.NaN, 10], 0, 100)).toEqual([10]);
  });
});
