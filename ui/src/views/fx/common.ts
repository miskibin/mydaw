import type { Fx } from '../../api/types';
import type { FxFrame } from '../../api/telemetry';
import { clamp } from '../../lib/format';
import { fx2x } from '../../lib/eq';

export const MONO10 = '10px "Geist Mono Variable", "Geist Mono", monospace';
export const MONO11 = '500 11px "Geist Mono Variable", "Geist Mono", monospace';
export const BAND_COLORS = ['#e05263', '#e0a44a', '#f0c25a', '#5fbf8a', '#47b8a8', '#5aa9e6', '#9a8cf0'];
export const BAND_F = Float32Array.from({ length: 128 }, (_, k) => 20 * Math.pow(1000, k / 127));

/** A per-window visualisation. `update` consumes telemetry and says whether a redraw is needed. */
export interface Viz {
  update(fx: Fx, a: FxFrame | null, now: number): boolean;
  draw(g: CanvasRenderingContext2D, W: number, H: number, fx: Fx, a: FxFrame | null, extra: VizExtra): void;
  /** Pointer handling on the canvas (optional). */
  cursor: string;
}
export interface VizExtra { selBand: number | null; fxId: number }

export function freqGrid(g: CanvasRenderingContext2D, W: number, H: number) {
  g.font = MONO10; g.lineWidth = 1;
  for (const f of [50, 100, 200, 500, 1000, 2000, 5000, 10000]) {
    const x = Math.round(fx2x(f, W)) + 0.5;
    g.strokeStyle = f === 100 || f === 1000 || f === 10000 ? '#20262f' : '#161b22';
    g.beginPath(); g.moveTo(x, 0); g.lineTo(x, H); g.stroke();
    g.fillStyle = '#4a5360'; g.fillText(f >= 1000 ? f / 1000 + 'k' : String(f), x + 3, H - 5);
  }
}

/** Spectrum (128 log bands, dBFS) with the prototype's 4.5 dB/oct tilt. */
export function specPath(g: CanvasRenderingContext2D, spec: Float32Array, W: number, H: number, fill: string | null, stroke: string | null) {
  const floor = -96;
  g.beginPath(); g.moveTo(0, H);
  for (let k = 0; k < spec.length; k++) {
    const db = spec[k] + 4.5 * Math.log2(BAND_F[k] / 1000);
    g.lineTo(k / (spec.length - 1) * W, H - clamp((db - floor) / -floor, 0, 1) * H);
  }
  g.lineTo(W, H); g.closePath();
  if (fill) { g.fillStyle = fill; g.fill(); }
  if (stroke) { g.strokeStyle = stroke; g.lineWidth = 1; g.stroke(); }
}

export function scope(g: CanvasRenderingContext2D, td: Float32Array, x0: number, y0: number, w: number, h: number, col: string) {
  const n = td.length, half = n >> 1;
  let st = 0;
  for (let i = 1; i < half; i++) if (td[i - 1] < 0 && td[i] >= 0) { st = i; break; }
  const m = n - half;
  g.beginPath();
  for (let i = 0; i < m; i++) {
    const x = x0 + i / (m - 1) * w, y = y0 + h / 2 - clamp(td[st + i] || 0, -1, 1) * h / 2 * 0.9;
    i ? g.lineTo(x, y) : g.moveTo(x, y);
  }
  g.strokeStyle = col; g.lineWidth = 1.2; g.stroke();
}

export const isSilent = (a: FxFrame | null) => !a || (a.inPeak < 1e-5 && a.outPeak < 1e-5);

/** Tracks telemetry seq and silence so idle windows stop redrawing. */
export class TeleGate {
  private seq = -1; private silentRun = 0;
  /** True when there is a new analysis frame that is worth drawing (`keep` = frames to keep drawing after silence). */
  fresh(a: FxFrame | null, keep = 2): boolean {
    if (!a || a.seq === this.seq) return false;
    this.seq = a.seq;
    if (isSilent(a)) { this.silentRun++; return this.silentRun <= keep; }
    this.silentRun = 0;
    return true;
  }
}
