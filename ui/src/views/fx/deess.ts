// De-esser: a scrolling level history (last 4 s) — input level, output level, the sidechain
// (high band) level against the threshold line, sibilant moments tinted, and the gain reduction
// hanging from the top (darker = more reduction). Right: analyser with the detection band.
import type { Fx } from '../../api/types';
import type { FxFrame } from '../../api/telemetry';
import { fx2x, x2f } from '../../lib/eq';
import { clamp, hzText } from '../../lib/format';
import { FXMETA } from '../../ui/icons';
import { Layer } from '../../ui/frameLoop';
import { MONO10, MONO9, Needle, rgba, SpecSmoother, specCurve, T, TimeRing, type Viz, type VizExtra } from './common';

const SECS = 4;
const DB_TOP = 0, DB_BOT = -66;
/** Split of the canvas: history | analyser. */
export const deessGeom = (W: number, H: number) => {
  const sw = Math.round(clamp(W * 0.34, 180, 260));
  return { hx: 0, hw: W - sw - 1, sx: W - sw, sw, top: 26, bot: H - 4, strip: 20 };
};
export const deessY = (db: number, H: number) => { const g = deessGeom(1000, H); return g.top + clamp((DB_TOP - db) / (DB_TOP - DB_BOT), 0, 1) * (g.bot - g.top); };
export const deessDb = (y: number, H: number) => { const g = deessGeom(1000, H); return DB_TOP - (y - g.top) / (g.bot - g.top) * (DB_TOP - DB_BOT); };
/** Analyser x for frequency f inside the spectrum pane. */
export const deessFx = (f: number, W: number, H: number) => { const g = deessGeom(W, H); return g.sx + fx2x(f, g.sw); };
export const deessXf = (x: number, W: number, H: number) => { const g = deessGeom(W, H); return x2f(clamp(x - g.sx, 0, g.sw), g.sw); };

const lin2db = (x: number) => x > 1e-6 ? 20 * Math.log10(x) : -120;

export class DeessViz implements Viz {
  cursor = 'default';
  /** channels: in dB, out dB, sidechain dB, GR dB, sibilance */
  private ring = new TimeRing(600, 5);
  private seq = -1; private lastPush = 0;
  private spec = new SpecSmoother(128, 40, 0.8, 20);
  private sib = new Needle(30, 1);
  private grN = new Needle(30, 1);
  private sibNow = 0; private grNow = 0;
  private bg = new Layer();
  hoverThr = false; hoverFreq = false;

  ingest(a: FxFrame, at: number) {
    const quiet = a.inPeak < 1e-6, m = a.meters;
    this.ring.push(at, [lin2db(a.inPeak), lin2db(a.outPeak), quiet ? -120 : m[1], quiet ? 0 : Math.min(0, m[0]), quiet ? 0 : m[2]], a.seq);
    this.lastPush = at;
  }

  update(_fx: Fx, a: FxFrame | null, now: number): boolean {
    if (a && a.seq !== this.seq) {
      this.seq = a.seq;
      const quiet = a.inPeak < 1e-6;
      const m = a.meters;
      this.sibNow = quiet ? 0 : m[2]; this.grNow = quiet ? 0 : Math.min(0, m[0]);
      if (quiet) this.spec.silence(); else this.spec.feed(a.specIn);
    } else if (!a && now - this.lastPush > 33) {
      this.ring.push(now, [-120, -120, -120, 0, 0]); this.lastPush = now; this.sibNow = 0; this.grNow = 0; this.spec.silence();
    }
    const s1 = this.spec.step(now), s2 = this.sib.step(this.sibNow, now), s3 = this.grN.step(this.grNow, now);
    const live = this.ring.n > 0 && this.ring.clock(now) - this.lastLoud() < SECS * 1000 + 200;
    return s1 || s2 || s3 || live;
  }
  private lastLoud() {
    for (let i = this.ring.n - 1; i >= 0; i--) { const j = this.ring.at(i); if (this.ring.ch[0][j] > -90) return this.ring.t[j]; }
    return -1e12;
  }

