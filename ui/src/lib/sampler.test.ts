import { describe, expect, it } from 'vitest';
import { SAMPLER } from '../api/types';
import { attackFromT, attackToT, decayFromT, decayToT, envelopePoints, msText, releaseFromT, releaseToT } from './sampler';

describe('sampler knob scales', () => {
  it('cover the whole range and round-trip', () => {
    expect([attackFromT(0), attackFromT(1)]).toEqual([SAMPLER.attack.min, SAMPLER.attack.max]);
    expect([decayFromT(0), decayFromT(1)]).toEqual([SAMPLER.decay.min, SAMPLER.decay.max]);
    expect([releaseFromT(0), releaseFromT(1)]).toEqual([SAMPLER.release.min, SAMPLER.release.max]);
    for (const ms of [0, 1.5, 12, 80, 350, 2000]) expect(attackFromT(attackToT(ms))).toBeCloseTo(ms, 0);
    for (const ms of [0, 5, 120, 900, 5000]) expect(decayFromT(decayToT(ms))).toBeCloseTo(ms, 0);
    for (const ms of [1, 20, 90, 700, 5000]) expect(releaseFromT(releaseToT(ms))).toBeCloseTo(ms, 0);
    // out-of-range values (an old file, a script) still give a knob position
    expect(attackToT(9999)).toBe(1);
    expect(releaseToT(0)).toBe(0);
  });
  it('give the short times most of the travel', () => {
    expect(attackFromT(1 / 3)).toBeLessThan(80);
    expect(releaseFromT(0.5)).toBeLessThan(80);
  });
  it('print times', () => {
    expect([msText(0), msText(12.5), msText(350), msText(1200)]).toEqual(['0 ms', '12.5 ms', '350 ms', '1.20 s']);
  });
});

describe('envelope shape', () => {
  it('is not drawn when it does nothing', () => {
    expect(envelopePoints({ attack: 0, decay: 0, sustain: 1 }, 1)).toEqual([]);
    expect(envelopePoints({ attack: 0, decay: 200, sustain: 1 }, 1)).toEqual([]);
    expect(envelopePoints({ attack: 10, decay: 0, sustain: 1 }, 0)).toEqual([]);
  });
  it('rises over the attack, falls to the sustain level and holds it', () => {
    expect(envelopePoints({ attack: 100, decay: 200, sustain: 0.5 }, 1)).toEqual([[0, 0], [0.1, expect.closeTo(1, 5)], [expect.closeTo(0.3, 9), 0.5], [1, 0.5]]);
    // no attack: it starts at full level
    expect(envelopePoints({ attack: 0, decay: 500, sustain: 0.25 }, 1)).toEqual([[0, 1], [0.5, 0.25], [1, 0.25]]);
    // an instant drop to the sustain level
    expect(envelopePoints({ attack: 0, decay: 0, sustain: 0.5 }, 1)).toEqual([[0, 1], [0, 0.5], [1, 0.5]]);
  });
  it('ends with the decay when the sustain is zero, and is cut off by a short part', () => {
    expect(envelopePoints({ attack: 0, decay: 250, sustain: 0 }, 1)).toEqual([[0, 1], [0.25, 0]]);
    // the part ends half way down the decay
    const cut = envelopePoints({ attack: 0, decay: 1000, sustain: 0 }, 0.5);
    expect(cut).toEqual([[0, 1], [1, expect.closeTo(0.5, 9)]]);
    // an attack longer than the part never gets to full level
    const slow = envelopePoints({ attack: 2000, decay: 0, sustain: 1 }, 1);
    expect(slow[slow.length - 1]).toEqual([1, expect.closeTo(0.5, 6)]);
  });
});
