// View navigation maths shared by the editors (playlist, piano roll, channel rack).

/**
 * Auto-scroll speed (px per frame) for a pointer at `p` while something is dragged inside the
 * span [lo, hi]: 0 in the middle, ramping up over the last `zone` px before an edge and beyond
 * it, negative towards `lo`. Capped at `max`.
 */
export function edgeSpeed(p: number, lo: number, hi: number, zone = 28, max = 22): number {
  if (hi - lo < zone * 2) zone = Math.max(1, (hi - lo) / 4);
  const d = p < lo + zone ? p - (lo + zone) : p > hi - zone ? p - (hi - zone) : 0;
  if (!d) return 0;
  return Math.sign(d) * Math.min(max, Math.ceil((Math.abs(d) / zone) * (max / 2)));
}

/**
 * Scroll (px) that keeps the content under `anchor` (px from the view's left edge) in place when
 * the scale changes from `k0` to `k1` px per unit (zoom around the pointer). Never negative.
 */
export function zoomScroll(scroll: number, anchor: number, k0: number, k1: number): number {
  return Math.max(0, ((scroll + anchor) / k0) * k1 - anchor);
}

/**
 * Scale (px per unit, within [min, max]) and scroll (px) that show the range [a, b] across a view
 * `w` px wide. A range the limits do not let fill the view exactly is centred in it.
 */
export function zoomToRange(a: number, b: number, w: number, min: number, max: number): { k: number; scroll: number } {
  const lo = Math.min(a, b), hi = Math.max(a, b);
  const k = Math.min(max, Math.max(min, w / Math.max(1e-9, hi - lo)));
  return { k, scroll: Math.max(0, ((lo + hi) / 2) * k - w / 2) };
}

/** Units a "fit everything" zoom shows: the content's end rounded up to a whole `unit`, at least `minSpan`. */
export function fitSpan(end: number, unit: number, minSpan: number): number {
  return Math.max(minSpan, Math.ceil(Math.max(0, end) / unit - 1e-9) * unit);
}
