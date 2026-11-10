// Denoiser: analyser with the input (grey) and cleaned output (accent) spectra, the learned
// noise profile as a hatched floor, and the reduction applied per band hanging from the top
// (meters 0‥16 = noise floor dB, 16‥32 = gain dB, both on 16 log bands 50 Hz – 16 kHz).
import type { Fx } from '../../api/types';
import type { FxFrame } from '../../api/telemetry';
import { fx2x } from '../../lib/eq';
import { clamp } from '../../lib/format';
import { t as tr } from '../../lib/i18n';
import { FXMETA } from '../../ui/icons';
import { MONO10, MONO9, Needle, rgba, SpecSmoother, specCurve, T, type Viz, type VizExtra } from './common';

const NB = 16;
const bandF = (b: number) => 50 * Math.pow(16000 / 50, b / NB);

export class DenoiseViz implements Viz {
  cursor = 'default';
  private seq = -1;
  private sin = new SpecSmoother(128, 40, 0.8, 20);
  private sout = new SpecSmoother(128, 40, 0.8, 20);
  private noise = new Float32Array(NB).fill(-120);
  private gain = new Float32Array(NB);
  private gainS = new Float32Array(NB);
  private gr = new Needle(30, 1);
  private grNow = 0;
  private live = false;

  update(_fx: Fx, a: FxFrame | null, now: number): boolean {
    if (a && a.seq !== this.seq) {
      this.seq = a.seq;
      const quiet = a.inPeak < 1e-6;
      this.live = !quiet;
      if (quiet) { this.sin.silence(); this.sout.silence(); this.grNow = 0; }
      else {
        this.sin.feed(a.specIn); this.sout.feed(a.specOut);
        for (let b = 0; b < NB; b++) { this.noise[b] = a.meters[b]; this.gain[b] = Math.min(0, a.meters[NB + b]); }
        this.grNow = Math.min(0, a.grDb);
      }
    } else if (!a) { this.sin.silence(); this.sout.silence(); this.grNow = 0; this.live = false; }
    let moving = false;
    for (let b = 0; b < NB; b++) {
      const tgt = this.live ? this.gain[b] : 0, d = tgt - this.gainS[b];
      if (Math.abs(d) > 0.02) { this.gainS[b] += d * 0.25; moving = true; } else this.gainS[b] = tgt;
    }
    const s1 = this.sin.step(now), s2 = this.sout.step(now), s3 = this.gr.step(this.grNow, now);
    return s1 || s2 || s3 || moving;
  }

  draw(g: CanvasRenderingContext2D, W: number, H: number, fx: Fx, _a: FxFrame | null, _ex: VizExtra) {
    const accent = FXMETA.denoise.color, top = 24, bot = H - 16;
    const X = (f: number) => fx2x(f, W);
    const Y = (db: number) => top + clamp(-db / 96, 0, 1.15) * (bot - top);
    g.fillStyle = T.viz; g.fillRect(0, 0, W, H);
    // grid
    g.lineWidth = 1; g.font = MONO9;
    for (const f of [50, 100, 200, 500, 1000, 2000, 5000, 10000]) {
      const x = Math.round(X(f)) + 0.5; g.strokeStyle = T.vizGrid; g.beginPath(); g.moveTo(x, top); g.lineTo(x, H); g.stroke();
      g.fillStyle = T.vizLabel; g.fillText(f >= 1000 ? f / 1000 + 'k' : String(f), x + 3, H - 4);
    }
    for (const db of [-24, -48, -72]) {
      const y = Math.round(Y(db)) + 0.5; g.strokeStyle = T.vizGrid; g.beginPath(); g.moveTo(0, y); g.lineTo(W, y); g.stroke();
      g.fillStyle = T.vizLabel; g.textAlign = 'right'; g.fillText(String(db), W - 4, y - 3); g.textAlign = 'left';
    }
    if (fx.bypass) g.globalAlpha = 0.5;
    // input and output spectra
    specCurve(g, this.sin.cur, W, Y, H);
    g.fillStyle = rgba(T.pre, 0.14); g.fill(); g.strokeStyle = rgba(T.pre, 0.6); g.lineWidth = 1; g.stroke();
    specCurve(g, this.sout.cur, W, Y, H);
    g.fillStyle = rgba(accent, 0.2); g.fill(); g.strokeStyle = accent; g.lineWidth = 1.4; g.stroke();
    // learned noise profile: a hatched floor
    if (this.noise.some(v => v > -110)) {
      g.beginPath(); g.moveTo(X(bandF(0)), bot + 16);
      for (let b = 0; b < NB; b++) { const xc = X(Math.sqrt(bandF(b) * bandF(b + 1))); g.lineTo(xc, Y(this.noise[b])); }
      g.lineTo(X(bandF(NB)), bot + 16); g.closePath();
      g.save(); g.clip();
      g.strokeStyle = rgba(T.red, 0.28); g.lineWidth = 1;
      for (let x = -H; x < W; x += 6) { g.beginPath(); g.moveTo(x, bot + 16); g.lineTo(x + H, bot + 16 - H); g.stroke(); }
      g.restore();
      g.beginPath();
      for (let b = 0; b < NB; b++) { const xc = X(Math.sqrt(bandF(b) * bandF(b + 1))), y = Y(this.noise[b]); if (b) g.lineTo(xc, y); else g.moveTo(xc, y); }
      g.strokeStyle = rgba(T.red, 0.85); g.lineWidth = 1.5; g.setLineDash([4, 3]); g.stroke(); g.setLineDash([]);
    }
    // reduction per band hanging from the top
    const grH = (bot - top) * 0.35;
    for (let b = 0; b < NB; b++) {
      const gr = -this.gainS[b]; if (gr < 0.1) continue;
      const x0 = X(bandF(b)) + 1, x1 = X(bandF(b + 1)) - 1, h = clamp(gr / 30, 0, 1) * grH;
      g.fillStyle = rgba(T.red, 0.18 + 0.5 * clamp(gr / 24, 0, 1)); g.fillRect(x0, top, x1 - x0, h);
    }
    g.globalAlpha = 1;
    // legend + total reduction
    g.font = MONO9; let lx = 8; const ly = 14;
    const leg = (col: string, t: string) => { g.fillStyle = col; g.fillRect(lx, ly - 6, 8, 3); g.fillStyle = T.fg4; g.fillText(t, lx + 11, ly - 2); lx += g.measureText(t).width + 24; };
    leg(T.pre, tr('c.viz.input')); leg(accent, tr('c.viz.denoised')); leg(T.red, tr('c.viz.noiseProfile'));
    g.font = '600 11px "Geist Mono Variable", monospace'; g.textAlign = 'right';
    g.fillStyle = this.gr.v < -0.5 ? T.red : T.fg3;
    g.fillText((this.gr.v < -0.05 ? '−' + Math.abs(this.gr.v).toFixed(1) : '0.0') + ' dB', W - 8, ly + 1);
    g.font = MONO10; g.fillStyle = T.fg4;
    if (fx.params.learn) g.fillText(tr('c.viz.profileFrozen'), W - 80, ly + 1);
    if (fx.params.listen) { g.fillStyle = T.amber; g.fillText(tr('c.viz.listenNoise'), W - 80, ly + 15); }
    g.textAlign = 'left';
  }
}
