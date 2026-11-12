// Editing an automation envelope on its playlist clip, without an edit mode (as in FL / Ableton /
// Bitwig): what is under the pointer (a point, a segment's tension handle, or the clip itself),
// and the point edits as pure functions of the points. Points are envelope-time (`t` beats from
// the automation start) and normalised values; the view maps them through `EnvGeom`.
import type { AutoPoint, CurveShape } from '../api/types';
import { curveAt } from './automation';

export interface EnvGeom {
  /** Envelope time → grid x, and back. */
  X: (t: number) => number; T: (x: number) => number;
  /** Normalised value → grid y, and back (clamped 0..1). */
  Y: (v: number) => number; V: (y: number) => number;
}

/** Pointer radius (px) that picks a point, and a tension handle. */
export const POINT_R = 7, HANDLE_R = 6;
/** Segments narrower than this (px) show no handle: it would sit on the points. */
export const HANDLE_MIN_W = 18;

/** Whether a segment ending at a point of `shape` has a handle (a hold has nothing to bend). */
export const hasHandle = (shape: CurveShape | undefined) => shape !== 'hold';

/** Where the handle of the segment ending at point `i` sits: halfway in time, on the curve. */
export function handleAt(pts: readonly AutoPoint[], i: number): { t: number; v: number } | null {
  if (i < 1 || i >= pts.length || !hasHandle(pts[i].shape)) return null;
  const a = pts[i - 1], b = pts[i];
  return { t: (a.t + b.t) / 2, v: a.v + (b.v - a.v) * curveAt(b.shape, b.tension, 0.5) };
}

export type EnvHit = { kind: 'point'; i: number } | { kind: 'handle'; i: number };

/** The point or handle nearest to (x, y) within reach; points win over handles. */
export function hitEnv(pts: readonly AutoPoint[], G: EnvGeom, x: number, y: number): EnvHit | null {
  let best = -1, bd = POINT_R;
  pts.forEach((q, i) => { const d = Math.hypot(G.X(q.t) - x, G.Y(q.v) - y); if (d <= bd) { bd = d; best = i; } });
  if (best >= 0) return { kind: 'point', i: best };
  let hb = -1, hd = HANDLE_R;
  for (let i = 1; i < pts.length; i++) {
    if (G.X(pts[i].t) - G.X(pts[i - 1].t) < HANDLE_MIN_W) continue;
    const h = handleAt(pts, i); if (!h) continue;
    const d = Math.hypot(G.X(h.t) - x, G.Y(h.v) - y);
    if (d <= hd) { hd = d; hb = i; }
  }
  return hb >= 0 ? { kind: 'handle', i: hb } : null;
}

/**
 * The tension after dragging the handle of the segment `a` → `b` by `dy` px (negative = up) from
 * `k0`. A curve follows the pointer (up bends it up, whichever way the segment goes); an S gets
 * sharper going up; stairs get more steps going up. `fine` (Ctrl) is 5× slower.
 */
export function dragTension(k0: number, dy: number, a: AutoPoint, b: AutoPoint, fine = false): number {
  const d = dy / (fine ? 400 : 80);
  // on a rising curve, a higher middle is a negative tension (fast start); on a falling one, positive
  const dk = (b.shape ?? 'curve') === 'curve' ? (b.v >= a.v ? d : -d) : -d;
  return Math.min(1, Math.max(-1, k0 + dk));
}

/**
 * Points `sel` moved by `dt` beats and `dv`: values clamped 0..1, times kept between the
 * unselected neighbours (points never pass each other, so each keeps its segment shape) and ≥ 0.
 * Returns the new points and the `dt` actually applied.
 */
export function movePoints(pts: readonly AutoPoint[], sel: ReadonlySet<number>, dt: number, dv: number): { points: AutoPoint[]; dt: number } {
  let lo = -Infinity, hi = Infinity;
  for (const i of sel) {
    let j = i - 1; while (j >= 0 && sel.has(j)) j--;
    let k = i + 1; while (k < pts.length && sel.has(k)) k++;
    const minT = j >= 0 ? pts[j].t : 0, maxT = k < pts.length ? pts[k].t : Infinity;
    lo = Math.max(lo, minT - pts[i].t); hi = Math.min(hi, maxT - pts[i].t);
  }
  const d = Math.min(hi, Math.max(lo, dt));
  return { dt: d, points: pts.map((q, i) => (sel.has(i) ? { ...q, t: q.t + d, v: Math.min(1, Math.max(0, q.v + dv)) } : { ...q })) };
}

/** The points without `sel` (at least one point stays: an envelope needs a value). */
export function deletePoints(pts: readonly AutoPoint[], sel: ReadonlySet<number>): AutoPoint[] | null {
  const left = pts.filter((_, i) => !sel.has(i));
  if (!left.length || left.length === pts.length) return null;
  // the segment now ending at the first survivor after a gap keeps that point's own shape
  return left.map(q => ({ ...q }));
}

/** A point added at `t` with value `v`, its segment straight; returns the points and its index. */
export function addPoint(pts: readonly AutoPoint[], t: number, v: number): { points: AutoPoint[]; i: number } {
  const q: AutoPoint = { t: Math.max(0, t), v: Math.min(1, Math.max(0, v)) };
  // inserted after any point at the same time, so the new one is the one dragged
  let i = 0; while (i < pts.length && pts[i].t <= q.t) i++;
  const points = [...pts.slice(0, i).map(p => ({ ...p })), q, ...pts.slice(i).map(p => ({ ...p }))];
  // the segment it splits keeps its shape on both halves
  const next = points[i + 1];
  if (next && (next.shape || next.tension)) { if (next.shape) q.shape = next.shape; if (next.tension) q.tension = next.tension; }
  return { points, i };
}

/** Indices of the points inside a box (grid px). */
export function pointsInBox(pts: readonly AutoPoint[], G: EnvGeom, x0: number, y0: number, x1: number, y1: number): number[] {
  const [ax, bx] = x0 < x1 ? [x0, x1] : [x1, x0], [ay, by] = y0 < y1 ? [y0, y1] : [y1, y0];
  const out: number[] = [];
  pts.forEach((q, i) => { const x = G.X(q.t), y = G.Y(q.v); if (x >= ax && x <= bx && y >= ay && y <= by) out.push(i); });
  return out;
}

/** The segments ending at `idx` set to `shape` (tension reset: each shape reads it its own way). */
export function setShape(pts: readonly AutoPoint[], idx: Iterable<number>, shape: CurveShape): AutoPoint[] {
  const s = new Set(idx);
  return pts.map((q, i) => {
    if (!s.has(i) || i === 0) return { ...q };
    const { tension: _t, shape: _s, ...rest } = q;
    return shape === 'curve' ? rest : { ...rest, shape };
  });
}
