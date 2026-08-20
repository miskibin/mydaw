/**
 * Viewport arithmetic for a scrolling timeline: zoom that keeps its anchor,
 * scroll that follows the playhead, and the tick a pointer is over.
 *
 * Solid-free and DOM-free on purpose -- these are the three sums the playlist
 * used to do inline and get wrong, and they are far easier to argue with as
 * pure functions than as a wheel handler.
 *
 * ## The coordinate systems, once
 *
 * - **Timeline px** counts from tick 0 of the arrangement.
 * - **Content px** counts from the left edge of the scrolled element, which
 *   starts with the lane headers: `content = header + timeline`.
 * - **Viewport px** counts from the left edge of the visible box, so the
 *   pointer's `clientX - rect.left`.
 *
 * The lane headers are `position: sticky; left: 0`, so the leftmost `header`
 * pixels of the viewport are *always* covered by them: the visible slice of the
 * timeline is `[scrollLeft, scrollLeft + viewport - header]`, not
 * `[scrollLeft, scrollLeft + viewport]`. Forgetting that is what makes a
 * "scroll to the playhead" leave the playhead behind a lane header.
 */

/** Hold `value` inside `0..max`. */
export function clampScroll(value: number, max: number): number {
  if (!Number.isFinite(value)) return 0;
  return Math.min(Math.max(0, max), Math.max(0, value));
}

/** Timeline px under a pointer, from its offset in the viewport. */
export function timelinePxAt(pointerOffset: number, scrollLeft: number, headerPx: number): number {
  return scrollLeft + pointerOffset - headerPx;
}

export interface ZoomAnchorArgs {
  /** Where the scroller is now. */
  scrollLeft: number;
  /** Pointer position inside the viewport (`clientX - rect.left`). */
  pointerOffset: number;
  /** Width of the sticky lane headers. */
  headerPx: number;
  /** Pixels per tick before the zoom. */
  pxPerTick: number;
  /** Pixels per tick after it. */
  nextPxPerTick: number;
  /** The widest the scroller can go after the zoom. */
  maxScroll: number;
}

/**
 * Where to scroll so the tick under the pointer stays under the pointer.
 *
 * Zooming about the left edge -- which is what changing the zoom and leaving
 * `scrollLeft` alone does -- throws whatever you were looking at off the side of
 * the screen, so every zoom becomes a zoom *and* a hunt for your place again.
 */
export function zoomAnchoredScroll(args: ZoomAnchorArgs): number {
  const { scrollLeft, pointerOffset, headerPx, pxPerTick, nextPxPerTick, maxScroll } = args;
  if (pxPerTick <= 0 || nextPxPerTick <= 0) return clampScroll(scrollLeft, maxScroll);
  // Anchor on the timeline, never on the headers: a pointer over a lane header
  // is not over any tick, so hold the left edge of the timeline still instead.
  const anchor = Math.max(0, pointerOffset - headerPx) + headerPx;
  const ticks = timelinePxAt(anchor, scrollLeft, headerPx) / pxPerTick;
  return clampScroll(headerPx + ticks * nextPxPerTick - anchor, maxScroll);
}

export interface FollowArgs {
  /** The playhead, in timeline px. */
  playheadPx: number;
  scrollLeft: number;
  /** Width of the visible box, headers included. */
  viewportPx: number;
  headerPx: number;
  maxScroll: number;
}

/**
 * Where to scroll so a playing playhead stays on screen, or `null` to stay put.
 *
 * Only moves when the playhead has actually left the comfortable middle, so a
 * transport crossing the screen scrolls in pages rather than dragging the whole
 * arrangement under the cursor a pixel at a time.
 */
export function followScroll(args: FollowArgs): number | null {
  const { playheadPx, scrollLeft, viewportPx, headerPx, maxScroll } = args;
  const width = viewportPx - headerPx;
  if (width <= 0) return null;
  const margin = Math.min(width * 0.12, 120);
  const from = scrollLeft + margin;
  const to = scrollLeft + width - margin;
  if (playheadPx >= from && playheadPx <= to) return null;
  const next = clampScroll(playheadPx - margin, maxScroll);
  return Math.abs(next - scrollLeft) < 1 ? null : next;
}

/** How a wheel event over a timeline should be read. */
export type WheelIntent = 'zoom' | 'scrollX' | 'scrollY';

/**
 * What one wheel event means over the timeline.
 *
 * The playlist used to spend **shift** on zoom, which is the one modifier every
 * OS and browser already means "scroll sideways" by -- so the arrangement had no
 * horizontal wheel scroll at all, and getting back to bar 1 meant hunting for
 * the scrollbar. Zoom moves to ctrl/alt, which is where a DAW puts it.
 */
export function wheelIntent(event: {
  ctrlKey: boolean;
  altKey: boolean;
  metaKey: boolean;
  shiftKey: boolean;
  deltaX: number;
}): WheelIntent {
  if (event.ctrlKey || event.altKey || event.metaKey) return 'zoom';
  // A horizontal wheel -- a trackpad swipe, or Chromium's rendering of a
  // shift-held wheel -- is already sideways travel and needs no modifier.
  if (event.shiftKey || event.deltaX !== 0) return 'scrollX';
  return 'scrollY';
}

/**
 * Pixels one wheel event should scroll by.
 *
 * `deltaMode` is read here (unlike {@link import('../ui/drag').wheelNotches},
 * which only wants a sign) because this *is* a scroll: a line-mode wheel of 3
 * has to become pixels or the timeline crawls.
 */
export function wheelPixels(event: { deltaX: number; deltaY: number; deltaMode: number }): number {
  const raw = event.deltaX !== 0 ? event.deltaX : event.deltaY;
  if (raw === 0) return 0;
  // 0 = pixel, 1 = line, 2 = page.
  const scale = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? 320 : 1;
  return raw * scale;
}
