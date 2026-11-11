import { describe, expect, it } from 'vitest';
import { centreAt, clampPos, dragEdge, dragThumb, thumbOf, zoomAround } from './zoomBar';

const lim = { total: 100, minLen: 5, maxLen: 200 };

describe('zoom scrollbar: thumb', () => {
  it('is proportional to the visible part', () => {
    expect(thumbOf({ pos: 25, len: 25 }, 100, 400)).toEqual({ x: 100, w: 100 });
    expect(thumbOf({ pos: 75, len: 25 }, 100, 400)).toEqual({ x: 300, w: 100 });
  });
  it('keeps a minimum size and still reaches both ends', () => {
    const a = thumbOf({ pos: 0, len: 1 }, 1000, 200, 20), b = thumbOf({ pos: 999, len: 1 }, 1000, 200, 20);
    expect(a).toEqual({ x: 0, w: 20 });
    expect(b.x + b.w).toBeCloseTo(200);
  });
  it('fills the track when everything is visible (also past the content)', () => {
    expect(thumbOf({ pos: 0, len: 150 }, 100, 300)).toEqual({ x: 0, w: 300 });
  });
});

describe('zoom scrollbar: scrolling', () => {
  it('moves with the pointer and stays inside the content', () => {
    const r0 = { pos: 10, len: 25 };
    expect(dragThumb(r0, 100, 400, 40).pos).toBeCloseTo(20);
    expect(dragThumb(r0, 100, 400, 4000).pos).toBe(75);
    expect(dragThumb(r0, 100, 400, -4000).pos).toBe(0);
    expect(dragThumb(r0, 100, 400, 40).len).toBe(25);
  });
  it('follows the pointer with a minimum-size thumb too', () => {
    const r0 = { pos: 0, len: 1 }, W = 200, total = 1000;
    const r = dragThumb(r0, total, W, 90, 20);
    expect(thumbOf(r, total, W, 20).x).toBeCloseTo(90);
  });
  it('does nothing when there is nothing to scroll', () => {
    expect(dragThumb({ pos: 0, len: 120 }, 100, 300, 50)).toEqual({ pos: 0, len: 120 });
  });
  it('centres the view where the track is pressed', () => {
    expect(centreAt({ pos: 0, len: 20 }, 100, 400, 200).pos).toBe(40);
    expect(centreAt({ pos: 50, len: 20 }, 100, 400, 0).pos).toBe(0);
    expect(centreAt({ pos: 0, len: 20 }, 100, 400, 400).pos).toBe(80);
  });
  it('clamps a position', () => {
    expect(clampPos({ pos: 0, len: 30 }, 100, 90)).toEqual({ pos: 70, len: 30 });
    expect(clampPos({ pos: 0, len: 130 }, 100, 10)).toEqual({ pos: 0, len: 130 });
  });
});

describe('zoom scrollbar: edge drag zooms', () => {
  it('right end: the left end stays put', () => {
    const r = dragEdge({ pos: 20, len: 20 }, 'hi', lim, 400, 40); // 40 px = 10 units
    expect(r).toEqual({ pos: 20, len: 30 });
    const s = dragEdge({ pos: 20, len: 20 }, 'hi', lim, 400, -40);
    expect(s).toEqual({ pos: 20, len: 10 });
  });
  it('left end: the right end stays put', () => {
    const r = dragEdge({ pos: 20, len: 20 }, 'lo', lim, 400, -40);
    expect(r.pos).toBeCloseTo(10); expect(r.pos + r.len).toBeCloseTo(40);
    const s = dragEdge({ pos: 20, len: 20 }, 'lo', lim, 400, 40);
    expect(s.pos).toBeCloseTo(30); expect(s.pos + s.len).toBeCloseTo(40);
  });
  it('stops at the zoom limits instead of flipping or moving the other end', () => {
    const r = dragEdge({ pos: 20, len: 20 }, 'lo', lim, 400, 400);
    expect(r).toEqual({ pos: 35, len: 5 });
    const s = dragEdge({ pos: 20, len: 20 }, 'hi', lim, 400, -400);
    expect(s).toEqual({ pos: 20, len: 5 });
    const t = dragEdge({ pos: 20, len: 20 }, 'hi', { ...lim, maxLen: 25 }, 400, 400);
    expect(t).toEqual({ pos: 20, len: 25 });
  });
  it('stays inside the content', () => {
    const r = dragEdge({ pos: 20, len: 20 }, 'lo', lim, 400, -4000);
    expect(r).toEqual({ pos: 0, len: 40 });
    const s = dragEdge({ pos: 20, len: 20 }, 'hi', lim, 400, 4000);
    expect(s).toEqual({ pos: 20, len: 80 });
  });
});

describe('zoom scrollbar: zoom around a point', () => {
  it('keeps the anchor at the same place in the view', () => {
    const r = zoomAround({ pos: 20, len: 20 }, lim, 25, 2);
    expect(r.len).toBe(40);
    expect((25 - r.pos) / r.len).toBeCloseTo((25 - 20) / 20);
  });
  it('respects the limits and the content', () => {
    expect(zoomAround({ pos: 20, len: 20 }, lim, 30, 0.01).len).toBe(5);
    expect(zoomAround({ pos: 0, len: 20 }, lim, 0, 2).pos).toBe(0);
  });
});
