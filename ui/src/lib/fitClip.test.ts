import { describe, expect, it } from 'vitest';
import type { Clip } from '../api/types';
import { fitClip } from './clipOps';

const clip = (p: Partial<Clip> = {}): Clip => ({ id: 1, track: 0, start: 4, len: 3, offset: 0, kind: 'audio', ref: 9, muted: false, gain: 0, fadeIn: 0, fadeOut: 0, pan: 0, norm: false, rev: false, ...p });

describe('fitClip', () => {
  it('stretches the clip to the length, keeping its start', () => {
    const a = fitClip(clip(), 8)!, b = fitClip(clip(), 2)!;
    expect(a).toMatchObject({ start: 4, len: 8, offset: 0 });
    expect(a.stretch).toBeCloseTo(8 / 3, 8);
    expect(b.len).toBe(2);
    expect(b.stretch).toBeCloseTo(2 / 3, 8);
  });
  it('keeps the same audio: offset and stretch scale together', () => {
    // a region 2 beats into an already 2× stretched clip, 4 beats long → 8 beats
    const f = fitClip(clip({ stretch: 2, offset: 2, len: 4 }), 8)!;
    expect(f).toMatchObject({ stretch: 4, offset: 4, len: 8 });
    // the region in unstretched beats is the same
    expect(f.offset / f.stretch!).toBeCloseTo(2 / 2);
    expect(f.len / f.stretch!).toBeCloseTo(4 / 2);
  });
  it('keeps pitch, resample and the other settings', () => {
    expect(fitClip(clip({ pitch: 3, resample: true, gain: -2 }), 4)).toMatchObject({ pitch: 3, resample: true, gain: -2 });
  });
  it('refuses lengths outside the stretch range or past the song end', () => {
    expect(fitClip(clip({ len: 1 }), 8)).toBeNull(); // ×8
    expect(fitClip(clip({ len: 32 }), 4)).toBeNull(); // ×⅛
    expect(fitClip(clip({ start: 2040, len: 4 }), 16)).toBeNull();
    expect(fitClip(clip({ kind: 'pat' }), 4)).toBeNull();
  });
});
