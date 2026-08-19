import { describe, expect, it } from 'vitest';

import { driveGain, saturatorOut, saturatorType, softClip } from './saturator';

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
