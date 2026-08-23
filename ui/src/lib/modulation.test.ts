import { describe, expect, it } from 'vitest';

import {
  filterMode,
  flangerDelayMs,
  flangerMagDb,
  lfoSine,
  modulatedCutoffHz,
  phaserMagDb,
  phaserNotchHz,
  resonancePercent,
  resonanceQ,
  svfMagDb,
  syncRateHz,
} from './modulation';

const SR = 48_000;

describe('flangerMagDb', () => {
  it('notches where a comb of that delay cancels', () => {
    // 1 ms of delay at 50 % mix: the same 500 Hz / 1500 Hz notches and 1 kHz
    // peak that `flanger.rs` measures out of the audio.
    expect(flangerMagDb(500, 1, 0, 0.5)).toBeLessThan(-40);
    expect(flangerMagDb(1_500, 1, 0, 0.5)).toBeLessThan(-40);
    expect(flangerMagDb(1_000, 1, 0, 0.5)).toBeCloseTo(0, 4);
    expect(flangerMagDb(2_000, 1, 0, 0.5)).toBeCloseTo(0, 4);
  });

  it('moves its notches when the delay moves', () => {
    // Twice the delay, half the spacing: 250 Hz is now a notch and 500 Hz a peak.
    expect(flangerMagDb(250, 2, 0, 0.5)).toBeLessThan(-40);
    expect(flangerMagDb(500, 2, 0, 0.5)).toBeCloseTo(0, 4);
  });

  it('resonates with positive feedback and inverts with negative', () => {
    const flat = flangerMagDb(1_000, 1, 0, 0.5);
    const resonant = flangerMagDb(1_000, 1, 0.8, 0.5);
    expect(resonant).toBeGreaterThan(flat + 6);
    // The polarity switch is a different filter, not a quieter one: with the
    // feedback inverted, 500 Hz -- the deepest notch at zero feedback -- becomes
    // the resonant peak, and 1 kHz, which was a peak, is pulled down.
    expect(flangerMagDb(500, 1, -0.8, 0.5)).toBeGreaterThan(4);
    expect(flangerMagDb(1_000, 1, -0.8, 0.5)).toBeLessThan(flat - 2);
  });

  it('is flat when the mix is dry', () => {
    for (const hz of [100, 500, 3_000]) {
      expect(flangerMagDb(hz, 1, 0.9, 0)).toBeCloseTo(0, 6);
    }
  });
});

describe('flangerDelayMs', () => {
  it('sweeps up from the manual setting and never below it', () => {
    expect(flangerDelayMs(1, 4, 0)).toBeCloseTo(3, 6); // sin 0 -> halfway
    expect(flangerDelayMs(1, 4, 0.25)).toBeCloseTo(5, 6); // the top
    expect(flangerDelayMs(1, 4, 0.75)).toBeCloseTo(1, 6); // the bottom
  });
});

describe('phaserMagDb', () => {
  it('puts its first notch where the all-pass phase reaches half a turn', () => {
    const notch = phaserNotchHz(800, 4, 0, SR);
    expect(notch).toBeGreaterThan(325);
    expect(notch).toBeLessThan(337);
    expect(phaserMagDb(notch, 800, 4, 0, 0.5, SR)).toBeLessThan(-40);
    // A full turn at the corner: the wet path arrives in phase.
    expect(phaserMagDb(800, 800, 4, 0, 0.5, SR)).toBeCloseTo(0, 2);
  });

  it('adds a notch for every two stages, and they are not harmonics', () => {
    const first = phaserNotchHz(800, 8, 0, SR);
    const second = phaserNotchHz(800, 8, 1, SR);
    expect(phaserMagDb(first, 800, 8, 0, 0.5, SR)).toBeLessThan(-30);
    expect(phaserMagDb(second, 800, 8, 0, 0.5, SR)).toBeLessThan(-30);
    // A comb would put the second at three times the first; an all-pass
    // cascade does not.
    expect(second / first).toBeGreaterThan(2.5);
    expect(second / first).toBeLessThan(4);
  });

  it('resonates at the corner with feedback', () => {
    const flat = phaserMagDb(800, 800, 4, 0, 0.5, SR);
    expect(phaserMagDb(800, 800, 4, 0.9, 0.5, SR)).toBeGreaterThan(flat + 6);
  });

  it('is an all-pass at full wet with no feedback', () => {
    for (const hz of [80, 400, 2_000, 9_000]) {
      expect(phaserMagDb(hz, 800, 6, 0, 1, SR)).toBeCloseTo(0, 4);
    }
  });
});

