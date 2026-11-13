import { describe, expect, it } from 'vitest';
import { clipOfPattern, loopPos, loopSpan, patBeatAt, songBeatIn } from './patLoop';

const clip = (start: number, len: number, offset = 0, ref = 1) => ({ kind: 'pat', ref, start, len, offset, muted: false });

describe('pattern loop range (as rt.rs loop_span plays it)', () => {
  it('keeps a range inside the pattern, in order', () => {
    expect(loopSpan({ a: 3, b: 1 }, 4)).toEqual({ a: 1, b: 3 });
    expect(loopSpan({ a: -2, b: 9 }, 4)).toEqual({ a: 0, b: 4 });
  });
  it('loops the whole pattern without a range or with one too short to loop', () => {
    expect(loopSpan(null, 8)).toEqual({ a: 0, b: 8 });
    expect(loopSpan({ a: 2, b: 2 }, 8)).toEqual({ a: 0, b: 8 });
    expect(loopSpan({ a: 5, b: 9 }, 4)).toEqual({ a: 0, b: 4 }); // past the end: nothing left of it
    expect(loopSpan({ a: NaN, b: 1 }, 4)).toEqual({ a: 0, b: 4 });
  });
  it('wraps the position inside the range; a start outside it goes to the range start', () => {
    expect(loopPos(1, 0.5, null, 4)).toBe(1.5);
    expect(loopPos(3.5, 1, null, 4)).toBe(0.5);
    expect(loopPos(2, 1.5, { a: 2, b: 3 }, 4)).toBe(2.5);
    expect(loopPos(0, 0.25, { a: 2, b: 3 }, 4)).toBe(2.25);
  });
});

describe('pattern beat ↔ song beat (the Piano roll ruler in SONG mode)', () => {
  const clips = [clip(16, 4), clip(4, 8), clip(0, 4, 0, 2)];
  it('picks the clip playing at the position, else the next one, else the first', () => {
    expect(clipOfPattern(clips, 1, 5)?.start).toBe(4);
    expect(clipOfPattern(clips, 1, 13)?.start).toBe(16);
    expect(clipOfPattern(clips, 1, 30)?.start).toBe(4);
    expect(clipOfPattern(clips, 3, 0)).toBeNull();
  });
  it('maps into the pass nearest the position in a clip that repeats the pattern', () => {
    const c = clip(4, 8); // a 4-beat pattern played twice from beat 4
    expect(songBeatIn(c, 4, 1, 0)).toBe(5);
    expect(songBeatIn(c, 4, 1, 9)).toBe(9);
    expect(songBeatIn(c, 4, 3, 9)).toBe(11);
    expect(songBeatIn(c, 4, 0, 11.9)).toBe(8);
    expect(songBeatIn(c, 4, 0, 30)).toBe(8);
  });
  it('honours the clip offset and is null for a beat the clip never plays', () => {
    const c = clip(8, 2, 1); // plays pattern beats 1..3 of a 4-beat pattern
    expect(songBeatIn(c, 4, 2, 0)).toBe(9);
    expect(songBeatIn(c, 4, 0, 0)).toBeNull();
    expect(patBeatAt(c, 4, 9.5)).toBe(2.5);
    expect(patBeatAt(c, 4, 10)).toBeNull();
  });
  it('round-trips', () => {
    const c = clip(4, 8, 2);
    for (const pb of [0, 1.5, 3.75]) {
      const s = songBeatIn(c, 4, pb, 6)!;
      expect(patBeatAt(c, 4, s)).toBeCloseTo(pb);
    }
  });
});
