import { describe, expect, it } from 'vitest';
import { edgeSpeed, fitSpan, zoomScroll, zoomToRange } from './viewNav';

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

describe('zoom helpers', () => {
  it('keeps the content under the anchor in place', () => {
    // beat 10 under x = 200 at 20 px/beat (scroll 0): still under x = 200 at 40 px/beat
    const s = zoomScroll(0, 200, 20, 40);
    expect((s + 200) / 40).toBeCloseTo(10);
    expect(zoomScroll(0, 0, 20, 40)).toBe(0);
    expect(zoomScroll(0, 200, 40, 20)).toBe(0); // never scrolls before the start
  });
  it('fits a range to the view, centring one the limits cannot fit exactly', () => {
    expect(zoomToRange(8, 16, 800, 4, 96)).toEqual({ k: 96, scroll: 12 * 96 - 400 }); // 100 px per beat is past the limit
    const r = zoomToRange(16, 32, 800, 4, 96);
    expect(r.k).toBe(50); expect(r.scroll).toBe(800);
    // reversed ends are the same range
    expect(zoomToRange(32, 16, 800, 4, 96)).toEqual(r);
    // too long for the minimum zoom: centred
    const w = zoomToRange(0, 1000, 800, 4, 96);
    expect(w.k).toBe(4); expect(w.scroll).toBe(500 * 4 - 400);
  });
  it('fits the content to whole units, with a minimum', () => {
    expect(fitSpan(0, 4, 16)).toBe(16);
    expect(fitSpan(17, 4, 16)).toBe(20);
    expect(fitSpan(20, 4, 16)).toBe(20);
    expect(fitSpan(64.5, 4, 16)).toBe(68);
  });
});