describe('svfMagDb', () => {
  it('is three decibels down at the corner and forty a decade past it', () => {
    expect(svfMagDb(1_000, 1_000, Math.SQRT1_2, 'lowpass', SR)).toBeCloseTo(-3.01, 1);
    expect(svfMagDb(100, 1_000, Math.SQRT1_2, 'lowpass', SR)).toBeCloseTo(0, 1);
    const decade = svfMagDb(10_000, 1_000, Math.SQRT1_2, 'lowpass', SR);
    expect(decade).toBeLessThan(-34);
    expect(decade).toBeGreaterThan(-46);
  });

  it('turns over the other way in highpass and peaks at the corner in bandpass', () => {
    expect(svfMagDb(100, 1_000, Math.SQRT1_2, 'highpass', SR)).toBeLessThan(-34);
    expect(svfMagDb(10_000, 1_000, Math.SQRT1_2, 'highpass', SR)).toBeCloseTo(0, 1);
    // Normalised: the bandpass peaks at unity, not at Q.
    expect(svfMagDb(1_000, 1_000, 8, 'bandpass', SR)).toBeCloseTo(0, 2);
    expect(svfMagDb(200, 1_000, 8, 'bandpass', SR)).toBeLessThan(-20);
  });

  it('peaks at Q with resonance up, which is what the engine measures', () => {
    expect(svfMagDb(1_000, 1_000, 12, 'lowpass', SR)).toBeCloseTo(20 * Math.log10(12), 0);
  });

  it('blends against the dry path when the mix is down', () => {
    expect(svfMagDb(10_000, 200, 1, 'lowpass', SR, 0)).toBeCloseTo(0, 6);
    const half = svfMagDb(10_000, 200, 1, 'lowpass', SR, 0.5);
    expect(half).toBeGreaterThan(-7);
    expect(half).toBeLessThan(-5);
  });
});

describe('resonanceQ', () => {
  it('runs exponentially between the engine constants and back', () => {
    expect(resonanceQ(0)).toBeCloseTo(0.7, 6);
    expect(resonanceQ(100)).toBeCloseTo(12, 6);
    expect(resonanceQ(50)).toBeCloseTo(Math.sqrt(0.7 * 12), 6);
    expect(resonancePercent(resonanceQ(37))).toBeCloseTo(37, 4);
  });
});

describe('modulatedCutoffHz', () => {
  it('moves the cutoff in octaves, both ways', () => {
    const base = { cutoffHz: 500, lfoAmount: 100, phase: 0.25 };
    expect(modulatedCutoffHz(base)).toBeCloseTo(500 * 8, 3); // +3 octaves
    expect(modulatedCutoffHz({ ...base, phase: 0.75 })).toBeCloseTo(500 / 8, 3);
    expect(modulatedCutoffHz({ ...base, lfoAmount: 0 })).toBeCloseTo(500, 6);
  });

  it('adds the envelope on top, and a negative amount closes the filter', () => {
    const open = modulatedCutoffHz({
      cutoffHz: 300,
      lfoAmount: 0,
      phase: 0,
      envAmount: 100,
      envelope: 1,
    });
    expect(open).toBeCloseTo(300 * 16, 3);
    const closed = modulatedCutoffHz({
      cutoffHz: 300,
      lfoAmount: 0,
      phase: 0,
      envAmount: -100,
      envelope: 1,
    });
    expect(closed).toBeCloseTo(300 / 16, 3);
  });
});

describe('syncRateHz', () => {
  it('is one LFO cycle per division', () => {
    expect(syncRateHz(2, 120)).toBeCloseTo(2, 6); // 1/4 at 120 BPM
    expect(syncRateHz(3, 120)).toBeCloseTo(4, 6); // 1/8
    expect(syncRateHz(0, 120)).toBeCloseTo(0.5, 6); // a whole bar
    expect(syncRateHz(2, 0)).toBe(0);
  });
});

describe('lfoSine and filterMode', () => {
  it('wraps the phase and names the modes', () => {
    expect(lfoSine(0)).toBeCloseTo(0, 6);
    expect(lfoSine(0.25)).toBeCloseTo(1, 6);
    expect(lfoSine(1.25)).toBeCloseTo(1, 6);
    expect(filterMode(0)).toBe('lowpass');
    expect(filterMode(1)).toBe('highpass');
    expect(filterMode(2)).toBe('bandpass');
    expect(filterMode(9)).toBe('lowpass');
  });
});
