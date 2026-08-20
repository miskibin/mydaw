/**
 * FL Studio's playlist pointer grammar, as a pure function.
 *
 * `Playlist.tsx` executes whatever this returns; it does not decide. The
 * irritating cases this exists to kill are the ones FL solved years ago:
 * clicking a clip loads it as the thing you paint next, a click on empty
 * stamps a clip of the source's own length, and a drag stamps a run.
 *
 * Source: Image-Line's playlist manual. Draw is P, Paint is B, Select is E,
 * Delete is D. This shell has one paint tool covering Draw and Paint: a
 * click places one clip, a drag paints a run, Shift-drag stretches one.
 */

import type { ClipDto } from '../../api/types';
import type { Brush, PlaylistTool } from '../../shell/view';

/** What the pointer is on, inside a lane. */
export type PlaylistHit = 'empty' | 'clip' | 'resize';

export type PlaylistGesture =
  | { kind: 'ignore' }
  | { kind: 'place' }
  | { kind: 'grab' }
  | { kind: 'resize' }
  | { kind: 'erase' }
  | { kind: 'erase-drag' }
  | { kind: 'deselect' };

export interface PlaylistGestureInput {
  tool: PlaylistTool;
  button: number;
  /** Ctrl on Windows, Command on macOS. */
  ctrl: boolean;
  hit: PlaylistHit;
}

/**
 * The gesture a pointer-down should start.
 *
 * Priority matches FL: right-click erases, the delete tool erases, a clip
 * under the paint or select tool is *picked* (and dragged only if the pointer
 * then moves), empty space with the paint tool places.
 */
export function resolvePlaylistGesture(input: PlaylistGestureInput): PlaylistGesture {
  const { tool, button, hit } = input;

  if (button === 1) return { kind: 'ignore' };

  if (button === 2) {
    if (hit === 'clip' || hit === 'resize') return { kind: 'erase' };
    if (tool === 'select') return { kind: 'ignore' };
    return { kind: 'erase-drag' };
  }

  if (button !== 0) return { kind: 'ignore' };

  if (tool === 'delete') {
    if (hit === 'empty') return { kind: 'erase-drag' };
    return { kind: 'erase' };
  }

  if (hit === 'resize') return { kind: 'resize' };

  if (hit === 'clip') return { kind: 'grab' };

  if (tool === 'paint') return { kind: 'place' };
  return { kind: 'deselect' };
}

/** CSS cursor for hover, when no gesture is running. */
export function playlistHoverCursor(tool: PlaylistTool, hit: PlaylistHit): string {
  if (hit === 'resize' && tool !== 'delete') return 'ew-resize';
  if (tool === 'delete') return 'pointer';
  if (hit === 'clip') return tool === 'paint' ? 'copy' : 'grab';
  if (tool === 'paint') return 'copy';
  return 'default';
}

/**
 * Physical keys that pick a playlist tool.
 *
 * P is FL Draw and B is FL Paint; both arm this shell's one paint tool.
 */
export const PLAYLIST_TOOL_BY_CODE: Readonly<Record<string, PlaylistTool>> = {
  KeyP: 'paint',
  KeyB: 'paint',
  KeyE: 'select',
  KeyD: 'delete',
};

/** The brush a playlist clip arms. `null` only if the clip is missing its id. */
export function brushFromClip(clip: ClipDto): Brush {
  if (clip.kind === 'pattern' && clip.patternId !== undefined) {
    return { kind: 'pattern', id: clip.patternId };
  }
  if (clip.kind === 'audio' && clip.sourceId !== undefined) {
    return { kind: 'audio', id: clip.sourceId };
  }
  return null;
}

/** Whether `clip` is an instance of the loaded brush. */
export function clipMatchesBrush(clip: ClipDto, brush: Brush): boolean {
  if (!brush) return false;
  if (brush.kind === 'pattern') return clip.kind === 'pattern' && clip.patternId === brush.id;
  return clip.kind === 'audio' && clip.sourceId === brush.id;
}

/**
 * Starts of stamped clips along a paint drag. Always includes `origin`.
 *
 * Stepping by `length` so a one-bar pattern dragged four bars becomes four
 * clips, which is what FL's paint tool does and what a hardcoded four-bar
 * stamp was getting wrong.
 */
export function paintStarts(origin: number, cursor: number, length: number, limit = 64): number[] {
  if (length <= 0) return [Math.max(0, origin)];
  const starts: number[] = [];
  if (cursor >= origin) {
    for (let tick = origin; tick <= cursor && starts.length < limit; tick += length) {
      starts.push(tick);
    }
  } else {
    for (let tick = origin; tick >= cursor && tick >= 0 && starts.length < limit; tick -= length) {
      starts.push(tick);
    }
    starts.reverse();
  }
  return starts.length > 0 ? starts : [Math.max(0, origin)];
}

/**
 * How long a Shift-drag stretch is, snapped, never zero.
 *
 * Origin is the pointer-down tick; the clip grows toward the cursor in either
 * direction. A click that never moved must not go through here — that places
 * the source's own length instead.
 */
export function stretchLength(origin: number, cursor: number, snap: number): number {
  const raw = Math.abs(cursor - origin);
  const grid = snap > 0 ? snap : 1;
  return Math.max(grid, Math.round(raw / grid) * grid);
}

/** Where a stretched clip starts: the left edge of origin and cursor. */
export function stretchStart(origin: number, cursor: number): number {
  return Math.max(0, Math.min(origin, cursor));
}
