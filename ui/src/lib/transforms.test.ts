import { describe, expect, it } from 'vitest';
import type { Note } from '../api/types';
import { SCALES, scaleMask } from './pattern';
import { applyTransform, SplitMix64 } from './transforms';

const note = (ch: number, step: number, p = 60, v = 0.8): Note => ({ ch, step, len: 1, p, v });
const notes = () => [note(1, 0, 61), note(1, 4.3, 64), note(1, 8, 60)];

describe('scale ids (engine id space, pitch::scale_mask)', () => {
  it('list engine ids, not list positions', () => {
    expect(SCALES.map(s => s[1])).toEqual([1, 2, 3, 4, 0]);
  });

  it('masks match pitch.rs', () => {
    expect(scaleMask(0)).toBe(0b111111111111);
    expect(scaleMask(1)).toBe(0b101010110101);
    expect(scaleMask(2)).toBe(0b010110101101);
    expect(scaleMask(3)).toBe(0b001010010101);
    expect(scaleMask(4)).toBe(0b010010101001);
  });

  it('A natural minor = id 2 (what the roll highlight and scale-aware transforms read)', () => {
    // bits = semitones above the key: A, B, C, D, E, F, G
    expect([0, 2, 3, 5, 7, 8, 10].every(pc => scaleMask(2) & (1 << pc))).toBe(true);
    expect(scaleMask(2) & (1 << 1)).toBeFalsy();
    expect(scaleMask(2) & (1 << 6)).toBeFalsy();
  });
});

describe('transformNotes (TS mirror)', () => {
  it('humanize is deterministic in the seed', () => {
    const a = notes(), b = notes();
    const roots = [{ id: 1, root: 60 }];
    applyTransform(a, { kind: 'humanize', timing: 0.5, velocity: 0.4 }, 41, null, roots, null, 1, 16);
    applyTransform(b, { kind: 'humanize', timing: 0.5, velocity: 0.4 }, 41, null, roots, null, 1, 16);
    expect(a).toEqual(b);
  });

  it('quantize with a project key snaps into the scale', () => {
    const a = notes();
    applyTransform(a, { kind: 'quantize', grid: 1, amount: 1, scaleAware: true }, 0, null, [{ id: 1, root: 60 }], 0, 1, 16);
    expect(a.map(n => n.p)).toEqual([60, 64, 60]); // C#→C (tie), E already in scale, C in scale
  });

  it('euclidFill replaces the channel with k notes and keeps the others', () => {
    const a = [...notes(), note(2, 2, 70)];
    applyTransform(a, { kind: 'euclidFill', ch: 1, k: 5, n: 16 }, 0, null, [{ id: 1, root: 60 }, { id: 2, root: 60 }], null, 1, 16);
    expect(a.filter(n => n.ch === 1)).toHaveLength(5);
    expect(a.some(n => n.ch === 2 && n.step === 2)).toBe(true);
  });

  it('splitmix64 matches the engine by construction (stability smoke)', () => {
    const r = new SplitMix64(7);
    const seq = [r.unit(), r.unit(), r.unit()];
    const r2 = new SplitMix64(7);
    expect([r2.unit(), r2.unit(), r2.unit()]).toEqual(seq);
    expect(seq).not.toEqual([0, 0, 0]);
  });
});
