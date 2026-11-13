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

  it('a generator with a selection merges instead of overwriting positionally', () => {
    const a = [note(1, 0, 60, 1), note(2, 2, 70, 1), note(1, 4, 62, 1)];
    applyTransform(a, { kind: 'euclidFill', ch: 1, k: 3, n: 8, rotate: 0, vel: 0.7 }, 0, { kind: 'indices', indices: [0, 2] }, [{ id: 1, root: 60 }, { id: 2, root: 60 }], null, 1, 16);
    // both selected ch-1 notes are replaced by the 3 generated onsets; the ch-2 note survives
    expect(a).toHaveLength(4);
    expect(a.filter(n => n.ch === 2)).toHaveLength(1);
    expect(a.filter(n => n.ch === 1)).toHaveLength(3);
  });

  it('a range select scoped to a channel touches only that channel', () => {
    const a = [note(1, 0, 60, 1), note(2, 2, 70, 1), note(1, 8, 62, 1)];
    applyTransform(a, { kind: 'humanize', timing: 1, velocity: 0 }, 41, { kind: 'range', from: 0, to: 1e9, ch: 1 }, [{ id: 1, root: 60 }, { id: 2, root: 60 }], null, 1, 16);
    // the ch-2 note is untouched; the in-scope ch-1 note moved off step 8
    expect(a[1].step).toBe(2);
    expect(a[1].v).toBe(1);
    expect(a[2].step).not.toBe(8);
  });

  describe('golden: engine parity (values from the engine golden_fixed_seed_values test)', () => {
    const f = Math.fround;
    const noteF = (ch: number, step: number, p: number, v: number): Note => ({ ch, step, len: 1, p, v: f(v) });
    const roots = [{ id: 1, root: 60 }, { id: 3, root: 57 }];

    it('humanize', () => {
      const h = [noteF(1, 4, 60, 0.8)];
      applyTransform(h, { kind: 'humanize', timing: 0.5, velocity: 0.4 }, 41, null, roots, null, 1, 16);
      expect(h[0].step).toBe(f(4.033452));
      expect(h[0].v).toBe(f(0.67452526));
    });

    it('strum', () => {
      const s = [noteF(1, 4, 60, 0.8), noteF(1, 4, 64, 0.8), noteF(1, 4, 57, 0.8)];
      applyTransform(s, { kind: 'strum', width: 0.25, ramp: 0.05 }, 0, null, roots, null, 1, 16);
      const by = (p: number) => s.find(n => n.p === p)!;
      expect(by(57).step).toBe(4);
      expect(by(60).step).toBe(f(4.25));
      expect(by(64).step).toBe(f(4.5));
      expect(by(64).v).toBe(f(0.90000004));
    });

    it('euclidFill (accent on every fourth emitted onset)', () => {
      const e = [note(1, 2, 60, 0.9)];
      applyTransform(e, { kind: 'euclidFill', ch: 1, k: 5, n: 16, rotate: 0, vel: 0.7 }, 0, null, roots, null, 1, 16);
      expect(e.map(n => n.step)).toEqual([0, 4, 7, 10, 13]);
      expect(e.map(n => n.v)).toEqual([1, 0.7, 0.7, 0.7, 1]);
    });

    it('melody (A minor, seed 7)', () => {
      const m: Note[] = [];
      applyTransform(m, { kind: 'melody', ch: 3, bars: 2, density: 0.55, repeat: 1 }, 7, null, [{ id: 3, root: 57 }], 9, 2, 32);
      expect(m.map(n => n.p)).toEqual([57, 57, 57, 59, 59, 59, 62, 65, 64, 62, 64, 64, 62, 62, 62, 62]);
      expect(m.map(n => n.step)).toEqual([1, 2, 3, 5, 8, 10, 12, 15, 17, 18, 19, 21, 24, 26, 28, 31]);
    });
  });
});
