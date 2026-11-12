// Warp map math shared by the editor and tests (mirrors crates/engine/src/dsp/warp.rs).
import type { AutoPoint } from '../api/types';
import { valueAt } from './automation';
import { t as tr } from './i18n';

/** A time map's depth, beats (`v` 0 = this far back), and its width (one bar). */
export const DEPTH = 8, BAR = 4;

/** Playback speed of a time map at `x` beats (1 = normal, 0 = stopped, −1 = reverse). */
export function speedAt(pts: AutoPoint[], x: number): number {
  const e = 1e-3, a = valueAt(pts, Math.max(0, x - e)) ?? 1, b = valueAt(pts, Math.min(BAR, x + e)) ?? 1;
  return 1 + (b - a) * DEPTH / (Math.min(BAR, x + e) - Math.max(0, x - e));
}

/** "×0.50 · −12.0 st", "stop", "reverse ×1.00". */
export function speedText(v: number): string {
  if (Math.abs(v) < 0.02) return tr('c.fx.warp.stop');
  if (v < 0) return tr('c.fx.warp.reverse') + ' ×' + (-v).toFixed(2);
  const st = 12 * Math.log2(v);
  return '×' + v.toFixed(2) + (Math.abs(st) < 0.05 ? '' : ' · ' + (st > 0 ? '+' : '−') + Math.abs(st).toFixed(1) + ' st');
}
