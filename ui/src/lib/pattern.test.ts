import { describe, expect, it } from 'vitest';

import type { PatternDto } from '../api/types';
import {
  MAX_PATTERN_TICKS,
  TICKS_PER_BAR,
  TICKS_PER_STEP,
  audioWaveLayout,
  clipArtLayout,
  emptyRow,
  lengthForTicks,
  notesOf,
  withClearedChannel,
  withContentLength,
  withFilledSteps,
  withGrownLength,
  withInvertedSteps,
  withNotes,
  withRandomSteps,
  withRepeatedFirstBar,
  withShiftedSteps,
} from './pattern';

function blank(steps = 16): PatternDto {
  return { id: 1, name: 'P', steps, channels: [] };
}

function ons(pattern: PatternDto, channel = 0): number[] {
  const cells = pattern.channels.find((clip) => clip.channel === channel)?.steps?.steps ?? [];
  return cells.flatMap((cell, index) => (cell.on ? [index] : []));
}

describe('withFilledSteps', () => {
  it('puts a hit on each beat of a sixteenth grid', () => {
    expect(ons(withFilledSteps(blank(), 0, 4, 60))).toEqual([0, 4, 8, 12]);
  });

  it('puts eighths, a bar downbeat, and a full row', () => {
    expect(ons(withFilledSteps(blank(), 0, 2, 60))).toEqual([0, 2, 4, 6, 8, 10, 12, 14]);
    expect(ons(withFilledSteps(blank(), 0, 16, 60))).toEqual([0]);
    expect(ons(withFilledSteps(blank(), 0, 1, 60))).toHaveLength(16);
  });

  it('returns the same object when the fill is already there or the interval is illegal', () => {
    const filled = withFilledSteps(blank(), 0, 4, 60);
    expect(withFilledSteps(filled, 0, 4, 60)).toBe(filled);
    expect(withFilledSteps(filled, 0, 0, 60)).toBe(filled);
  });

  it('drops notes so a fill cannot double under a melody', () => {
    const before: PatternDto = {
      ...blank(),
      channels: [
        {
          channel: 0,
          mode: 'roll',
          steps: emptyRow(60, 16),
          notes: [{ key: 60, start: 0, length: 240, velocity: 0.8 }],
        },
      ],
    };
    const after = withFilledSteps(before, 0, 4, 60);
    expect(after.channels[0]!.notes).toEqual([]);
    expect(ons(after)).toEqual([0, 4, 8, 12]);
  });
});

describe('withInvertedSteps', () => {
  it('turns an empty row into a full one', () => {
    expect(ons(withInvertedSteps(blank(), 0, 60))).toHaveLength(16);
  });
});

describe('withShiftedSteps', () => {
  it('moves hits earlier and wraps the first cell', () => {
    const filled = withFilledSteps(blank(), 0, 4, 60);
    expect(ons(withShiftedSteps(filled, 0, -1, 60))).toEqual([3, 7, 11, 15]);
    expect(ons(withShiftedSteps(filled, 0, 1, 60))).toEqual([1, 5, 9, 13]);
  });
});

describe('withRandomSteps', () => {
  it('is the same pattern for the same seed', () => {
    const a = withRandomSteps(blank(), 0, 42, 60);
    const b = withRandomSteps(blank(), 0, 42, 60);
    expect(ons(a)).toEqual(ons(b));
    expect(ons(a).length).toBeGreaterThan(0);
    expect(ons(a).length).toBeLessThan(16);
  });
});

describe('withClearedChannel', () => {
  it('empties a filled row', () => {
    const filled = withFilledSteps(blank(), 0, 4, 60);
    expect(ons(withClearedChannel(filled, 0, 60))).toEqual([]);
  });
});

describe('clipArtLayout', () => {
  const oneBar = 16 * TICKS_PER_STEP;
  const px = TICKS_PER_BAR / 48;

  it('keeps one pattern-sized cycle when the clip is shorter, so overflow can crop', () => {
    expect(clipArtLayout(oneBar / 2, oneBar, px)).toEqual({
      cyclePx: oneBar * px,
      cycles: 1,
    });
  });

  it('tiles extra cycles when the clip is longer, instead of stretching one copy', () => {
    expect(clipArtLayout(oneBar * 2.25, oneBar, px)).toEqual({
      cyclePx: oneBar * px,
      cycles: 3,
    });
  });

  it('is one cycle when clip and pattern are the same length', () => {
    expect(clipArtLayout(oneBar, oneBar, px).cycles).toBe(1);
  });
});

describe('audioWaveLayout', () => {
  const px = 0.5;
  const fourBars = 4 * TICKS_PER_BAR;

  it('draws the wave at the file duration, so a shorter clip crops instead of shrinking', () => {
    const layout = audioWaveLayout(fourBars, 48_000, 0, px);
    expect(layout.wavePx).toBe(fourBars * px);
    expect(layout.offsetPx).toBe(0);
  });

  it('shifts left by the file offset so the audible start sits on the clip edge', () => {
    const { wavePx, offsetPx } = audioWaveLayout(fourBars, 1_000, 250, px);
    expect(offsetPx).toBe(wavePx * 0.25);
  });

  it('clamps an offset past the file to the end', () => {
    const { wavePx, offsetPx } = audioWaveLayout(fourBars, 1_000, 9_000, px);
    expect(offsetPx).toBe(wavePx);
  });

  it('is unshifted when the file has no frames', () => {
    expect(audioWaveLayout(fourBars, 0, 100, px).offsetPx).toBe(0);
  });
});

