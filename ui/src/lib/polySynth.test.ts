import { describe, expect, it } from 'vitest';

import { eqHzFromX, eqY } from './eq';
import {
  ENV_OCTAVES,
  FILTER_RANGE_DB,
  WAVE_SAW,
  WAVE_SINE,
  WAVE_SQUARE,
  adsrFromDrag,
  adsrLayout,
  envCutoffHz,
  envSweepActive,
  filterFromPointer,
  hitAdsrHandle,
  oscSample,
  oscillatorPath,
  oscillatorStack,
  pitchRatio,
  signalPath,
  svfLowpassCurveDb,
  svfLowpassMagDb,
  usesPolySynthEditor,
  normalisedScope,
} from './polySynth';

describe('usesPolySynthEditor', () => {
  it('is only the poly synth, and only when it actually published knobs', () => {
    expect(usesPolySynthEditor('poly-synth', 10)).toBe(true);
    expect(usesPolySynthEditor('poly-synth', 0)).toBe(false);
    expect(usesPolySynthEditor('kick', 10)).toBe(false);
    expect(usesPolySynthEditor('drum-kit', 0)).toBe(false);
  });
});

describe('envCutoffHz', () => {
  it('is a no-op at 0% and four octaves at the knob extremes', () => {
    expect(envCutoffHz(1_000, 0)).toBe(1_000);
    expect(envCutoffHz(1_000, 100)).toBeCloseTo(1_000 * 2 ** ENV_OCTAVES, 5);
    expect(envCutoffHz(1_000, -100)).toBeCloseTo(1_000 / 2 ** ENV_OCTAVES, 5);
    expect(envSweepActive(0)).toBe(false);
    expect(envSweepActive(12)).toBe(true);
  });
});

describe('svfLowpassMagDb', () => {
  const sr = 48_000;

  it('passes DC and puts the resonance bump at the cutoff', () => {
    expect(svfLowpassMagDb(20, 3_000, 0.707, sr)).toBeCloseTo(0, 1);
    const atCutoff = svfLowpassMagDb(3_000, 3_000, 4, sr);
    const below = svfLowpassMagDb(300, 3_000, 4, sr);
    const above = svfLowpassMagDb(10_000, 3_000, 4, sr);
    expect(atCutoff).toBeGreaterThan(6);
    expect(atCutoff).toBeGreaterThan(below);
    expect(atCutoff).toBeGreaterThan(above);
  });

  it('closes the top when cutoff drops, and raises the bump when Q rises', () => {
    const open = svfLowpassMagDb(8_000, 12_000, 0.8, sr);
    const closed = svfLowpassMagDb(8_000, 400, 0.8, sr);
    expect(closed).toBeLessThan(open - 12);
    expect(svfLowpassMagDb(1_000, 1_000, 8, sr)).toBeGreaterThan(
      svfLowpassMagDb(1_000, 1_000, 0.8, sr),
    );
  });

  it('falls back to 48 kHz when the device has not opened a rate', () => {
    expect(svfLowpassMagDb(1_000, 1_000, 1, 0)).toBeCloseTo(svfLowpassMagDb(1_000, 1_000, 1, sr), 5);
  });

  it('samples a log-spaced curve the EQ path helper can stroke', () => {
    const curve = svfLowpassCurveDb(2_000, 2, sr, 64);
    expect(curve).toHaveLength(64);
    expect(curve[0]!).toBeCloseTo(0, 1);
    expect(Math.max(...curve)).toBeGreaterThan(0);
  });
});

describe('oscillatorStack', () => {
  const saw: { wave: number; coarse: number; fine: number; mix: number } = {
    wave: WAVE_SAW,
    coarse: 0,
    fine: 0,
    mix: 1,
  };

  it('draws a rising saw and a square that flips at half-cycle', () => {
    expect(oscSample(WAVE_SAW, 0)).toBeCloseTo(-1, 5);
    expect(oscSample(WAVE_SAW, 0.75)).toBeGreaterThan(0);
    expect(oscSample(WAVE_SQUARE, 0.25)).toBe(-1);
    expect(oscSample(WAVE_SQUARE, 0.75)).toBe(1);
    expect(oscSample(WAVE_SINE, 0.25)).toBeCloseTo(1, 5);
  });

  it('stacks a second oscillator at its own pitch on the same cycle', () => {
    const one = oscillatorStack([{ ...saw, mix: 1 }, { ...saw, mix: 0 }, { ...saw, mix: 0 }], 32);
    const two = oscillatorStack(
      [
        { ...saw, mix: 1 },
        { ...saw, mix: 1, coarse: 12 },
        { ...saw, mix: 0 },
      ],
      32,
    );
    expect(one.layers).toHaveLength(1);
    expect(two.layers).toHaveLength(2);
    expect(rms(two.mix)).toBeGreaterThan(rms(one.mix));
    expect(pitchRatio(12, 0)).toBeCloseTo(2, 5);
    expect(pitchRatio(-12, 0)).toBeCloseTo(0.5, 5);
  });

  it('strokes a path that starts on the left of the box', () => {
    const stacked = oscillatorStack(
      [
        { ...saw, mix: 1 },
        { ...saw, mix: 0 },
        { wave: WAVE_SINE, coarse: -12, fine: 0, mix: 0.5 },
      ],
      8,
    );
    expect(oscillatorPath(stacked.mix, 100, 40).startsWith('M0.00 ')).toBe(true);
  });
});

