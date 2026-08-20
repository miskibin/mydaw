import { describe, expect, it } from 'vitest';

import {
  MIN_REGION_FRAMES,
  dragRegion,
  formatFrames,
  frameAt,
  framePercent,
  pitchLabel,
  resolveRegion,
  stepSemitone,
} from './sampleRegion';

describe('resolveRegion', () => {
  it("reads the engine's zero end as the last frame", () => {
    expect(resolveRegion(100, 0, 48_000)).toEqual({ start: 100, end: 48_000 });
  });

  it('falls back to the whole file when the pair is nonsense', () => {
    expect(resolveRegion(9_000, 500, 48_000)).toEqual({ start: 0, end: 48_000 });
    expect(resolveRegion(0, 0, 0)).toEqual({ start: 0, end: 0 });
  });

  it('holds both ends inside the file', () => {
    expect(resolveRegion(-20, 90_000, 48_000)).toEqual({ start: 0, end: 48_000 });
  });
});

describe('dragRegion', () => {
  it('accepts a drag in either direction', () => {
    expect(dragRegion(30_000, 1_000, 48_000)).toEqual({
      start: 1_000,
      end: 30_000,
      storedEnd: 30_000,
    });
  });

  it('stores a region that runs to the end as the shorthand zero', () => {
    expect(dragRegion(1_000, 48_000, 48_000).storedEnd).toBe(0);
  });

  it('never leaves a region shorter than the minimum', () => {
    const region = dragRegion(1_000, 1_002, 48_000);
    expect(region.end - region.start).toBe(MIN_REGION_FRAMES);
  });

  it('pushes a too-short region at the very end backwards instead of past it', () => {
    const region = dragRegion(48_000, 48_000, 48_000);
    expect(region.end).toBe(48_000);
    expect(region.start).toBe(48_000 - MIN_REGION_FRAMES);
    expect(region.storedEnd).toBe(0);
  });
});

describe('framePercent and frameAt', () => {
  it('map between frames and the drawn width', () => {
    expect(framePercent(24_000, 48_000)).toBe(50);
    expect(framePercent(90_000, 48_000)).toBe(100);
    expect(framePercent(10, 0)).toBe(0);
    expect(frameAt(0.25, 48_000)).toBe(12_000);
    expect(frameAt(-1, 48_000)).toBe(0);
    expect(frameAt(2, 48_000)).toBe(48_000);
  });
});

describe('formatFrames', () => {
  it('reads seconds, and minutes once there are any', () => {
    expect(formatFrames(48_000, 48_000)).toBe('1.000 s');
    expect(formatFrames(48_000 * 75, 48_000)).toBe('1:15.000');
    expect(formatFrames(1_000, 0)).toBe('1000');
  });
});

describe('pitchLabel', () => {
  it('speaks semitones and cents', () => {
    expect(pitchLabel(0)).toBe('root');
    expect(pitchLabel(214)).toBe('+2 st +14 ct');
    expect(pitchLabel(-1_200)).toBe('-12 st');
    expect(pitchLabel(-8)).toBe('-8 ct');
    expect(pitchLabel(100)).toBe('+1 st');
  });
});

describe('stepSemitone', () => {
  it('lands on the semitone grid rather than carrying a detune along', () => {
    expect(stepSemitone(214, 1)).toBe(300);
    expect(stepSemitone(214, -1)).toBe(200);
    expect(stepSemitone(-214, 1)).toBe(-200);
    expect(stepSemitone(0, -1)).toBe(-100);
  });

  it('stops at the octave', () => {
    expect(stepSemitone(1_200, 1)).toBe(1_200);
    expect(stepSemitone(-1_200, -1)).toBe(-1_200);
  });
});
