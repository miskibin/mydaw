import { describe, expect, it } from 'vitest';
import { sungToPattern, type SungNote } from './vocalNotes';

// 120 BPM: 0.5 s per beat, 0.125 s per step
const plain = { start: 0, t0: 0, t1: 100, spbSrc: 0.5 };
const n = (start: number, end: number, pitch: number, level = 1): SungNote => ({ start, end, pitch, level });

describe('sungToPattern (pitch editor → Piano roll)', () => {
  it('rounds pitch to semitones and time to the 1/16 grid', () => {
    const r = sungToPattern([n(0, 0.5, 60.3), n(0.51, 1.0, 61.6), n(1.0, 1.24, 63.49)], plain);
    expect(r.notes.map(x => [x.step, x.len, x.p])).toEqual([[0, 4, 60], [4, 4, 62], [8, 2, 63]]);
    expect(r.songStep).toBe(0);
  });

  it('follows the clip: song position, offset into the source and stretch', () => {
    // clip at beat 9 (bar 3, beat 2), source from 2 s, stretched 2×: 0.25 source s per beat
    const r = sungToPattern([n(2.25, 2.5, 67)], { start: 9, t0: 2, t1: 10, spbSrc: 0.25 });
    // source 2.25 s = beat 9 + 1 = beat 10 = step 40; the bar starts at step 32
    expect(r.songStep).toBe(32);
    expect(r.notes).toEqual([{ ch: 0, step: 8, len: 4, p: 67, v: 1 }]);
  });

  it('keeps only what the clip plays', () => {
    const r = sungToPattern([n(0, 1, 60), n(1.5, 2.5, 62), n(4, 5, 64)], { start: 0, t0: 2, t1: 4, spbSrc: 0.5 });
    // the second note is cut to its part from 2 s; the first and the last are outside
    expect(r.notes.map(x => [x.step, x.len, x.p])).toEqual([[0, 4, 62]]);
  });

  it('gives a very short note one step and keeps the voice monophonic', () => {
    const r = sungToPattern([n(0, 0.02, 60), n(0.5, 1.1, 62), n(1.06, 1.5, 64), n(1.5, 1.52, 65), n(1.51, 2, 67)], plain);
    // 60: one step. 62 overlaps 64 after rounding → ends where 64 starts. 65 and 67 land on step 12 → the longer stays
    expect(r.notes.map(x => [x.step, x.len, x.p])).toEqual([[0, 1, 60], [4, 4, 62], [8, 4, 64], [12, 4, 67]]);
  });

  it('maps level to velocity relative to the loudest note', () => {
    const r = sungToPattern([n(0, 0.5, 60, 0.2), n(0.5, 1, 62, 0.1), n(1, 1.5, 64, 0)], plain);
    expect(r.notes.map(x => x.v)).toEqual([1, 0.68, 0.35]);
  });

  it('keeps the melody\'s place in the bar and clamps the key range', () => {
    const r = sungToPattern([n(3.25, 3.5, 140), n(3.5, 3.75, -3)], plain);
    // 3.25 s = step 26 → bar 2 starts at step 16
    expect(r.songStep).toBe(16);
    expect(r.notes.map(x => [x.step, x.p])).toEqual([[10, 127], [12, 0]]);
  });

  it('returns nothing for no notes or notes with no pitch', () => {
    expect(sungToPattern([], plain)).toEqual({ notes: [], songStep: 0 });
    expect(sungToPattern([n(0, 1, NaN)], plain).notes).toEqual([]);
  });
});
