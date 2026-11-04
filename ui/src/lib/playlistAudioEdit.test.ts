import { describe, expect, it } from 'vitest';

import { TICKS_PER_BAR, TICKS_PER_STEP } from './pattern';
import {
  CLIP_GAIN_MAX_DB,
  CLIP_GAIN_MIN_DB,
  MIN_CLIP_TICKS,
  clampClipGain,
  fileFramesPerTick,
  formatClipGain,
  gainPeaks,
  latestOnly,
  splitAudioClip,
  splitPoint,
  trimClipStart,
} from './playlistEdit';

const BAR = TICKS_PER_BAR;
const BEAT = BAR / 4;
// At 120 bpm one tick is 1/1920 s: 25 frames of a 48 kHz file.
const BPM = 120;
const FILE_48K = { frames: 48_000 * 10, sampleRate: 48_000 }; // ten seconds
const PER_TICK_48K = 25;

describe('fileFramesPerTick', () => {
  it('converts at the file rate, not the engine rate', () => {
    expect(fileFramesPerTick(48_000, BPM)).toBe(PER_TICK_48K);
    expect(fileFramesPerTick(44_100, BPM)).toBeCloseTo(22.96875, 6);
    // Twice the tempo, half the frames per tick.
    expect(fileFramesPerTick(48_000, 240)).toBe(12.5);
  });
});

