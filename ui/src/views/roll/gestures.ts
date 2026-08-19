/**
 * FL Studio's piano-roll pointer grammar, as a pure function.
 *
 * `Roll.tsx` executes whatever this returns; it does not decide. The matrix is
 * combinatorial (tool × button × modifiers × hit) and the one thing it must
 * not do is fight itself — in particular, Shift *then* click is a different
 * gesture from click *then* Shift. This file only sees modifiers at
 * pointer-down. Axis lock (Shift/Ctrl after the grab) is read per move inside
 * the drag handler, because that is how FL distinguishes "clone" from "lock
 * pitch".
 *
 * Source: Image-Line's piano roll manual. Draw is P, Paint is B — unofficial
 * blogs swap them.
 */

import type { RollTool } from '../../shell/view';
import { inGrid, inKeys, inRuler, inVelocity, type RollGeometry } from './draw';

export type RollRegion = 'grid' | 'ruler' | 'keys' | 'velocity' | 'outside';

/** What the pointer is on, inside the note grid. */
export type RollHit = 'empty' | 'body' | 'resize';

export type RollGesture =
  | { kind: 'ignore' }
  | { kind: 'pan' }
  | { kind: 'seek' }
  | { kind: 'select-time' }
  | { kind: 'preview-key' }
  | { kind: 'select-pitch' }
  | { kind: 'velocity' }
  | { kind: 'erase' }
  | { kind: 'marquee'; additive: boolean }
  | { kind: 'toggle-select' }
  | { kind: 'select-note' }
  | { kind: 'move-note' }
  | { kind: 'resize-note' }
  | { kind: 'clone-move' }
  | { kind: 'draw-place' }
  | { kind: 'draw-stretch' }
  | { kind: 'paint'; monophonic: boolean }
  | { kind: 'slice' }
  | { kind: 'slice-click' };

export interface GestureInput {
  tool: RollTool;
  button: number;
  shift: boolean;
  /** Ctrl on Windows, Command on macOS. */
  ctrl: boolean;
  alt: boolean;
  region: RollRegion;
  hit: RollHit;
}

/** Where `(x, y)` sits in the roll's chrome, or `'outside'`. */
export function regionAt(x: number, y: number, geo: RollGeometry): RollRegion {
  if (inRuler(x, y, geo)) return 'ruler';
  if (inKeys(x, y, geo)) return 'keys';
  if (inVelocity(x, y, geo)) return 'velocity';
  if (inGrid(x, y, geo)) return 'grid';
  return 'outside';
}

/**
 * The gesture a pointer-down should start.
 *
 * Priority matches FL: pan buttons, then region-specific chords (Ctrl on the
 * ruler or the keys), then right-erase, then Ctrl-as-select, then the tool.
 */
export function resolveGesture(input: GestureInput): RollGesture {
  const { tool, button, shift, ctrl, region, hit } = input;

  if (button === 1) return { kind: 'pan' };
  if (button === 2 && shift) return { kind: 'pan' };
  if (region === 'outside') return { kind: 'ignore' };

  if (region === 'ruler') {
    if (button !== 0) return { kind: 'ignore' };
    return ctrl ? { kind: 'select-time' } : { kind: 'seek' };
  }

  if (region === 'keys') {
    if (button !== 0) return { kind: 'ignore' };
    return ctrl ? { kind: 'select-pitch' } : { kind: 'preview-key' };
  }

  if (region === 'velocity') {
    if (button !== 0) return { kind: 'ignore' };
    return { kind: 'velocity' };
  }

  // Grid.
  if (button === 2) return { kind: 'erase' };
  if (button !== 0) return { kind: 'ignore' };

  if (ctrl) {
    if (hit === 'empty') return { kind: 'marquee', additive: shift };
    return shift ? { kind: 'toggle-select' } : { kind: 'select-note' };
  }

  if (tool === 'delete') return { kind: 'erase' };

  if (tool === 'slice') return shift ? { kind: 'slice-click' } : { kind: 'slice' };

  if (tool === 'select') {
    if (hit === 'empty') return { kind: 'marquee', additive: shift };
    if (shift) return { kind: 'toggle-select' };
    return hit === 'resize' ? { kind: 'resize-note' } : { kind: 'move-note' };
  }

  if (tool === 'paint') return { kind: 'paint', monophonic: shift };

  // Draw.
  if (hit === 'empty') return shift ? { kind: 'draw-stretch' } : { kind: 'draw-place' };
  if (shift) return { kind: 'clone-move' };
  return hit === 'resize' ? { kind: 'resize-note' } : { kind: 'move-note' };
}

/** CSS cursor for hover, when no gesture is running. */
export function hoverCursor(tool: RollTool, region: RollRegion, hit: RollHit): string {
  if (region === 'ruler') return 'ew-resize';
  if (region === 'keys') return 'pointer';
  if (region === 'velocity') return 'ns-resize';
  if (region !== 'grid') return 'default';
  if (tool === 'slice') return 'crosshair';
  if (tool === 'delete') return 'pointer';
  if (hit === 'resize') return 'ew-resize';
  if (hit === 'body') return 'grab';
  if (tool === 'paint') return 'copy';
  if (tool === 'select') return 'crosshair';
  return 'cell';
}

/** Physical keys that pick a roll tool, matching FL (P draw, B paint). */
export const ROLL_TOOL_BY_CODE: Readonly<Record<string, RollTool>> = {
  KeyP: 'draw',
  KeyB: 'paint',
  KeyE: 'select',
  KeyD: 'delete',
  KeyC: 'slice',
};
