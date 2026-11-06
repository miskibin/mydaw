// Saturator (curve + scope) and delay (tap diagram + scope).
import type { Fx } from '../../api/types';
import { SCOPE_LEN, type FxFrame } from '../../api/telemetry';
import { clamp, dbg } from '../../lib/format';
import { Layer } from '../../ui/frameLoop';
import { MONO10, scope, TeleGate, type Viz } from './common';

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
    const S = Math.min(H, W * 0.36), dr = dbg(fx.params.drive ?? 0);
    g.fillStyle = '#0c0e12'; g.fillRect(0, 0, W, H);
    g.drawImage(this.layer.get(W, H, 'sat' + dr + fx.bypass, c => {
      c.lineWidth = 1; c.strokeStyle = '#161b22'; c.beginPath(); c.moveTo(S / 2, 0); c.lineTo(S / 2, S); c.moveTo(0, S / 2); c.lineTo(S, S / 2); c.stroke();
      c.strokeStyle = '#2a313b'; c.strokeRect(0.5, 0.5, S - 1, S - 1); c.strokeStyle = '#20262f'; c.beginPath(); c.moveTo(0, S); c.lineTo(S, 0); c.stroke();
      c.beginPath();
      for (let i = 0; i <= 100; i++) { const x = i / 50 - 1, y = Math.tanh(clamp(x * dr, -1, 1) * 2.5) / Math.tanh(2.5), px = i / 100 * S, py = S / 2 - y * S / 2 * 0.95; i ? c.lineTo(px, py) : c.moveTo(px, py); }
      c.strokeStyle = fx.bypass ? '#4a5360' : '#e0a44a'; c.lineWidth = 2; c.stroke();
      const x0 = S + 14;
      c.strokeStyle = '#161b22'; c.lineWidth = 1; c.beginPath(); c.moveTo(x0, H / 2); c.lineTo(W, H / 2); c.stroke();
    }) as CanvasImageSource, 0, 0, W, H);
    const x0 = S + 14, w = W - x0;
    scope(g, this.sIn, x0, 0, w, H, 'rgba(154,162,173,.35)');
    scope(g, this.sOut, x0, 0, w, H, '#e0a44a');
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
    g.fillStyle = '#0c0e12'; g.fillRect(0, 0, W, H);
    g.drawImage(this.layer.get(W, H, 'dly', c => {
      c.font = MONO10; c.lineWidth = 1;
      for (let t = 0; t <= maxT; t += 250) {
        const x = Math.round(t / maxT * W) + 0.5;
        c.strokeStyle = '#161b22'; c.beginPath(); c.moveTo(x, 0); c.lineTo(x, base); c.stroke();
        if (t % 500 === 0) { c.fillStyle = '#4a5360'; c.fillText(t + ' ms', x + 3, H - 4); }
      }
      c.fillStyle = '#f2f4f7'; c.fillRect(1, base - base * 0.85, 4, base * 0.85);
    }) as CanvasImageSource, 0, 0, W, H);
    scope(g, this.sOut, 0, 0, W, base * 0.55, 'rgba(143,182,255,.35)');
    const a0 = fx.bypass ? 0.25 : 0.35 + 0.65 * Math.min(1, this.lvl * 3);
    g.fillStyle = 'rgba(79,142,247,' + a0 + ')';
    for (let k = 1; k < 24; k++) {
      const t = p.time * k; if (t > maxT) break;
      const a = p.mix / 100 * Math.pow(p.fb / 100, k - 1); if (a < 0.01) break;
      const x = t / maxT * W, bh = a * base * 0.85;
      g.fillRect(x - 2, base - bh, 4, bh);
    }
  }
}
