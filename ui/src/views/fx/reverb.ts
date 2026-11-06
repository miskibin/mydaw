// Reverb 3D decay waterfall. X = frequency (log), Z = time after the hit (front = now), Y = level.
// Amber ridges = what the settings do (model: RT60, pre-delay, HF damping, low cut) — projected once
// per (params, view, size). Teal ridges = the live wet spectrum (`specOut`), one row per time step.
import type { Fx } from '../../api/types';
import type { FxFrame } from '../../api/telemetry';
import { clamp } from '../../lib/format';
import { Layer } from '../../ui/frameLoop';
import { MONO10, MONO11, type Viz } from './common';

const NS = 30, K = 72;
const FK = Float32Array.from({ length: K }, (_, k) => 20 * Math.pow(1000, k / (K - 1)));
const XK = Float32Array.from({ length: K }, (_, k) => k / (K - 1) * 2 - 1);
/** For each of the 72 bins, the range of the 128 telemetry bands it covers. */
const KB = Array.from({ length: K }, (_, k) => {
  const idx = (f: number) => Math.log(f / 20) / Math.log(1000) * 127;
  const f1 = k < K - 1 ? FK[k + 1] : 20000;
  let a = Math.ceil(idx(FK[k])), b = Math.floor(idx(f1));
  if (b < a) a = b = clamp(Math.round(idx(FK[k])), 0, 127);
  return [clamp(a, 0, 127), clamp(b, 0, 127)];
});

export interface View3D { yaw: number; pitch: number }
export const views3d = new Map<number, View3D>();
export const defaultView = (): View3D => ({ yaw: -0.5, pitch: 0.55 });

export class ReverbViz implements Viz {
  cursor = 'grab';
  private floor = new Layer();
  private frames: Float32Array[] = [];
  private last = 0;
  private zeroRows = NS;
  private modelKey = '';
  private model: { pts: Float32Array; n: number }[] = [];
  private proj: ((x: number, y: number, z: number) => [number, number]) | null = null;
  private projKey = '';
  /** Bumped by the window when the orbit changes. */
  viewVersion = 0;
  private drawnView = -1;

  constructor(private fxId: number) {
    for (let i = 0; i < NS; i++) this.frames.push(new Float32Array(K));
  }

  private view(): View3D { let v = views3d.get(this.fxId); if (!v) { v = defaultView(); views3d.set(this.fxId, v); } return v; }

  update(fx: Fx, a: FxFrame | null, now: number): boolean {
    const p = fx.params, Tmax = Math.max(0.6, ((p.pre ?? 0) / 1000 + (p.size ?? 2)) * 1.05);
    const t = now / 1000;
    let changed = this.drawnView !== this.viewVersion;
    if (t - this.last >= Tmax / (NS - 1) || t < this.last) {
      this.last = t;
      const row = this.frames.pop()!;
      let any = false;
      if (a && a.outPeak > 1e-6) {
        for (let k = 0; k < K; k++) {
          const [b0, b1] = KB[k];
          let m = -200;
          for (let b = b0; b <= b1; b++) if (a.specOut[b] > m) m = a.specOut[b];
          const v = clamp((m + 4.5 * Math.log2(FK[k] / 1000) + 105) / 75, 0, 1);
          row[k] = v; if (v > 0) any = true;
        }
      } else row.fill(0);
      this.frames.unshift(row);
      this.zeroRows = any ? 0 : this.zeroRows + 1;
      if (this.zeroRows <= NS) changed = true;
    }
    return changed;
  }

