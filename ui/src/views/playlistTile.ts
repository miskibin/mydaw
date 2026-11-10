// The playlist canvases are tiles in grid (content) coordinates, a margin larger than the
// view: the browser scrolls them on the compositor together with the lane headers, so a
// scroll step needs no redraw and never shows the grid a frame behind the DOM. When the view
// nears a tile edge the tile is re-centred: the pixels it already has are shifted (blit) and
// only the newly exposed strips are drawn.

export interface Tile { x: number; y: number; w: number; h: number }
/** A strip to draw, in tile-local CSS px. */
export interface Rect { x: number; y: number; w: number; h: number }
export interface TilePlan {
  tile: Tile;
  /** redraw everything (new size, first draw, or a jump past the old tile) */
  full: boolean;
  /** the old pixels move by this much (CSS px, a whole number of device px) */
  dx: number; dy: number;
  /** exposed strips after the shift (empty when `full` or unchanged) */
  strips: Rect[];
}

/** Device-pixel aligned value (CSS px), so blits and strip edges land on whole pixels. */
export const snapPx = (v: number, dpr: number) => Math.round(v * dpr) / dpr;

/** Margins around the view (CSS px), bounded so a tile stays under ~`maxPx` device pixels. */
export function margins(vw: number, vh: number, dpr: number, maxPx = 12e6): { mx: number; my: number } {
  let mx = Math.min(384, Math.max(96, Math.round(vw * 0.3)));
  let my = Math.min(256, Math.max(64, Math.round(vh * 0.3)));
  const area = (a: number, b: number) => (vw + 2 * a) * (vh + 2 * b) * dpr * dpr;
  while ((mx > 32 || my > 32) && area(mx, my) > maxPx) { mx = Math.max(32, Math.round(mx * 0.8)); my = Math.max(32, Math.round(my * 0.8)); }
  return { mx, my };
}

/**
 * Where the tile must be for a view at (`sx`, `sy`) of `vw`×`vh` over a `gw`×`gh` grid.
 * Keeps `prev` while the view stays at least a third of a margin inside it.
 */
export function planTile(prev: Tile | null, sx: number, sy: number, vw: number, vh: number, gw: number, gh: number, dpr: number): TilePlan {
  const { mx, my } = margins(vw, vh, dpr);
  const w = snapPx(Math.max(1, Math.min(gw, vw + 2 * mx)), dpr), h = snapPx(Math.max(1, Math.min(gh, vh + 2 * my)), dpr);
  const place = (s: number, m: number, size: number, total: number) => snapPx(Math.max(0, Math.min(Math.max(0, total - size), s - m)), dpr);
  const fresh = (): TilePlan => ({ tile: { x: place(sx, mx, w, gw), y: place(sy, my, h, gh), w, h }, full: true, dx: 0, dy: 0, strips: [] });
  if (!prev || prev.w !== w || prev.h !== h) return fresh();
  // inside with a guard band (an edge of the grid needs none: nothing lies past it)
  const okAxis = (p: number, size: number, s: number, v: number, m: number, total: number) =>
    (s >= p + m / 3 || p <= 0) && (s + v <= p + size - m / 3 || p + size >= total - 0.5);
  const okX = okAxis(prev.x, w, sx, vw, mx, gw), okY = okAxis(prev.y, h, sy, vh, my, gh);
  if (okX && okY) return { tile: prev, full: false, dx: 0, dy: 0, strips: [] };
  const x = okX ? prev.x : place(sx, mx, w, gw), y = okY ? prev.y : place(sy, my, h, gh);
  const dx = prev.x - x, dy = prev.y - y;
  if (Math.abs(dx) >= w || Math.abs(dy) >= h) return { ...fresh(), tile: { x, y, w, h } };
  const strips: Rect[] = [];
  // columns exposed by the horizontal shift (full height), then rows of the rest (no overlap:
  // a strip drawn twice would double its translucent pixels)
  let cx0 = 0, cx1 = w;
  if (dx > 0) { strips.push({ x: 0, y: 0, w: dx, h }); cx0 = dx; }
  else if (dx < 0) { strips.push({ x: w + dx, y: 0, w: -dx, h }); cx1 = w + dx; }
  if (cx1 > cx0) {
    if (dy > 0) strips.push({ x: cx0, y: 0, w: cx1 - cx0, h: dy });
    else if (dy < 0) strips.push({ x: cx0, y: h + dy, w: cx1 - cx0, h: -dy });
  }
  return { tile: { x, y, w, h }, full: false, dx, dy, strips };
}

/**
 * Follow playback: the scroll position that keeps the playhead (grid x `x`) in a view at
 * `sl` of width `view` — a page jump when it nears the right edge or is left of the view.
 * All in grid coordinates (mixing in view-relative x made the view jump back and forth every frame).
 */
export function followScroll(x: number, sl: number, view: number): number {
  if (x > sl + view - 40) return x - 80;
  if (x < sl) return Math.max(0, x - 80);
  return sl;
}
