// Saturator (live transfer trace, added harmonics, waveform) and delay (tap diagram + scope).
import type { Fx } from '../../api/types';
import { SCOPE_LEN, type FxFrame } from '../../api/telemetry';
import { clamp, dbg } from '../../lib/format';
import { lang, tName } from '../../lib/i18n';
import { satDriveDb } from '../../lib/saturator';
import { Layer } from '../../ui/frameLoop';
import { t as tr } from '../../lib/i18n';
import { FXMETA } from '../../ui/icons';
import { freqGrid, MONO10, MONO9, rgba, scope, specCurve, T, TeleGate, type Viz } from './common';

const SAT_STYLES = ['Taśma', 'Konsola', 'Transformator', 'Trioda', 'Pentoda'];
/** Longest in → out delay searched when lining the two scopes up (the oversampler's latency). */
const SAT_LAG = 48;
const SAT_N = SCOPE_LEN - SAT_LAG;
const SAT_KEEP = 16;
const SAT_HIST = 180;

/**
 * Saturator: what the stage does to the signal that is playing, three ways.
 * Left: the transfer curve, with the live input → output trace over it (an X/Y scope: the
 * further the trace bends away from the diagonal, the harder the stage works) and a glow
 * that follows the amount of saturation; the trace keeps its last half second, so a bass note
 * (a few milliseconds per frame is a sliver of its wave) still draws the whole curve.
 * Right: the spectrum before and after (what is tinted is what the stage added: harmonics),
 * and how much was added over the last six seconds.
 */
export class SatViz implements Viz {
  cursor = 'default';
  private layer = new Layer();
  private gate = new TeleGate();
  /** The last SAT_KEEP frames of aligned input / output samples (a ring; `head` = newest). */
  private xin = new Float32Array(SAT_N * SAT_KEEP);
  private xout = new Float32Array(SAT_N * SAT_KEEP);
  private head = 0; private kept = 0;
  /** Added-harmonics share per analysis frame, newest last. */
  private hist = new Float32Array(SAT_HIST);
  private specIn = new Float32Array(128).fill(-120);
  private specOut = new Float32Array(128).fill(-120);
  private live = false;
  /** Share of the output that is not a scaled copy of the input (0..1), smoothed. */
  private amount = 0;
  private scale = 1;

  update(_fx: Fx, a: FxFrame | null) {
    if (!this.gate.fresh(a, 2)) return false;
    const f = a!, i0 = f.scopeIn, o0 = f.scopeOut;
    this.live = f.inPeak > 1e-4;
    this.specIn.set(f.specIn); this.specOut.set(f.specOut);
    // line the output up with the input: the lag with the strongest correlation
    let best = 0, bc = 0;
    for (let lag = 0; lag <= SAT_LAG; lag++) {
      let c = 0;
      for (let i = 0; i < SAT_N; i += 2) c += i0[i] * o0[i + lag];
      if (Math.abs(c) > Math.abs(bc)) { bc = c; best = lag; }
    }
    this.head = (this.head + 1) % SAT_KEEP; this.kept = this.live ? Math.min(SAT_KEEP, this.kept + 1) : 0;
    const base = this.head * SAT_N;
    let sii = 0, sio = 0, soo = 0, pk = 0;
    for (let i = 0; i < SAT_N; i++) {
      const x = i0[i], y = o0[i + best];
      this.xin[base + i] = x; this.xout[base + i] = y;
      sii += x * x; sio += x * y; soo += y * y; pk = Math.max(pk, Math.abs(x), Math.abs(y));
    }
    // what is left of the output after the best-fitting gain of the input is taken out
    const gain = sii > 1e-9 ? sio / sii : 0, res = Math.max(0, soo - gain * sio);
    const amt = this.live && soo > 1e-9 ? Math.sqrt(res / soo) : 0;
    this.amount += (amt - this.amount) * 0.25;
    this.hist.copyWithin(0, 1); this.hist[SAT_HIST - 1] = this.amount;
    const want = 1 / Math.max(0.05, pk);
    this.scale += (want - this.scale) * (want < this.scale ? 0.6 : 0.08);
    return true;
  }

