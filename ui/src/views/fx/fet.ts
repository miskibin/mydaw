// FET compressor: a backlit needle meter with VU ballistics (≈300 ms, slight overshoot) reading
// gain reduction or output level, input/output peak bars, and a scrolling GR history strip.
import type { Fx } from '../../api/types';
import type { FxFrame } from '../../api/telemetry';
import { clamp } from '../../lib/format';
import { Layer } from '../../ui/frameLoop';
import { setU, U } from '../../state/ui';
import { FXMETA } from '../../ui/icons';
import { MONO10, MONO9, Needle, rgba, T, TimeRing, type Viz, type VizExtra } from './common';

export type FetMode = 'gr' | 'out';
/** Meter mode per effect (UI state, not saved; reactive so the window's buttons follow). */
export const fetModes = { get: (id: number): FetMode => U.fetMode[id] ?? 'gr', set: (id: number, m: FetMode) => setU('fetMode', id, m) };
/** 0 VU = −18 dBFS on the output reading. */
export const VU_REF = -18;
const HIST_S = 6;

/** VU scale law: needle position 0..1 for −20…+3 (proportional to voltage, like a real VU face). */
export function vuPos(db: number): number {
  const v = (x: number) => Math.pow(10, x / 20);
  return (v(clamp(db, -22, 3.6)) - v(-20)) / (v(3) - v(-20));
}

/** The meter is a physical object (a backlit face in a bezel): its colours are the same in both themes. */
const FACE = {
  top: '#e7ddbd', bot: '#cbbd93', ink: '#2a2418', inkSoft: 'rgba(42,36,24,.6)', red: '#b5352a', glass: 'rgba(255,255,255,.18)',
  needle: '#16120c', needleOff: '#5a5245', needleShadow: 'rgba(40,30,15,.22)', pivot: '#1b1d21', bezel: '#121418', rim: '#2a2e35',
  inner: 'rgba(0,0,0,.6)', innerBlur: 'rgba(0,0,0,.45)',
};
/** Peak-meter gradient, as the mixer meters (styles.css .meter). */
const METER = ['#3f9e72', '#4ec08a', '#f0c25a', '#e05263'];

export class FetViz implements Viz {
  cursor = 'default';
  private face = new Layer();
  private needle = new Needle(16, 0.78);
  private hist = new TimeRing(720, 1);
  private seq = -1; private lastPush = 0;
  private gr = 0; private inDb = -120; private outDb = -120;
  private inBar = -60; private outBar = -60; private inPk = -60; private outPk = -60; private pkT = [0, 0];
  private lastNow = 0;
  constructor(private fxId: number) {}

  update(_fx: Fx, a: FxFrame | null, now: number): boolean {
    if (a && a.seq !== this.seq) {
      this.seq = a.seq;
      this.gr = Math.min(0, a.meters[0]); this.inDb = a.meters[1]; this.outDb = a.meters[2];
      if (a.inPeak < 1e-6 && a.outPeak < 1e-6) { this.gr = 0; this.inDb = this.outDb = -120; }
      this.hist.push(a.at || now, [this.gr]); this.lastPush = now;
    } else if (!a && now - this.lastPush > 33) { this.gr = 0; this.inDb = this.outDb = -120; this.hist.push(now, [0]); this.lastPush = now; }
    const dt = this.lastNow ? clamp((now - this.lastNow) / 1000, 0, 0.1) : 0.016; this.lastNow = now;
    const mode = fetModes.get(this.fxId);
    const target = mode === 'gr' ? this.gr : this.outDb - VU_REF;
    const moving = this.needle.step(mode === 'gr' ? clamp(target, -21, 0) : clamp(target, -21.5, 3.5), now);
    // peak bars: instant attack, 20 dB/s fall, 1 s hold
    const fall = (cur: number, v: number) => v > cur ? v : Math.max(v, cur - 20 * dt);
    this.inBar = fall(this.inBar, clamp(this.inDb, -60, 6)); this.outBar = fall(this.outBar, clamp(this.outDb, -60, 6));
    for (const [k, v] of [[0, this.inBar], [1, this.outBar]] as const) {
      const pk = k ? this.outPk : this.inPk;
      let np = pk;
      if (v >= pk) { np = v; this.pkT[k] = now; } else if (now - this.pkT[k] > 1000) np = Math.max(v, pk - 12 * dt);
      if (k) this.outPk = np; else this.inPk = np;
    }
    const histLive = this.hist.n > 0 && this.hist.clock(now) - this.lastNonZero() < HIST_S * 1000 + 200;
    return moving || histLive || this.inBar > -59.9 || this.outBar > -59.9 || this.inPk > -59.9 || this.outPk > -59.9;
  }
  private lastNonZero() {
    for (let i = this.hist.n - 1; i >= 0; i--) { const j = this.hist.at(i); if (this.hist.ch[0][j] < -0.05) return this.hist.t[j]; }
    return -1e12;
  }

