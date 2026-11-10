// Half-Speed: one loop window drawn twice — what goes in (top) and what comes out (bottom) —
// with the stretch between them, the write and read heads, the crossfade zones, and a band
// strip whose edges can be dragged (FxWindow wires the pointer handlers).
import type { Fx } from '../../api/types';
import type { FxFrame } from '../../api/telemetry';
import { clamp } from '../../lib/format';
import { t as tr, tName } from '../../lib/i18n';
import { S } from '../../state/store';
import { tele } from '../../state/telemetry';
import { Layer } from '../../ui/frameLoop';
import { MONO10, MONO9, rgba, SANS10, T, type Viz } from './common';

export const HALF_COLOR = '#a3d65f';
const SPEED = [1.5, 2, 4];
const LOOP_BEATS = [0.25, 0.5, 1, 2, 4, 8, 16, 32];
const LOOP_NAMES = ['1/16', '1/8', '1/4', '1/2', '1 takt', '2 takty', '4 takty', '8 taktów'];
const SEMIS = ['c.fx.half.semi0', 'c.fx.half.semi1', 'c.fx.half.semi2'] as const;
const NB = 160;
const F0 = 20, F1 = 20000;

/** smooth knob → crossfade ms (same curve as dsp::halftime::smooth_ms). */
export function smoothMs(v: number) {
  const t = clamp(Math.abs(v) / 100, 0, 1);
  return v >= 0 ? 1 + 499 * Math.pow(t, 2.2) : 1 + 299 * t * t;
}

/** Geometry shared by drawing and hit-testing. */
export function halfGeom(W: number, H: number) {
  const pad = 12, top = 22, bandH = 46;
  const bandY = H - bandH - 18, laneGap = 26;
  const laneH = Math.max(28, (bandY - top - 14 - laneGap) / 2);
  return { pad, top, inY: top, outY: top + laneH + laneGap, laneH, bandY, bandH, x0: pad, w: W - 2 * pad };
}
export const halfFx = (f: number, W: number) => { const g = halfGeom(W, 100); return g.x0 + Math.log(clamp(f, F0, F1) / F0) / Math.log(F1 / F0) * g.w; };
export const halfXf = (x: number, W: number) => { const g = halfGeom(W, 100); return F0 * Math.pow(F1 / F0, clamp((x - g.x0) / g.w, 0, 1)); };
/** 'lo' / 'hi' when (x, y) is on a band edge handle. */
export function halfHit(fx: Fx, x: number, y: number, W: number, H: number): 'lo' | 'hi' | null {
  const g = halfGeom(W, H);
  if (y < g.bandY - 6 || y > g.bandY + g.bandH + 6) return null;
  const xl = halfFx(fx.params.lo ?? 20, W), xh = halfFx(fx.params.hi ?? 20000, W);
  const dl = Math.abs(x - xl), dh = Math.abs(x - xh);
  if (Math.min(dl, dh) > 10) return x > xl && x < xh ? null : dl < dh ? 'lo' : 'hi';
  return dl <= dh ? 'lo' : 'hi';
}

export class HalfViz implements Viz {
  cursor = 'default';
  hover: 'lo' | 'hi' | null = null;
  private layer = new Layer();
  private envIn = new Float32Array(NB);
  private envOut = new Float32Array(NB);
  private spec = new Float32Array(128);
  private lastBin = -1;
  private phase = -1;
  private seq = -1;
  private key = '';
  update(fx: Fx, a: FxFrame | null) {
    const f = tele.frame;
    const beat = f.playing ? f.beat : f.patBeat >= 0 ? f.patBeat : null;
    const lb = LOOP_BEATS[Math.round(fx.params.loop ?? 4)] ?? 4;
    const key = lb + '|' + (fx.params.speed ?? 1);
    if (key !== this.key) { this.key = key; this.envIn.fill(0); this.envOut.fill(0); this.lastBin = -1; }
    const ph = beat == null ? -1 : ((beat % lb) + lb) % lb / lb;
    let changed = ph !== this.phase;
    this.phase = ph;
    if (a && a.seq !== this.seq) {
      this.seq = a.seq; changed = true;
      for (let k = 0; k < 128; k++) this.spec[k] += (a.specIn[k] - this.spec[k]) * 0.3;
      if (ph >= 0) {
        const b = Math.min(NB - 1, Math.floor(ph * NB));
        // fill every bin passed since the last frame (short windows move many bins per frame)
        let from = this.lastBin < 0 || this.lastBin === b ? b : (this.lastBin + 1) % NB;
        for (let guard = 0; guard < NB; guard++) {
          this.envIn[from] = a.inPeak; this.envOut[from] = a.outPeak;
          if (from === b) break;
          from = (from + 1) % NB;
        }
        this.lastBin = b;
      } else this.lastBin = -1;
    }
    return changed;
  }

