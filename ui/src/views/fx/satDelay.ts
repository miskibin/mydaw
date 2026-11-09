// Saturator (curve + scope) and delay (tap diagram + scope).
import type { Fx } from '../../api/types';
import { SCOPE_LEN, type FxFrame } from '../../api/telemetry';
import { clamp, dbg } from '../../lib/format';
import { satDriveDb } from '../../lib/saturator';
import { Layer } from '../../ui/frameLoop';
import { MONO10, rgba, scope, T, TeleGate, type Viz } from './common';

export class SatViz implements Viz {
  cursor = 'default';
  private layer = new Layer();
  private gate = new TeleGate();
  private sIn = new Float32Array(SCOPE_LEN);
  private sOut = new Float32Array(SCOPE_LEN);
  update(_fx: Fx, a: FxFrame | null) {
    if (!this.gate.fresh(a, 2)) return false;
    this.sIn.set(a!.scopeIn); this.sOut.set(a!.scopeOut); return true;
  }
  draw(g: CanvasRenderingContext2D, W: number, H: number, fx: Fx) {
    const S = Math.min(H, W * 0.36), dr = dbg(Math.min(satDriveDb(fx.params), 40) * 0.6);
    // even-harmonic styles (Taśma, Transformator, Trioda) bend asymmetrically
    const style = Math.round(fx.params.style ?? 0), asym = [0.18, 0.04, 0.12, 0.3, 0.02][style] ?? 0;
    g.fillStyle = T.viz; g.fillRect(0, 0, W, H);
    g.drawImage(this.layer.get(W, H, 'sat' + dr + style + fx.bypass, c => {
      c.lineWidth = 1; c.strokeStyle = T.vizGrid; c.beginPath(); c.moveTo(S / 2, 0); c.lineTo(S / 2, S); c.moveTo(0, S / 2); c.lineTo(S, S / 2); c.stroke();
      c.strokeStyle = T.vizAxis; c.strokeRect(0.5, 0.5, S - 1, S - 1); c.strokeStyle = T.vizGrid2; c.beginPath(); c.moveTo(0, S); c.lineTo(S, 0); c.stroke();
      c.beginPath();
      for (let i = 0; i <= 100; i++) { const x = i / 50 - 1, xb = x * dr + asym * Math.min(1, dr / 4) * (1 - x * x), y = Math.tanh(clamp(xb, -1.2, 1.2) * 2.2) / Math.tanh(2.2), px = i / 100 * S, py = S / 2 - y * S / 2 * 0.95; i ? c.lineTo(px, py) : c.moveTo(px, py); }
      c.strokeStyle = fx.bypass ? T.vizLabel : T.amber; c.lineWidth = 2; c.stroke();
      const x0 = S + 14;
      c.strokeStyle = T.vizGrid; c.lineWidth = 1; c.beginPath(); c.moveTo(x0, H / 2); c.lineTo(W, H / 2); c.stroke();
    }) as CanvasImageSource, 0, 0, W, H);
    const x0 = S + 14, w = W - x0;
    scope(g, this.sIn, x0, 0, w, H, rgba(T.pre, 0.35));
    scope(g, this.sOut, x0, 0, w, H, T.amber);
  }
}

export class DelayViz implements Viz {
  cursor = 'default';
  private layer = new Layer();
  private gate = new TeleGate();
  private sOut = new Float32Array(SCOPE_LEN);
  private lvl = 0;
  update(_fx: Fx, a: FxFrame | null) {
    if (!this.gate.fresh(a, 2)) return false;
    this.sOut.set(a!.scopeOut); this.lvl = a!.inPeak; return true;
  }
  draw(g: CanvasRenderingContext2D, W: number, H: number, fx: Fx) {
    const p = fx.params, maxT = 2000, base = H - 16;
    g.fillStyle = T.viz; g.fillRect(0, 0, W, H);
    g.drawImage(this.layer.get(W, H, 'dly', c => {
      c.font = MONO10; c.lineWidth = 1;
      for (let t = 0; t <= maxT; t += 250) {
        const x = Math.round(t / maxT * W) + 0.5;
        c.strokeStyle = T.vizGrid; c.beginPath(); c.moveTo(x, 0); c.lineTo(x, base); c.stroke();
        if (t % 500 === 0) { c.fillStyle = T.vizLabel; c.fillText(t + ' ms', x + 3, H - 4); }
      }
      c.fillStyle = T.ink; c.fillRect(1, base - base * 0.85, 4, base * 0.85);
    }) as CanvasImageSource, 0, 0, W, H);
    scope(g, this.sOut, 0, 0, W, base * 0.55, rgba(T.post, 0.35));
    const a0 = fx.bypass ? 0.25 : 0.35 + 0.65 * Math.min(1, this.lvl * 3);
    g.fillStyle = rgba(T.blue, +a0.toFixed(2));
    for (let k = 1; k < 24; k++) {
      const t = p.time * k; if (t > maxT) break;
      const a = p.mix / 100 * Math.pow(p.fb / 100, k - 1); if (a < 0.01) break;
      const x = t / maxT * W, bh = a * base * 0.85;
      g.fillRect(x - 2, base - bh, 4, bh);
    }
  }
}
