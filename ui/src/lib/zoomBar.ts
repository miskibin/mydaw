// Range math of a zoom scrollbar (FL Studio / Ableton style): the thumb shows the visible part
// of the content; drag the thumb to scroll, drag either end of it to zoom that axis with the
// other end staying put. Pure: everything is in content units (steps, keys…) and track pixels.

/** The visible part of the content: start and length, in content units. */
export interface ZRange { pos: number; len: number }
/** Content length and the visible lengths the zoom limits allow. */
export interface ZLimits { total: number; minLen: number; maxLen: number }

/** Content the bar spans: at least the whole content, and the view when it reaches past it. */
export const spanOf = (r: ZRange, total: number) => Math.max(total, r.pos + r.len, 1e-9);

/** Thumb position and size in px on a track `W` px long; never smaller than `minPx`. */
export function thumbOf(r: ZRange, total: number, W: number, minPx = 20): { x: number; w: number } {
  const span = spanOf(r, total);
  const w = Math.min(W, Math.max(Math.min(minPx, W), (r.len / span) * W));
  const room = span - r.len;
  const x = room > 1e-9 ? (r.pos / room) * (W - w) : 0;
  return { x: Math.max(0, Math.min(W - w, x)), w };
}

/** The range scrolled to `pos`, kept inside the content. */
export function clampPos(r: ZRange, total: number, pos: number): ZRange {
  return { len: r.len, pos: Math.max(0, Math.min(Math.max(0, total - r.len), pos)) };
}

/** Thumb dragged by `dPx` from where it was at `r0` (the thumb follows the pointer). */
export function dragThumb(r0: ZRange, total: number, W: number, dPx: number, minPx = 20): ZRange {
  const span = spanOf(r0, total), { w } = thumbOf(r0, total, W, minPx);
  const room = span - r0.len, track = W - w;
  if (room <= 1e-9 || track <= 0) return clampPos(r0, total, r0.pos);
  return clampPos(r0, total, r0.pos + (dPx * room) / track);
}

/** Pressed on the track outside the thumb at `px`: the view centred there. */
export function centreAt(r: ZRange, total: number, W: number, px: number): ZRange {
  const span = spanOf(r, total);
  return clampPos(r, total, (px / Math.max(1, W)) * span - r.len / 2);
}

/** Visible length kept within the zoom limits. */
export const clampLen = (len: number, lim: ZLimits) => Math.max(lim.minLen, Math.min(Math.max(lim.minLen, lim.maxLen), len));

/**
 * One end of the thumb dragged by `dPx` (from `r0`, when the drag started): that end follows the
 * pointer, the other one stays put; the length stays inside the zoom limits (then the dragged
 * end stops) and the range inside the content.
 */
export function dragEdge(r0: ZRange, edge: 'lo' | 'hi', lim: ZLimits, W: number, dPx: number): ZRange {
  const span = spanOf(r0, lim.total), d = (dPx / Math.max(1, W)) * span;
  if (edge === 'lo') {
    const end = r0.pos + r0.len;
    const len = clampLen(end - Math.max(0, r0.pos + d), lim);
    return { pos: Math.max(0, end - len), len };
  }
  const len = clampLen(Math.min(Math.max(lim.total, r0.pos + r0.len), r0.pos + r0.len + d) - r0.pos, lim);
  return clampPos({ pos: r0.pos, len }, lim.total, r0.pos);
}

/** Zoomed by `factor` (> 1 = see more) around content point `at`, which stays where it is on screen. */
export function zoomAround(r: ZRange, lim: ZLimits, at: number, factor: number): ZRange {
  const len = clampLen(r.len * factor, lim), k = r.len > 0 ? (at - r.pos) / r.len : 0.5;
  return clampPos({ pos: r.pos, len }, lim.total, at - k * len);
}
