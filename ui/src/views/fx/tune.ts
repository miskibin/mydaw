// Pitch Correct: a continuously scrolling pitch graph (last 6 s, newest at the right edge) over a
// piano-roll of the key/scale, input pitch (thin, neutral) vs corrected output (bright) with the
// correction shaded between them and the target note as a bar; plus a big current-note readout
// with a −50…+50 cent meter (input offset and applied correction), fading with confidence.
import type { Fx } from '../../api/types';
import type { FxFrame } from '../../api/telemetry';
import { clamp } from '../../lib/format';
import { follow, graphX, graphY, hzToMidi, inScale, isBlack, NOTE_NAMES, scaleMask } from '../../lib/pitch';
import { FXMETA } from '../../ui/icons';
import { MONO10, MONO9, Needle, rgba, T, TimeRing, type Viz, type VizExtra } from './common';

export const TUNE_SECS = 6;
const KEYS_W = 38, PANEL_W = 196;
const NAN = Number.NaN;

export class TuneViz implements Viz {
  cursor = 'default';
  /** channels: input midi, output midi, target midi (NaN = unvoiced) */
  private ring = new TimeRing(600, 3);
  private seq = -1;
  private center = 60; private cvel = 0; private span = 14; private svel = 0;
  private lastNow = 0;
  private conf = new Needle(14, 1);
  private inC = new Needle(26, 0.9);
  private outC = new Needle(26, 0.9);
  private note = NAN; private noteHz = 0; private outHz = 0; private m4 = 0; private m5 = 0; private voiced = false;
  private lastPush = 0;

  update(_fx: Fx, a: FxFrame | null, now: number): boolean {
    if (a && a.seq !== this.seq) {
      this.seq = a.seq;
      const m = a.meters, v = m[0] > 0;
      this.ring.push(a.at || now, [v ? hzToMidi(m[0]) : NAN, v && m[2] > 0 ? hzToMidi(m[2]) : NAN, v && m[1] > 0 ? hzToMidi(m[1]) : NAN]);
      this.lastPush = now;
      this.voiced = v;
      if (v) { this.note = hzToMidi(m[1]); this.noteHz = m[1]; this.outHz = m[2]; this.m4 = m[4]; this.m5 = m[5]; }
      this.confTarget = clamp(m[3], 0, 1) * (v ? 1 : 0.25);
    } else if (!a && now - this.lastPush > 33) {
      // no telemetry (stopped / not watched): keep time flowing so the history scrolls away
      this.ring.push(now, [NAN, NAN, NAN]); this.lastPush = now; this.voiced = false; this.confTarget = 0;
    }
    const dt = this.lastNow ? clamp((now - this.lastNow) / 1000, 0, 0.1) : 0.016; this.lastNow = now;
    // vertical range follows the singer: centre on the recent voiced pitch, widen for big leaps
    const tNow = this.ring.clock(now);
    let lo = Infinity, hi = -Infinity, sum = 0, cnt = 0;
    for (let i = this.ring.n - 1; i >= 0; i--) {
      const j = this.ring.at(i), t = this.ring.t[j];
      if (tNow - t > 2500) break;
      for (const c of [0, 1]) { const v = this.ring.ch[c][j]; if (!Number.isNaN(v)) { lo = Math.min(lo, v); hi = Math.max(hi, v); sum += v; cnt++; } }
    }
    if (cnt) {
      const want = clamp((lo + hi) / 2 * 0.6 + (sum / cnt) * 0.4, 24, 96);
      [this.center, this.cvel] = follow(this.center, this.cvel, want, dt, 2.6);
      [this.span, this.svel] = follow(this.span, this.svel, clamp(hi - lo + 6, 12, 30), dt, 2);
    }
    const c1 = this.conf.step(this.confTarget, now);
    const c2 = this.inC.step(clamp(this.m4, -50, 50), now);
    const c3 = this.outC.step(clamp(this.m4 + this.m5, -50, 50), now);
    // keep drawing while anything is on screen or moving
    const visible = this.ring.n > 0 && tNow - this.lastVoicedT() < TUNE_SECS * 1000 + 200;
    return visible || c1 || c2 || c3 || Math.abs(this.cvel) > 1e-3;
  }
  private confTarget = 0;