  draw(g: CanvasRenderingContext2D, W: number, H: number, fx: Fx) {
    const p = fx.params, G = halfGeom(W, H);
    const si = Math.round(p.speed ?? 1), M = SPEED[si] ?? 2, li = Math.round(p.loop ?? 4), lb = LOOP_BEATS[li] ?? 4;
    const winMs = lb * 60000 / Math.max(1, S.project.bpm);
    const sm = p.smooth ?? 10, fadeFrac = clamp(smoothMs(sm) / winMs, 0.004, sm >= 0 ? 0.9 : 0.5);
    const used = 1 / M, col = fx.bypass ? T.vizLabel : HALF_COLOR;
    const X = (u: number) => G.x0 + u * G.w;

    g.fillStyle = T.viz; g.fillRect(0, 0, W, H);
    // static layer: grid, lanes, labels, stretch fan, band strip frame
    g.drawImage(this.layer.get(W, H, ['half', si, li, sm, p.lo, p.hi, p.solo, fx.bypass, S.project.bpm, tr('c.fx.half.in')].join('|'), c => {
      c.lineWidth = 1;
      // beat / subdivision grid across the window
      const div = lb >= 4 ? lb : lb >= 1 ? lb * 4 : 4;
      for (let k = 0; k <= div; k++) {
        const x = Math.round(X(k / div)) + 0.5, major = lb >= 4 ? k % 4 === 0 : k % 4 === 0;
        c.strokeStyle = major ? T.vizGrid2 : T.vizGrid; c.beginPath(); c.moveTo(x, G.inY); c.lineTo(x, G.outY + G.laneH); c.stroke();
      }
      // lanes
      for (const y of [G.inY, G.outY]) { c.fillStyle = rgba(T.vizGrid, 0.5); c.fillRect(G.x0, y, G.w, G.laneH); c.strokeStyle = T.vizAxis; c.strokeRect(G.x0 + 0.5, y + 0.5, G.w - 1, G.laneH - 1); }
      // the part of the input that is never heard
      c.save(); c.beginPath(); c.rect(X(used), G.inY, G.w * (1 - used), G.laneH); c.clip();
      c.strokeStyle = rgba(T.vizLabel, 0.35);
      for (let x = X(used) - G.laneH; x < X(1); x += 7) { c.beginPath(); c.moveTo(x, G.inY + G.laneH); c.lineTo(x + G.laneH, G.inY); c.stroke(); }
      c.restore();
      // stretch fan: input [0, 1/M] → output [0, 1]
      const grad = c.createLinearGradient(0, G.inY + G.laneH, 0, G.outY);
      grad.addColorStop(0, rgba(col, 0.28)); grad.addColorStop(1, rgba(col, 0.1));
      c.fillStyle = grad; c.beginPath();
      c.moveTo(X(0), G.inY + G.laneH); c.lineTo(X(used), G.inY + G.laneH); c.lineTo(X(1), G.outY); c.lineTo(X(0), G.outY); c.closePath(); c.fill();
      c.strokeStyle = rgba(col, 0.6); c.setLineDash([3, 3]);
      c.beginPath(); c.moveTo(X(used), G.inY + G.laneH); c.lineTo(X(1), G.outY); c.stroke(); c.setLineDash([]);
      // crossfade zone on the output lane
      c.fillStyle = rgba(T.amber, 0.22);
      if (sm >= 0) c.fillRect(X(0), G.outY, G.w * fadeFrac, G.laneH);
      else c.fillRect(X(1 - fadeFrac), G.outY, G.w * fadeFrac, G.laneH);
      // labels
      c.font = SANS10; c.fillStyle = T.vizLabel; c.textBaseline = 'alphabetic';
      c.font = MONO10; c.textAlign = 'right';
      c.fillStyle = col; c.fillText(`${tName(LOOP_NAMES[li])} · ${tr('c.fx.half.slower', { n: M })} · ${tr(SEMIS[si] ?? SEMIS[1])}`, G.x0 + G.w, G.inY - 6);
      c.textAlign = 'left';
      c.font = MONO9; c.fillStyle = rgba(T.vizLabel, 0.9);
      if (used < 0.98) c.fillText(tr('c.fx.half.skipped'), X(used) + 5, G.inY + G.laneH - 5);

      // band strip
      const lo = p.lo ?? 20, hi = p.hi ?? 20000, solo = (p.solo ?? 0) >= 0.5;
      const xl = halfFx(lo, W), xh = halfFx(hi, W);
      c.fillStyle = rgba(T.vizGrid, 0.6); c.fillRect(G.x0, G.bandY, G.w, G.bandH);
      c.fillStyle = solo ? rgba(T.viz, 0.9) : rgba(T.pre, 0.12);
      c.fillRect(G.x0, G.bandY, xl - G.x0, G.bandH); c.fillRect(xh, G.bandY, G.x0 + G.w - xh, G.bandH);
      c.fillStyle = rgba(col, 0.22); c.fillRect(xl, G.bandY, xh - xl, G.bandH);
      c.strokeStyle = T.vizAxis; c.strokeRect(G.x0 + 0.5, G.bandY + 0.5, G.w - 1, G.bandH - 1);
      c.font = MONO9; c.fillStyle = T.vizLabel;
      for (const f of [50, 100, 200, 500, 1000, 2000, 5000, 10000]) {
        const x = Math.round(halfFx(f, W)) + 0.5;
        c.strokeStyle = T.vizGrid2; c.beginPath(); c.moveTo(x, G.bandY + G.bandH - 4); c.lineTo(x, G.bandY + G.bandH); c.stroke();
        c.fillText(f >= 1000 ? f / 1000 + 'k' : String(f), x + 2, G.bandY + G.bandH + 11);
      }
      c.font = SANS10; c.fillStyle = col;
      const mid = (xl + xh) / 2, txt = tr(solo ? 'c.fx.half.solo' : 'c.fx.half.slowed');
      if (xh - xl > 80) { c.textAlign = 'center'; c.fillText(txt, mid, G.bandY + 14); c.textAlign = 'left'; }
      c.font = MONO9; c.fillStyle = T.vizLabel;
      if (xl - G.x0 > 50) c.fillText(tr(solo ? 'c.fx.half.muted' : 'c.fx.half.unchanged'), G.x0 + 5, G.bandY + 14);
      if (G.x0 + G.w - xh > 50) { c.textAlign = 'right'; c.fillText(tr(solo ? 'c.fx.half.muted' : 'c.fx.half.unchanged'), G.x0 + G.w - 5, G.bandY + 14); c.textAlign = 'left'; }
    }) as CanvasImageSource, 0, 0, W, H);

    // envelopes
    const bw = G.w / NB;
    const env = (e: Float32Array, y: number, c: string, from: number, to: number) => {
      g.fillStyle = c;
      for (let b = 0; b < NB; b++) {
        const u = b / NB; if (u < from || u >= to) continue;
        const v = clamp(Math.sqrt(e[b]), 0, 1) * (G.laneH - 4);
        if (v < 0.5) continue;
        g.fillRect(X(u), y + G.laneH / 2 - v / 2, Math.max(1, bw - 0.6), v);
      }
    };
    env(this.envIn, G.inY, rgba(T.pre, 0.75), 0, used);
    env(this.envIn, G.inY, rgba(T.pre, 0.3), used, 1);
    env(this.envOut, G.outY, rgba(col, 0.85), 0, 1);

    // spectrum backdrop in the band strip
    g.save(); g.beginPath(); g.rect(G.x0, G.bandY, G.w, G.bandH); g.clip();
    g.beginPath(); g.moveTo(G.x0, G.bandY + G.bandH);
    for (let k = 0; k < 128; k++) {
      const f = 20 * Math.pow(1000, k / 127), x = halfFx(f, W), y = G.bandY + G.bandH - clamp((this.spec[k] + 90 + 4.5 * Math.log2(f / 1000)) / 80, 0, 1) * (G.bandH - 6);
      g.lineTo(x, y);
    }
    g.lineTo(G.x0 + G.w, G.bandY + G.bandH); g.closePath(); g.fillStyle = rgba(T.ink, 0.07); g.fill();
    g.restore();

    // heads
    const ph = this.phase;
    if (ph >= 0 && !fx.bypass) {
      const xw = X(ph), xr = X(ph * used);
      g.strokeStyle = rgba(T.ink, 0.35); g.lineWidth = 1;
      g.beginPath(); g.moveTo(xw, G.inY); g.lineTo(xw, G.inY + G.laneH); g.stroke();
      g.strokeStyle = col; g.lineWidth = 2;
      g.beginPath(); g.moveTo(xr, G.inY); g.lineTo(xr, G.inY + G.laneH); g.stroke();
      g.beginPath(); g.moveTo(xr, G.inY + G.laneH); g.lineTo(xw, G.outY); g.lineWidth = 1; g.setLineDash([2, 3]); g.stroke(); g.setLineDash([]);
      g.strokeStyle = T.ink; g.lineWidth = 2;
      g.beginPath(); g.moveTo(xw, G.outY); g.lineTo(xw, G.outY + G.laneH); g.stroke();
      g.font = MONO9; g.fillStyle = col; g.fillText(tr('c.fx.half.read'), Math.min(xr + 4, X(1) - 34), G.inY + 10);
    } else {
      g.font = MONO10; g.fillStyle = T.vizLabel; g.textAlign = 'center';
      g.fillText(tr(fx.bypass ? 'c.fx.half.bypass' : 'c.fx.half.idle'), W / 2, G.outY + G.laneH / 2 + 4);
      g.textAlign = 'left';
    }

    // lane labels on top of the envelopes
    g.font = SANS10;
    for (const [t, y] of [[tr('c.fx.half.in'), G.inY], [tr('c.fx.half.out'), G.outY]] as const) {
      const tw = g.measureText(t).width + 10;
      g.fillStyle = rgba(T.viz, 0.85); g.fillRect(G.x0 + 1, y + 1, tw, 15);
      g.fillStyle = T.vizLabel; g.fillText(t, G.x0 + 6, y + 12);
    }
    g.font = MONO10; g.textAlign = 'right';
    const ft = tr(sm >= 0 ? (sm > 55 ? 'c.fx.half.swell' : 'c.fx.half.sustain') : sm < -55 ? 'c.fx.half.fx' : 'c.fx.half.perc', { ms: Math.round(smoothMs(sm)) });
    const fw = g.measureText(ft).width + 10;
    g.fillStyle = rgba(T.viz, 0.85); g.fillRect(G.x0 + G.w - fw - 1, G.outY + 1, fw, 15);
    g.fillStyle = T.amber; g.fillText(ft, G.x0 + G.w - 6, G.outY + 12);
    g.textAlign = 'left';
    // band handles
    for (const which of ['lo', 'hi'] as const) {
      const f = which === 'lo' ? p.lo ?? 20 : p.hi ?? 20000, x = halfFx(f, W), on = this.hover === which;
      g.fillStyle = on ? T.ink : col; g.fillRect(Math.round(x) - (on ? 1.5 : 1), G.bandY, on ? 3 : 2, G.bandH);
      g.beginPath(); g.arc(x, G.bandY + G.bandH / 2, on ? 6 : 5, 0, Math.PI * 2); g.fill();
      const off = which === 'lo' ? f <= 21 : f >= 19500;
      g.font = MONO9; g.fillStyle = on ? T.ink : T.vizLabel;
      const lab = off ? tr(which === 'lo' ? 'c.fx.half.allLow' : 'c.fx.half.allHigh') : f >= 1000 ? (f / 1000).toFixed(f >= 10000 ? 0 : 1) + ' kHz' : Math.round(f) + ' Hz';
      g.textAlign = which === 'lo' ? 'left' : 'right';
      g.fillText(lab, which === 'lo' ? x + 8 : x - 8, G.bandY + G.bandH - 6);
      g.textAlign = 'left';
    }
  }
}
