// Delay: where the echoes land and how loud they are (per channel, on a beat grid when synced),
// the read heads moving live (LFO), how hard the loop works, and the loop's waveshaper curve.
import type { Fx } from '../../api/types';
import type { FxFrame } from '../../api/telemetry';
import { clamp } from '../../lib/format';
import { delayMs, echoTaps, loopGain, shapeCurve } from '../../lib/delay';
import { t as tr } from '../../lib/i18n';
import { beatFraction, noteName } from '../../lib/beatDiv';
import { S } from '../../state/store';
import { Layer } from '../../ui/frameLoop';
import { FXMETA } from '../../ui/icons';
import { MONO10, MONO9, rgba, SANS10, scope, T, TeleGate, type Viz } from './common';

export class DelayViz implements Viz {
  cursor = 'default';
  private layer = new Layer();
  private gate = new TeleGate();
  private sOut = new Float32Array(256);
  private m = new Float32Array(8);
  update(_fx: Fx, a: FxFrame | null) {
    if (!this.gate.fresh(a, 2)) return false;
    this.sOut.set(a!.scopeOut.subarray(0, this.sOut.length));
    for (let i = 0; i < 8; i++) this.m[i] = a!.meters[i] ?? 0;
    return true;
  }
  draw(g: CanvasRenderingContext2D, W: number, H: number, fx: Fx) {
    const p = fx.params, bpm = S.project.bpm, synced = (p.sync ?? 0) < 0.5, model = Math.round(p.model ?? 1);
    const col = fx.bypass ? T.vizLabel : FXMETA.delay.color;
    const insetW = Math.min(150, W * 0.24), x0 = 10, x1 = W - insetW - 18, top = 18, base = H - 18;
    const mid = (top + base) / 2, half = (base - top) / 2 - 4;
    const T0 = delayMs(p, bpm);
    // window: at least 2 beats (synced) or 1 s, up to 16 echoes, never more than 8 s
    const beatMs = 60000 / Math.max(1, bpm);
    const span = clamp(Math.max(synced ? 2 * beatMs : 1000, T0 * 6.5), 200, 8000);
    const X = (ms: number) => x0 + ms / span * (x1 - x0);
    g.fillStyle = T.viz; g.fillRect(0, 0, W, H);

    g.drawImage(this.layer.get(W, H, ['dly', span, bpm, synced, W, H, model, fx.bypass, tr('c.fx.dly.loop')].join('|'), c => {
      c.lineWidth = 1; c.font = MONO10;
      // grid: beats when synced (16ths when zoomed in), else ms
      if (synced) {
        const sub = span / beatMs < 3 ? 4 : 1;
        for (let k = 0; k * beatMs / sub <= span; k++) {
          const x = Math.round(X(k * beatMs / sub)) + 0.5, beat = k % sub === 0;
          c.strokeStyle = beat ? T.vizGrid2 : T.vizGrid; c.beginPath(); c.moveTo(x, top); c.lineTo(x, base); c.stroke();
          if (beat && k > 0) { c.fillStyle = T.vizLabel; c.fillText(beatFraction(k / sub), x + 3, H - 5); }
        }
      } else {
        const step = span > 3000 ? 1000 : span > 1200 ? 500 : span > 500 ? 100 : 50;
        for (let ms = 0; ms <= span; ms += step) {
          const x = Math.round(X(ms)) + 0.5;
          c.strokeStyle = ms % (step * 5) === 0 ? T.vizGrid2 : T.vizGrid; c.beginPath(); c.moveTo(x, top); c.lineTo(x, base); c.stroke();
          if (ms > 0) { c.fillStyle = T.vizLabel; c.fillText(ms >= 1000 ? ms / 1000 + ' s' : ms + ' ms', x + 3, H - 5); }
        }
      }
      // channel lanes: L above, R below
      c.strokeStyle = T.vizAxis; c.beginPath(); c.moveTo(x0, Math.round(mid) + 0.5); c.lineTo(x1, Math.round(mid) + 0.5); c.stroke();
      c.font = SANS10; c.fillStyle = T.vizLabel;
      c.fillText('L', x0 + 2, top + 10); c.fillText('R', x0 + 2, base - 3);
      // inset frame (waveshaper)
      c.strokeStyle = T.vizAxis; c.strokeRect(W - insetW - 8.5, top + 0.5, insetW, base - top - 1);
    }) as CanvasImageSource, 0, 0, W, H);

    // output scope behind the taps
    scope(g, this.sOut, x0, top, x1 - x0, base - top, rgba(T.post, 0.22));

    if (model === 3) {
      g.font = SANS10; g.fillStyle = T.vizLabel; g.textAlign = 'center';
      g.fillText(tr('c.fx.dly.noEcho'), (x0 + x1) / 2, mid - 6); g.textAlign = 'left';
    } else {
      // the dry click at 0 (full scale) and the echo taps; the loop filter darkens later repeats
      g.fillStyle = rgba(T.ink, 0.8); g.fillRect(X(0) - 1, mid - half * 0.9, 3, half * 1.8);
      const filtered = Math.round(p.ftype ?? 1) !== 0;
      const taps = echoTaps(p, bpm, span);
      taps.forEach((t, i) => {
        // dB scale (−48 dB floor): the decay reads the same at any wet level
        const h = clamp((20 * Math.log10(Math.max(1e-6, t.amp)) + 48) / 48, 0.03, 1) * half * 0.95, x = X(t.ms);
        const fade = filtered ? Math.max(0.35, 1 - i * 0.04) : 1;
        g.fillStyle = rgba(col, +(fx.bypass ? 0.3 : 0.9 * fade).toFixed(2));
        if (t.side === 0) g.fillRect(x - 2, mid - h, 4, h); else g.fillRect(x - 2, mid, 4, h);
      });
      // live read heads (with modulation): thin markers at the first echo
      const live = this.m[0] > 0 || this.m[1] > 0;
      if (live && !fx.bypass) {
        g.strokeStyle = rgba(T.amber, 0.9); g.lineWidth = 1.5;
        for (const [ms, y0, y1] of [[this.m[0], mid - half, mid], [this.m[1], mid, mid + half]] as const) {
          if (ms <= 0 || ms > span) continue;
          const x = Math.round(X(ms)) + 0.5; g.beginPath(); g.moveTo(x, y0); g.lineTo(x, y1); g.stroke();
        }
        g.lineWidth = 1;
      }
      // time readout
      g.font = MONO10; g.fillStyle = T.vizLabel;
      const nn = synced ? noteName(p.beats ?? 0.75) : null;
      const label = synced ? beatFraction(p.beats ?? 0.75) + (nn ? ' (' + nn + ')' : '') + ' · ' + Math.round(T0) + ' ms' : Math.round(T0) + ' ms';
      g.textAlign = 'center'; g.fillText(label, (x0 + x1) / 2, top - 5); g.textAlign = 'left';
      if (loopGain(p) >= 1) {
        g.fillStyle = T.red; g.textAlign = 'right'; g.fillText('∞ ' + tr('c.fx.dly.selfOsc'), x1 - 2, top - 5); g.textAlign = 'left';
      }
    }

    // waveshaper inset: input → output of the loop drive, with the live drive amount
    const ix = W - insetW - 8, iw = insetW, iy = top, ih = base - top;
    const dist = (p.dist ?? 1) >= 0.5 ? 1 : 0, th = Math.pow(10, (p.level ?? 0) / 20), knee = (p.knee ?? 50) / 100, sym = (p.sym ?? 0) / 100;
    const R = 1.6, cx = ix + iw / 2, cy = iy + ih / 2, sx = iw / 2 / R, sy = ih / 2 / R;
    g.strokeStyle = T.vizGrid; g.beginPath(); g.moveTo(ix, cy + 0.5); g.lineTo(ix + iw, cy + 0.5); g.moveTo(cx + 0.5, iy); g.lineTo(cx + 0.5, iy + ih); g.stroke();
    g.setLineDash([2, 3]); g.beginPath(); g.moveTo(cx - R * sx, cy + R * sy); g.lineTo(cx + R * sx, cy - R * sy); g.stroke(); g.setLineDash([]);
    g.strokeStyle = col; g.lineWidth = 2; g.beginPath();
    for (let k = 0; k <= 80; k++) {
      const x = -R + 2 * R * k / 80, y = shapeCurve(x, dist, th, knee, sym);
      const px = cx + x * sx, py = cy - clamp(y, -R, R) * sy;
      if (k) g.lineTo(px, py); else g.moveTo(px, py);
    }
    g.stroke(); g.lineWidth = 1;
    g.font = MONO9; g.fillStyle = T.vizLabel; g.fillText(tr('c.fx.dly.drive'), ix + 4, iy + 11);
    const red = this.m[3];
    if (red < -0.1 && !fx.bypass) { g.fillStyle = T.amber; g.textAlign = 'right'; g.fillText(red.toFixed(1) + ' dB', ix + iw - 4, iy + 11); g.textAlign = 'left'; }
    // loop level bar on the inset's edge
    const lv = clamp((this.m[2] + 60) / 60, 0, 1);
    g.fillStyle = rgba(col, 0.25); g.fillRect(ix + iw - 5, iy + ih - 2 - (ih - 20) * lv, 3, (ih - 20) * lv);
    g.fillStyle = T.vizLabel; g.fillText(tr('c.fx.dly.loop'), ix + 4, iy + ih - 5);
  }
}
