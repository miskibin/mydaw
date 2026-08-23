import { describe, expect, it } from 'vitest';

import { driveGain, liveDriveSample, saturatorOut, saturatorType, softClip, spectrumBarHeights } from './saturator';

describe('saturatorType', () => {
  it('maps the engine choice switch onto the three shapers', () => {
    expect(saturatorType(0)).toBe('tape');
    expect(saturatorType(1)).toBe('tube');
    expect(saturatorType(2)).toBe('clip');
  });
});

describe('softClip', () => {
  it('is odd, unity-slope at the origin, and bounded', () => {
    expect(softClip(0)).toBe(0);
    expect(softClip(0.8) + softClip(-0.8)).toBeCloseTo(0, 6);
    expect(Math.abs(softClip(8))).toBeLessThanOrEqual(1.01);
  });
});

describe('saturatorOut', () => {
  it('is identity at 0 dB drive for a small tape input', () => {
    expect(saturatorOut('tape', 0.2, 0)).toBeCloseTo(softClip(0.2), 8);
    expect(driveGain(6)).toBeCloseTo(2, 2);
    expect(Math.abs(saturatorOut('clip', 2, 0))).toBeLessThanOrEqual(1);
  });
});

describe('liveDriveSample', () => {
  it('returns the peak sample, signed, so the transfer dot tracks the hop', () => {
    expect(liveDriveSample([])).toBe(0);
    expect(liveDriveSample([0.1, -0.8, 0.4])).toBe(-0.8);
  });
});

describe('spectrumBarHeights', () => {
  it('maps log-spaced dB bins into 0..1 bars', () => {
    const quiet = spectrumBarHeights([-90, -90, -90, -90], 2);
    const hot = spectrumBarHeights([0, 0, -90, -90], 2);
    expect(quiet.every((h) => h === 0)).toBe(true);
    expect(hot[0]).toBeGreaterThan(0.9);
    expect(hot[1]).toBeLessThan(0.1);
  });
});