  private lastVoicedT(): number {
    for (let i = this.ring.n - 1; i >= 0; i--) { const j = this.ring.at(i); if (!Number.isNaN(this.ring.ch[0][j])) return this.ring.t[j]; }
    return -1e12;
  }

  draw(g: CanvasRenderingContext2D, W: number, H: number, fx: Fx, _a: FxFrame | null, ex: VizExtra) {
    const p = fx.params, key = Math.round(p.key ?? 0), mask = scaleMask(p.scale ?? 0);
    const accent = FXMETA.tune.color;
    const gx0 = KEYS_W, gw = W - KEYS_W - PANEL_W, gH = H;
    const span = this.span, center = this.center;
    const Y = (m: number) => graphY(m, center, span, gH);
    g.fillStyle = T.viz; g.fillRect(0, 0, W, H);
    // ---- piano-roll rows + keyboard
    const rowH = gH / span;
    const mTop = Math.ceil(center + span / 2 + 1), mBot = Math.floor(center - span / 2 - 1);
    g.font = MONO9; g.textBaseline = 'middle';
    for (let m = mBot; m <= mTop; m++) {
      const y = Y(m + 0.5), inS = inScale(m, key, mask), root = ((m - key) % 12 + 12) % 12 === 0;
      g.fillStyle = root ? rgba(accent, 0.09 * T.tint) : inS ? T.vizPanel : T.viz;
      g.fillRect(gx0, y, gw, rowH);
      g.fillStyle = T.vizGrid; g.fillRect(gx0, Math.round(y + rowH) - 0.5, gw, 1);
      // keyboard
      const black = isBlack(m);
      g.fillStyle = black ? T.keyB : T.keyW;
      g.fillRect(0, y + 0.5, KEYS_W - 2, rowH - 1);
      if (!inS) { g.fillStyle = rgba(T.viz, 0.55); g.fillRect(0, y + 0.5, KEYS_W - 2, rowH - 1); }
      if (root) { g.fillStyle = accent; g.fillRect(KEYS_W - 5, y + 1, 3, rowH - 2); }
      if (rowH >= 11 && (root || m % 12 === 0)) {
        g.fillStyle = black ? T.keyBText : T.keyWText;
        g.fillText(NOTE_NAMES[((m % 12) + 12) % 12] + (Math.floor(m / 12) - 1), 4, y + rowH / 2 + 0.5);
      }
    }
    g.textBaseline = 'alphabetic';
    // ---- time grid (scrolls continuously with the display clock)
    const ring = this.ring, tNow = ring.clock(ex.now);
    const secsMs = TUNE_SECS * 1000, X = (t: number) => gx0 + graphX(t, tNow, gw, secsMs);
    g.strokeStyle = T.vizGrid2; g.lineWidth = 1;
    for (let s = Math.floor((tNow - secsMs) / 1000) + 1; s * 1000 <= tNow; s++) {
      const x = Math.round(X(s * 1000)) + 0.5;
      g.beginPath(); g.moveTo(x, 0); g.lineTo(x, gH); g.stroke();
    }
    g.save(); g.beginPath(); g.rect(gx0, 0, gw, gH); g.clip();
    if (fx.bypass) g.globalAlpha = 0.45;
    // collect visible samples, ending with an interpolated point at the display clock
    const n = ring.n;
    let i0 = n;
    while (i0 > 0 && ring.t[ring.at(i0 - 1)] > tNow - secsMs - 200) i0--;
    // target bars
    let runStart = -1, runNote = NAN;
    const flushRun = (endT: number) => {
      if (runStart < 0 || Number.isNaN(runNote)) return;
      const xa = X(runStart), xb = X(Math.min(endT, tNow));
      if (xb > xa) { g.fillStyle = rgba(accent, 0.16); g.beginPath(); g.roundRect(xa, Y(runNote + 0.5) + 1, xb - xa, rowH - 2, 3); g.fill(); }
    };
    for (let i = i0; i < n; i++) {
      const j = ring.at(i), t = ring.t[j]; if (t > tNow) { flushRun(t); runStart = -1; break; }
      const tn = Math.round(ring.ch[2][j]);
      if (tn !== runNote || Number.isNaN(ring.ch[2][j])) { flushRun(t); runStart = Number.isNaN(ring.ch[2][j]) ? -1 : t; runNote = tn; }
    }
    flushRun(tNow);
    // voiced runs: shaded correction area, then the two lines
    const pts: number[] = [];
    const runs: [number, number][] = [];
    let rs = -1;
    for (let i = i0; i < n; i++) {
      const j = ring.at(i), t = ring.t[j];
      let vin = ring.ch[0][j], vout = ring.ch[1][j], tt = t;
      if (t > tNow) {
        // interpolate the head at the display clock
        const pj = i > 0 ? ring.at(i - 1) : j, pt = ring.t[pj];
        const u = clamp((tNow - pt) / Math.max(1e-6, t - pt), 0, 1);
        vin = ring.ch[0][pj] + (vin - ring.ch[0][pj]) * u; vout = ring.ch[1][pj] + (vout - ring.ch[1][pj]) * u; tt = tNow;
      }
      const voiced = !Number.isNaN(vin) && !Number.isNaN(vout);
      if (voiced) { if (rs < 0) rs = pts.length; pts.push(X(tt), Y(vin), Y(vout)); }
      else if (rs >= 0) { runs.push([rs, pts.length]); rs = -1; }
      if (t > tNow) break;
    }
    if (rs >= 0) runs.push([rs, pts.length]);
    for (const [a, b] of runs) {
      if (b - a < 6) continue;
      g.beginPath(); g.moveTo(pts[a], pts[a + 1]);
      for (let k = a; k < b; k += 3) g.lineTo(pts[k], pts[k + 1]);
      for (let k = b - 3; k >= a; k -= 3) g.lineTo(pts[k], pts[k + 2]);
      g.closePath(); g.fillStyle = rgba(accent, 0.2); g.fill();
    }
    g.lineJoin = 'round'; g.lineCap = 'round';
    for (const [a, b] of runs) {
      if (b - a < 6) continue;
      g.beginPath(); for (let k = a; k < b; k += 3) k === a ? g.moveTo(pts[k], pts[k + 1]) : g.lineTo(pts[k], pts[k + 1]);
      g.strokeStyle = rgba(T.pre, 0.85); g.lineWidth = 1.25; g.stroke();
    }
    for (const [a, b] of runs) {
      if (b - a < 6) continue;
      g.beginPath(); for (let k = a; k < b; k += 3) k === a ? g.moveTo(pts[k], pts[k + 2]) : g.lineTo(pts[k], pts[k + 2]);
      g.strokeStyle = rgba(accent, 0.25); g.lineWidth = 5; g.stroke();
      g.strokeStyle = accent; g.lineWidth = 2; g.stroke();
    }
    // head dot at the right edge
    const last = runs[runs.length - 1];
    if (last && Math.abs(pts[last[1] - 3] - (gx0 + gw)) < 3) {
      g.beginPath(); g.arc(pts[last[1] - 3], pts[last[1] - 1], 3.5, 0, Math.PI * 2); g.fillStyle = accent; g.fill();
    }
    g.restore();
    g.globalAlpha = 1;
    // time labels
    g.font = MONO9; g.fillStyle = T.vizLabel; g.textAlign = 'right';
    g.fillText('−6 s', gx0 + 26, gH - 5);
    g.fillText('teraz', gx0 + gw - 4, gH - 5);
    g.textAlign = 'left';
    this.drawPanel(g, W - PANEL_W, 0, PANEL_W, H, accent, fx);
  }

