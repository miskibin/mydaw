// Pointer drag helper: window-level move/up listeners plus a body cursor while dragging.
import { edgeSpeed } from '../lib/viewNav';

export function track(move: (e: PointerEvent) => void, up?: ((e: PointerEvent) => void) | null, cursor?: string) {
  const u = (ev: PointerEvent) => {
    window.removeEventListener('pointermove', move);
    window.removeEventListener('pointerup', u);
    window.removeEventListener('pointercancel', u);
    document.body.style.cursor = '';
    up?.(ev);
  };
  if (cursor) document.body.style.cursor = cursor;
  window.addEventListener('pointermove', move);
  window.addEventListener('pointerup', u);
  window.addEventListener('pointercancel', u);
}

let lastTap: { k: string; t: number } | null = null;
/** True when this is the second tap on `key` within 320 ms (pointerdown-based double click). */
export function dblCheck(key: string): boolean {
  const now = performance.now();
  const d = !!lastTap && lastTap.k === key && now - lastTap.t < 320;
  lastTap = d ? null : { k: key, t: now };
  return d;
}

/** `track` with the moves coalesced to one per animation frame (high-rate mice and touchpads send
 * several pointermoves per frame; each one re-applied a whole pattern). The last move is flushed
 * before `up`. */
export function trackFrame(move: (e: PointerEvent) => void, up?: ((e: PointerEvent) => void) | null, cursor?: string) {
  let last: PointerEvent | null = null, raf = 0;
  const run = () => { raf = 0; const e = last; last = null; if (e) move(e); };
  track(e => { last = e; if (!raf) raf = requestAnimationFrame(run); }, e => { if (raf) cancelAnimationFrame(raf); run(); up?.(e); }, cursor);
}

/**
 * Auto-scroll while dragging near the edge of a view. Call `move(e)` from the drag's move
 * handler and `stop()` when it ends. While the pointer sits in an edge zone of `rect()`, every
 * frame `scrollBy(dx, dy)` is called (return false when the view could not scroll any further)
 * and `replay(e)` re-runs the drag with the last pointer event, so what is dragged keeps up
 * with the view. `axes`: which directions scroll.
 */
export function edgeScroller(
  rect: () => { left: number; top: number; right: number; bottom: number },
  scrollBy: (dx: number, dy: number) => boolean,
  replay: (e: PointerEvent) => void,
  axes: { x?: boolean; y?: boolean } = { x: true, y: true },
) {
  let last: PointerEvent | null = null, raf = 0;
  const tick = () => {
    raf = 0;
    const e = last; if (!e) return;
    const r = rect();
    const dx = axes.x ? edgeSpeed(e.clientX, r.left, r.right) : 0, dy = axes.y ? edgeSpeed(e.clientY, r.top, r.bottom) : 0;
    if (!dx && !dy) return;
    if (scrollBy(dx, dy)) { replay(e); raf = requestAnimationFrame(tick); }
  };
  return {
    move(e: PointerEvent) { last = e; if (!raf) raf = requestAnimationFrame(tick); },
    stop() { last = null; if (raf) cancelAnimationFrame(raf); raf = 0; },
  };
}

/**
 * Middle-button pan (FL): call from pointerdown; returns true when it took the event. `at()` is
 * the view's scroll (px), `set(x, y)` applies a new one (the view clamps it).
 */
export function panDrag(e: PointerEvent, at: () => { x: number; y: number }, set: (x: number, y: number) => void): boolean {
  if (e.button !== 1) return false;
  e.preventDefault();
  const o = at(), sx = e.clientX, sy = e.clientY;
  track(ev => set(o.x - (ev.clientX - sx), o.y - (ev.clientY - sy)), null, 'grabbing');
  return true;
}
