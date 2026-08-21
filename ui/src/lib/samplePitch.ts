/**
 * Sample root-pitch formatting and transpose math for the sample editor.
 *
 * Detection reports a MIDI note plus remaining cents. Calibration stores that
 * as the channel root; the target-note control writes the existing pitch knob.
 */

import { noteName } from './pattern';

export type DetectedPitchView = {
  midiNote: number;
  cents: number;
  confidence: number;
};

/** `F#3 +12 ct · 92%`, or `A4 · 100%` when the cents round to zero. */
export function formatDetectedPitch(result: DetectedPitchView): string {
  const cents = Math.round(result.cents);
  const name = noteName(result.midiNote);
  const pitch = cents === 0 ? name : `${name} ${cents > 0 ? '+' : ''}${cents} ct`;
  const confidence = Math.round(Math.min(1, Math.max(0, result.confidence)) * 100);
  return `${pitch} · ${confidence}%`;
}

/**
 * Channel transpose that retunes a detected source pitch to `targetNote`.
 *
 * Independent of the stored root: Find pitch names the sample, this writes the
 * existing pitch knob so C4 on the keyboard can be a C in concert.
 */
export function targetTransposeCents(
  detectedNote: number,
  detectedCents: number,
  targetNote: number,
): number {
  if (!Number.isFinite(detectedNote) || !Number.isFinite(targetNote)) return 0;
  const cents = Number.isFinite(detectedCents) ? detectedCents : 0;
  const value = (targetNote - detectedNote) * 100 - cents;
  if (!Number.isFinite(value)) return 0;
  return Math.min(1_200, Math.max(-1_200, Math.round(value)));
}

/**
 * Inclusive MIDI start and exclusive end of a keyboard viewport centred on
 * `center`, `octaves` wide, clamped to 0..=128.
 */
export function visibleKeyboardRange(center: number, octaves: number): readonly [number, number] {
  const span = Math.max(12, Math.round(Math.max(1, octaves) * 12));
  const midi = Number.isFinite(center) ? Math.round(center) : 60;
  let start = midi - Math.floor(span / 2);
  let end = start + span;
  if (start < 0) {
    end = Math.min(128, end - start);
    start = 0;
  }
  if (end > 128) {
    start = Math.max(0, start - (end - 128));
    end = 128;
  }
  return [start, end];
}

export type NoPitchReason = 'silent' | 'tooShort' | 'unvoiced' | 'unstable' | 'polyphonic';

/** Short status copy for a typed no-result. */
export function noPitchReasonCopy(reason: NoPitchReason): string {
  switch (reason) {
    case 'silent':
      return 'No stable pitch — silence';
    case 'tooShort':
      return 'No stable pitch — too short';
    case 'unvoiced':
      return 'No stable pitch — no tone';
    case 'unstable':
      return 'No stable pitch — unstable';
    case 'polyphonic':
      return 'No stable pitch — more than one pitch';
  }
}

export function pitchAnalysisKey(
  trackId: number,
  sourceId: number | null | undefined,
  startFrames: number,
  endFrames: number,
): string {
  return `${trackId}:${sourceId ?? 'none'}:${startFrames}:${endFrames}`;
}

export type WaveformGrab = 'body' | 'handle';
export type WaveformAction = 'audition' | 'edit' | 'reset';

/**
 * Waveform pointer arbitration: click, drag, handle drag, and double-click
 * are mutually exclusive.
 */
export function decideWaveformAction(input: {
  kind: WaveformGrab;
  movedPx: number;
  slopPx: number;
  doubleClick: boolean;
}): WaveformAction {
  if (input.doubleClick) return 'reset';
  if (input.kind === 'handle') return 'edit';
  if (input.movedPx >= input.slopPx) return 'edit';
  return 'audition';
}
