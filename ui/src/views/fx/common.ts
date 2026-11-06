import type { Fx } from '../../api/types';
import type { FxFrame } from '../../api/telemetry';
import { clamp } from '../../lib/format';
import { fx2x } from '../../lib/eq';
import { rgba, T } from '../../ui/theme';

export const MONO9 = '500 9px "Geist Mono Variable", "Geist Mono", monospace';
export const MONO10 = '10px "Geist Mono Variable", "Geist Mono", monospace';
export const MONO11 = '500 11px "Geist Mono Variable", "Geist Mono", monospace';
export const SANS10 = '600 10px "Geist Variable", Geist, sans-serif';
export const SANS11 = '500 11px "Geist Variable", Geist, sans-serif';
/** 24 distinguishable band colours (cycle of hues, two lightness rows). */
export const BAND_COLORS = ['#e05263', '#e0a44a', '#f0c25a', '#5fbf8a', '#47b8a8', '#5aa9e6', '#9a8cf0', '#d9678f',
  '#ef7d57', '#a5d65f', '#4fc3d9', '#7c8cf8', '#c77ddb', '#f29bb0', '#c9a86a', '#6fd0a8',
  '#ff8a7a', '#ffd27a', '#8ad5ff', '#b3a6ff', '#e6a3e0', '#9ccf8f', '#ffb26b', '#7fb2ff'];
export const bandColor = (i: number) => BAND_COLORS[i % BAND_COLORS.length];
export const BAND_F = Float32Array.from({ length: 128 }, (_, k) => 20 * Math.pow(1000, k / 127));

/** A per-window visualisation. `update` consumes telemetry and says whether a redraw is needed. */
export interface Viz {
  update(fx: Fx, a: FxFrame | null, now: number): boolean;
  /** Every decoded analysis frame of this effect, at decode time (for histories). */
  ingest?(a: FxFrame, at: number): void;
  draw(g: CanvasRenderingContext2D, W: number, H: number, fx: Fx, a: FxFrame | null, extra: VizExtra): void;
  cursor: string;
}
export interface VizExtra { selBand: number | null; fxId: number; now: number; hover?: unknown }

export function freqGrid(g: CanvasRenderingContext2D, W: number, H: number, x0 = 0) {
  g.font = MONO10; g.lineWidth = 1;
  for (const f of [30, 50, 100, 200, 300, 500, 1000, 2000, 3000, 5000, 10000, 20000]) {
    const x = Math.round(x0 + fx2x(f, W)) + 0.5;
    const major = f === 100 || f === 1000 || f === 10000;
    g.strokeStyle = major ? T.vizGrid2 : T.vizGrid;
    g.beginPath(); g.moveTo(x, 0); g.lineTo(x, H); g.stroke();
    if (f === 20000) continue;
    g.fillStyle = T.vizLabel; g.fillText(f >= 1000 ? f / 1000 + 'k' : String(f), x + 3, H - 5);
  }
}

/** Display tilt of the analyser: 4.5 dB/oct around 1 kHz (pink-ish noise reads flat). */
export const specTilt = (k: number) => 4.5 * Math.log2(BAND_F[k] / 1000);

/** Smooth spectrum path through 128 log bands (quadratic midpoints). `y(db)` maps tilted dB to px. */
export function specCurve(g: CanvasRenderingContext2D, spec: ArrayLike<number>, W: number, y: (db: number) => number, H: number, x0 = 0) {
  const n = spec.length;
  const px = (k: number) => x0 + k / (n - 1) * W;
  g.beginPath(); g.moveTo(x0, H);
  let pxv = px(0), pyv = y(spec[0] + specTilt(0));
  g.lineTo(pxv, pyv);
  for (let k = 1; k < n; k++) {
    const nx = px(k), ny = y(spec[k] + specTilt(k));
    g.quadraticCurveTo(pxv, pyv, (pxv + nx) / 2, (pyv + ny) / 2);
    pxv = nx; pyv = ny;
  }
  g.lineTo(pxv, pyv); g.lineTo(x0 + W, H); g.closePath();
}

