import { describe, expect, it } from 'vitest';

import type { ClipDto } from '../api/types';
import { TICKS_PER_BAR } from './pattern';
import {
  clampGroupMove,
  clipScore,
  clipsIntersecting,
  duplicateToRight,
  pasteAtPlayhead,
  toggleClipId,
  unionClipIds,
} from './playlistEdit';

const BAR = TICKS_PER_BAR;

function clip(partial: Partial<ClipDto> & Pick<ClipDto, 'id'>): ClipDto {
  return {
    kind: 'pattern',
    track: 0,
    start: 0,
    length: BAR,
    patternId: 1,
    ...partial,
  };
}

describe('clipsIntersecting', () => {
  const clips = [
    clip({ id: 1, track: 0, start: 0, length: BAR }),
    clip({ id: 2, track: 0, start: BAR, length: BAR }),
    clip({ id: 3, track: 1, start: 0, length: BAR * 2 }),
  ];

  it('returns clips whose lane and time overlap the marquee', () => {
    expect(clipsIntersecting(clips, { track0: 0, track1: 0, start: 0, end: BAR / 2 })).toEqual([1]);
    expect(clipsIntersecting(clips, { track0: 0, track1: 1, start: 0, end: BAR * 2 })).toEqual([
      1, 2, 3,
    ]);
  });

  it('does not require the marquee to be dragged in a particular direction', () => {
    expect(clipsIntersecting(clips, { track0: 1, track1: 0, start: BAR * 2, end: BAR })).toEqual([
      2, 3,
    ]);
  });

  it('ignores a zero-size marquee', () => {
    expect(clipsIntersecting(clips, { track0: 0, track1: 0, start: BAR / 2, end: BAR / 2 })).toEqual(
      [],
    );
  });
});

describe('toggleClipId / unionClipIds', () => {
  it('adds a missing id and drops one that is already selected', () => {
    expect(toggleClipId([1, 2], 3)).toEqual([1, 2, 3]);
    expect(toggleClipId([1, 2], 2)).toEqual([1]);
  });

  it('unions without duplicating', () => {
    expect(unionClipIds([1, 2], [2, 3])).toEqual([1, 2, 3]);
  });
});

describe('duplicateToRight', () => {
  it('places copies flush against the selection\'s right edge, same lanes', () => {
    const scores = [
      clipScore(clip({ id: 1, track: 0, start: 0, length: BAR })),
      clipScore(clip({ id: 2, track: 1, start: BAR / 2, length: BAR })),
    ];
    // Group spans 0 .. 1.5 bars; copies start 1.5 bars later.
    expect(duplicateToRight(scores)).toEqual([
      { ...scores[0], start: BAR + BAR / 2 },
      { ...scores[1], start: BAR + BAR },
    ]);
  });

  it('is empty when nothing is selected', () => {
    expect(duplicateToRight([])).toEqual([]);
  });
});

describe('pasteAtPlayhead', () => {
  it('aligns the leftmost clip to the playhead and keeps relative time and lane', () => {
    const scores = [
      clipScore(clip({ id: 1, track: 2, start: BAR, length: BAR })),
      clipScore(clip({ id: 2, track: 3, start: BAR * 2, length: BAR })),
    ];
    expect(pasteAtPlayhead(scores, BAR * 4)).toEqual([
      { ...scores[0], start: BAR * 4 },
      { ...scores[1], start: BAR * 5 },
    ]);
  });

  it('does not place a clip before tick 0', () => {
    const scores = [clipScore(clip({ id: 1, start: BAR, length: BAR }))];
    expect(pasteAtPlayhead(scores, 0)[0]?.start).toBe(0);
  });
});

describe('clampGroupMove', () => {
  it('stops the leftmost clip at tick 0 and keeps every clip on a real lane', () => {
    const group = [
      { track: 1, start: BAR },
      { track: 2, start: 0 },
    ];
    expect(clampGroupMove(group, -4, -BAR * 2, 4)).toEqual({ dTrack: -1, dStart: 0 });
    expect(clampGroupMove(group, 8, BAR, 4)).toEqual({ dTrack: 1, dStart: BAR });
  });
});