  draw(g: CanvasRenderingContext2D, W: number, H: number, fx: Fx, _a: FxFrame | null, ex: VizExtra) {
    const mode = fetModes.get(this.fxId);
    const accent = FXMETA.fet.color;
    g.fillStyle = T.viz; g.fillRect(0, 0, W, H);
    const histH = 40, top = 10, faceH = H - histH - top - 18;
    const faceW = Math.min(W - 150, Math.round(faceH * 1.95));
    const fx0 = Math.round((W - 104 - faceW) / 2) + 6, fy0 = top;
    g.drawImage(this.face.get(W, H, 'face' + mode + faceW + 'x' + faceH, c => drawFace(c, fx0, fy0, faceW, faceH, mode)) as CanvasImageSource, 0, 0, W, H);
    // needle
    const cx = fx0 + faceW / 2, cy = fy0 + faceH * 1.18, R = faceH * 0.98;
    const ang = (p: number) => (-50 + 100 * p) * Math.PI / 180;
    const p = vuPos(this.needle.v), a = ang(p);
    g.save(); g.beginPath(); g.roundRect(fx0 + 6, fy0 + 6, faceW - 12, faceH - 12, 6); g.clip();
    const tipX = cx + Math.sin(a) * R * 0.93, tipY = cy - Math.cos(a) * R * 0.93;
    const baseX = cx + Math.sin(a) * R * 0.32, baseY = cy - Math.cos(a) * R * 0.32;
    g.strokeStyle = FACE.needleShadow; g.lineWidth = 2.5; g.beginPath(); g.moveTo(baseX + 3, baseY + 4); g.lineTo(tipX + 3, tipY + 4); g.stroke();
    g.strokeStyle = fx.bypass ? FACE.needleOff : FACE.needle; g.lineWidth = 1.6; g.beginPath(); g.moveTo(baseX, baseY); g.lineTo(tipX, tipY); g.stroke();
    // glass sheen
    const sh = g.createLinearGradient(0, fy0, 0, fy0 + faceH * 0.55);
    sh.addColorStop(0, FACE.glass); sh.addColorStop(1, 'rgba(255,255,255,0)');
    g.fillStyle = sh; g.fillRect(fx0, fy0, faceW, faceH * 0.55);
    g.restore();
    // pivot cover
    g.fillStyle = FACE.pivot; g.beginPath(); g.ellipse(cx, fy0 + faceH - 4, faceW * 0.13, 12, 0, Math.PI, 0); g.fill();
    // readout
    const rx = fx0 + faceW + 16, val = this.needle.v;
    g.textAlign = 'left';
    g.font = '600 9px "Geist Variable", Geist, sans-serif'; g.fillStyle = T.fg4;
    g.fillText(mode === 'gr' ? 'GAIN REDUCTION' : 'OUTPUT', rx, fy0 + 12);
    g.font = '600 26px "Geist Mono Variable", monospace'; g.fillStyle = mode === 'gr' && val < -0.5 ? accent : T.fgHi;
    g.fillText(mode === 'gr' ? (val < -0.05 ? '−' + Math.abs(val).toFixed(1) : '0.0') : (val >= 0 ? '+' : '−') + Math.abs(val).toFixed(1), rx, fy0 + 42);
    g.font = MONO10; g.fillStyle = T.fg4; g.fillText(mode === 'gr' ? 'dB' : 'VU (0 = −18 dBFS)', rx, fy0 + 56);
    // in/out bars
    const by0 = fy0 + 72, bh = faceH - 72, bw = 8;
    const bar = (x: number, v: number, pk: number, label: string) => {
      const Y = (db: number) => by0 + bh - clamp((db + 60) / 66, 0, 1) * bh;
      g.fillStyle = T.vizPanel; g.fillRect(x, by0, bw, bh);
      const yv = Y(v);
      const grd = g.createLinearGradient(0, by0 + bh, 0, by0);
      grd.addColorStop(0, METER[0]); grd.addColorStop(0.7, METER[1]); grd.addColorStop(0.88, METER[2]); grd.addColorStop(1, METER[3]);
      g.fillStyle = grd; g.fillRect(x, yv, bw, by0 + bh - yv);
      g.fillStyle = T.ink; g.fillRect(x, Math.round(Y(pk)), bw, 1.5);
      g.font = MONO9; g.fillStyle = T.fg4; g.textAlign = 'center'; g.fillText(label, x + bw / 2, by0 + bh + 11);
      g.textAlign = 'left';
    };
    bar(rx, this.inBar, this.inPk, 'IN');
    bar(rx + 26, this.outBar, this.outPk, 'OUT');
    g.font = MONO9; g.fillStyle = T.vizLabel;
    for (const db of [0, -12, -24, -48]) { const y = by0 + bh - clamp((db + 60) / 66, 0, 1) * bh; g.fillText(String(db), rx + 40, y + 3); }
    // GR history strip (continuous scroll)
    const hy = H - histH, hx = 8, hw = W - 16;
    g.fillStyle = T.vizPanel; g.beginPath(); g.roundRect(hx, hy, hw, histH - 8, 3); g.fill();
    g.strokeStyle = T.vizGrid2; g.lineWidth = 1;
    for (const d of [-6, -12]) { const y = Math.round(hy + (-d / 20) * (histH - 8)) + 0.5; g.beginPath(); g.moveTo(hx, y); g.lineTo(hx + hw, y); g.stroke(); }
    const ring = this.hist, tNow = ring.clock(ex.now), span = HIST_S * 1000;
    g.save(); g.beginPath(); g.rect(hx, hy, hw, histH - 8); g.clip();
    g.beginPath(); g.moveTo(hx + hw, hy);
    let first = true, lastX = hx + hw;
    for (let i = ring.n - 1; i >= 0; i--) {
      const j = ring.at(i), t = ring.t[j];
      let v = ring.ch[0][j], tt = t;
      if (t > tNow) { const pj = ring.at(Math.max(0, i - 1)); const u = clamp((tNow - ring.t[pj]) / Math.max(1e-6, t - ring.t[pj]), 0, 1); v = ring.ch[0][pj] + (v - ring.ch[0][pj]) * u; tt = tNow; }
      const x = hx + hw - (tNow - tt) / span * hw, y = hy + clamp(-v / 20, 0, 1) * (histH - 8);
      if (first) { g.lineTo(x, hy); first = false; }
      g.lineTo(x, y); lastX = x;
      if (t < tNow - span - 100) break;
    }
    g.lineTo(lastX, hy); g.closePath();
    g.fillStyle = rgba(accent, 0.38); g.fill(); g.strokeStyle = accent; g.lineWidth = 1.2; g.stroke();
    g.restore();
    g.font = MONO9; g.fillStyle = T.vizLabel; g.fillText('GR · 6 s', hx + 5, hy + 11); g.textAlign = 'right'; g.fillText('−12', hx + hw - 4, hy + (12 / 20) * (histH - 8) + 3); g.textAlign = 'left';
  }
}

