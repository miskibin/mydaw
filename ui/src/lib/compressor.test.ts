import { describe, expect, it } from 'vitest';

import { compressorOutputDb, inputDbFromGr, reductionDb } from './compressor';

describe('reductionDb', () => {
  it('is silent below the threshold and follows the slope above it', () => {
    expect(reductionDb(-30, -18, 4, 0)).toBe(0);
    expect(reductionDb(-6, -18, 4, 0)).toBeCloseTo(9, 5);
  });

  it('softens the corner inside the knee', () => {
    const hard = reductionDb(-18, -18, 4, 0);
    const soft = reductionDb(-18, -18, 4, 6);
    expect(hard).toBe(0);
    expect(soft).toBeGreaterThan(0);
    expect(soft).toBeLessThan(reductionDb(-15, -18, 4, 0));
  });
});

describe('inputDbFromGr', () => {
  it('inverts the curve so a GR readout can place the operating point', () => {
    const threshold = -18;
    const ratio = 4;
    const knee = 6;
    const input = -6;
    const gr = reductionDb(input, threshold, ratio, knee);
    expect(inputDbFromGr(gr, threshold, ratio, knee)).toBeCloseTo(input, 2);
  });

  it('lands at the threshold when there is no reduction', () => {
    expect(inputDbFromGr(0, -18, 4, 0)).toBeCloseTo(-18, 5);
  });
});

describe('compressorOutputDb', () => {
  it('adds makeup after reduction', () => {
    expect(compressorOutputDb(-6, -18, 4, 0, 3)).toBeCloseTo(-6 - 9 + 3, 5);
  });
});