describe('lengthForTicks', () => {
  it('picks the shortest legal length that holds the ticks', () => {
    expect(lengthForTicks(0)).toBe(16);
    expect(lengthForTicks(TICKS_PER_BAR)).toBe(16);
    expect(lengthForTicks(TICKS_PER_BAR + 1)).toBe(32);
    expect(lengthForTicks(3 * TICKS_PER_BAR)).toBe(64);
  });

  it('stops at four bars rather than inventing a length the engine refuses', () => {
    expect(lengthForTicks(99 * TICKS_PER_BAR)).toBe(64);
    expect(MAX_PATTERN_TICKS).toBe(4 * TICKS_PER_BAR);
  });
});

describe('withGrownLength', () => {
  it('leaves a pattern that already fits alone, by reference', () => {
    const pattern = blank();
    expect(withGrownLength(pattern, TICKS_PER_BAR)).toBe(pattern);
  });

  it('pads every step row to the new length, which is what the engine demands', () => {
    const pattern: PatternDto = {
      ...blank(),
      channels: [{ channel: 0, mode: 'steps', steps: emptyRow(60, 16), notes: [] }],
    };
    const grown = withGrownLength(pattern, TICKS_PER_BAR + 1);
    expect(grown.steps).toBe(32);
    expect(grown.channels[0]!.steps!.steps).toHaveLength(32);
  });
});

describe('withContentLength', () => {
  it('shrinks to the last event and trims step rows to the wire length', () => {
    const pattern: PatternDto = {
      ...blank(64),
      channels: [{ channel: 0, mode: 'steps', steps: emptyRow(60, 64), notes: [] }],
    };
    pattern.channels[0]!.steps!.steps[20] = { on: true, velocity: 0.8 };
    const shrunk = withContentLength(pattern);
    expect(shrunk.steps).toBe(32);
    expect(shrunk.channels[0]!.steps!.steps).toHaveLength(32);
  });
});

describe('withNotes', () => {
  it('grows the pattern to hold a note written past the loop', () => {
    const grown = withNotes(
      blank(),
      0,
      [{ key: 60, start: 2 * TICKS_PER_BAR, length: TICKS_PER_STEP, velocity: 0.8 }],
      60,
    );
    expect(grown.steps).toBe(64);
    expect(notesOf(grown, 0)).toHaveLength(1);
  });

  it('grows for a note that starts inside the loop and runs past it', () => {
    const grown = withNotes(
      blank(),
      0,
      [{ key: 60, start: TICKS_PER_BAR - TICKS_PER_STEP, length: TICKS_PER_BAR, velocity: 0.8 }],
      60,
    );
    expect(grown.steps).toBe(32);
    expect(notesOf(grown, 0)[0]!.length).toBe(TICKS_PER_BAR);
  });

  it('clamps past the longest pattern instead of growing further', () => {
    const long = withNotes(
      blank(64),
      0,
      [{ key: 60, start: 3 * TICKS_PER_BAR, length: 4 * TICKS_PER_BAR, velocity: 0.8 }],
      60,
    );
    expect(long.steps).toBe(64);
    expect(notesOf(long, 0)[0]!.length).toBe(TICKS_PER_BAR);
  });

  it('shrinks after the notes that extended it are deleted', () => {
    const extended = withNotes(
      blank(),
      0,
      [{ key: 60, start: TICKS_PER_BAR, length: TICKS_PER_STEP, velocity: 0.8 }],
      60,
    );
    expect(extended.steps).toBe(32);
    expect(withNotes(extended, 0, [], 60).steps).toBe(16);
  });

  it('does not shrink past content on another channel', () => {
    const pattern: PatternDto = {
      ...blank(64),
      channels: [
        { channel: 0, mode: 'roll', notes: [], steps: null },
        {
          channel: 1,
          mode: 'roll',
          notes: [{ key: 67, start: TICKS_PER_BAR, length: 120, velocity: 0.8 }],
          steps: null,
        },
      ],
    };
    expect(withNotes(pattern, 0, [], 60).steps).toBe(32);
  });
});

describe('withRepeatedFirstBar', () => {
  it('copies first-bar steps into every bar with their velocities', () => {
    const pattern: PatternDto = {
      ...blank(64),
      channels: [{ channel: 0, mode: 'steps', steps: emptyRow(60, 64), notes: [] }],
    };
    pattern.channels[0]!.steps!.steps[3] = { on: true, velocity: 0.91 };
    const repeated = withRepeatedFirstBar(pattern, 0, 60);
    expect(ons(repeated)).toEqual([3, 19, 35, 51]);
    expect(repeated.channels[0]!.steps!.steps[51]!.velocity).toBe(0.91);
  });

  it('copies first-bar piano-roll notes through the whole pattern', () => {
    const pattern: PatternDto = {
      ...blank(32),
      channels: [{ channel: 0, mode: 'roll', steps: null, notes: [
        { key: 64, start: 240, length: 120, velocity: 0.7 },
      ] }],
    };
    const starts = withRepeatedFirstBar(pattern, 0, 60).channels[0]!.notes.map((note) => note.start);
    expect(starts).toEqual([240, TICKS_PER_BAR + 240]);
  });

  it('is a no-op for a one-bar pattern', () => {
    const pattern = blank();
    expect(withRepeatedFirstBar(pattern, 0, 60)).toBe(pattern);
  });
});