function drawFace(g: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, mode: 'gr' | 'out') {
  // bezel
  g.fillStyle = FACE.bezel; g.beginPath(); g.roundRect(x - 2, y - 2, w + 4, h + 4, 9); g.fill();
  g.fillStyle = FACE.rim; g.beginPath(); g.roundRect(x, y, w, h, 8); g.fill();
  // backlit face
  const grd = g.createRadialGradient(x + w / 2, y + h * 0.75, h * 0.1, x + w / 2, y + h * 0.6, w * 0.7);
  grd.addColorStop(0, FACE.top); grd.addColorStop(1, FACE.bot);
  g.fillStyle = grd; g.beginPath(); g.roundRect(x + 6, y + 6, w - 12, h - 12, 6); g.fill();
  // inner shadow
  g.save(); g.beginPath(); g.roundRect(x + 6, y + 6, w - 12, h - 12, 6); g.clip();
  g.shadowColor = FACE.innerBlur; g.shadowBlur = 14; g.strokeStyle = FACE.inner; g.lineWidth = 6; g.stroke();
  g.restore();
  const cx = x + w / 2, cy = y + h * 1.18, R = h * 0.98;
  const ang = (p: number) => (-50 + 100 * p) * Math.PI / 180;
  const pt = (p: number, r: number): [number, number] => [cx + Math.sin(ang(p)) * r, cy - Math.cos(ang(p)) * r];
  const arcR = R * 0.82;
  // red zone
  g.strokeStyle = FACE.red; g.lineWidth = 5;
  g.beginPath(); g.arc(cx, cy, arcR + 3, ang(vuPos(0)) - Math.PI / 2, ang(1) - Math.PI / 2); g.stroke();
  g.strokeStyle = FACE.ink; g.lineWidth = 1.3;
  g.beginPath(); g.arc(cx, cy, arcR, ang(0) - Math.PI / 2, ang(1) - Math.PI / 2); g.stroke();
  // ticks + labels
  const major = [-20, -10, -7, -5, -3, -2, -1, 0, 1, 2, 3];
  const minor = [-15, -8, -6, -4, -2.5, -1.5, -0.5, 0.5, 1.5, 2.5];
  g.textAlign = 'center'; g.textBaseline = 'middle';
  for (const d of minor) { const [ax, ay] = pt(vuPos(d), arcR); const [bx, by] = pt(vuPos(d), arcR + 5); g.lineWidth = 1; g.strokeStyle = d > 0 ? FACE.red : FACE.ink; g.beginPath(); g.moveTo(ax, ay); g.lineTo(bx, by); g.stroke(); }
  for (const d of major) {
    const p = vuPos(d), [ax, ay] = pt(p, arcR), [bx, by] = pt(p, arcR + 9), [lx, ly] = pt(p, arcR + 19);
    g.lineWidth = 1.6; g.strokeStyle = d > 0 ? FACE.red : FACE.ink; g.beginPath(); g.moveTo(ax, ay); g.lineTo(bx, by); g.stroke();
    g.font = '600 ' + Math.round(h * 0.075) + 'px "Geist Variable", Geist, sans-serif';
    g.fillStyle = d > 0 ? FACE.red : FACE.ink;
    g.fillText(mode === 'gr' ? (d > 0 ? '' : String(Math.abs(d))) : (d > 0 ? '+' + d : String(d)), lx, ly);
  }
  // inner percentage-style arc (decoration with meaning: the −10…0 region in finer steps)
  g.font = '700 ' + Math.round(h * 0.085) + 'px "Geist Variable", Geist, sans-serif';
  g.fillStyle = FACE.ink; g.fillText(mode === 'gr' ? 'GR  dB' : 'VU', cx, y + h * 0.62);
  g.font = '600 ' + Math.round(h * 0.06) + 'px "Geist Variable", Geist, sans-serif'; g.fillStyle = FACE.inkSoft;
  g.fillText(mode === 'gr' ? 'gain reduction' : 'output level', cx, y + h * 0.73);
  g.textAlign = 'left'; g.textBaseline = 'alphabetic';
}
