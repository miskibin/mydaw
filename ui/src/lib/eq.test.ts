import { describe, expect, it } from 'vitest';

import {
  analyserLine,
  analyserPath,
  bandHandle,
  eqHzFromX,
  eqX,
  eqY,
  eqDbFromY,
  hitBand,
  inferBand,
} from './eq';

describe('eq axis inverses', () => {
  it('round-trips frequency and dB through the box', () => {
    expect(eqHzFromX(eqX(1000, 400), 400)).toBeCloseTo(1000, 0);
    expect(eqDbFromY(eqY(6, 200, 18), 200, 18)).toBeCloseTo(6, 5);
    expect(eqY(0, 200, 18)).toBe(100);
  });
});

describe('inferBand', () => {
  it('picks HP on the far left, a shelf at the extremes, a bell in between', () => {
    const empty = new Map<string, number>();
    expect(inferBand(80, 2, 400, empty)).toBe('hp');
    expect(inferBand(25, 80, 400, empty)).toBe('low');
    expect(inferBand(12000, 360, 400, empty)).toBe('high');
    expect(inferBand(800, 200, 400, empty)).toBe('bell1');
  });

  it('prefers a free bell when both exist', () => {
    const values = new Map<string, number>([
      ['bell1.enabled', 1],
      ['bell1.freq', 500],
      ['bell2.enabled', 0],
      ['bell2.freq', 3000],
    ]);
    expect(inferBand(800, 200, 400, values)).toBe('bell2');
  });
});

describe('hitBand', () => {
  it('hits the handle sitting at a band frequency', () => {
    const values = new Map<string, number>([
      ['bell1.enabled', 1],
      ['bell1.freq', 1000],
      ['bell1.gain', 6],
    ]);
    const handle = bandHandle(values, 'bell1', 400, 200, 18)!;
    expect(hitBand(values, handle.x, handle.y, 400, 200, 18, 12)).toBe('bell1');
    expect(hitBand(values, 0, 0, 400, 200, 18, 4)).toBeNull();
  });
});

describe('analyserPath', () => {
  it('fills from the bottom of the box, not around 0 dB', () => {
    const d = analyserPath([0, -90], 100, 50, -90);
    expect(d.startsWith('M0 50')).toBe(true);
    expect(d.includes('0.00 0.00')).toBe(true);
    expect(d.endsWith('L100 50 Z')).toBe(true);
  });

  it('draws the ridge as a polyline on the same Y scale', () => {
    expect(analyserLine([0, -90], 100, 50, -90)).toBe('M0.00 0.00 L100.00 50.00');
  });
});