  draw(g: CanvasRenderingContext2D, W: number, H: number, fx: Fx, _a: FxFrame | null, ex: VizExtra) {
    const p = fx.params, accent = FXMETA.deess.color, G = deessGeom(W, H);
    const thr = p.thr ?? -32, range = Math.max(1, p.range ?? 8), freq = p.freq ?? 6000;
    const Y = (db: number) => deessY(db, H);
    g.fillStyle = T.viz; g.fillRect(0, 0, W, H);
    g.drawImage(this.bg.get(W, H, 'deess', c => {
      c.font = MONO9; c.lineWidth = 1;
      for (const db of [-12, -24, -36, -48, -60]) {
        const y = Math.round(Y(db)) + 0.5;
        c.strokeStyle = T.vizGrid; c.beginPath(); c.moveTo(G.hx, y); c.lineTo(G.hx + G.hw, y); c.stroke();
        c.fillStyle = T.vizLabel; c.textAlign = 'right'; c.fillText(String(db), G.hx + G.hw - 4, y - 3);
      }
      c.textAlign = 'left';
      c.fillStyle = T.vizPanel; c.fillRect(G.hx, 0, G.hw, G.strip);
      c.fillStyle = T.vizGrid2; c.fillRect(G.hx, G.strip, G.hw, 1);
      c.fillStyle = T.vizAxis; c.fillRect(G.sx - 1, 0, 1, H);
    }) as CanvasImageSource, 0, 0, W, H);

    // ---- history
    const ring = this.ring, tNow = ring.clock(ex.now), span = SECS * 1000;
    const X = (t: number) => G.hx + G.hw - (tNow - t) / span * G.hw;
    g.save(); g.beginPath(); g.rect(G.hx, G.strip + 1, G.hw, H - G.strip - 1); g.clip();
    if (fx.bypass) g.globalAlpha = 0.5;
    // gather visible samples (with an interpolated head)
    const xs: number[] = [], vals: number[][] = [[], [], [], [], []];
    let i0 = ring.n; while (i0 > 0 && ring.t[ring.at(i0 - 1)] > tNow - span - 150) i0--;
    for (let i = Math.max(0, i0 - 1); i < ring.n; i++) {
      const j = ring.at(i), t = ring.t[j];
      if (t > tNow) {
        const pj = ring.at(Math.max(0, i - 1)), u = clamp((tNow - ring.t[pj]) / Math.max(1e-6, t - ring.t[pj]), 0, 1);
        xs.push(X(tNow)); for (let c = 0; c < 5; c++) vals[c].push(ring.ch[c][pj] + (ring.ch[c][j] - ring.ch[c][pj]) * u);
        break;
      }
      xs.push(X(t)); for (let c = 0; c < 5; c++) vals[c].push(ring.ch[c][j]);
    }
    const n = xs.length, base = G.bot;
    const area = (c: number) => { g.beginPath(); g.moveTo(xs[0] ?? 0, base); for (let k = 0; k < n; k++) g.lineTo(xs[k], Y(Math.max(DB_BOT, vals[c][k]))); g.lineTo(xs[n - 1] ?? 0, base); g.closePath(); };
    if (n > 1) {
      // sibilant moments: tint the column under the input level
      for (let k = 1; k < n; k++) {
        const s = vals[4][k]; if (s < 0.05) continue;
        g.fillStyle = rgba(accent, 0.07 + 0.16 * s); g.fillRect(xs[k - 1], G.top, xs[k] - xs[k - 1] + 0.6, base - G.top);
      }
      area(0); g.fillStyle = rgba(T.pre, 0.22); g.fill(); g.strokeStyle = rgba(T.pre, 0.55); g.lineWidth = 1; g.stroke();
      area(1); g.fillStyle = rgba(accent, 0.18); g.fill(); g.strokeStyle = rgba(accent, 0.75); g.lineWidth = 1; g.stroke();
      // sidechain level
      g.beginPath(); let pen = false;
      for (let k = 0; k < n; k++) { const v = vals[2][k]; if (v < DB_BOT) { pen = false; continue; } const y = Y(v); if (!pen) { g.moveTo(xs[k], y); pen = true; } else g.lineTo(xs[k], y); }
      g.strokeStyle = T.amber; g.lineWidth = 1.4; g.stroke();
      // gain reduction hanging from the top
      const grH = (G.bot - G.top) * 0.42;
      for (let k = 1; k < n; k++) {
        const gr = -vals[3][k]; if (gr < 0.05) continue;
        const u = clamp(gr / range, 0, 1), h = clamp(gr / 24, 0, 1) * grH;
        g.fillStyle = rgba(T.red, 0.25 + 0.6 * u); g.fillRect(xs[k - 1], G.strip, xs[k] - xs[k - 1] + 0.6, h + 2);
      }
    }
    g.globalAlpha = 1;
    g.restore();
    // threshold line + handle
    const ty = Math.round(Y(thr)) + 0.5;
    g.setLineDash([5, 4]); g.strokeStyle = this.hoverThr ? T.ink : rgba(T.amber, 0.9); g.lineWidth = this.hoverThr ? 1.5 : 1;
    g.beginPath(); g.moveTo(G.hx, ty); g.lineTo(G.hx + G.hw, ty); g.stroke(); g.setLineDash([]);
    g.font = MONO10; const lab = 'THR ' + Math.round(thr) + ' dB', lw = g.measureText(lab).width + 10;
    g.fillStyle = this.hoverThr ? T.amber : rgba(T.amber, 0.85); g.beginPath(); g.roundRect(G.hx + 6, ty - 8, lw, 16, 3); g.fill();
    g.fillStyle = T.onBand; g.fillText(lab, G.hx + 11, ty + 4);
    // legend + live readouts
    g.font = MONO9; g.textAlign = 'left';
    let lx = G.hx + 8; const ly = 13;
    const leg = (col: string, t: string) => { g.fillStyle = col; g.fillRect(lx, ly - 6, 8, 3); g.fillStyle = T.fg4; g.fillText(t, lx + 11, ly - 2); lx += g.measureText(t).width + 24; };
    leg(T.pre, 'wejście'); leg(accent, 'wyjście'); leg(T.amber, 'detektor'); leg(T.red, 'redukcja');
    // sibilance indicator + GR number
    const ix = G.hx + G.hw - 158, s = clamp(this.sib.v, 0, 1);
    g.fillStyle = T.fg4; g.fillText('SYBILANT', ix - 4, ly - 2);
    g.fillStyle = T.viz; g.fillRect(ix + 46, ly - 8, 40, 6);
    g.fillStyle = s > 0.5 ? accent : rgba(accent, 0.6); g.fillRect(ix + 46, ly - 8, 40 * s, 6);
    g.font = '600 11px "Geist Mono Variable", monospace'; g.fillStyle = this.grN.v < -0.5 ? T.red : T.fg3; g.textAlign = 'right';
    g.fillText((this.grN.v < -0.05 ? '−' + Math.abs(this.grN.v).toFixed(1) : '0.0') + ' dB', G.hx + G.hw - 6, ly + 1);
    g.textAlign = 'left';

    // ---- analyser with the detection band
    g.save(); g.beginPath(); g.rect(G.sx, 0, G.sw, H); g.clip();
    const sy = (db: number) => 18 + clamp(-db / 90, 0, 1.2) * (H - 30);
    const fxp = deessFx(freq, W, H);
    g.fillStyle = rgba(accent, this.hoverFreq ? 0.16 : 0.1); g.fillRect(fxp, 0, G.sx + G.sw - fxp, H);
    g.strokeStyle = T.vizGrid; g.lineWidth = 1; g.font = MONO9;
    for (const f of [100, 1000, 10000]) {
      const x = Math.round(deessFx(f, W, H)) + 0.5; g.beginPath(); g.moveTo(x, 0); g.lineTo(x, H); g.stroke();
      g.fillStyle = T.vizLabel; g.fillText(f >= 1000 ? f / 1000 + 'k' : String(f), x + 3, H - 5);
    }
    specCurve(g, this.spec.cur, G.sw, sy, H, G.sx);
    g.fillStyle = rgba(T.pre, 0.18); g.fill(); g.strokeStyle = rgba(T.pre, 0.7); g.lineWidth = 1; g.stroke();
    // the part of the spectrum the detector listens to
    g.save(); g.beginPath(); g.rect(fxp, 0, G.sx + G.sw - fxp, H); g.clip();
    specCurve(g, this.spec.cur, G.sw, sy, H, G.sx);
    g.fillStyle = rgba(accent, 0.3); g.fill(); g.strokeStyle = accent; g.stroke();
    g.restore();
    // handle
    g.strokeStyle = accent; g.lineWidth = this.hoverFreq ? 2 : 1.5;
    g.beginPath(); g.moveTo(Math.round(fxp) + 0.5, 22); g.lineTo(Math.round(fxp) + 0.5, H); g.stroke();
    g.fillStyle = accent; g.beginPath(); g.roundRect(fxp - 30, 4, 60, 16, 3); g.fill();
    g.fillStyle = T.onBand; g.textAlign = 'center'; g.font = MONO10; g.fillText(hzText(freq), fxp, 15.5); g.textAlign = 'left';
    g.restore();
    if (p.listen) { g.font = '600 10px "Geist Variable", sans-serif'; g.fillStyle = T.amber; g.textAlign = 'right'; g.fillText('ODSŁUCH S', G.sx + G.sw - 8, 34); g.textAlign = 'left'; }
  }
}
