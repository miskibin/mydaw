import { describe, expect, it } from 'vitest';
import type { Fx } from '../../api/types';
import { padFormant, padParams, padToVoice, voiceToPad } from './voice';

const fx = (params: Record<string, number>): Fx => ({ id: 1, type: 'voice', bypass: false, mix: 1, preset: null, params, bands: null, plugin: null } as unknown as Fx);

describe('voice pad', () => {
  it('maps semitones to the pad and back', () => {
    for (const [p, f] of [[0, 0], [-12, 12], [7.5, -3.2], [12, -12]]) {
      const at = voiceToPad(p, f, 700, 250), back = padToVoice(at.x, at.y, 700, 250);
      expect(back.pitch).toBeCloseTo(p, 6);
      expect(back.formant).toBeCloseTo(f, 6);
    }
  });
  it('clamps drags outside the pad to the range', () => {
    const v = padToVoice(-500, -500, 700, 250);
    expect(v.pitch).toBe(-12);
    expect(v.formant).toBe(12);
  });
  it('keeps Formant relative to Pitch when linked', () => {
    const linked = fx({ mode: 0, pitch: 5, formant: 1, link: 1 });
    expect(padFormant(linked, 5)).toBe(6);
    // dropping the handle at pitch +4, pad formant +6 → the knob keeps +2 on top of the shift
    expect(padParams(linked, 4, 6)).toEqual({ pitch: 4, formant: 2 });
    expect(padParams(fx({ mode: 0, link: 0 }), 4, 6)).toEqual({ pitch: 4, formant: 6 });
  });
  it('steps Robot by whole notes and ignores Link on the pad', () => {
    const robot = fx({ mode: 2, pitch: 0, formant: -2, link: 1 });
    expect(padFormant(robot, 7)).toBe(-2);
    expect(padParams(robot, 3.4, -2)).toEqual({ pitch: 3, formant: -2 });
    expect(padParams(fx({ mode: 0 }), 3.44, 0).pitch).toBe(3.4);
  });
});
