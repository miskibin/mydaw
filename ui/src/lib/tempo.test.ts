import { describe, expect, it } from 'vitest';

import {
  MAX_BPM,
  MIN_BPM,
  tempoFromCents,
  tempoFromWhole,
  tempoParts,
} from './tempo';

describe('tempoParts', () => {
  it('splits a whole tempo into integer BPM and zero hundredths', () => {
    expect(tempoParts(130)).toEqual({ whole: 130, cents: 0 });
  });

  it('keeps the hundredths on their own axis', () => {
    expect(tempoParts(130.47)).toEqual({ whole: 130, cents: 47 });
  });

  it('rounds float dust onto the nearest hundredth', () => {
    expect(tempoParts(129.996)).toEqual({ whole: 130, cents: 0 });
  });
});

describe('tempoFromWhole', () => {
  it('changes only the integer, leaving the hundredths where they were', () => {
    expect(tempoFromWhole(130.47, 128)).toBe(128.47);
  });

  it('clamps a too-low integer without going below the engine minimum', () => {
    expect(tempoFromWhole(20.3, 10)).toBe(MIN_BPM);
  });
});

describe('tempoFromCents', () => {
  it('changes only the hundredths, leaving the integer where it was', () => {
    expect(tempoFromCents(130.47, 12)).toBe(130.12);
  });

  it('stays inside 0..99 hundredths so a fine drag cannot jump a whole BPM', () => {
    expect(tempoFromCents(130.47, 100)).toBe(130.99);
    expect(tempoFromCents(130.47, -4)).toBe(130);
  });

  it('does not overshoot the engine maximum', () => {
    expect(tempoFromCents(MAX_BPM, 80)).toBe(MAX_BPM);
  });
});
