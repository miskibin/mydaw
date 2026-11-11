// Parametric EQ display: analyser (pre grey, post accent, peak hold), the engine-exact response
// per channel (solid = stereo bands, dashed = L/R/M/S paths), per-band fills, dynamic ranges with
// the live gain animated from telemetry, and spectrum-peak picking for "grab a resonance".
import type { EqBand, Fx, Placement } from '../../api/types';
import type { FxFrame } from '../../api/telemetry';
import { bandCurve, design, addDesign, eqCurve, FreqGrid, fx2x, handleDb, handleShown, hasGain, isCut, placeOf, usedPlacements, x2f } from '../../lib/eq';
import { clamp, hzText } from '../../lib/format';
import { hzToMidi, noteName } from '../../lib/pitch';
import { Layer } from '../../ui/frameLoop';
import { BAND_F, bandColor, freqGrid, MONO10, MONO9, rgba, SpecSmoother, specCurve, specTilt, T, TeleGate, type Viz, type VizExtra } from './common';

export const PLACE_COLOR: Record<Placement, string> = { stereo: '#9aa2ad', left: '#5aa9e6', right: '#e05263', mid: '#f0c25a', side: '#9a8cf0' };
export const PLACE_SHORT: Record<Placement, string> = { stereo: 'ST', left: 'L', right: 'R', mid: 'M', side: 'S' };

/** Visible gain range (±dB): 18, or 30 when a band goes beyond. */
export const eqRange = (bands: EqBand[]) => bands.some(b => Math.abs(b.g) > 17.9 || (b.dynamic && Math.abs(b.g + b.dynamic.range) > 17.9)) ? 30 : 18;
export const dbY = (db: number, H: number, range = 18) => clamp(H / 2 - db / range * (H / 2) * 0.86, -4, H + 4);
export const yDb = (y: number, H: number, range = 18) => -(y - H / 2) / ((H / 2) * 0.86) * range;
export const bandY = (b: EqBand, H: number, range = 18) => dbY(handleDb(b), H, range);
/** Analyser scale: tilted dB → y (0 dB near the top, −90 at the bottom). */
const specY = (db: number, H: number) => H * 0.04 + clamp(-db / 90, 0, 1.2) * H * 0.96;

export interface EqHover { x: number; y: number; band: number | null; inside: boolean }
export interface SpecPeak { f: number; x: number; y: number; db: number }

export class EqViz implements Viz {
  cursor = 'crosshair';
  private bg = new Layer();
  private fg = new Layer();
  private gate = new TeleGate();
  private pre = new SpecSmoother(128, 40, 0, 40);
  private post = new SpecSmoother(128, 36, 1.4, 14);
  /** live dynamic gain per band (smoothed toward telemetry meters) */
  private dyn = new Float32Array(32);
  private dynTarget = new Float32Array(32);
  private lastNow = 0;
  private grid: FreqGrid | null = null; private gridW = 0;
  /** static totals per channel, from the last curve-layer render */
  private totals = new Map<string, Float32Array>();
  hover: EqHover | null = null;
  /** bumped by the window when hover changes, so the layer redraws */
  hoverVersion = 0; private drawnHover = -1;
  peak: SpecPeak | null = null;

  update(fx: Fx, a: FxFrame | null, now: number): boolean {
    const fresh = this.gate.fresh(a, 3);
    if (fresh && a) {
      if (a.inPeak > 1e-6 || a.outPeak > 1e-6) { this.pre.feed(a.specIn); this.post.feed(a.specOut); } else { this.pre.silence(); this.post.silence(); }
      for (let k = 0; k < 24; k++) this.dynTarget[k] = a.meters[k] ?? 0;
    } else if (!a) { this.pre.silence(); this.post.silence(); this.dynTarget.fill(0); }
    const dt = this.lastNow ? clamp((now - this.lastNow) / 1000, 0, 0.1) : 0.016; this.lastNow = now;
    const k = 1 - Math.exp(-dt / 0.03);
    let dynMoving = false;
    const bands = fx.bands ?? [];
    for (let i = 0; i < bands.length && i < 24; i++) {
      const tg = bands[i].dynamic && bands[i].on ? this.dynTarget[i] : 0;
      const d = tg - this.dyn[i];
      if (Math.abs(d) > 0.01) { this.dyn[i] += d * k; dynMoving = true; } else this.dyn[i] = tg;
    }
    const a1 = this.pre.step(now), a2 = this.post.step(now);
    const hov = this.drawnHover !== this.hoverVersion;
    return a1 || a2 || dynMoving || hov || fresh;
  }

