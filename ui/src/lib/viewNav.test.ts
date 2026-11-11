import { describe, expect, it } from 'vitest';
import { edgeSpeed } from './viewNav';

describe('edge auto-scroll speed', () => {
  it('is zero away from the edges', () => {
    expect(edgeSpeed(200, 0, 400)).toBe(0);
    expect(edgeSpeed(28, 0, 400)).toBe(0);
    expect(edgeSpeed(372, 0, 400)).toBe(0);
  });
  it('ramps towards and past an edge, signed, capped', () => {
    expect(edgeSpeed(390, 0, 400)).toBeGreaterThan(0);
    expect(edgeSpeed(10, 0, 400)).toBeLessThan(0);
    expect(edgeSpeed(399, 0, 400)).toBeGreaterThan(edgeSpeed(380, 0, 400));
    expect(edgeSpeed(5000, 0, 400)).toBe(22);
    expect(edgeSpeed(-5000, 0, 400)).toBe(-22);
  });
  it('keeps a dead zone in a very small view', () => {
    expect(edgeSpeed(20, 0, 40)).toBe(0);
    expect(edgeSpeed(39, 0, 40)).toBeGreaterThan(0);
  });
});
