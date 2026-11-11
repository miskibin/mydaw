// Channel Rack window logic: how wide the window wants to be, where a channel moves.
import { gridW } from './pattern';

/**
 * The widest grid the window grows to on its own, in steps (16 bars). In practice the screen is
 * the limit: the window grows until it spans it, and only then does the grid scroll.
 */
export const RACK_FIT_STEPS = 256;
/** Steps of the greyed bar after the pattern (a click there makes the pattern a bar longer). */
export const RACK_EXTRA_STEPS = 16;

/**
 * The Channel Rack's width (px) for a pattern of `steps` steps: the pattern and the greyed extra
 * bar at step width `cw`, at most `RACK_FIT_STEPS` steps' worth, then no more than `room` (the
 * space to the screen's right edge) and no less than `min`. `left`: the channel strip; `pad`:
 * the frame around the grid.
 */
export function rackFitWidth(steps: number, o: { left: number; cw: number; pad: number; min: number; room: number }): number {
  const shown = Math.min(steps + RACK_EXTRA_STEPS, RACK_FIT_STEPS);
  const want = o.left + gridW(shown, o.cw) + o.pad;
  return Math.round(Math.max(o.min, Math.min(want, o.room)));
}

/**
 * Where a window of width `w` at `x` goes so that it stays on a screen `screen` px wide with
 * `margin` px to spare: it keeps its place while it fits and slides left when it has grown past
 * the right edge (the Channel Rack widening with its pattern).
 */
export function fitOnScreen(x: number, w: number, screen: number, margin = 10): { x: number; w: number } {
  const ww = Math.min(w, Math.max(0, screen - 2 * margin));
  return { x: Math.max(margin, Math.min(x, screen - margin - ww)), w: ww };
}

/** Alt+↑ / ↓: the index channel `i` of `n` moves to by `d`, or null at the top / bottom. */
export function moveIndex(i: number, n: number, d: number): number | null {
  const to = i + d;
  return i < 0 || to < 0 || to >= n || to === i ? null : to;
}
