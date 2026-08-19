import { describe, expect, it } from 'vitest';

import { clampDb, faderFraction, faderToDb, MAX_DB, MIN_DB } from './scale';

describe('clampDb', () => {
  it('rounds to a tenth so the readout and the engine agree', () => {
    expect(clampDb(-6.03)).toBe(-6.0);
    expect(clampDb(1.26)).toBe(1.3);
  });

  it('stays inside the mixer throw', () => {
    expect(clampDb(-90)).toBe(MIN_DB);
    expect(clampDb(12)).toBe(MAX_DB);
  });

  it('honours a live range from the engine', () => {
    expect(clampDb(-12, -18, 0)).toBe(-12);
    expect(clampDb(-24, -18, 0)).toBe(-18);
    expect(clampDb(3, -18, 0)).toBe(0);
  });
});

describe('faderFraction / faderToDb', () => {
  it('puts unity near four fifths of the throw, the way a console does', () => {
    expect(faderFraction(0)).toBeCloseTo(0.79, 2);
    expect(faderToDb(faderFraction(0))).toBe(0);
  });
});
