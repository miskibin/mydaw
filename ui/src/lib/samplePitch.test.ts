import { describe, expect, it } from 'vitest';

import {
  decideWaveformAction,
  formatDetectedPitch,
  noPitchReasonCopy,
  pitchAnalysisKey,
  targetTransposeCents,
  visibleKeyboardRange,
} from './samplePitch';

describe('formatDetectedPitch', () => {
  it('uses scientific note names and signed cents', () => {
    expect(formatDetectedPitch({ midiNote: 54, cents: 12, confidence: 0.92 })).toBe(
      'F#3 +12 ct · 92%',
    );
    expect(formatDetectedPitch({ midiNote: 69, cents: 0, confidence: 1 })).toBe('A4 · 100%');
    expect(formatDetectedPitch({ midiNote: 60, cents: -8.4, confidence: 0.5 })).toBe(
      'C4 -8 ct · 50%',
    );
  });

  it('rounds cents at the semitone boundary without inventing a neighbour', () => {
    expect(formatDetectedPitch({ midiNote: 60, cents: 49.6, confidence: 0.8 })).toBe(
      'C4 +50 ct · 80%',
    );
    expect(formatDetectedPitch({ midiNote: 61, cents: -49.4, confidence: 0.8 })).toBe(
      'C#4 -49 ct · 80%',
    );
  });
});

describe('targetTransposeCents', () => {
  it('retunes a detected F#3 +12 ct to C4', () => {
    expect(targetTransposeCents(54, 12, 60)).toBe(588);
  });

  it('clamps to the pitch knob', () => {
    expect(targetTransposeCents(36, 0, 96)).toBe(1_200);
    expect(targetTransposeCents(96, 0, 36)).toBe(-1_200);
  });
});

describe('visibleKeyboardRange', () => {
  it('centres two octaves on C4', () => {
    expect(visibleKeyboardRange(60, 2)).toEqual([48, 72]);
  });

  it('clamps at the MIDI edges', () => {
    expect(visibleKeyboardRange(0, 2)).toEqual([0, 24]);
    expect(visibleKeyboardRange(127, 2)).toEqual([104, 128]);
  });
});

describe('noPitchReasonCopy', () => {
  it('never offers C as a fallback', () => {
    expect(noPitchReasonCopy('unvoiced')).toContain('No stable pitch');
    expect(noPitchReasonCopy('polyphonic')).not.toMatch(/C4|middle C/i);
  });
});

describe('pitchAnalysisKey', () => {
  it('changes when the region or source changes', () => {
    expect(pitchAnalysisKey(1, 9, 0, 0)).not.toBe(pitchAnalysisKey(1, 9, 100, 0));
    expect(pitchAnalysisKey(1, 9, 0, 0)).not.toBe(pitchAnalysisKey(1, 10, 0, 0));
  });
});

describe('decideWaveformAction', () => {
  it('treats a still click on the body as audition', () => {
    expect(decideWaveformAction({ kind: 'body', movedPx: 2, slopPx: 3, doubleClick: false })).toBe(
      'audition',
    );
  });

  it('treats a drag on the body as a region edit', () => {
    expect(decideWaveformAction({ kind: 'body', movedPx: 8, slopPx: 3, doubleClick: false })).toBe(
      'edit',
    );
  });

  it('never auditions a handle drag', () => {
    expect(decideWaveformAction({ kind: 'handle', movedPx: 0, slopPx: 3, doubleClick: false })).toBe(
      'edit',
    );
  });

  it('gives double-click reset exclusive of audition', () => {
    expect(decideWaveformAction({ kind: 'body', movedPx: 0, slopPx: 3, doubleClick: true })).toBe(
      'reset',
    );
  });
});
