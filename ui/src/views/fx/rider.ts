// Vocal rider: scrolling history of the word level (fill), the phrase level it rides towards
// (amber line) and the gain it applies (green, ±12 dB band at the bottom). Meters: [gain dB,
// word level dB, phrase level dB, confidence 0..1] (crates/engine/src/rider.rs).
import type { Fx } from '../../api/types';
import type { FxFrame } from '../../api/telemetry';
import { clamp } from '../../lib/format';
import { t as tr } from '../../lib/i18n';
import { MONO10, MONO11, rgba, T, TeleGate, type Viz } from './common';

const HN = 360;

export class RiderViz implements Viz {
  cursor = 'default';
  private gate = new TeleGate();
  private word = new Float32Array(HN).fill(-90);
  private phrase = new Float32Array(HN).fill(-90);
  private gain = new Float32Array(HN);
  private conf = new Float32Array(HN);
  private n = 0; private head = 0;

  update(_fx: Fx, a: FxFrame | null): boolean {
    if (!this.gate.fresh(a, HN)) return false;
    const m = a!.meters;
    this.gain[this.head] = m[0] ?? 0;
    this.word[this.head] = m[1] ?? -90;
    this.phrase[this.head] = m[2] ?? -90;
    this.conf[this.head] = m[3] ?? 0;
    this.head = (this.head + 1) % HN; this.n = Math.min(HN, this.n + 1);
    return true;
  }

  draw(g: CanvasRenderingContext2D, W: number, H: number, fx: Fx) {
    g.fillStyle = T.viz; g.fillRect(0, 0, W, H);
    const top = 8, LH = Math.round(H * 0.58), GH = H - LH - 18, gy0 = LH + 12;
    const ly = (d: number) => top + (1 - clamp((d + 60) / 60, 0, 1)) * (LH - top);
    const gy = (d: number) => gy0 + GH / 2 - clamp(d / 12, -1, 1) * GH / 2;
    g.font = MONO10; g.lineWidth = 1;
    for (const d of [-48, -36, -24, -12]) { const y = Math.round(ly(d)) + 0.5; g.strokeStyle = T.vizGrid; g.beginPath(); g.moveTo(0, y); g.lineTo(W, y); g.stroke(); g.fillStyle = T.vizLabel; g.fillText(d + ' dB', 4, y - 3); }
    for (const d of [-6, 0, 6]) { const y = Math.round(gy(d)) + 0.5; g.strokeStyle = d ? T.vizGrid : T.vizGrid2; g.beginPath(); g.moveTo(0, y); g.lineTo(W, y); g.stroke(); g.fillStyle = T.vizLabel; g.fillText((d > 0 ? '+' : '') + d, 4, y - 3); }
    if (fx.bypass) g.globalAlpha = 0.5;
    const L = this.n; if (L < 2) { this.label(g, W); return; }
    const at = (i: number) => (this.head - L + i + HN) % HN, x = (i: number) => (i + HN - L) / (HN - 1) * W;
    // word level: fill, brighter where the rider trusts it is voice
    g.beginPath(); g.moveTo(x(0), LH);
    for (let i = 0; i < L; i++) g.lineTo(x(i), ly(this.word[at(i)]));
    g.lineTo(x(L - 1), LH); g.closePath(); g.fillStyle = rgba(T.blue, 0.22); g.fill();
    g.strokeStyle = rgba(T.post, 0.7); g.beginPath();
    for (let i = 0; i < L; i++) { const v = this.word[at(i)]; i ? g.lineTo(x(i), ly(v)) : g.moveTo(x(i), ly(v)); }
    g.stroke();
    // phrase level (the reference)
    g.strokeStyle = T.amber; g.lineWidth = 1.5; g.beginPath();
    for (let i = 0; i < L; i++) { const v = this.phrase[at(i)]; i ? g.lineTo(x(i), ly(v)) : g.moveTo(x(i), ly(v)); }
    g.stroke();
    // applied gain
    g.beginPath(); g.moveTo(x(0), gy(0));
    for (let i = 0; i < L; i++) g.lineTo(x(i), gy(this.gain[at(i)]));
    g.lineTo(x(L - 1), gy(0)); g.closePath(); g.fillStyle = rgba(T.green, 0.2); g.fill();
    g.strokeStyle = T.green; g.lineWidth = 2; g.beginPath();
    for (let i = 0; i < L; i++) { const v = this.gain[at(i)]; i ? g.lineTo(x(i), gy(v)) : g.moveTo(x(i), gy(v)); }
    g.stroke(); g.lineWidth = 1;
    g.globalAlpha = 1;
    this.label(g, W);
    const cur = this.gain[at(L - 1)];
    g.font = MONO11; g.textAlign = 'right'; g.fillStyle = T.green;
    g.fillText((cur >= 0 ? '+' : '') + cur.toFixed(1) + ' dB', W - 8, gy0 + 12); g.textAlign = 'left';
  }

  private label(g: CanvasRenderingContext2D, W: number) {
    g.font = MONO10; g.textAlign = 'right';
    g.fillStyle = T.post; g.fillText(tr('c.viz.words'), W - 120, 16);
    g.fillStyle = T.amber; g.fillText(tr('c.viz.phrase'), W - 70, 16);
    g.fillStyle = T.green; g.fillText(tr('c.viz.correction'), W - 8, 16);
    g.textAlign = 'left';
  }
}
