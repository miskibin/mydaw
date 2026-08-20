import { describe, expect, it } from 'vitest';

import type { ClipDto } from '../../api/types';
import {
  PLAYLIST_TOOL_BY_CODE,
  brushFromClip,
  clipMatchesBrush,
  paintStarts,
  playlistHoverCursor,
  resolvePlaylistGesture,
  stretchLength,
  stretchStart,
} from './gestures';

const pattern: ClipDto = {
  id: 1,
  kind: 'pattern',
  track: 0,
  start: 0,
  length: 3840,
  patternId: 7,
};

const audio: ClipDto = {
  id: 2,
  kind: 'audio',
  track: 0,
  start: 3840,
  length: 1920,
  sourceId: 3,
};

describe('resolvePlaylistGesture', () => {
  it('picks a clip with the paint tool so the next empty click can stamp it', () => {
    expect(
      resolvePlaylistGesture({ tool: 'paint', button: 0, ctrl: false, hit: 'clip' }),
    ).toEqual({ kind: 'grab' });
  });

  it('places on empty space with the paint tool', () => {
    expect(
      resolvePlaylistGesture({ tool: 'paint', button: 0, ctrl: false, hit: 'empty' }),
    ).toEqual({ kind: 'place' });
  });

  it('deselects on empty space with the select tool', () => {
    expect(
      resolvePlaylistGesture({ tool: 'select', button: 0, ctrl: false, hit: 'empty' }),
    ).toEqual({ kind: 'deselect' });
  });

  it('still grabs a clip with the select tool, so selecting arms the brush', () => {
    expect(
      resolvePlaylistGesture({ tool: 'select', button: 0, ctrl: false, hit: 'clip' }),
    ).toEqual({ kind: 'grab' });
  });

  it('erases a clip with the delete tool or a right click', () => {
    expect(
      resolvePlaylistGesture({ tool: 'delete', button: 0, ctrl: false, hit: 'clip' }),
    ).toEqual({ kind: 'erase' });
    expect(
      resolvePlaylistGesture({ tool: 'paint', button: 2, ctrl: false, hit: 'clip' }),
    ).toEqual({ kind: 'erase' });
  });

  it('right-drags empty space to erase when painting, not when selecting', () => {
    expect(
      resolvePlaylistGesture({ tool: 'paint', button: 2, ctrl: false, hit: 'empty' }),
    ).toEqual({ kind: 'erase-drag' });
    expect(
      resolvePlaylistGesture({ tool: 'select', button: 2, ctrl: false, hit: 'empty' }),
    ).toEqual({ kind: 'ignore' });
  });

  it('resizes from the grip unless the delete tool is on', () => {
    expect(
      resolvePlaylistGesture({ tool: 'paint', button: 0, ctrl: false, hit: 'resize' }),
    ).toEqual({ kind: 'resize' });
    expect(
      resolvePlaylistGesture({ tool: 'delete', button: 0, ctrl: false, hit: 'resize' }),
    ).toEqual({ kind: 'erase' });
  });
});

describe('brushFromClip / clipMatchesBrush', () => {
  it('arms the pattern or audio the clip plays', () => {
    expect(brushFromClip(pattern)).toEqual({ kind: 'pattern', id: 7 });
    expect(brushFromClip(audio)).toEqual({ kind: 'audio', id: 3 });
  });

  it('matches only clips of that same source', () => {
    const brush = brushFromClip(pattern);
    expect(clipMatchesBrush(pattern, brush)).toBe(true);
    expect(clipMatchesBrush(audio, brush)).toBe(false);
    expect(clipMatchesBrush(pattern, { kind: 'audio', id: 3 })).toBe(false);
    expect(clipMatchesBrush(pattern, null)).toBe(false);
  });
});

describe('paintStarts', () => {
  it('always includes the origin, even on a click', () => {
    expect(paintStarts(3840, 3840, 3840)).toEqual([3840]);
  });

  it('stamps a run of native-length clips as the drag covers each slot', () => {
    expect(paintStarts(0, 3840, 3840)).toEqual([0, 3840]);
    expect(paintStarts(0, 3839, 3840)).toEqual([0]);
    expect(paintStarts(0, 7680, 3840)).toEqual([0, 3840, 7680]);
  });

  it('paints leftward without going past tick 0', () => {
    expect(paintStarts(3840, 0, 3840)).toEqual([0, 3840]);
    // Origin is the grid: a half-length drag left does not invent a clip at 0.
    expect(paintStarts(1920, 0, 3840)).toEqual([1920]);
  });

  it('caps the run so a drag across the song cannot stamp hundreds', () => {
    expect(paintStarts(0, 1_000_000, 3840, 4)).toHaveLength(4);
  });
});

describe('stretchLength / stretchStart', () => {
  it('grows a clip toward the cursor and never returns zero', () => {
    expect(stretchStart(3840, 1000)).toBe(1000);
    expect(stretchLength(3840, 1000, 240)).toBe(2880);
    expect(stretchLength(0, 10, 240)).toBe(240);
  });
});

describe('PLAYLIST_TOOL_BY_CODE', () => {
  it('maps FL\'s P/B/E/D onto this shell\'s three tools', () => {
    expect(PLAYLIST_TOOL_BY_CODE.KeyP).toBe('paint');
    expect(PLAYLIST_TOOL_BY_CODE.KeyB).toBe('paint');
    expect(PLAYLIST_TOOL_BY_CODE.KeyE).toBe('select');
    expect(PLAYLIST_TOOL_BY_CODE.KeyD).toBe('delete');
  });
});

describe('playlistHoverCursor', () => {
  it('keeps the paint tool looking like a stamp, even over a clip', () => {
    expect(playlistHoverCursor('paint', 'empty')).toBe('copy');
    expect(playlistHoverCursor('paint', 'clip')).toBe('copy');
    expect(playlistHoverCursor('select', 'clip')).toBe('grab');
    expect(playlistHoverCursor('paint', 'resize')).toBe('ew-resize');
  });
});