  private ensureGrid(W: number) {
    if (this.grid && this.gridW === W) return this.grid;
    const n = Math.ceil(W / 2) + 1, fr = new Float32Array(n);
    for (let i = 0; i < n; i++) fr[i] = x2f(Math.min(W, i * 2), W);
    this.grid = new FreqGrid(fr); this.gridW = W;
    return this.grid;
  }

  /** Nearest spectral peak to x (±⅓ oct), from the smoothed post (or pre) analyser. */
  findPeak(x: number, W: number, H: number): SpecPeak | null {
    const n = 128, k0 = clamp(Math.round(x / W * (n - 1)), 0, n - 1);
    const src = this.post.cur;
    let best = -1, bv = -1e9;
    for (let k = Math.max(1, k0 - 5); k <= Math.min(n - 2, k0 + 5); k++) {
      const v = src[k] + specTilt(k);
      if (src[k] >= src[k - 1] && src[k] >= src[k + 1] && v > bv) { bv = v; best = k; }
    }
    if (best < 0 || src[best] < -100) return null;
    // parabolic interpolation between bins
    const a = src[best - 1], b = src[best], c = src[best + 1], den = a - 2 * b + c;
    const off = den < -1e-6 ? clamp(0.5 * (a - c) / den, -0.5, 0.5) : 0;
    const kk = best + off, f = 20 * Math.pow(1000, kk / (n - 1));
    const db = b - 0.25 * (a - c) * off;
    return { f, x: fx2x(f, W), y: specY(db + 4.5 * Math.log2(f / 1000), H), db };
  }

  draw(g: CanvasRenderingContext2D, W: number, H: number, fx: Fx, _a: FxFrame | null, ex: VizExtra) {
    const bands = fx.bands ?? [];
    const range = eqRange(bands);
    this.drawnHover = this.hoverVersion;
    g.fillStyle = T.viz; g.fillRect(0, 0, W, H);
    g.drawImage(this.bg.get(W, H, 'bg' + range, (c, w, h) => drawBg(c, w, h, range)) as CanvasImageSource, 0, 0, W, H);
    // analyser
    const sy = (db: number) => specY(db, H);
    specCurve(g, this.pre.cur, W, sy, H);
    g.fillStyle = rgba(T.pre, 0.07); g.fill(); g.strokeStyle = rgba(T.pre, 0.35); g.lineWidth = 1; g.stroke();
    specCurve(g, this.post.cur, W, sy, H);
    const grad = g.createLinearGradient(0, H * 0.1, 0, H);
    grad.addColorStop(0, rgba(T.blue, 0.26 * T.tint)); grad.addColorStop(1, rgba(T.blue, 0.02));
    g.fillStyle = grad; g.fill(); g.strokeStyle = rgba(T.post, 0.8); g.lineWidth = 1; g.stroke();
    // peak hold: thin line only
    g.beginPath();
    for (let k = 0; k < 128; k++) { const x = k / 127 * W, y = sy(this.post.peak[k] + specTilt(k)); k ? g.lineTo(x, y) : g.moveTo(x, y); }
    g.strokeStyle = rgba(T.post, 0.28); g.stroke();

    const grid = this.ensureGrid(W);
    const hb = this.hover?.band ?? null;
    const key = JSON.stringify(bands) + '|' + (fx.params.out ?? 0) + '|' + fx.bypass + '|' + ex.selBand + '|' + hb + '|' + range;
    g.drawImage(this.fg.get(W, H, key, (c, w, h) => this.drawCurves(c, w, h, bands, fx.params.out ?? 0, fx.bypass, ex.selBand, hb, grid, range)) as CanvasImageSource, 0, 0, W, H);

    // dynamic bands: live response + live handle
    const dynIdx = bands.map((b, i) => (b.on && b.dynamic && hasGain(b.type) ? i : -1)).filter(i => i >= 0);
    if (dynIdx.length && !fx.bypass) {
      const byCh = new Map<string, number[]>();
      for (const i of dynIdx) { const ch = placeOf(bands[i]); (byCh.get(ch) ?? byCh.set(ch, []).get(ch)!).push(i); }
      for (const [ch, idx] of byCh) {
        const base = this.totals.get(ch); if (!base) continue;
        const live = Float64Array.from(base);
        let any = false;
        for (const i of idx) {
          if (Math.abs(this.dyn[i]) < 0.02) continue;
          any = true;
          addDesign(design(bands[i], this.dyn[i]), grid, live, 1);
          addDesign(design(bands[i], 0), grid, live, -1);
        }
        if (!any) continue;
        g.beginPath();
        for (let k = 0; k < grid.length; k++) { const y = dbY(live[k], H, range); k ? g.lineTo(Math.min(W, k * 2), y) : g.moveTo(0, y); }
        g.strokeStyle = ch === 'stereo' ? rgba(T.ink, 0.9) : PLACE_COLOR[ch as Placement]; g.lineWidth = 1.5; g.stroke();
      }
      for (const i of dynIdx) {
        const b = bands[i], x = fx2x(b.f, W), y = dbY(handleDb(b) + this.dyn[i] * (b.type === 'tilt' ? 0.5 : 1), H, range);
        g.beginPath(); g.arc(x, y, 4, 0, Math.PI * 2); g.fillStyle = bandColor(i); g.fill();
        g.strokeStyle = T.viz; g.lineWidth = 1.5; g.stroke();
      }
    }

    // spectrum grab: highlight the peak under the cursor
    const h = this.hover;
    this.peak = h && h.inside && h.band == null ? this.findPeak(h.x, W, H) : null;
    if (this.peak) {
      const p = this.peak;
      g.strokeStyle = rgba(T.ink, 0.35); g.setLineDash([2, 3]); g.lineWidth = 1;
      g.beginPath(); g.moveTo(Math.round(p.x) + 0.5, p.y); g.lineTo(Math.round(p.x) + 0.5, H); g.stroke(); g.setLineDash([]);
      g.beginPath(); g.arc(p.x, p.y, 4.5, 0, Math.PI * 2); g.strokeStyle = T.ink; g.lineWidth = 1.5; g.stroke();
      const label = hzText(p.f) + ' · ' + noteName(hzToMidi(p.f));
      g.font = MONO10; const tw = g.measureText(label).width + 10;
      const lx = clamp(p.x - tw / 2, 4, W - tw - 4), ly = Math.max(4, p.y - 26);
      g.fillStyle = rgba(T.bg7, 0.92); g.beginPath(); g.roundRect(lx, ly, tw, 17, 3); g.fill();
      g.strokeStyle = T.line3; g.lineWidth = 1; g.stroke();
      g.fillStyle = T.fgHi; g.textAlign = 'left'; g.fillText(label, lx + 5, ly + 12);
    }
  }

