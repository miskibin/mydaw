import type { EqBand, Fx } from '../../api/types';
import type { FxFrame } from '../../api/telemetry';
import { bandCurve, eqCurve, x2f, fx2x } from '../../lib/eq';
import { clamp } from '../../lib/format';
import { Layer } from '../../ui/frameLoop';
import { BAND_COLORS, freqGrid, MONO10, specPath, TeleGate, type Viz, type VizExtra } from './common';

export const bandY = (b: EqBand, H: number) => b.type === 'lowcut' || b.type === 'highcut' ? H / 2 : H / 2 - b.g / 18 * (H / 2) * 0.9;
export const dbY = (db: number, H: number) => clamp(H / 2 - db / 18 * (H / 2) * 0.9, -2, H + 2);

export class EqViz implements Viz {
  cursor = 'pointer';
  private bg = new Layer();
  private fg = new Layer();
  private gate = new TeleGate();
  private specIn = new Float32Array(128).fill(-120);
  private specOut = new Float32Array(128).fill(-120);

  update(_fx: Fx, a: FxFrame | null): boolean {
    if (!this.gate.fresh(a, 3)) return false;
    this.specIn.set(a!.specIn); this.specOut.set(a!.specOut);
    return true;
  }

  draw(g: CanvasRenderingContext2D, W: number, H: number, fx: Fx, _a: FxFrame | null, ex: VizExtra) {
    g.fillStyle = '#0c0e12'; g.fillRect(0, 0, W, H);
    g.drawImage(this.bg.get(W, H, 'bg', drawBg) as CanvasImageSource, 0, 0, W, H);
    specPath(g, this.specIn, W, H, null, 'rgba(154,162,173,.28)');
    specPath(g, this.specOut, W, H, 'rgba(79,142,247,.16)', 'rgba(143,182,255,.6)');
    const bands = fx.bands ?? [];
    const key = JSON.stringify(bands) + '|' + (fx.params.out ?? 0) + '|' + fx.bypass + '|' + ex.selBand;
    g.drawImage(this.fg.get(W, H, key, (c, w, h) => drawCurve(c, w, h, bands, fx.params.out ?? 0, fx.bypass, ex.selBand)) as CanvasImageSource, 0, 0, W, H);
  }
}

function drawBg(g: CanvasRenderingContext2D, W: number, H: number) {
  freqGrid(g, W, H);
  g.font = MONO10;
  for (const db of [-12, -6, 6, 12]) {
    const y = Math.round(dbY(db, H)) + 0.5;
    g.strokeStyle = '#161b22'; g.beginPath(); g.moveTo(0, y); g.lineTo(W, y); g.stroke();
    g.fillStyle = '#4a5360'; g.fillText((db > 0 ? '+' : '') + db, 4, y - 3);
  }
  g.strokeStyle = '#2a313b'; g.beginPath(); g.moveTo(0, Math.round(H / 2) + 0.5); g.lineTo(W, Math.round(H / 2) + 0.5); g.stroke();
}

function drawCurve(g: CanvasRenderingContext2D, W: number, H: number, bands: EqBand[], out: number, bypass: boolean, sel: number | null) {
  const n = Math.ceil(W / 2) + 1, fr = new Float32Array(n);
  for (let i = 0; i < n; i++) fr[i] = x2f(i * 2, W);
  if (sel != null && bands[sel]?.on) {
    const one = bandCurve(bands[sel], fr, new Float32Array(n));
    g.beginPath(); g.moveTo(0, H / 2);
    for (let k = 0; k < n; k++) g.lineTo(k * 2, dbY(one[k], H));
    g.lineTo(W, H / 2); g.closePath(); g.fillStyle = BAND_COLORS[sel] + '30'; g.fill();
  }
  const tot = eqCurve(bands, out, fr, new Float32Array(n));
  g.beginPath();
  for (let k = 0; k < n; k++) { const y = dbY(tot[k], H); k ? g.lineTo(k * 2, y) : g.moveTo(0, y); }
  g.strokeStyle = bypass ? '#4a5360' : '#f2f4f7'; g.lineWidth = 2; g.stroke();
  g.textAlign = 'center';
  bands.forEach((b, i) => {
    const x = fx2x(b.f, W), y = bandY(b, H);
    g.beginPath(); g.arc(x, y, i === sel ? 8.5 : 7, 0, Math.PI * 2);
    if (b.on) { g.fillStyle = BAND_COLORS[i]; g.fill(); } else { g.fillStyle = '#0c0e12'; g.fill(); g.strokeStyle = BAND_COLORS[i]; g.lineWidth = 1.5; g.stroke(); }
    if (i === sel) { g.strokeStyle = '#f2f4f7'; g.lineWidth = 1.5; g.stroke(); }
    g.fillStyle = b.on ? '#0b1220' : BAND_COLORS[i]; g.font = '600 9px "Geist Mono Variable", monospace'; g.fillText(String(i + 1), x, y + 3);
  });
  g.textAlign = 'left';
}