/** Old API kept for simple views: spectrum on a −96..0 dB scale filling H. */
export function specPath(g: CanvasRenderingContext2D, spec: Float32Array, W: number, H: number, fill: string | null, stroke: string | null) {
  specCurve(g, spec, W, db => H - clamp((db + 96) / 96, 0, 1) * H, H);
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
  /** Seq of the frame seen last (−1 = none). */
  get last() { return this.seq; }
}

/**
 * Time-stamped history for scrolling displays. Every analysis frame is pushed when it is decoded
 * (bursts included). Sample times come from the engine's frame counter (`seq`) through a
 * recovered clock — milliseconds per seq, learnt slowly and kept locked to real arrival time — so
 * frames that arrive in bursts or late still land at their true, evenly spaced times. The display
 * runs `lag` behind real time, where `lag` covers the largest recent gap between arrivals, so the
 * newest point is always interpolated between two samples and the graph glides at the refresh
 * rate instead of stepping, stalling or compressing.
 */
export class TimeRing {
  readonly t: Float64Array;
  readonly ch: Float32Array[];
  head = 0; n = 0;
  private lastArr = 0; private lastSeq = -1; private pushes = 0;
  /** ms per seq step */
  private k = 1000 / 60;
  /** recent largest gap between arrivals (ms), decays slowly */
  private gapEnv = 1000 / 60;
  constructor(readonly cap: number, channels: number) {
    this.t = new Float64Array(cap);
    this.ch = Array.from({ length: channels }, () => new Float32Array(cap));
  }
  /** Index of the i-th oldest sample. */
  at(i: number) { return (this.head - this.n + i + this.cap) % this.cap; }
  get lastT() { return this.n ? this.t[this.at(this.n - 1)] : 0; }
  /** Add a sample that arrived at `arrival` (performance.now ms); `seq` = engine frame counter if known. */
  push(arrival: number, vals: ArrayLike<number>, seq?: number) {
    const prev = this.lastT, gap = arrival - this.lastArr;
    let t: number;
    if (!this.n || gap > 400 || gap < -1) t = Math.max(arrival, prev + 1);
    else {
      const ds = seq != null && this.lastSeq >= 0 && seq > this.lastSeq ? seq - this.lastSeq : 1;
      // ms per seq: converges fast at first, then averages over ~30 frames (bursts average out)
      this.pushes++;
      this.k = Math.max(2, this.k + (clamp(gap / ds, 0, 100) - this.k) * Math.max(0.03, 1 / this.pushes));
      t = prev + ds * this.k;
      t += (arrival - this.gapEnv * 0.5 - t) * 0.05; // locked to real time (centred in the arrival jitter)
      const lo = arrival - 2 * this.gapEnv - 2 * this.k, hi = arrival + this.gapEnv;
      t = Math.max(prev + 0.25 * ds * this.k, clamp(t, lo, hi));
    }
    if (this.n && gap >= 0 && gap <= 400) this.gapEnv = Math.max(gap, this.gapEnv + (this.k - this.gapEnv) * 0.01);
    this.lastArr = arrival;
    if (seq != null) this.lastSeq = seq;
    const i = this.head;
    this.t[i] = t;
    for (let c = 0; c < this.ch.length; c++) this.ch[c][i] = vals[c] ?? 0;
    this.head = (this.head + 1) % this.cap; this.n = Math.min(this.cap, this.n + 1);
  }
  private lag = 30; private lastNow = 0;
  /**
   * Display clock (ms): real time minus the interpolation lag, never far past the newest sample.
   * The lag follows the arrival gaps but changes slowly (the clock keeps ≥ 80 % speed while it
   * grows), so a sudden late burst never freezes or rewinds the scroll.
   */
  clock(now: number) {
    const want = this.gapEnv * 1.5 + this.k * 0.5 + 4;
    const dt = this.lastNow ? clamp(now - this.lastNow, 0, 100) : 0;
    this.lastNow = now;
    this.lag = want > this.lag ? Math.min(want, this.lag + 0.2 * dt) : Math.max(want, this.lag - 0.05 * dt);
    if (!dt) this.lag = Math.max(this.lag, want);
    return Math.min(now - this.lag, this.lastT + this.gapEnv * 2 + this.k * 2);
  }
  /** Interpolated value of channel `c` at display time `tt` (or NaN when outside). */
  valueAt(c: number, tt: number): number {
    for (let i = this.n - 1; i > 0; i--) {
      const b = this.at(i), a = this.at(i - 1);
      if (this.t[a] <= tt) {
        if (this.t[b] < tt) return this.ch[c][b];
        const u = (tt - this.t[a]) / Math.max(1e-6, this.t[b] - this.t[a]);
        return this.ch[c][a] + (this.ch[c][b] - this.ch[c][a]) * u;
      }
    }
    return this.n ? this.ch[c][this.at(0)] : NaN;
  }
  clear() { this.n = 0; }
}