describe('trimClipStart', () => {
  const clip = { start: BAR, length: 2 * BAR, offsetFrames: 0 };

  it('moves the left edge in and advances the offset by the same time', () => {
    const next = trimClipStart(clip, FILE_48K, BPM, BAR + BEAT + 10, BEAT);
    expect(next).toEqual({
      start: BAR + BEAT,
      length: 2 * BAR - BEAT,
      offsetFrames: BEAT * PER_TICK_48K,
    });
    // The right edge never moves.
    expect(next.start + next.length).toBe(clip.start + clip.length);
  });

  it('keeps audio anchored at a 44.1 kHz file rate', () => {
    const file = { frames: 44_100 * 10, sampleRate: 44_100 };
    const next = trimClipStart(clip, file, BPM, BAR + BEAT, BEAT);
    // A beat at 120 bpm is half a second.
    expect(next.offsetFrames).toBe(22_050);
  });

  it('cannot open before the first frame of the file', () => {
    const trimmed = { start: BAR + BEAT, length: 2 * BAR - BEAT, offsetFrames: BEAT * PER_TICK_48K };
    // Dragged two bars left: stops where the file starts.
    const next = trimClipStart(trimmed, FILE_48K, BPM, 0, BEAT);
    expect(next).toEqual({ start: BAR, length: 2 * BAR, offsetFrames: 0 });
  });

  it('stops at an off-grid file start without moving the audio', () => {
    // An offset of 1000 frames = 40 ticks: the file starts at tick BAR - 40.
    const trimmed = { start: BAR, length: BAR, offsetFrames: 1_000 };
    const next = trimClipStart(trimmed, FILE_48K, BPM, 0, BEAT);
    expect(next.start).toBe(BAR - 40);
    expect(next.offsetFrames).toBe(0);
    expect(next.start + next.length).toBe(2 * BAR);
  });

  it('leaves the sub-tick remainder in the offset rather than shifting the audio', () => {
    // 1010 frames = 40.4 ticks: the earliest whole tick keeps 10 frames of offset.
    const trimmed = { start: BAR, length: BAR, offsetFrames: 1_010 };
    const next = trimClipStart(trimmed, FILE_48K, BPM, 0, 0);
    expect(next.start).toBe(BAR - 40);
    expect(next.offsetFrames).toBe(10);
  });

  it('drags back out to exactly where it started, whatever the frame rounding', () => {
    // 44.1 kHz at 130 bpm: 21.2019... frames a tick, so a bar of offset is
    // rounded to whole frames on the way in.
    const file = { frames: 44_100 * 10, sampleRate: 44_100 };
    const original = { start: 2 * BAR, length: 3 * BAR, offsetFrames: 0 };
    const trimmed = trimClipStart(original, file, 130, 3 * BAR, BEAT);
    expect(trimmed.start).toBe(3 * BAR);
    expect(trimClipStart(trimmed, file, 130, 0, BEAT)).toEqual(original);
  });

  it('cannot go before song tick 0', () => {
    const early = { start: BEAT, length: BAR, offsetFrames: 2 * BAR * PER_TICK_48K };
    const next = trimClipStart(early, FILE_48K, BPM, -5 * BAR, BEAT);
    expect(next.start).toBe(0);
    expect(next.offsetFrames).toBe(2 * BAR * PER_TICK_48K - BEAT * PER_TICK_48K);
  });

  it('cannot trim past the end of the clip', () => {
    const next = trimClipStart(clip, FILE_48K, BPM, 10 * BAR, BEAT);
    expect(next.length).toBe(MIN_CLIP_TICKS);
    expect(next.start + next.length).toBe(3 * BAR);
    expect(next.offsetFrames).toBe((2 * BAR - MIN_CLIP_TICKS) * PER_TICK_48K);
  });

  it('cannot trim past the end of the audio when the clip outlasts its file', () => {
    // A one-bar file (2 s at 120 bpm) in a four-bar clip.
    const file = { frames: BAR * PER_TICK_48K, sampleRate: 48_000 };
    const long = { start: 0, length: 4 * BAR, offsetFrames: 0 };
    const next = trimClipStart(long, file, BPM, 3 * BAR, BEAT);
    expect(next.start).toBe(BAR - MIN_CLIP_TICKS);
    expect(next.offsetFrames).toBeLessThan(file.frames);
  });

  it('does nothing when a short clip is dragged inwards', () => {
    const tiny = { start: BAR, length: MIN_CLIP_TICKS / 2, offsetFrames: 0 };
    expect(trimClipStart(tiny, FILE_48K, BPM, BAR + 10, 0)).toEqual(tiny);
  });

  it('bypasses the grid when free', () => {
    const next = trimClipStart(clip, FILE_48K, BPM, BAR + 123.4, BEAT, true);
    expect(next.start).toBe(BAR + 123);
    expect(next.offsetFrames).toBe(123 * PER_TICK_48K);
  });

  it('treats an unknown file length as unbounded on the right', () => {
    const next = trimClipStart(clip, { frames: 0, sampleRate: 48_000 }, BPM, 2 * BAR, BEAT);
    expect(next.start).toBe(2 * BAR);
  });
});

describe('splitPoint', () => {
  const clip = { start: BAR, length: 2 * BAR };

  it('snaps the cut to the grid', () => {
    expect(splitPoint(clip, 2 * BAR + 100, BEAT)).toBe(2 * BAR);
    expect(splitPoint(clip, 2 * BAR + 100, 0)).toBe(2 * BAR + 100);
    expect(splitPoint(clip, 2 * BAR + 100.6, BEAT, true)).toBe(2 * BAR + 101);
  });

  it('cuts where the pointer is when no grid line falls inside the clip', () => {
    const short = { start: BAR + 100, length: 500 };
    expect(splitPoint(short, BAR + 300, BAR)).toBe(BAR + 300);
  });

  it('does not jump to a far grid line when the nearest one is the clip edge', () => {
    // Bar 2.2 rounds to bar 2, the clip's start; bar 3 is inside but is not
    // where the user pointed.
    expect(splitPoint(clip, BAR + 0.2 * BAR, BAR)).toBeNull();
  });

  it('refuses a cut that would leave a sliver', () => {
    expect(splitPoint(clip, BAR + 5, 0)).toBeNull();
    expect(splitPoint(clip, 3 * BAR - 5, 0)).toBeNull();
    expect(splitPoint(clip, BAR, 0)).toBeNull();
    expect(splitPoint(clip, 0, 0)).toBeNull();
  });
});

