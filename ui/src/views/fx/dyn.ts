// Compressor / limiter: transfer curve (static layer) + input dot, level & GR history.
import type { Fx } from '../../api/types';
import type { FxFrame } from '../../api/telemetry';
import { clamp } from '../../lib/format';
import { Layer } from '../../ui/frameLoop';
import { MONO11, rgba, T, TeleGate, type Viz } from './common';

const HN = 300;

export class DynViz implements Viz {
  cursor = 'default';
  private layer = new Layer();
  private gate = new TeleGate();
  private hin = new Float32Array(HN).fill(-90);
  private hgr = new Float32Array(HN);
  private n = 0; private head = 0;
  private inDb = -90; private gr = 0;

  update(fx: Fx, a: FxFrame | null): boolean {
    if (!this.gate.fresh(a, HN)) return false;
    const lim = fx.type === 'limit';
    this.inDb = a!.inPeak > 1e-5 ? 20 * Math.log10(a!.inPeak) : -90;
    this.gr = a!.grDb;
    this.hin[this.head] = this.inDb + (lim ? fx.params.gain ?? 0 : 0);
    this.hgr[this.head] = this.gr;
    this.head = (this.head + 1) % HN; this.n = Math.min(HN, this.n + 1);
    return true;
  }

  draw(g: CanvasRenderingContext2D, W: number, H: number, fx: Fx) {
    const p = fx.params, lim = fx.type === 'limit';
    const thr = lim ? p.ceil : p.thr, S = Math.min(H, W * 0.36), u = (d: number) => clamp((d + 60) / 60, 0, 1) * S;
    const tf = (d: number) => lim ? Math.min(d + p.gain, thr) : (d < thr ? d : thr + (d - thr) / p.ratio) + p.gain;
    const key = fx.type + JSON.stringify(p);
    g.fillStyle = T.viz; g.fillRect(0, 0, W, H);
    g.drawImage(this.layer.get(W, H, key, c => {
      c.lineWidth = 1; c.strokeStyle = T.vizGrid;
      for (let d = -48; d < 0; d += 12) { const q = u(d); c.beginPath(); c.moveTo(q, 0); c.lineTo(q, S); c.moveTo(0, S - q); c.lineTo(S, S - q); c.stroke(); }
      c.strokeStyle = T.vizAxis; c.strokeRect(0.5, 0.5, S - 1, S - 1);
      c.strokeStyle = T.vizGrid2; c.beginPath(); c.moveTo(0, S); c.lineTo(S, 0); c.stroke();
      c.beginPath(); for (let i = 0; i <= 120; i++) { const d = -60 + i / 2, x = u(d), y = S - u(tf(d)); i ? c.lineTo(x, y) : c.moveTo(x, y); }
      c.strokeStyle = T.ink; c.lineWidth = 2; c.stroke();
      c.setLineDash([3, 3]); c.strokeStyle = T.amber; c.lineWidth = 1;
      const tx = u(lim ? thr - p.gain : thr);
      c.beginPath(); c.moveTo(tx, 0); c.lineTo(tx, S); c.stroke();
      const x0 = S + 14, HW = W - x0 - 26, ty = H - clamp((thr + 60) / 60, 0, 1) * H;
      c.beginPath(); c.moveTo(x0, ty); c.lineTo(x0 + HW, ty); c.stroke(); c.setLineDash([]);
    }) as CanvasImageSource, 0, 0, W, H);
    if (fx.bypass) g.globalAlpha = 0.5;
    if (this.inDb > -60) { g.beginPath(); g.arc(u(this.inDb), S - u(tf(this.inDb)), 5, 0, Math.PI * 2); g.fillStyle = T.blue; g.fill(); }
    const x0 = S + 14, HW = W - x0 - 26, L = this.n;
    if (L > 1) {
      const at = (i: number) => (this.head - L + i + HN) % HN;
      g.beginPath(); g.moveTo(x0, H);
      for (let i = 0; i < L; i++) g.lineTo(x0 + i / (HN - 1) * HW, H - clamp((this.hin[at(i)] + 60) / 60, 0, 1) * H);
      g.lineTo(x0 + (L - 1) / (HN - 1) * HW, H); g.closePath();
      g.fillStyle = rgba(T.blue, 0.22); g.fill(); g.strokeStyle = rgba(T.post, 0.6); g.lineWidth = 1; g.stroke();
      g.beginPath(); g.moveTo(x0, 0);
      for (let i = 0; i < L; i++) g.lineTo(x0 + i / (HN - 1) * HW, clamp(-this.hgr[at(i)] / 24, 0, 1) * H);
      g.lineTo(x0 + (L - 1) / (HN - 1) * HW, 0); g.closePath();
      g.fillStyle = rgba(T.red, 0.4); g.fill();
    }
    const gx = W - 16, gr = this.gr;
    g.fillStyle = T.vizPanel; g.fillRect(gx, 0, 12, H);
    g.fillStyle = T.red; g.fillRect(gx, 0, 12, clamp(-gr / 24, 0, 1) * H);
    g.globalAlpha = 1;
    g.font = MONO11; g.fillStyle = T.ink; g.fillText('GR ' + (gr < -0.05 ? gr.toFixed(1) : '0.0') + ' dB', x0 + 6, 16);
    g.fillStyle = T.fg4; g.fillText('IN ' + (this.inDb > -90 ? this.inDb.toFixed(1) : '-∞') + ' dB', x0 + 6, 30);
  }
}
