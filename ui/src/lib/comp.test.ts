import { describe, expect, it } from 'vitest';
import type { Comp, Project } from '../api/types';
import { boundaries, landTakes, moveBoundary, removeTake, setBoundaryXfade, swapRange, unusedTakes, validateComp, writeRange } from './comp';

const comp = (regions: Comp['regions'], n = 3): Comp => ({
  id: 100, name: 'Comp 1', xfade: 0.01, regions,
  takes: Array.from({ length: n }, (_, k) => ({ id: 1 + k, name: 'Take ' + (k + 1), ref: 50 + k, start: 0, len: 8, offset: 0 })),
});
const r = (take: number, from: number, to: number) => ({ take, from, to });

describe('swipe comping: writeRange', () => {
  it('replaces whatever overlapped the swiped range', () => {
    const c = comp([r(1, 0, 8)]);
    expect(writeRange(c, 2, 2, 4)).toEqual([r(1, 0, 2), r(2, 2, 4), r(1, 4, 8)]);
  });
  it('removes regions fully covered and trims partial ones', () => {
    const c = comp([r(1, 0, 3), r(2, 3, 5), r(3, 5, 8)]);
    expect(writeRange(c, 1, 2, 6)).toEqual([r(1, 0, 6), r(3, 6, 8)]);
  });
  it('merges adjacent pieces of the same take', () => {
    const c = comp([r(1, 0, 2), r(2, 2, 4), r(1, 4, 8)]);
    expect(writeRange(c, 1, 2, 4)).toEqual([r(1, 0, 8)]);
    expect(writeRange(comp([r(2, 0, 2)]), 2, 2, 5)).toEqual([r(2, 0, 5)]);
  });
  it('fills gaps and is clipped to the take', () => {
    const c = comp([r(1, 0, 2)]);
    c.takes[1] = { ...c.takes[1], start: 1, len: 3 };
    expect(writeRange(c, 2, -5, 99)).toEqual([r(1, 0, 1), r(2, 1, 4)]);
  });
  it('ignores empty ranges and unknown takes', () => {
    const c = comp([r(1, 0, 8)]);
    expect(writeRange(c, 2, 3, 3)).toEqual([r(1, 0, 8)]);
    expect(writeRange(c, 42, 0, 2)).toEqual([r(1, 0, 8)]);
  });
  it('keeps the crossfade of untouched boundaries but not of the split piece', () => {
    const c = comp([r(1, 0, 4), { take: 2, from: 4, to: 8, xfade: 0.05 }]);
    const out = writeRange(c, 3, 5, 6);
    expect(out[1]).toEqual({ take: 2, from: 4, to: 5, xfade: 0.05 });
    expect(out[3]).toEqual(r(2, 6, 8));
  });
});

describe('boundaries', () => {
  it('lists shared and lone edges', () => {
    const b = boundaries([r(1, 0, 2), r(2, 2, 4), r(3, 5, 8)]);
    expect(b.map(x => [x.at, x.left, x.right])).toEqual([[0, -1, 0], [2, 0, 1], [4, 1, -1], [5, -1, 2], [8, 2, -1]]);
  });
  it('drags a shared boundary within both regions and takes', () => {
    const c = comp([r(1, 0, 2), r(2, 2, 4)]);
    const b = boundaries(c.regions)[1];
    expect(moveBoundary(c, b, 3, 0.25)).toEqual([r(1, 0, 3), r(2, 3, 4)]);
    expect(moveBoundary(c, b, 10, 0.25)).toEqual([r(1, 0, 3.75), r(2, 3.75, 4)]);
  });
  it('sets a boundary crossfade on the region starting there', () => {
    const c = comp([r(1, 0, 2), r(2, 2, 4)]);
    expect(setBoundaryXfade(c, boundaries(c.regions)[1], 0.02)[1].xfade).toBe(0.02);
    expect(setBoundaryXfade(c, boundaries(c.regions)[2], 0.05)[1].xfade).toBe(0.05);
  });
});

describe('take swaps and removal', () => {
  it('Ctrl+↓ moves a range to the next take, Ctrl+↑ back', () => {
    const c = comp([r(1, 0, 8)]);
    const down = swapRange(c, 2, 4, 1)!;
    expect(down.take.id).toBe(2);
    expect(down.regions).toEqual([r(1, 0, 2), r(2, 2, 4), r(1, 4, 8)]);
    const up = swapRange({ ...c, regions: down.regions }, 2, 4, -1)!;
    expect(up.regions).toEqual([r(1, 0, 8)]);
    expect(swapRange(c, 2, 4, -1)).toBeNull();
  });
  it('removing a take silences its regions; unused takes are found', () => {
    const c = comp([r(1, 0, 2), r(2, 2, 8)]);
    expect(removeTake(c, 2).regions).toEqual([r(1, 0, 2)]);
    expect(unusedTakes(c).map(t => t.id)).toEqual([3]);
  });
  it('validation rejects overlaps and normalises order', () => {
    const p = { sources: [50, 51, 52].map(id => ({ id })) } as unknown as Project;
    expect(() => validateComp(p, comp([r(1, 0, 3), r(2, 2, 4)]))).toThrow(/overlap/);
    expect(validateComp(p, comp([r(2, 2, 4), r(1, 0, 2)])).regions).toEqual([r(1, 0, 2), r(2, 2, 4)]);
  });
});

describe('landing recorded passes', () => {
  const base = (): Project => ({ version: 2, bpm: 120, tracks: [{ id: 1 } as never], inserts: [], patterns: [], sources: [], clips: [], loop: null, nextId: 1000, comps: [] });
  it('five loop passes make one comp with five takes, newest active', () => {
    const p = base(); let id = 2000;
    landTakes(p, 0, [1, 2, 3, 4, 5].map(k => [k, 8, 4] as [number, number, number]), () => id++);
    expect(p.comps!.length).toBe(1);
    const c = p.comps![0];
    expect(c.takes.length).toBe(5);
    expect(c.regions).toEqual([{ take: c.takes[4].id, from: 0, to: 4 }]);
    expect(p.clips[0]).toMatchObject({ kind: 'comp', start: 8, len: 4, offset: 0, ref: c.id });
  });
  it('a last pass cut short by Stop plays only where it has audio', () => {
    const p = base(); let id = 2000;
    landTakes(p, 0, [[1, 8, 4], [2, 8, 4], [3, 8, 1.5]], () => id++);
    const c = p.comps![0];
    expect(c.regions).toEqual([{ take: c.takes[2].id, from: 0, to: 1.5 }, { take: c.takes[1].id, from: 1.5, to: 4 }]);
  });
  it('one pass over a plain clip turns both into takes; over a comp it adds a take', () => {
    const p = base(); let id = 2000;
    landTakes(p, 0, [[1, 4, 4]], () => id++);
    expect(p.clips[0].kind).toBe('audio');
    landTakes(p, 0, [[2, 4, 4]], () => id++);
    expect(p.clips.map(c => c.kind)).toEqual(['comp']);
    expect(p.comps![0].takes.map(t => t.ref)).toEqual([1, 2]);
    landTakes(p, 0, [[3, 2, 4]], () => id++); // starts earlier: comp time shifts, clip grows
    const c = p.comps![0], clip = p.clips[0];
    expect(c.takes.map(t => t.start)).toEqual([2, 2, 0]);
    expect(clip).toMatchObject({ start: 2, len: 6, offset: 0 });
    expect(c.regions).toEqual([{ take: c.takes[2].id, from: 0, to: 4 }, { take: c.takes[1].id, from: 4, to: 6 }]);
  });
});
