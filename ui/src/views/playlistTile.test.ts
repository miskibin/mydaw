import { describe, expect, it } from 'vitest';
import { followScroll, margins, planTile, type Tile } from './playlistTile';

const GW = 20000, GH = 4000;

describe('playlist tile', () => {
  it('covers the view with margins and stays put for small scrolls', () => {
    const a = planTile(null, 1000, 500, 800, 600, GW, GH, 1.5);
    expect(a.full).toBe(true);
    const t = a.tile;
    expect(t.x).toBeLessThanOrEqual(1000);
    expect(t.x + t.w).toBeGreaterThanOrEqual(1800);
    expect(t.y).toBeLessThanOrEqual(500);
    expect(t.y + t.h).toBeGreaterThanOrEqual(1100);
    // a touchpad step: nothing to draw
    const b = planTile(t, 1004, 503, 800, 600, GW, GH, 1.5);
    expect(b).toEqual({ tile: t, full: false, dx: 0, dy: 0, strips: [] });
  });

  it('re-centres with a whole-device-pixel shift and draws only the exposed strip', () => {
    for (const dpr of [1, 1.25, 1.5, 2]) {
      const t = planTile(null, 1000, 0, 800, 600, GW, GH, dpr).tile;
      const p = planTile(t, t.x + t.w - 800 - 10, 0, 800, 600, GW, GH, dpr);
      expect(p.full).toBe(false);
      expect(p.dx).toBeLessThan(0);
      expect(Math.abs(p.dx * dpr - Math.round(p.dx * dpr))).toBeLessThan(1e-6);
      expect(p.strips).toEqual([{ x: t.w + p.dx, y: 0, w: -p.dx, h: t.h }]);
      // the new tile covers the view again
      expect(p.tile.x + p.tile.w).toBeGreaterThanOrEqual(t.x + t.w - 10);
    }
  });

  it('never overlaps strips on a diagonal move', () => {
    const t = planTile(null, 3000, 2000, 800, 600, GW, GH, 2).tile;
    const p = planTile(t, t.x + 5, t.y + 1, 800, 600, GW, GH, 2);
    expect(p.full).toBe(false);
    const [c, r] = p.strips;
    expect(c.x + c.w).toBeLessThanOrEqual(r.x);
    const area = p.strips.reduce((s, q) => s + q.w * q.h, 0);
    const shifted = (t.w - Math.abs(p.dx)) * (t.h - Math.abs(p.dy));
    expect(area + shifted).toBeCloseTo(t.w * t.h, 6);
  });

  it('redraws all after a jump past the tile, or a new size', () => {
    const t = planTile(null, 0, 0, 800, 600, GW, GH, 1).tile;
    expect(planTile(t, 9000, 0, 800, 600, GW, GH, 1).full).toBe(true);
    expect(planTile(t, 0, 0, 900, 600, GW, GH, 1).full).toBe(true);
  });

  it('keeps inside the grid and needs no guard at its edges', () => {
    const t = planTile(null, 0, 0, 800, 600, 1000, 700, 1).tile;
    expect(t).toEqual<Tile>({ x: 0, y: 0, w: 1000, h: 700 });
    expect(planTile(t, 200, 100, 800, 600, 1000, 700, 1).full).toBe(false);
    expect(planTile(t, 200, 100, 800, 600, 1000, 700, 1).strips).toEqual([]);
  });

  it('bounds the backing store', () => {
    const { mx, my } = margins(3000, 2000, 2);
    expect((3000 + 2 * mx) * (2000 + 2 * my) * 4).toBeLessThanOrEqual(Math.max(12e6, 3064 * 2064 * 4));
  });
});

describe('follow', () => {
  it('jumps a page once and then holds while the playhead moves on', () => {
    const view = 1000;
    let sl = 0;
    const jumps: number[] = [];
    for (let x = 0; x < 5000; x += 7) {
      const to = followScroll(x, sl, view);
      if (to !== sl) { jumps.push(to); sl = to; }
      expect(x).toBeGreaterThanOrEqual(sl);
      expect(x).toBeLessThanOrEqual(sl + view);
      expect(followScroll(x, sl, view)).toBe(sl); // stable: no back-and-forth
    }
    expect(jumps.length).toBeGreaterThan(3);
    expect(jumps.length).toBeLessThan(7);
  });
});
