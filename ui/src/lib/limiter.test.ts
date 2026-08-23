import { describe, expect, it } from 'vitest';

import { limiterOutputDb, limiterTransferPoint } from './limiter';

describe('limiterOutputDb', () => {
  it('is a gain boost until the ceiling, then a brick wall', () => {
    expect(limiterOutputDb(-12, 6, 0)).toBeCloseTo(-6, 5);
    expect(limiterOutputDb(-1, 6, 0)).toBe(0);
    expect(limiterOutputDb(3, 0, -1)).toBe(-1);
  });
});

describe('limiterTransferPoint', () => {
  it('places the live dot at the input that produced this GR', () => {
    const point = limiterTransferPoint(6, 0, 3);
    expect(point.y).toBe(0);
    expect(point.x).toBeCloseTo(-3, 5);
    expect(limiterOutputDb(point.x, 6, 0)).toBeCloseTo(point.y, 5);
  });

  it('sits on the 1:1+gain line when nothing is reduced', () => {
    const idle = limiterTransferPoint(6, 0, 0);
    expect(idle.y).toBeCloseTo(idle.x + 6, 5);
    expect(idle.y).toBeLessThanOrEqual(0);
  });
});