describe('adsrLayout', () => {
  const box = { attackMs: 10, decayMs: 40, sustain: 0.5, releaseMs: 80 };

  it('puts attack, decay, sustain and release in that order across the box', () => {
    const laid = adsrLayout(box, 200, 100);
    expect(laid.points.attack.x).toBeLessThan(laid.points.decay.x);
    expect(laid.points.decay.x).toBeLessThan(laid.points.sustain.x);
    expect(laid.points.sustain.x).toBeLessThan(laid.points.release.x);
    expect(laid.points.attack.y).toBeLessThan(laid.points.decay.y);
    expect(laid.points.decay.y).toBeCloseTo(laid.points.sustain.y, 5);
    expect(laid.points.release.y).toBeCloseTo(laid.points.sustain.y, 5);
    expect(laid.path.startsWith('M')).toBe(true);
  });

  it('keeps a 1 ms attack left of a 4 s release instead of collapsing it', () => {
    const laid = adsrLayout({ attackMs: 1, decayMs: 1, sustain: 0.6, releaseMs: 4_000 }, 200, 80);
    expect(laid.points.attack.x).toBeGreaterThan(8);
    expect(laid.points.release.x - laid.points.sustain.x).toBeGreaterThan(laid.points.attack.x);
  });

  it('raises the sustain plateau when the level rises', () => {
    const low = adsrLayout({ ...box, sustain: 0.2 }, 200, 100);
    const high = adsrLayout({ ...box, sustain: 0.9 }, 200, 100);
    expect(high.points.decay.y).toBeLessThan(low.points.decay.y);
  });

  it('hits the nearest handle and writes the dragged stage back into the times', () => {
    const laid = adsrLayout(box, 200, 100);
    expect(hitAdsrHandle(laid, laid.points.attack.x, laid.points.attack.y, 8)).toBe('attack');
    expect(hitAdsrHandle(laid, 0, 0, 4)).toBeNull();

    const longer = adsrFromDrag(box, 'attack', laid.points.decay.x, laid.points.attack.y, 200, 100);
    expect(longer.attackMs).toBeGreaterThan(box.attackMs);
    const quieter = adsrFromDrag(box, 'decay', laid.points.decay.x, 80, 200, 100);
    expect(quieter.sustain).toBeLessThan(box.sustain);
    const slower = adsrFromDrag(box, 'release', laid.points.attack.x, laid.points.sustain.y, 200, 100);
    expect(slower.releaseMs).toBeGreaterThan(box.releaseMs);
  });
});

describe('filterFromPointer', () => {
  it('reads log frequency from X and resonance from the dB axis', () => {
    const left = filterFromPointer(0, 50, 200, 100, FILTER_RANGE_DB);
    const right = filterFromPointer(200, 50, 200, 100, FILTER_RANGE_DB);
    expect(left.cutoffHz).toBeCloseTo(eqHzFromX(0, 200), 5);
    expect(right.cutoffHz).toBeGreaterThan(left.cutoffHz);

    const dull = filterFromPointer(100, 80, 200, 100, FILTER_RANGE_DB);
    const peaky = filterFromPointer(100, 10, 200, 100, FILTER_RANGE_DB);
    expect(peaky.resonance).toBeGreaterThan(dull.resonance);
    expect(eqY(20 * Math.log10(peaky.resonance), 100, FILTER_RANGE_DB)).toBeCloseTo(10, 0);
  });
});

describe('signalPath', () => {
  it('shifts stacked traces off the midline', () => {
    const samples = [0, 1, 0, -1];
    const centre = signalPath(samples, 100, 100, 0);
    const raised = signalPath(samples, 100, 100, 0.1);
    expect(centre.startsWith('M')).toBe(true);
    expect(raised).not.toBe(centre);
  });
});

describe('normalisedScope', () => {
  it('keeps a quiet or silent hop visible instead of hiding the trace', () => {
    expect(normalisedScope([0, 0, 0])).toEqual([0, 0, 0]);
    expect(normalisedScope([0.004, -0.004])).toEqual([1, -1]);
  });

  it('scales a playing hop to ±1 by peak', () => {
    expect(normalisedScope([-0.5, 0.25])).toEqual([-1, 0.5]);
    expect(normalisedScope([0.8])).toBeNull();
  });
});

function rms(samples: readonly number[]): number {
  if (samples.length === 0) return 0;
  return Math.sqrt(samples.reduce((sum, value) => sum + value * value, 0) / samples.length);
}
