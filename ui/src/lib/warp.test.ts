import { describe, expect, it } from 'vitest';
import { mockCatalog } from '../api/mockData';
import { newFx } from './presets';
import { curveAt, valueAt } from './automation';
import { speedAt, speedText } from './warp';
import warpJson from '../../../crates/engine/src/warp_factory.json?raw';
import { names } from '../i18n/names-fx';

const cat = mockCatalog();

describe('Warp', () => {
  it('a new Warp carries the factory slots the engine embeds', () => {
    const f = newFx(cat, 1, 'warp', 120);
    const file = JSON.parse(warpJson);
    expect(f.maps!.time).toEqual(file.time);
    expect(f.maps!.vol).toEqual(file.vol);
    expect(f.maps!.time).toHaveLength(16);
    // a copy, not the catalog's own object
    f.maps!.time[0].points[0].v = 0;
    expect(cat.warp!.time[0].points[0].v).toBe(1);
  });
  it('reads speed from the slope of a time map', () => {
    const half = [{ t: 0, v: 1 }, { t: 4, v: 0.75 }];
    expect(speedAt(half, 2)).toBeCloseTo(0.5);
    const rev = [{ t: 0, v: 1 }, { t: 1, v: 0.75 }];
    expect(speedAt(rev, 0.5)).toBeCloseTo(-1);
    const stop = [{ t: 0, v: 1 }, { t: 4, v: 0.5 }];
    expect(speedAt(stop, 1)).toBeCloseTo(0);
    expect(speedText(0.5)).toContain('−12.0 st');
    expect(speedText(1)).toBe('×1.00');
  });
  it('factory tape stop ends stopped (half sine, slope −1 at the end)', () => {
    const ts = cat.warp!.time.find(s => s.name.startsWith('Tape stop'))!.points;
    expect(speedAt(ts, 1)).toBeCloseTo(1);
    expect(speedAt(ts, 3.995)).toBeCloseTo(0, 1);
  });
  it('new segment shapes end on their point and stay in range', () => {
    for (const s of ['pulse', 'wave', 'halfSine'] as const) {
      for (const k of [-1, 0, 1]) {
        expect(curveAt(s, k, 1)).toBeCloseTo(1);
        for (let f = 0; f <= 1; f += 0.01) { const g = curveAt(s, k, f); expect(g).toBeGreaterThanOrEqual(-1e-9); expect(g).toBeLessThanOrEqual(1 + 1e-9); }
      }
    }
    // a vertical jump: two points at one time
    expect(valueAt([{ t: 0, v: 1 }, { t: 2, v: 1 }, { t: 2, v: 0.5 }, { t: 4, v: 0.5 }], 2)).toBe(0.5);
  });
  it('the map library is in the catalog, every map is valid and has an English name', () => {
    const lib = cat.warpLibrary!;
    expect(lib.time.length).toBeGreaterThanOrEqual(40);
    expect(lib.vol.length).toBeGreaterThanOrEqual(30);
    for (const m of [...lib.time, ...lib.vol]) {
      expect(m.points.every((p, i, a) => i === 0 || a[i - 1].t <= p.t)).toBe(true);
      expect(m.points.every(p => p.v >= 0 && p.v <= 1 && p.t >= 0 && p.t <= 4)).toBe(true);
      // Polish letters left untranslated would show in the English UI
      for (const n of [m.name, m.cat]) expect(names[n] ?? (/[ąćęłńóśźż]/i.test(n) ? undefined : n), n).toBeDefined();
    }
    // reverse maps really read backwards: slope −2 somewhere
    const rev = lib.time.find(m => m.name === 'Wstecz — każda ćwierćnuta')!;
    expect(speedAt(rev.points, 0.5)).toBeCloseTo(-1);
  });
});
