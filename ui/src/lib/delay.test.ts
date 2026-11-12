import { describe, expect, it } from 'vitest';
import { beatFraction, BEAT_DIVS, noteName, snapBeats, stepBeats } from './beatDiv';
import { delayMs, echoTaps, loopGain, shapeCurve } from './delay';
import { knobVal, pFmt } from './presets';
import type { ParamDef } from '../api/types';

const beatsDef: ParamDef = { id: 'beats', label: 'Time', min: 1 / 48, max: 4, def: 0.75, unit: 'beat', log: true };

describe('synced delay times', () => {
  it('reads as beats with musical fractions', () => {
    expect(beatFraction(0.5)).toBe('1/2');
    expect(beatFraction(0.75)).toBe('3/4');
    expect(beatFraction(1 / 3)).toBe('1/3');
    expect(beatFraction(1.5)).toBe('1 1/2');
    expect(beatFraction(4 / 3)).toBe('1 1/3');
    expect(beatFraction(2)).toBe('2');
    expect(pFmt(beatsDef, 0.5)).toMatch(/^1\/2 beat/);
    expect(pFmt(beatsDef, 2)).toMatch(/^2 beat/);
  });
  it('names note values, dotted and triplet included', () => {
    expect(noteName(0.75)).toBe('1/8.');
    expect(noteName(1 / 3)).toBe('1/8T');
    expect(noteName(1)).toBe('1/4');
    expect(noteName(0.37)).toBeNull();
  });
  it('the knob snaps to note values and the wheel steps one value', () => {
    expect(snapBeats(0.48)).toBeCloseTo(0.5);
    expect(knobVal(beatsDef, 0.999)).toBe(4);
    expect(stepBeats(0.5, 1)).toBeCloseTo(2 / 3);
    expect(stepBeats(0.5, -1)).toBeCloseTo(3 / 8);
    expect(stepBeats(0.45, 1)).toBeCloseTo(0.5, 6); // off-grid: up goes to the next value, not past it
    expect(stepBeats(BEAT_DIVS[BEAT_DIVS.length - 1].b, 1)).toBe(4);
  });
  it('follows the tempo', () => {
    expect(delayMs({ sync: 0, beats: 0.5 }, 120)).toBeCloseTo(250);
    expect(delayMs({ sync: 0, beats: 0.5 }, 100)).toBeCloseTo(300);
    expect(delayMs({ sync: 1, time: 90 }, 100)).toBeCloseTo(90);
    expect(delayMs({ sync: 0, beats: 4 }, 30)).toBe(4000); // clamped like dsp::delay::MAX_DELAY_S
  });
});

describe('echo taps', () => {
  const base = { sync: 1, time: 100, fb: 50, mix: 100, input: 100, level: 0, ftype: 0 };
  it('stereo: both sides at every multiple, falling by the feedback', () => {
    const t = echoTaps({ ...base, model: 1 }, 120, 350);
    expect(t.map(x => x.ms)).toEqual([100, 100, 200, 200, 300, 300]);
    expect(t[2].amp).toBeCloseTo(0.5);
  });
  it('ping-pong starts opposite a left-panned source', () => {
    const t = echoTaps({ ...base, model: 2, pan: -100 }, 120, 250);
    const loud = t.filter(x => x.amp > 0.01);
    expect(loud.map(x => [x.ms, x.side])).toEqual([[100, 1], [200, 0]]);
  });
  it('model off has no echoes; offset delays one side', () => {
    expect(echoTaps({ ...base, model: 3 }, 120, 1000)).toEqual([]);
    const t = echoTaps({ ...base, model: 1, offset: 50 }, 120, 160);
    expect(t).toEqual([{ ms: 100, side: 0, amp: 1 }, { ms: 150, side: 1, amp: 1 }]);
  });
  it('loop gain flags self-oscillation (feedback or resonance)', () => {
    expect(loopGain({ fb: 110, ftype: 0 })).toBeGreaterThan(1);
    expect(loopGain({ fb: 40, ftype: 1, res: 0 })).toBeLessThan(1);
    expect(loopGain({ fb: 40, ftype: 1, res: 90 })).toBeGreaterThan(1);
  });
  it('the drive curve never exceeds its threshold', () => {
    for (let x = -5; x <= 5; x += 0.05) expect(Math.abs(shapeCurve(x, 1, 0.5, 0.5, 0))).toBeLessThanOrEqual(0.5 + 1e-9);
    expect(shapeCurve(0.2, 1, 1, 0.5, 0)).toBeCloseTo(0.2);
    expect(shapeCurve(3, 0, 0.5, 0, 0)).toBe(0.5);
  });
});
