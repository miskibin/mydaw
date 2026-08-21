/**
 * Frame a set of piano-roll notes in the viewport: zoom and scroll so they
 * fill the grid, with a little air around the edges.
 *
 * Selection vs all-notes is the caller's choice — this only sees the notes it
 * was given. Empty input is `null`, not a default C3 window, because "zoom to
 * notes" with nothing to zoom to should say so rather than invent a range.
 */

import { TICKS_PER_STEP } from '../../lib/pattern';
import { clampZoom } from '../../shell/view';
import { ROW_H, STEP_W, TOP_KEY, clampScroll } from './draw';

export interface NoteSpan {
  start: number;
  length: number;
  key: number;
}

export interface NoteBounds {
  start: number;
  end: number;
  lowKey: number;
  highKey: number;
}

export interface FitNotesResult {
  zoomX: number;
  zoomY: number;
  scrollX: number;
  scrollY: number;
}

/** Extra time around the notes, as a fraction of their span. */
const TIME_PAD = 0.12;
/** Never frame tighter than a beat — a single 16th must not eat the window. */
const MIN_SPAN_TICKS = TICKS_PER_STEP * 4;
/** Keys of breathing room above and below the highest/lowest note. */
const KEY_PAD = 1;
/** Floor on the pitched window, so one note is not a single giant row. */
const MIN_KEY_SPAN = 8;

export function notesBounds(notes: readonly NoteSpan[]): NoteBounds | null {
  if (notes.length === 0) return null;
  let start = Infinity;
  let end = -Infinity;
  let lowKey = 127;
  let highKey = 0;
  for (const note of notes) {
    start = Math.min(start, note.start);
    end = Math.max(end, note.start + Math.max(1, note.length));
    lowKey = Math.min(lowKey, note.key);
    highKey = Math.max(highKey, note.key);
  }
  return { start, end, lowKey, highKey };
}

export function fitNotes(
  notes: readonly NoteSpan[],
  viewportW: number,
  viewportH: number,
  contentTicks: number,
): FitNotesResult | null {
  const bounds = notesBounds(notes);
  if (!bounds || viewportW <= 0 || viewportH <= 0) return null;

  const spanTicks = Math.max(MIN_SPAN_TICKS, bounds.end - bounds.start);
  const padTicks = Math.max(TICKS_PER_STEP, spanTicks * TIME_PAD);
  let left = bounds.start - padTicks;
  let right = bounds.end + padTicks;
  if (right - left < MIN_SPAN_TICKS) {
    const extra = (MIN_SPAN_TICKS - (right - left)) / 2;
    left -= extra;
    right += extra;
  }
  left = Math.max(0, left);
  right = Math.max(left + MIN_SPAN_TICKS, right);

  const zoomX = clampZoom((viewportW / (right - left)) * (TICKS_PER_STEP / STEP_W));
  const pxPerTick = (STEP_W * zoomX) / TICKS_PER_STEP;
  const spanPx = (right - left) * pxPerTick;
  const contentW = Math.max(right, contentTicks) * pxPerTick;
  const scrollX = clampScroll(
    left * pxPerTick - Math.max(0, viewportW - spanPx) / 2,
    viewportW,
    contentW,
  );

  let low = Math.max(0, bounds.lowKey - KEY_PAD);
  let high = Math.min(TOP_KEY, bounds.highKey + KEY_PAD);
  while (high - low + 1 < MIN_KEY_SPAN) {
    if (low > 0) low -= 1;
    else if (high < TOP_KEY) high += 1;
    else break;
  }
  const topRow = TOP_KEY - high;
  const botRow = TOP_KEY - low + 1;
  const zoomY = clampZoom(viewportH / ((botRow - topRow) * ROW_H));
  const rowH = ROW_H * zoomY;
  const spanKeysPx = (botRow - topRow) * rowH;
  const contentH = (TOP_KEY + 1) * rowH;
  const scrollY = clampScroll(
    topRow * rowH - Math.max(0, viewportH - spanKeysPx) / 2,
    viewportH,
    contentH,
  );

  return { zoomX, zoomY, scrollX, scrollY };
}