  draw(g: CanvasRenderingContext2D, W: number, H: number, fx: Fx) {
    const p = fx.params, v = this.view();
    this.drawnView = this.viewVersion;
    const pre = (p.pre ?? 0) / 1000, size = p.size ?? 2.2, lo = p.lo ?? 120, hi = p.hi ?? 8000;
    const Tmax = Math.max(0.6, (pre + size) * 1.05);
    // camera: yaw/pitch orbit + mild perspective, auto-fit to the canvas
    const pk = v.yaw.toFixed(4) + v.pitch.toFixed(4) + W + 'x' + H;
    if (pk !== this.projKey || !this.proj) {
      this.projKey = pk;
      const cy = Math.cos(v.yaw), sy = Math.sin(v.yaw), cp = Math.cos(v.pitch), sp = Math.sin(v.pitch);
      const raw = (x: number, y: number, z: number): [number, number] => {
        const X = x * 1.25, Y = y * 0.62, Z = (z - 0.5) * 1.5, x1 = X * cy + Z * sy, z1 = -X * sy + Z * cy, y2 = Y * cp + z1 * sp, z2 = -Y * sp + z1 * cp, k = 4 / (4 + z2);
        return [x1 * k, -y2 * k];
      };
      let mnx = 1e9, mxx = -1e9, mny = 1e9, mxy = -1e9;
      for (const x of [-1, 1]) for (const y of [0, 1]) for (const z of [0, 1]) { const q = raw(x, y, z); mnx = Math.min(mnx, q[0]); mxx = Math.max(mxx, q[0]); mny = Math.min(mny, q[1]); mxy = Math.max(mxy, q[1]); }
      const sc = Math.min((W - 70) / (mxx - mnx), (H - 46) / (mxy - mny)), ox = W / 2 - (mnx + mxx) / 2 * sc, oy = 24 + (H - 46) / 2 - (mny + mxy) / 2 * sc;
      this.proj = (x, y, z) => { const q = raw(x, y, z); return [ox + q[0] * sc, oy + q[1] * sc]; };
      this.modelKey = '';
    }
    const P = this.proj!;
    // model ridges, projected once per params/view
    const mk = pk + '|' + pre + '|' + size + '|' + lo + '|' + hi;
    if (mk !== this.modelKey) {
      this.modelKey = mk;
      const model = (hz: number, t: number) => {
        if (t < pre) return -1;
        const tt = t - pre, fc = Math.max(200, hi * Math.pow(0.3, tt / size)), r = (hz / lo) * (hz / lo);
        return clamp((-60 * tt / size - 10 * Math.log10(1 + (hz / fc) * (hz / fc)) + 20 * Math.log10(r / Math.sqrt(1 + r * r)) + 60) / 60, 0, 1);
      };
      this.model = [];
      for (let i = 0; i < NS; i++) {
        const z = i / (NS - 1), t = z * Tmax, pts = new Float32Array(K * 2);
        let n = 0;
        for (let k = 0; k < K; k++) { const y = model(FK[k], t); if (y < 0) break; const q = P(XK[k], y, z); pts[2 * n] = q[0]; pts[2 * n + 1] = q[1]; n++; }
        this.model.push({ pts, n });
      }
    }
    // static floor (grid, labels, pre-delay gate)
    g.fillStyle = '#0c0e12'; g.fillRect(0, 0, W, H);
    g.drawImage(this.floor.get(W, H, mk + fx.bypass, c => drawFloor(c, P, Tmax, pre)) as CanvasImageSource, 0, 0, W, H);
    g.globalAlpha = fx.bypass ? 0.45 : 1;
    g.lineWidth = 1;
    for (let i = NS - 1; i >= 0; i--) {
      const z = i / (NS - 1), fr = 1 - z, row = this.frames[i];
      // live ridge (filled, occludes what is behind)
      g.beginPath(); let q = P(-1, 0, z); g.moveTo(q[0], q[1]);
      for (let k = 0; k < K; k++) { q = P(XK[k], row[k], z); g.lineTo(q[0], q[1]); }
      q = P(1, 0, z); g.lineTo(q[0], q[1]); g.closePath();
      g.fillStyle = 'rgba(12,14,18,.9)'; g.fill(); g.fillStyle = 'rgba(71,184,168,' + (0.03 + 0.12 * fr) + ')'; g.fill();
      g.strokeStyle = 'rgba(105,195,214,' + (0.18 + 0.75 * fr) + ')'; g.lineWidth = i === 0 ? 1.6 : 1; g.stroke();
      // model ridge
      const m = this.model[i];
      if (m && m.n > 0) {
        g.beginPath(); g.moveTo(m.pts[0], m.pts[1]);
        for (let k = 1; k < m.n; k++) g.lineTo(m.pts[2 * k], m.pts[2 * k + 1]);
        g.strokeStyle = 'rgba(224,164,74,' + (0.16 + 0.55 * fr) + ')'; g.lineWidth = 1; g.stroke();
      }
    }
    g.globalAlpha = 1;
    g.font = MONO11; g.fillStyle = '#f2f4f7'; g.textAlign = 'left'; g.fillText('RT60 ' + size.toFixed(2) + ' s', 10, 16);
    g.fillStyle = '#7d8592'; g.fillText('PRE ' + Math.round(p.pre ?? 0) + ' ms', 110, 16);
    g.font = MONO10; g.textAlign = 'right';
    g.fillStyle = '#e0a44a'; g.fillText('— ustawienia', W - 112, 16); g.fillStyle = '#69c3d6'; g.fillText('— sygnał na żywo', W - 8, 16);
    g.textAlign = 'left';
  }
}

