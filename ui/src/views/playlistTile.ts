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

/** Tile sizes are multiples of this (CSS px), or the whole grid when that is smaller. */
export const SIZE_STEP = 160;
/** How much larger than needed (CSS px per axis) a tile may be and still be kept. */
export const SIZE_KEEP = 640;

/**
 * Where the tile must be for a view at (`sx`, `sy`) of `vw`×`vh` over a `gw`×`gh` grid.
 * Keeps `prev` while the view stays at least a third of a margin inside it.
 */
export function planTile(prev: Tile | null, sx: number, sy: number, vw: number, vh: number, gw: number, gh: number, dpr: number): TilePlan {
  const { mx, my } = margins(vw, vh, dpr);
  // sizes in steps of `SIZE_STEP`: resizing the view (the mixer's height, the browser's width, a
  // window drag) keeps the tile and its pixels, where an exact fit redrew every clip on each frame
  const fit = (v: number, m: number, total: number) => snapPx(Math.max(1, Math.min(total, Math.ceil((v + 2 * m) / SIZE_STEP) * SIZE_STEP)), dpr);
  // a tile somewhat larger than needed is kept as well: closing and reopening a panel (the mixer,
  // F9) grows the tile once and then repaints nothing
  const keep = (need: number, had: number | undefined) => (had != null && had >= need && had - need <= SIZE_KEEP ? had : need);
  const w = keep(fit(vw, mx, gw), prev?.w), h = keep(fit(vh, my, gh), prev?.h);
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

/**
 * The damaged boxes (grid coordinates) that fall on `tile`, as tile-local rects on whole device
 * pixels (so a repaint clipped to one never blends into its neighbours), overlapping ones merged.
 * Null when one full paint is cheaper: many boxes, or most of the tile.
 */
export function damageRects(damage: readonly Rect[], tile: Tile, dpr: number): Rect[] | null {
  const out: Rect[] = [];
  for (const d of damage) {
    const x0 = Math.max(tile.x, d.x), y0 = Math.max(tile.y, d.y);
    const x1 = Math.min(tile.x + tile.w, d.x + d.w), y1 = Math.min(tile.y + tile.h, d.y + d.h);
    if (x1 <= x0 || y1 <= y0) continue;
    const a = Math.floor((x0 - tile.x) * dpr) / dpr, b = Math.floor((y0 - tile.y) * dpr) / dpr;
    out.push({ x: a, y: b, w: Math.ceil((x1 - tile.x) * dpr) / dpr - a, h: Math.ceil((y1 - tile.y) * dpr) / dpr - b });
  }
  if (out.length > 48) return null;
  const merged = mergeRects(out);
  let area = 0;
  for (const r of merged) area += r.w * r.h;
  return area > tile.w * tile.h * 0.5 ? null : merged;
}

/** Union of overlapping (or touching) rects until none overlap: a clipped repaint never covers a pixel twice. */
export function mergeRects(rs: readonly Rect[]): Rect[] {
  const out = rs.map(r => ({ ...r }));
  for (let changed = true; changed;) {
    changed = false;
    for (let i = 0; i < out.length && !changed; i++) {
      for (let j = i + 1; j < out.length; j++) {
        const a = out[i], b = out[j];
        if (a.x > b.x + b.w || b.x > a.x + a.w || a.y > b.y + b.h || b.y > a.y + a.h) continue;
        const x = Math.min(a.x, b.x), y = Math.min(a.y, b.y);
        out[i] = { x, y, w: Math.max(a.x + a.w, b.x + b.w) - x, h: Math.max(a.y + a.h, b.y + b.h) - y };
        out.splice(j, 1);
        changed = true;
        break;
      }
    }
  }
  return out;
}
