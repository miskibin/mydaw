import { describe, expect, it } from 'vitest';

import { crushedCycle, holdRateHz, linePath, quantise, quantiseStep, stepPath } from './bitcrusher';

describe('quantise', () => {
  it('lands on the mid-tread grid the engine uses', () => {
    // One bit is three levels: −1, 0, +1.
    for (const x of [-1, -0.6, -0.2, 0, 0.2, 0.6, 1]) {
      expect([-1, 0, 1]).toContain(quantise(x, 1));
    }
    expect(quantise(0.6, 1)).toBe(1);
    expect(quantise(0.4, 1)).toBe(0);
    // Zero is a level at every depth, so silence stays silent.
    for (const bits of [1, 2, 8, 16]) expect(quantise(0, bits)).toBe(0);
  });

  it('halves the step for every bit added', () => {
    expect(quantiseStep(2)).toBeCloseTo(0.5, 12);
    expect(quantiseStep(3)).toBeCloseTo(0.25, 12);
    expect(quantiseStep(8)).toBeCloseTo(2 ** -7, 12);
    // At the top of the knob the quantiser is switched out.
    expect(quantiseStep(24)).toBeNull();
  });
});

describe('crushedCycle', () => {
  it('holds every sample when nothing is downsampled', () => {
    const { clean, crushed } = crushedCycle(24, 1, 64);
    expect(clean).toHaveLength(64);
    // 24 bits is the off position: the crushed wave is the clean one, one
    // sample late (the hold captures at the top of the sample).
    expect(crushed).toEqual(clean);
  });

  it('holds each value for exactly the ratio the knob says', () => {
    const { crushed } = crushedCycle(24, 8, 256);
    let runs = 0;
    let previous = crushed[0];
    let length = 0;
    const lengths: number[] = [];
    for (const value of crushed) {
      if (value === previous) length += 1;
      else {
        lengths.push(length);
        length = 1;
        previous = value;
        runs += 1;
      }
    }
    expect(runs).toBeGreaterThan(8);
    // Ignore the first and last, which the window clips.
    for (const run of lengths.slice(1)) expect(run).toBe(8);
  });

  it('quantises to the levels the bit depth allows', () => {
    const { crushed } = crushedCycle(2, 1, 128);
    const levels = new Set(crushed.map((value) => value.toFixed(6)));
    // Two bits over a sine that reaches ±1: −1, −0.5, 0, 0.5, 1.
    expect(levels.size).toBeLessThanOrEqual(5);
    for (const value of crushed) expect(Math.abs(value * 2 - Math.round(value * 2))).toBeLessThan(1e-9);
  });
});

describe('paths', () => {
  it('draws the hold as treads and risers rather than as diagonals', () => {
    const d = stepPath([0, 1], 100, 100);
    // A horizontal move for each sample and a vertical one between them.
    expect(d.startsWith('M0 50')).toBe(true);
    expect(d).toContain('H50.00');
    expect(d).toContain('V0.00');
    expect(d).toContain('H100.00');
    expect(stepPath([], 100, 100)).toBe('');
  });

  it('draws the reference wave as a polyline', () => {
    expect(linePath([1, -1], 100, 100)).toBe('M0.00 0.00 L100.00 100.00');
    expect(linePath([0], 10, 10)).toBe('');
  });
});

describe('holdRateHz', () => {
  it('is the sample rate divided by the ratio', () => {
    expect(holdRateHz(48_000, 8)).toBe(6_000);
    expect(holdRateHz(48_000, 1)).toBe(48_000);
    expect(holdRateHz(0, 4)).toBe(12_000);
  });
});