describe('splitAudioClip', () => {
  it('shortens the left half and starts the right half where the audio was', () => {
    const clip = { start: BAR, length: 2 * BAR, offsetFrames: 1_000 };
    const { left, right } = splitAudioClip(clip, 2 * BAR, { sampleRate: 48_000 }, BPM);
    expect(left).toEqual({ start: BAR, length: BAR, offsetFrames: 1_000 });
    expect(right).toEqual({
      start: 2 * BAR,
      length: BAR,
      offsetFrames: 1_000 + BAR * PER_TICK_48K,
    });
    expect(left.length + right.length).toBe(clip.length);
  });

  it('converts at the file rate', () => {
    const clip = { start: 0, length: 2 * BAR, offsetFrames: 0 };
    const { right } = splitAudioClip(clip, BAR, { sampleRate: 44_100 }, BPM);
    // A bar at 120 bpm is two seconds.
    expect(right.offsetFrames).toBe(88_200);
  });

  it('trim then split lands on the same audio as split alone', () => {
    const clip = { start: 0, length: 4 * BAR, offsetFrames: 0 };
    const trimmed = trimClipStart(clip, FILE_48K, BPM, BAR, BEAT);
    const at = 2 * BAR + TICKS_PER_STEP;
    const a = splitAudioClip(trimmed, at, { sampleRate: 48_000 }, BPM).right;
    const b = splitAudioClip(clip, at, { sampleRate: 48_000 }, BPM).right;
    expect(a).toEqual(b);
  });
});

describe('clip gain', () => {
  it('clamps to the offered range and to a tenth of a dB', () => {
    expect(clampClipGain(100)).toBe(CLIP_GAIN_MAX_DB);
    expect(clampClipGain(-100)).toBe(CLIP_GAIN_MIN_DB);
    expect(clampClipGain(-3.04)).toBe(-3);
    expect(clampClipGain(Number.NaN)).toBe(0);
    expect(Object.is(clampClipGain(-0.01), 0)).toBe(true);
  });

  it('labels only a gain worth reading', () => {
    expect(formatClipGain(0)).toBe('');
    expect(formatClipGain(undefined)).toBe('');
    expect(formatClipGain(0.02)).toBe('');
    expect(formatClipGain(3)).toBe('+3.0 dB');
    expect(formatClipGain(-6.5)).toBe('−6.5 dB');
  });

  it('scales the drawn peaks, clamped to full scale', () => {
    const peaks = [
      [-0.5, 0.5],
      [-0.9, 0.9],
    ] as const;
    expect(gainPeaks(peaks, 0)).toEqual(peaks);
    const down = gainPeaks(peaks, -20 * Math.log10(2));
    expect(down[0]![0]).toBeCloseTo(-0.25, 2);
    expect(down[0]![1]).toBeCloseTo(0.25, 2);
    expect(gainPeaks(peaks, 12)[1]).toEqual([-1, 1]);
  });
});

describe('latestOnly', () => {
  it('sends one at a time and skips values superseded while one was in flight', async () => {
    const sent: number[] = [];
    const releases: (() => void)[] = [];
    const queue = latestOnly((value: number) => {
      sent.push(value);
      return new Promise<void>((resolve) => releases.push(resolve));
    });
    queue.push(1);
    queue.push(2);
    queue.push(3);
    expect(sent).toEqual([1]);
    releases.shift()!();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(sent).toEqual([1, 3]);
    releases.shift()!();
    await queue.settled();
    expect(sent).toEqual([1, 3]);
  });

  it('keeps going after a failed send', async () => {
    const sent: number[] = [];
    const queue = latestOnly(async (value: number) => {
      sent.push(value);
      if (value === 1) throw new Error('refused');
    });
    queue.push(1);
    await queue.settled();
    queue.push(2);
    await queue.settled();
    expect(sent).toEqual([1, 2]);
  });
});