  private drawPanel(g: CanvasRenderingContext2D, x0: number, y0: number, w: number, H: number, accent: string, fx: Fx) {
    g.fillStyle = T.vizPanel; g.fillRect(x0, y0, w, H);
    g.fillStyle = T.vizAxis; g.fillRect(x0, y0, 1, H);
    const conf = clamp(this.conf.v, 0, 1), a = 0.25 + 0.75 * conf;
    const cx = x0 + w / 2;
    g.textAlign = 'center';
    g.font = SANS9; g.fillStyle = T.fg4; g.fillText('NUTA', cx, 26);
    if (!Number.isNaN(this.note)) {
      const nm = NOTE_NAMES[((Math.round(this.note) % 12) + 12) % 12], oct = String(Math.floor(Math.round(this.note) / 12) - 1);
      g.globalAlpha = a;
      g.font = '600 48px "Geist Variable", Geist, sans-serif';
      const nw = g.measureText(nm).width;
      g.font = '500 20px "Geist Variable", Geist, sans-serif';
      const ow = g.measureText(oct).width;
      const sx = cx - (nw + ow + 2) / 2;
      g.textAlign = 'left';
      g.font = '600 48px "Geist Variable", Geist, sans-serif'; g.fillStyle = T.fgHi; g.fillText(nm, sx, 80);
      g.font = '500 20px "Geist Variable", Geist, sans-serif'; g.fillStyle = T.fg3; g.fillText(oct, sx + nw + 2, 80);
      g.textAlign = 'center';
      g.font = MONO10; g.fillStyle = T.fg4;
      g.fillText(this.noteHz.toFixed(1) + ' Hz', cx, 100);
      g.globalAlpha = 1;
    } else {
      g.font = '600 48px "Geist Variable", Geist, sans-serif'; g.fillStyle = T.fg6; g.fillText('–', cx, 80);
    }
    // cents meter
    const mx = x0 + 18, mw = w - 36, my = 140, Xc = (c: number) => mx + (clamp(c, -50, 50) + 50) / 100 * mw;
    g.font = SANS9; g.fillStyle = T.fg4; g.fillText('CENTY', cx, 124);
    g.fillStyle = T.viz; g.beginPath(); g.roundRect(mx - 4, my - 9, mw + 8, 18, 4); g.fill();
    g.strokeStyle = T.vizAxis; g.lineWidth = 1; g.stroke();
    for (let c = -50; c <= 50; c += 10) {
      const x = Math.round(Xc(c)) + 0.5, big = c % 50 === 0 || c === 0;
      g.strokeStyle = c === 0 ? T.fg3 : T.vizGrid2; g.beginPath(); g.moveTo(x, my - (big ? 9 : 4)); g.lineTo(x, my + (big ? 9 : 4)); g.stroke();
    }
    g.font = MONO9; g.fillStyle = T.vizLabel;
    g.fillText('−50', Xc(-50), my + 22); g.fillText('0', Xc(0), my + 22); g.fillText('+50', Xc(50), my + 22);
    g.globalAlpha = a;
    const xi = Xc(this.inC.v), xo = Xc(this.outC.v);
    // correction applied: accent bar from input to output
    g.fillStyle = rgba(accent, 0.55); g.fillRect(Math.min(xi, xo), my - 3, Math.abs(xo - xi), 6);
    // input: neutral tick
    g.fillStyle = T.pre; g.fillRect(xi - 1, my - 8, 2, 16);
    // output: accent diamond
    g.fillStyle = accent; g.beginPath(); g.moveTo(xo, my - 7); g.lineTo(xo + 5, my); g.lineTo(xo, my + 7); g.lineTo(xo - 5, my); g.closePath(); g.fill();
    g.globalAlpha = 1;
    // numbers
    g.font = MONO10; g.textAlign = 'left'; g.fillStyle = T.fg4;
    const row = (label: string, v: string, y: number, col: string) => { g.fillStyle = T.fg4; g.textAlign = 'left'; g.fillText(label, mx - 4, y); g.textAlign = 'right'; g.fillStyle = col; g.fillText(v, mx + mw + 4, y); };
    const cents = (c: number) => (c > 0.5 ? '+' : c < -0.5 ? '−' : '') + Math.abs(Math.round(c)) + ' ct';
    row('wejście', this.voiced ? cents(this.m4) : '—', 186, T.fg2);
    row('korekta', this.voiced ? cents(this.m5) : '—', 202, accent);
    row('wyjście', this.voiced && this.outHz > 0 ? this.outHz.toFixed(1) + ' Hz' : '—', 218, T.fg2);
    // confidence
    g.textAlign = 'left'; g.fillStyle = T.fg4; g.fillText('pewność', mx - 4, 240);
    g.fillStyle = T.viz; g.fillRect(mx + 54, 234, mw - 50, 5);
    g.fillStyle = rgba(accent, 0.8); g.fillRect(mx + 54, 234, (mw - 50) * conf, 5);
    if (fx.bypass) { g.font = SANS9; g.fillStyle = T.amber; g.textAlign = 'center'; g.fillText('BYPASS', cx, H - 10); }
    g.textAlign = 'left';
  }
}

const SANS9 = '600 9px "Geist Variable", Geist, sans-serif';
