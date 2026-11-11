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
