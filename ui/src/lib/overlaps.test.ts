import { describe, expect, it } from 'vitest';
import type { Clip } from '../api/types';
import { laneOverlaps } from './overlaps';

let nextId = 1;
const clip = (track: number, start: number, len: number, id = nextId++): Clip =>
  ({ id, kind: 'pat', ref: 1, track, start, len, offset: 0, muted: false, fadeIn: 0, fadeOut: 0, gain: 1 } as unknown as Clip);

describe('overlapping clips on a track', () => {
  it('finds nothing in an ordinary arrangement, touching ends included', () => {
    const o = laneOverlaps([clip(0, 0, 4), clip(0, 4, 4), clip(1, 0, 8), clip(0, 8, 4)]);
    expect(o.covers.size).toBe(0);
    expect(o.spans).toEqual([]);
  });
  it('counts an exact duplicate on the clip drawn on top', () => {
    const a = clip(0, 4, 4), b = clip(0, 4, 4), c = clip(0, 4, 4);
    const o = laneOverlaps([a, b, c]);
    expect([...o.covers]).toEqual([[c.id, 2]]); // c hides a and b
    expect(o.spans).toEqual([{ track: 0, a: 4, b: 8 }]);
  });
  it('counts a short clip hidden under a longer one drawn later', () => {
    const short = clip(2, 5, 1), long = clip(2, 4, 8);
    expect([...laneOverlaps([short, long]).covers]).toEqual([[long.id, 1]]);
  });
  it('does not count a clip drawn on top of a longer one (it is visible)', () => {
    const long = clip(0, 0, 8), short = clip(0, 2, 2);
    const o = laneOverlaps([long, short]);
    expect(o.covers.size).toBe(0);
    expect(o.spans).toEqual([{ track: 0, a: 2, b: 4 }]);
  });
  it('marks a partial overlap (a crossfade) as a span without hiding anything', () => {
    const o = laneOverlaps([clip(0, 0, 4), clip(0, 3, 4)]);
    expect(o.covers.size).toBe(0);
    expect(o.spans).toEqual([{ track: 0, a: 3, b: 4 }]);
  });
  it('merges a run of overlaps and keeps tracks apart', () => {
    const o = laneOverlaps([clip(0, 0, 4), clip(0, 2, 4), clip(0, 3, 4), clip(1, 0, 2), clip(1, 1, 2)]);
    expect(o.spans).toEqual([{ track: 0, a: 2, b: 6 }, { track: 1, a: 1, b: 2 }]);
  });
  it('ignores clips of no length', () => {
    expect(laneOverlaps([clip(0, 0, 0), clip(0, 0, 4)]).spans).toEqual([]);
  });
  it('stays fast on a big arrangement (20 000 clips, one long clip under a lane)', () => {
    const cs: Clip[] = [clip(0, 0, 4000)];
    for (let t = 0; t < 40; t++) for (let k = 0; k < 500; k++) cs.push(clip(t, k * 4, 4));
    const t0 = performance.now();
    const o = laneOverlaps(cs);
    const ms = performance.now() - t0;
    expect(o.covers.get(cs[1].id)).toBeUndefined();
    expect(o.spans.length).toBe(1);
    expect(ms).toBeLessThan(150);
  });
});