  private drawCurves(g: CanvasRenderingContext2D, W: number, H: number, bands: EqBand[], out: number, bypass: boolean, sel: number | null, hov: number | null, grid: FreqGrid, range: number) {
    const n = grid.length, X = (k: number) => Math.min(W, k * 2), zero = dbY(0, H, range);
    const tmp = new Float32Array(n);
    this.totals.clear();
    const chans: Placement[] = ['stereo', ...usedPlacements(bands)];
    for (const ch of chans) this.totals.set(ch, eqCurve(bands, out, grid, new Float32Array(n), ch));
    // dynamic range regions (where the gain can move)
    bands.forEach((b, i) => {
      if (!b.on || !b.dynamic || !hasGain(b.type)) return;
      const tot = this.totals.get(placeOf(b))!;
      const a0 = new Float32Array(n), a1 = new Float32Array(n);
      addDesign(design(b, 0), grid, a0); addDesign(design(b, b.dynamic.range), grid, a1);
      g.beginPath();
      for (let k = 0; k < n; k++) { const y = dbY(tot[k], H, range); k ? g.lineTo(X(k), y) : g.moveTo(0, y); }
      for (let k = n - 1; k >= 0; k--) g.lineTo(X(k), dbY(tot[k] + a1[k] - a0[k], H, range));
      g.closePath();
      const grad = g.createLinearGradient(0, dbY(b.g, H, range), 0, dbY(b.g + b.dynamic.range, H, range));
      grad.addColorStop(0, rgba(bandColor(i), 0.06)); grad.addColorStop(1, rgba(bandColor(i), 0.3));
      g.fillStyle = grad; g.fill();
      g.setLineDash([1, 3]); g.strokeStyle = rgba(bandColor(i), 0.7); g.lineWidth = 1;
      g.beginPath(); for (let k = 0; k < n; k++) { const y = dbY(tot[k] + a1[k] - a0[k], H, range); k ? g.lineTo(X(k), y) : g.moveTo(0, y); } g.stroke(); g.setLineDash([]);
    });
    // fill of the selected / hovered band
    for (const i of [sel, hov]) {
      if (i == null || !bands[i]?.on) continue;
      const one = bandCurve(bands[i], grid, tmp);
      g.beginPath(); g.moveTo(0, zero);
      for (let k = 0; k < n; k++) g.lineTo(X(k), dbY(one[k], H, range));
      g.lineTo(W, zero); g.closePath();
      g.fillStyle = rgba(bandColor(i), i === sel ? 0.22 : 0.14); g.fill();
      g.strokeStyle = rgba(bandColor(i), 0.65); g.lineWidth = 1; g.stroke();
    }
    // L / R / M / S paths: dashed in their colour
    for (const ch of chans) {
      if (ch === 'stereo') continue;
      const c = this.totals.get(ch)!;
      g.beginPath(); for (let k = 0; k < n; k++) { const y = dbY(c[k], H, range); k ? g.lineTo(X(k), y) : g.moveTo(0, y); }
      g.setLineDash([5, 4]); g.strokeStyle = bypass ? T.vizLabel : PLACE_COLOR[ch]; g.lineWidth = 1.5; g.stroke(); g.setLineDash([]);
      g.font = MONO9; g.fillStyle = PLACE_COLOR[ch]; g.textAlign = 'right';
      g.fillText(ch === 'mid' ? 'MID' : ch === 'side' ? 'SIDE' : ch === 'left' ? 'LEFT' : 'RIGHT', W - 6, dbY(c[n - 1], H, range) - 5);
    }
    // the stereo (main) curve, with a soft glow
    const tot = this.totals.get('stereo')!;
    g.beginPath(); for (let k = 0; k < n; k++) { const y = dbY(tot[k], H, range); k ? g.lineTo(X(k), y) : g.moveTo(0, y); }
    g.lineJoin = 'round';
    if (!bypass) { g.strokeStyle = rgba(T.ink, 0.12); g.lineWidth = 6; g.stroke(); }
    g.strokeStyle = bypass ? T.vizLabel : T.ink; g.lineWidth = 2; g.stroke();
    // handles
    g.textAlign = 'center'; g.textBaseline = 'middle';
    bands.forEach((b, i) => {
      if (!handleShown(b, i, sel)) return;
      const x = fx2x(b.f, W), y = dbY(handleDb(b), H, range), col = bandColor(i), r = i === sel ? 8.5 : 7.5;
      if (b.dynamic && b.on && hasGain(b.type)) {
        const y2 = dbY(handleDb(b) + b.dynamic.range * (b.type === 'tilt' ? 0.5 : 1), H, range);
        g.strokeStyle = rgba(col, 0.8); g.lineWidth = 1.5; g.beginPath(); g.moveTo(x, y); g.lineTo(x, y2); g.stroke();
        g.beginPath(); g.moveTo(x - 5, y2); g.lineTo(x + 5, y2); g.stroke();
      }
      g.beginPath(); g.arc(x, y, r + 2, 0, Math.PI * 2); g.fillStyle = rgba(T.viz, 0.7); g.fill();
      g.beginPath(); g.arc(x, y, r, 0, Math.PI * 2);
      if (b.on) { g.fillStyle = col; g.fill(); } else { g.fillStyle = T.viz; g.fill(); g.strokeStyle = col; g.lineWidth = 1.5; g.stroke(); }
      if (i === sel) { g.strokeStyle = T.ink; g.lineWidth = 1.5; g.beginPath(); g.arc(x, y, r + 2.5, 0, Math.PI * 2); g.stroke(); }
      g.fillStyle = b.on ? T.onBand : col; g.font = '600 9px "Geist Mono Variable", monospace'; g.fillText(String(i + 1), x, y + 0.5);
      const pl = placeOf(b);
      if (pl !== 'stereo' || b.dynamic) {
        const tag = (pl !== 'stereo' ? PLACE_SHORT[pl] : '') + (b.dynamic ? 'D' : '');
        g.font = '700 8px "Geist Mono Variable", monospace';
        const tw = g.measureText(tag).width + 5, bx = x + r - 1, by = y - r - 6;
        g.fillStyle = pl !== 'stereo' ? PLACE_COLOR[pl] : col; g.beginPath(); g.roundRect(bx, by, tw, 10, 2.5); g.fill();
        g.fillStyle = T.onBand; g.fillText(tag, bx + tw / 2, by + 5.5);
      }
      if (isCut(b.type) || b.type === 'notch' || b.type === 'bandpass') { /* frequency-only handle */ }
    });
    g.textAlign = 'left'; g.textBaseline = 'alphabetic';
  }
}

function drawBg(g: CanvasRenderingContext2D, W: number, H: number, range: number) {
  freqGrid(g, W, H);
  g.font = MONO10;
  const step = range > 18 ? 10 : 6;
  for (let db = -range + (range % step); db <= range; db += step) {
    if (db === 0) continue;
    const y = Math.round(dbY(db, H, range)) + 0.5;
    g.strokeStyle = T.vizGrid; g.beginPath(); g.moveTo(0, y); g.lineTo(W, y); g.stroke();
    g.fillStyle = T.vizLabel; g.fillText((db > 0 ? '+' : '') + db, 4, y - 3);
  }
  const z = Math.round(dbY(0, H, range)) + 0.5;
  g.strokeStyle = T.vizAxis; g.beginPath(); g.moveTo(0, z); g.lineTo(W, z); g.stroke();
}

export { BAND_F };
