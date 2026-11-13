import { describe, expect, it } from 'vitest';
import { dragLoop, drawnLoop, loopEdgeAt, sameRange } from './rulerEdit';

describe('time ruler loop editing (Playlist, Piano roll, Pitch editor)', () => {
  const lp = { a: 4, b: 8 };
  it('grabs an edge within 8 px, the body only with Shift', () => {
    expect(loopEdgeAt(lp, 4.2, 20)).toBe('a'); // 4 px away
    expect(loopEdgeAt(lp, 7.7, 20)).toBe('b');
    expect(loopEdgeAt(lp, 6, 20)).toBeNull();
    expect(loopEdgeAt(lp, 6, 20, true)).toBe('body');
    expect(loopEdgeAt(lp, 4.5, 20)).toBeNull(); // 10 px
    expect(loopEdgeAt(null, 4, 20)).toBeNull();
    expect(loopEdgeAt(lp, 4, 0)).toBeNull();
  });
  it('moves an edge on the grid, never past the other one or the end', () => {
    expect(dragLoop(lp, 'a', 2.1, 4, 1, 64)).toEqual({ a: 2, b: 8 });
    expect(dragLoop(lp, 'a', 9, 4, 1, 64)).toEqual({ a: 7, b: 8 });
    expect(dragLoop(lp, 'b', 3, 8, 1, 64)).toEqual({ a: 4, b: 5 });
    expect(dragLoop(lp, 'b', 99, 8, 1, 16)).toEqual({ a: 4, b: 16 });
    expect(dragLoop(lp, 'a', -3, 4, 1, 64)).toEqual({ a: 0, b: 8 });
    // free (Alt): at least 1/64 beat
    expect(dragLoop(lp, 'a', 8, 4, 0, 64).a).toBeCloseTo(8 - 1 / 64);
  });
  it('moves the whole loop keeping its length, inside the ruler', () => {
    expect(dragLoop(lp, 'body', 7.4, 5, 1, 64)).toEqual({ a: 6, b: 10 });
    expect(dragLoop(lp, 'body', 0, 5, 1, 64)).toEqual({ a: 0, b: 4 });
    expect(dragLoop(lp, 'body', 50, 5, 1, 10)).toEqual({ a: 6, b: 10 });
  });
  it('draws a loop either way; nothing while start and end are the same', () => {
    expect(drawnLoop(8, 4)).toEqual({ a: 4, b: 8 });
    expect(drawnLoop(4, 4)).toBeNull();
    expect(sameRange({ a: 1, b: 2 }, { a: 1, b: 2 })).toBe(true);
    expect(sameRange(null, undefined)).toBe(true);
    expect(sameRange(null, { a: 1, b: 2 })).toBe(false);
  });
});
