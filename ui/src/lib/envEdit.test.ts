import { describe, expect, it } from 'vitest';
import type { AutoPoint } from '../api/types';
import { valueAt } from './automation';
import { addPoint, deletePoints, dragTension, handleAt, hitEnv, movePoints, pointsInBox, setShape, type EnvGeom } from './envEdit';

// 10 px per beat, values 0..1 over 100 px (y down)
const G: EnvGeom = { X: t => t * 10, T: x => x / 10, Y: v => 100 - v * 100, V: y => Math.min(1, Math.max(0, (100 - y) / 100)) };
const P: AutoPoint[] = [{ t: 0, v: 0 }, { t: 8, v: 1 }, { t: 16, v: 0.5, shape: 'hold' }, { t: 24, v: 0 }];

describe('what the pointer is on', () => {
  it('picks a point within reach, the nearest one', () => {
    expect(hitEnv(P, G, 81, 2)).toEqual({ kind: 'point', i: 1 });
    expect(hitEnv(P, G, 40, 100)).toBeNull();
  });
  it('picks the handle in the middle of a curve, on the curve', () => {
    const h = handleAt(P, 1)!;
    expect(h).toEqual({ t: 4, v: 0.5 });
    expect(hitEnv(P, G, 40, 50)).toEqual({ kind: 'handle', i: 1 });
  });
  it('shows no handle on a hold, before the first point or on a narrow segment', () => {
    expect(handleAt(P, 2)).toBeNull();
    expect(handleAt(P, 0)).toBeNull();
    expect(hitEnv([{ t: 0, v: 0 }, { t: 1, v: 1 }], G, 5, 50)).toBeNull();
  });
  it('prefers a point over a handle under it', () => {
    // an 18 px segment: handle at x 9, point at x 18; the pointer at 13 reaches both
    expect(hitEnv([{ t: 0, v: 0.5 }, { t: 1.8, v: 0.5 }], G, 13, 50)).toEqual({ kind: 'point', i: 1 });
  });
});

describe('bending a segment', () => {
  it('follows the pointer up on a rising and on a falling curve', () => {
    const up = (a: AutoPoint, b: AutoPoint) => {
      const k = dragTension(0, -40, a, b);
      return valueAt([a, { ...b, tension: k }], (a.t + b.t) / 2)!;
    };
    expect(up({ t: 0, v: 0 }, { t: 4, v: 1 })).toBeGreaterThan(0.5);
    expect(up({ t: 0, v: 1 }, { t: 4, v: 0 })).toBeGreaterThan(0.5);
  });
  it('is finer with Ctrl and stays within −1..1', () => {
    const a = { t: 0, v: 0 }, b = { t: 4, v: 1 };
    expect(Math.abs(dragTension(0, 40, a, b, true))).toBeLessThan(Math.abs(dragTension(0, 40, a, b)));
    expect(dragTension(0.9, -1000, a, b)).toBe(-1);
    expect(dragTension(0, -1000, a, { ...b, shape: 'stairs' })).toBe(1); // up = more steps
  });
});

describe('point edits', () => {
  it('moves a selection together without passing the other points', () => {
    const r = movePoints(P, new Set([1, 2]), 20, 0.1);
    expect(r.dt).toBe(8); // up to the next unselected point (t 24 − 16)
    expect(r.points.map(q => q.t)).toEqual([0, 16, 24, 24]);
    expect(r.points[1].v).toBe(1); // clamped
    expect(r.points[2].shape).toBe('hold'); // keeps its segment
    expect(movePoints(P, new Set([1]), -50, 0).points[1].t).toBe(0);
  });
  it('adds a point that keeps the split segment\'s shape, and deletes leaving at least one', () => {
    const a = addPoint(P, 20, 0.7);
    expect(a.i).toBe(3);
    expect(a.points[3]).toEqual({ t: 20, v: 0.7 });
    const b = addPoint(P, 12, 0.2);
    expect(b.points[2]).toMatchObject({ t: 12, v: 0.2, shape: 'hold' });
    expect(deletePoints(P, new Set([0, 1, 2, 3]))).toBeNull();
    expect(deletePoints(P, new Set([1]))!.map(q => q.t)).toEqual([0, 16, 24]);
  });
  it('selects points in a box and sets a shape on their segments', () => {
    expect(pointsInBox(P, G, 70, -5, 170, 60)).toEqual([1, 2]);
    const s = setShape(P, [0, 1, 2], 'stairs');
    expect(s[0]).toEqual({ t: 0, v: 0 }); // the first point has no segment
    expect(s[1].shape).toBe('stairs');
    expect(setShape(s, [1], 'curve')[1]).toEqual({ t: 8, v: 1 });
  });
});