function drawFloor(g: CanvasRenderingContext2D, P: (x: number, y: number, z: number) => [number, number], Tmax: number, pre: number) {
  const seg = (a: [number, number], b: [number, number], col: string) => { g.strokeStyle = col; g.beginPath(); g.moveTo(a[0], a[1]); g.lineTo(b[0], b[1]); g.stroke(); };
  g.lineWidth = 1; g.font = MONO10;
  const fl = [P(-1, 0, 0), P(1, 0, 0), P(1, 0, 1), P(-1, 0, 1)];
  g.beginPath(); fl.forEach((q, i) => i ? g.lineTo(q[0], q[1]) : g.moveTo(q[0], q[1])); g.closePath(); g.fillStyle = '#10141a'; g.fill();
  [0.5, 1].forEach(y => seg(P(-1, y, 1), P(1, y, 1), '#161b22'));
  seg(P(-1, 0, 1), P(-1, 1, 1), '#1c222a'); seg(P(1, 0, 1), P(1, 1, 1), '#1c222a');
  const xOf = (hz: number) => Math.log(hz / 20) / Math.log(1000) * 2 - 1;
  [50, 100, 200, 500, 1000, 2000, 5000, 10000].forEach(hz => seg(P(xOf(hz), 0, 0), P(xOf(hz), 0, 1), hz === 100 || hz === 1000 || hz === 10000 ? '#232a33' : '#181d24'));
  const step = Tmax > 4 ? 1 : Tmax > 1.6 ? 0.5 : 0.25;
  g.fillStyle = '#4a5360'; g.textAlign = 'left';
  for (let t = 0; t <= Tmax + 1e-6; t += step) {
    const z = t / Tmax; seg(P(-1, 0, z), P(1, 0, z), '#181d24');
    const q = P(1.05, 0, z); g.fillText((step < 1 ? t.toFixed(t % 1 ? 2 : 0) : t) + ' s', q[0] + 2, q[1] + 3);
  }
  g.textAlign = 'center';
  ([[100, '100'], [1000, '1k'], [10000, '10k']] as [number, string][]).forEach(([hz, l]) => { const q = P(xOf(hz), 0, -0.07); g.fillText(l, q[0], q[1] + 8); });
  g.textAlign = 'left';
  if (pre > 0.002) {
    const z = pre / Tmax, a = [P(-1, 0, 0), P(1, 0, 0), P(1, 0, z), P(-1, 0, z)];
    g.beginPath(); a.forEach((q, i) => i ? g.lineTo(q[0], q[1]) : g.moveTo(q[0], q[1])); g.closePath(); g.fillStyle = 'rgba(224,164,74,.07)'; g.fill();
  }
}
