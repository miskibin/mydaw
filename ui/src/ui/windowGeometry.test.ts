import { describe, expect, it } from 'vitest';
import { fitWindow } from './windowGeometry';

describe('floating editor bounds', () => {
  it('keeps a remembered window reachable on a smaller display', () => {
    const next = fitWindow({ x: 1500, y: 900, w: 1000, h: 500 }, { w: 800, h: 400 });
    expect(next).toEqual({ x: 8, y: 8, w: 784, h: 384 });
  });
  it('keeps the whole title and resize handle on screen during drag and resize', () => {
    const next = fitWindow({ x: -100, y: 1000, w: 600, h: 340 }, { w: 1200, h: 700 });
    expect(next).toEqual({ x: 8, y: 352, w: 600, h: 340 });
  });
  it('allows a compact rack while retaining larger plugin minimums', () => {
    expect(fitWindow({ x: 30, y: 30, w: 200, h: 80 }, { w: 1000, h: 700 }, 180)).toEqual({ x: 30, y: 30, w: 480, h: 180 });
  });
});
