import { describe, expect, it } from 'vitest';

import { delayTimeMs, syncDelayMs } from './delayTime';
import { correlation, goniometerFromPoint } from './goniometer';

describe('syncDelayMs', () => {
  it('puts a quarter note at 120 BPM on 500 ms', () => {
    expect(syncDelayMs(2, 0, 120)).toBeCloseTo(500, 5);
    expect(delayTimeMs(false, 2, 0, 375, 120)).toBe(375);
  });

  it('halves a delay past the eight-second line, like the engine does', () => {
    // Dotted 1/1 at 20 BPM asks for 18 s; the line holds 8, so the engine
    // plays 18 → 9 → 4.5 s, and the readout must say so.
    expect(syncDelayMs(0, 1, 20)).toBeCloseTo(18_000, 5);
    expect(delayTimeMs(true, 0, 1, 0, 20)).toBeCloseTo(4_500, 5);
  });
});

describe('correlation', () => {
  it('is 1 for identical channels and −1 when they invert', () => {
    expect(correlation([1, 0, -1, 0], [1, 0, -1, 0])).toBeCloseTo(1, 5);
    expect(correlation([1, 0, -1, 0], [-1, 0, 1, 0])).toBeCloseTo(-1, 5);
  });
});

describe('goniometerFromPoint', () => {
  it('reads centre as pan 0 and mid-height as 100 % width', () => {
    expect(goniometerFromPoint(100, 50, 200, 100)).toEqual({ pan: 0, widthPct: 100 });
    expect(goniometerFromPoint(200, 0, 200, 100).pan).toBe(1);
    expect(goniometerFromPoint(0, 100, 200, 100).widthPct).toBe(0);
  });
});
