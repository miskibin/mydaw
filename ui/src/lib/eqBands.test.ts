import { describe, expect, it } from 'vitest';
import { mockCatalog } from '../api/mockData';
import type { EqBand } from '../api/types';
import { defaultFreq, defaultQ, f2t, handleShown, q2t, SHAPE_STEPS, shapePatch, shapeStep, t2f, t2q } from './eq';
import { loadPreset } from './presets';

const band = (p: Partial<EqBand> = {}): EqBand => ({ on: true, type: 'peak', f: 1000, g: 0, q: 1, slope: 12, place: 'stereo', ...p });

describe('EQ band strips', () => {
  it('maps knob travel to frequency and Q and back', () => {
    for (const f of [10, 20, 100, 1000, 18000, 22000]) expect(t2f(f2t(f))).toBeCloseTo(f, 6);
    for (const q of [0.1, 0.71, 1, 4, 18]) expect(t2q(q2t(q))).toBeCloseTo(q, 6);
    expect(f2t(5)).toBe(0); expect(f2t(40000)).toBe(1);
  });

  it('steps the shape selector through Off and every shape', () => {
    expect(SHAPE_STEPS[0]).toBe('off');
    expect(shapeStep(band({ on: false, type: 'highshelf' }))).toBe(0);
    expect(SHAPE_STEPS[shapeStep(band({ type: 'lowcut' }))]).toBe('lowcut');
  });

  it('keeps a band coherent when its shape changes', () => {
    const b = band({ g: 6, q: 1.5, dynamic: { range: -6, thr: -30 } });
    expect(shapePatch(b, 'lowcut')).toMatchObject({ type: 'lowcut', on: true, g: 0, q: 0.71, dynamic: null });
    expect(shapePatch(b, 'notch').q).toBe(4);
    expect(shapePatch(band({ on: false, g: 3 }), 'highshelf')).toMatchObject({ on: true, g: 3 });
  });

  it('resets to sensible defaults', () => {
    expect(defaultFreq(0)).toBe(30);
    expect(defaultFreq(20)).toBe(1000);
    expect(defaultQ('peak')).toBe(1);
    expect(defaultQ('lowcut')).toBe(0.71);
  });

  it('hides switched-off bands on the graph unless selected', () => {
    expect(handleShown(band(), 2, null)).toBe(true);
    expect(handleShown(band({ on: false }), 2, null)).toBe(false);
    expect(handleShown(band({ on: false }), 2, 2)).toBe(true);
  });
});

describe('EQ cut presets', () => {
  const cat = mockCatalog();
  const load = (name: string) => {
    const i = cat.presets.eq!.findIndex(p => p.name === name);
    expect(i, name).toBeGreaterThanOrEqual(0);
    return loadPreset(cat, { id: 1, type: 'eq', bypass: false, preset: null, params: {}, bands: [] }, i, 120).bands!;
  };

  it('"Cut < 20 Hz and > 18 kHz" uses only the two cut bands', () => {
    const on = load('Wytnij < 20 Hz i > 18 kHz').filter(b => b.on);
    expect(on.map(b => [b.type, b.f, b.slope])).toEqual([['lowcut', 20, 24], ['highcut', 18000, 24]]);
  });

  it('single-cut presets switch every other band off', () => {
    expect(load('Low cut 80 Hz').filter(b => b.on).map(b => [b.type, b.f])).toEqual([['lowcut', 80]]);
    expect(load('Low cut 150 Hz (wokal)').filter(b => b.on).map(b => [b.type, b.f])).toEqual([['lowcut', 150]]);
    expect(load('High cut 10 kHz').filter(b => b.on).map(b => [b.type, b.f])).toEqual([['highcut', 10000]]);
  });

  it('Flat keeps the five tone bands ready to use', () => {
    expect(load('Flat').filter(b => b.on).length).toBe(5);
  });
});