  draw(g: CanvasRenderingContext2D, W: number, H: number, fx: Fx) {
    const col = fx.bypass ? T.vizLabel : FXMETA.sat.color;
    const S = Math.min(H - 12, Math.round(W * 0.3)), sx = 8, sy = Math.round((H - S) / 2);
    const driveDb = satDriveDb(fx.params), dr = dbg(Math.min(driveDb, 40) * 0.6);
    // even-harmonic styles (Taśma, Transformator, Trioda) bend asymmetrically
    const style = clamp(Math.round(fx.params.style ?? 0), 0, 4), asym = [0.18, 0.04, 0.12, 0.3, 0.02][style];
    const x1 = sx + S + 18, w = W - x1 - 8, specH = Math.round((H - 12) * 0.62), scY = specH + 12, scH = H - scY - 6;
    g.fillStyle = T.viz; g.fillRect(0, 0, W, H);
    g.drawImage(this.layer.get(W, H, 'sat2' + lang() + dr.toFixed(3) + style + fx.bypass + S, c => {
      // curve panel
      c.fillStyle = T.vizPanel; c.beginPath(); c.roundRect(sx, sy, S, S, 4); c.fill();
      c.lineWidth = 1; c.strokeStyle = T.vizGrid;
      c.beginPath();
      for (const k of [0.25, 0.5, 0.75]) { c.moveTo(sx + S * k, sy); c.lineTo(sx + S * k, sy + S); c.moveTo(sx, sy + S * k); c.lineTo(sx + S, sy + S * k); }
      c.stroke();
      c.strokeStyle = T.vizGrid2; c.beginPath(); c.moveTo(sx, sy + S); c.lineTo(sx + S, sy); c.stroke();
      c.beginPath();
      for (let i = 0; i <= 120; i++) {
        const x = i / 60 - 1, xb = x * dr + asym * Math.min(1, dr / 4) * (1 - x * x), y = Math.tanh(clamp(xb, -1.2, 1.2) * 2.2) / Math.tanh(2.2);
        const px = sx + i / 120 * S, py = sy + S / 2 - y * S / 2 * 0.92;
        if (i) c.lineTo(px, py); else c.moveTo(px, py);
      }
      c.strokeStyle = rgba(col, 0.45); c.lineWidth = 1.5; c.stroke();
      c.font = MONO9; c.fillStyle = T.vizLabel; c.fillText(tr('c.viz.in'), sx + S - 16, sy + S / 2 - 4); c.fillText(tr('c.viz.out'), sx + S / 2 + 4, sy + 11);
      // spectrum and scope panels
      c.save(); c.translate(0, 6); c.beginPath(); c.rect(x1, 0, w, specH); c.clip(); freqGrid(c, w, specH, x1); c.restore();
      c.fillStyle = T.vizPanel; c.beginPath(); c.roundRect(x1, scY, w, scH, 3); c.fill();
      c.strokeStyle = T.vizGrid; c.beginPath(); c.moveTo(x1, Math.round(scY + scH / 2) + 0.5); c.lineTo(x1 + w, Math.round(scY + scH / 2) + 0.5); c.stroke();
    }) as CanvasImageSource, 0, 0, W, H);

    // the glow of a working stage
    const heat = clamp(this.amount * 2.2, 0, 1);
    if (heat > 0.02 && !fx.bypass) {
      const gr = g.createRadialGradient(sx + S / 2, sy + S / 2, 2, sx + S / 2, sy + S / 2, S * 0.62);
      gr.addColorStop(0, rgba(col, 0.34 * heat)); gr.addColorStop(1, rgba(col, 0));
      g.fillStyle = gr; g.fillRect(sx, sy, S, S);
    }
    // live input → output trace
    if (this.kept) {
      g.save(); g.beginPath(); g.rect(sx, sy, S, S); g.clip();
      const k = this.scale * S / 2 * 0.92;
      for (let age = this.kept - 1; age >= 0; age--) {
        const base = ((this.head - age + SAT_KEEP) % SAT_KEEP) * SAT_N;
        g.beginPath();
        for (let i = 0; i < SAT_N; i++) {
          const px = sx + S / 2 + clamp(this.xin[base + i] * k, -S, S), py = sy + S / 2 - clamp(this.xout[base + i] * k, -S, S);
          if (i) g.lineTo(px, py); else g.moveTo(px, py);
        }
        if (age) { g.strokeStyle = rgba(col, 0.5 * (1 - age / SAT_KEEP)); g.lineWidth = 1.5; g.stroke(); }
        else { g.strokeStyle = rgba(col, 0.35); g.lineWidth = 4; g.stroke(); g.strokeStyle = fx.bypass ? T.vizLabel : T.fgHi; g.lineWidth = 1.2; g.stroke(); }
      }
      g.restore();
    }

    // spectrum: the output first, then the input over it, so the tint that stays is what was added
    const Y = (db: number) => 6 + (1 - clamp((db + 96) / 90, 0, 1)) * specH;
    g.save(); g.beginPath(); g.rect(x1, 6, w, specH); g.clip();
    specCurve(g, this.specOut, w, Y, 6 + specH, x1); g.fillStyle = rgba(col, 0.55); g.fill();
    specCurve(g, this.specIn, w, Y, 6 + specH, x1); g.fillStyle = T.vizPanel; g.fill(); g.fillStyle = rgba(T.pre, 0.3); g.fill();
    specCurve(g, this.specOut, w, Y, 6 + specH, x1); g.strokeStyle = col; g.lineWidth = 1.2; g.stroke();
    g.restore();

    // added harmonics, the last six seconds (full height = 50 %)
    g.save(); g.beginPath(); g.rect(x1, scY, w, scH); g.clip();
    g.beginPath(); g.moveTo(x1, scY + scH);
    for (let i = 0; i < SAT_HIST; i++) g.lineTo(x1 + i / (SAT_HIST - 1) * w, scY + scH - clamp(this.hist[i] * 2, 0, 1) * (scH - 2));
    g.lineTo(x1 + w, scY + scH); g.closePath();
    g.fillStyle = rgba(col, 0.3); g.fill(); g.strokeStyle = col; g.lineWidth = 1.2; g.stroke();
    g.restore();

    // readouts
    g.font = MONO9; g.textAlign = 'right';
    g.fillStyle = T.vizLabel; g.fillText(tName(SAT_STYLES[style]).toUpperCase() + '  ·  DRIVE +' + driveDb.toFixed(1) + ' dB', x1 + w - 4, 18);
    g.fillStyle = heat > 0.05 && !fx.bypass ? col : T.vizLabel;
    g.fillText(tr('c.viz.addedHarmonics') + ' ' + (this.live && !fx.bypass ? Math.round(this.amount * 100) + ' %' : '—'), x1 + w - 4, 30);
    g.textAlign = 'left'; g.fillStyle = T.vizLabel;
    g.fillText(tr('c.viz.harmonics6s'), x1 + 5, scY + 11); g.textAlign = 'right'; g.fillText('25 %', x1 + w - 4, scY + scH / 2 - 3); g.textAlign = 'left';
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