/**
 * Analyser display: instant attack (lightly smoothed), time-based falloff in dB/s, peak hold
 * that sits for `holdS` then falls. Advanced every animation frame, so motion is smooth even
 * when analysis frames arrive at 30 Hz.
 */
export class SpecSmoother {
  readonly cur: Float32Array; readonly peak: Float32Array; private held: Float32Array; private lastNow = 0; private target: Float32Array;
  constructor(n = 128, private fall = 36, private holdS = 1.2, private peakFall = 18) {
    this.cur = new Float32Array(n).fill(-140); this.peak = new Float32Array(n).fill(-140);
    this.held = new Float32Array(n); this.target = new Float32Array(n).fill(-140);
  }
  /** A new analysis frame. */
  feed(spec: ArrayLike<number>) { for (let k = 0; k < this.target.length; k++) this.target[k] = spec[k]; }
  /** Silence: let everything fall. */
  silence() { this.target.fill(-140); }
  /** Advance to `now` (ms). Returns true while anything is still moving or visible. */
  step(now: number): boolean {
    const dt = this.lastNow ? clamp((now - this.lastNow) / 1000, 0, 0.1) : 0.016; this.lastNow = now;
    let live = false;
    const rise = 1 - Math.exp(-dt / 0.012);
    for (let k = 0; k < this.cur.length; k++) {
      const tg = this.target[k];
      let v = this.cur[k];
      v = tg > v ? v + (tg - v) * rise : Math.max(tg, v - this.fall * dt);
      this.cur[k] = v;
      if (v >= this.peak[k]) { this.peak[k] = v; this.held[k] = this.holdS; }
      else if (this.held[k] > 0) this.held[k] -= dt;
      else this.peak[k] = Math.max(v, this.peak[k] - this.peakFall * dt);
      if (v > -110 || this.peak[k] > -110) live = true;
    }
    return live;
  }
}

/** Ballistic pointer: a damped mass–spring stepped in real time (VU-style needles, readouts). */
export class Needle {
  v = 0; vel = 0; private last = 0;
  constructor(private omega = 17, private zeta = 0.78) {}
  /** Advance toward `target` at time `now` (ms); returns true while moving. */
  step(target: number, now: number): boolean {
    let dt = this.last ? (now - this.last) / 1000 : 0.016; this.last = now;
    dt = clamp(dt, 0, 0.1);
    const w = this.omega, z = this.zeta;
    for (let s = 0; s < 8 && dt > 1e-6; s++) {
      const h = Math.min(dt, 0.004); dt -= h;
      const acc = w * w * (target - this.v) - 2 * z * w * this.vel;
      this.vel += acc * h; this.v += this.vel * h;
    }
    if (dt > 0) { this.v = target; this.vel = 0; }
    return Math.abs(target - this.v) > 1e-3 || Math.abs(this.vel) > 1e-3;
  }
}

/** Rounded-rect helper. */
export function rr(g: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, r: number) {
  g.beginPath(); g.roundRect(x, y, w, h, r);
}
export { rgba, T };
