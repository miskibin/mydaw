// Pitch Correct, laid out like a hardware tuner (Auto-Tune Pro Auto mode / Waves Tune Real-Time):
// a big readout of the note being sung to, an arc meter of how far the voice is from it
// (needle = input offset in cents) and how much is being corrected (filled arc from 0),
// a marker where the output lands, and a ribbon of the 12 notes showing the key/scale with
// the current target lit. Nothing scrolls or rescales, so the picture stays put while singing.
import type { Fx } from '../../api/types';
import type { FxFrame } from '../../api/telemetry';
import { clamp } from '../../lib/format';
import { hzToMidi, NOTE_NAMES, scaleMask } from '../../lib/pitch';
import { t as tr, type TranslationKey } from '../../lib/i18n';
import { FXMETA } from '../../ui/icons';
import { MONO10, MONO9, Needle, rgba, T, type Viz, type VizExtra } from './common';

const RANGE = 100; // cents, each side of the meter
const SCALE_NAMES: TranslationKey[] = ['c.viz.scale.chromatic', 'c.viz.scale.major', 'c.viz.scale.minor', 'c.viz.scale.majPent', 'c.viz.scale.minPent'];

export class TuneViz implements Viz {
  cursor = 'default';
  private seq = -1;
  private inN = new Needle(15, 0.85);   // input offset from target, cents
  private corrN = new Needle(15, 0.85); // correction applied, cents
  private confN = new Needle(10, 1);
  private hold = 0;                     // last time a voiced frame arrived (ms)
  private target = Number.NaN; private inHz = 0;
  private inC = 0; private corrC = 0; private conf = 0;

  update(_fx: Fx, a: FxFrame | null, now: number): boolean {
    if (a && a.seq !== this.seq) {
      this.seq = a.seq;
      const m = a.meters;
      if (m[0] > 0 && m[1] > 0) {
        this.target = Math.round(hzToMidi(m[1]));
        this.inHz = m[0];
        this.inC = clamp(m[4], -RANGE, RANGE);
        this.corrC = clamp(-m[5], -RANGE, RANGE);
        this.conf = clamp(m[3], 0, 1);
        this.hold = now;
      }
    }
    // between phrases: hold the last note briefly, then let everything settle at 0
    const live = now - this.hold < 250;
    if (!live) { this.inC = 0; this.corrC = 0; this.conf = 0; }
    const s1 = this.inN.step(this.inC, now), s2 = this.corrN.step(this.corrC, now), s3 = this.confN.step(this.conf, now);
    return s1 || s2 || s3 || live;
  }

  draw(g: CanvasRenderingContext2D, W: number, H: number, fx: Fx, _a: FxFrame | null, _ex: VizExtra) {
    const accent = FXMETA.tune.color, p = fx.params;
    const key = Math.round(p.key ?? 0), scale = Math.round(p.scale ?? 0), mask = scaleMask(scale);
    const conf = clamp(this.confN.v, 0, 1), on = !fx.bypass;
    g.fillStyle = T.viz; g.fillRect(0, 0, W, H);

    const ribbonH = 34, mainH = H - ribbonH - 8;
    const leftW = Math.round(clamp(W * 0.28, 170, 240));

    // ---- note readout (left)
    const vis = 0.25 + 0.75 * conf;
    g.textAlign = 'center';
    g.font = MONO9; g.fillStyle = T.vizLabel;
    g.fillText(tr('c.viz.targetNote'), leftW / 2, 22);
    const name = Number.isNaN(this.target) ? '—' : NOTE_NAMES[((this.target % 12) + 12) % 12];
    const oct = Number.isNaN(this.target) ? '' : String(Math.floor(this.target / 12) - 1);
    g.font = `600 ${Math.round(clamp(mainH * 0.42, 44, 92))}px "Geist Variable", Geist, sans-serif`;
    g.fillStyle = rgba(on ? accent : T.fg3, vis);
    const nameW = g.measureText(name).width;
    const ny = mainH * 0.62;
    g.fillText(name, leftW / 2 - (oct ? 10 : 0), ny);
    if (oct) {
      g.font = `500 ${Math.round(clamp(mainH * 0.16, 16, 30))}px "Geist Variable", Geist, sans-serif`;
      g.textAlign = 'left'; g.fillStyle = rgba(T.fg3, vis);
      g.fillText(oct, leftW / 2 - 10 + nameW / 2 + 3, ny);
      g.textAlign = 'center';
    }
    g.font = MONO10; g.fillStyle = T.fg4;
    const inTxt = conf > 0.05 && this.inHz > 0 ? `${this.inHz.toFixed(1)} Hz   ${fmtC(this.inN.v)}` : 'cisza';
    g.fillText(inTxt, leftW / 2, ny + 26);
    // tracking lamp
    g.beginPath(); g.arc(leftW / 2 - 34, mainH - 4, 3.5, 0, Math.PI * 2);
    g.fillStyle = conf > 0.5 ? accent : rgba(T.fg3, 0.35); g.fill();
    g.font = MONO9; g.textAlign = 'left'; g.fillStyle = T.vizLabel;
    g.fillText('TRACKING', leftW / 2 - 26, mainH - 1);
    g.fillStyle = T.vizAxis; g.fillRect(leftW, 12, 1, mainH - 12);

    // ---- arc meter (centre/right)
    const mx0 = leftW + 1, mw = W - mx0;
    const cx = mx0 + mw / 2;
    const cy = mainH - 4;                       // needle hub sits just above the ribbon
    const R = Math.max(40, Math.min(mw * 0.4, cy - 40)); // leave room for the labels above the arc
    const a0 = Math.PI * 1.12, a1 = Math.PI * 1.88; // arc span (left → right)
    const ang = (c: number) => a0 + (clamp(c, -RANGE, RANGE) + RANGE) / (2 * RANGE) * (a1 - a0);
    // track
    g.lineCap = 'butt';
    g.lineWidth = 14; g.strokeStyle = rgba(T.fg3, 0.12);
    g.beginPath(); g.arc(cx, cy, R, a0, a1); g.stroke();
    // in-tune zone ±10 c
    g.strokeStyle = rgba(accent, 0.22);
    g.beginPath(); g.arc(cx, cy, R, ang(-10), ang(10)); g.stroke();
    // correction applied: filled arc from 0
    const corr = this.corrN.v;
    if (Math.abs(corr) > 0.5 && on) {
      g.strokeStyle = rgba(accent, 0.85);
      g.beginPath();
      if (corr > 0) g.arc(cx, cy, R, ang(0), ang(corr)); else g.arc(cx, cy, R, ang(corr), ang(0));
      g.stroke();
    }
    // ticks + labels
    g.font = MONO9; g.textAlign = 'center';
    for (let c = -RANGE; c <= RANGE; c += 10) {
      const major = c % 50 === 0, a = ang(c);
      const r0 = R + 10, r1 = R + (major ? 18 : 14);
      g.strokeStyle = major ? T.fg3 : rgba(T.fg3, 0.45); g.lineWidth = major ? 1.4 : 1;
      g.beginPath(); g.moveTo(cx + Math.cos(a) * r0, cy + Math.sin(a) * r0); g.lineTo(cx + Math.cos(a) * r1, cy + Math.sin(a) * r1); g.stroke();
      if (major) { g.fillStyle = T.vizLabel; g.fillText(c === 0 ? '0' : (c > 0 ? '+' : '−') + Math.abs(c), cx + Math.cos(a) * (R + 30), cy + Math.sin(a) * (R + 30) + 3); }
    }
    // output marker: where the corrected pitch lands
    if (conf > 0.05 && on) {
      const ao = ang(this.inN.v + corr);
      g.fillStyle = T.ink;
      g.beginPath(); g.arc(cx + Math.cos(ao) * (R - 14), cy + Math.sin(ao) * (R - 14), 3.2, 0, Math.PI * 2); g.fill();
    }
    // needle: input offset
    const an = ang(this.inN.v);
    g.strokeStyle = rgba(T.amber, 0.25 + 0.75 * conf); g.lineWidth = 2.4; g.lineCap = 'round';
    g.beginPath(); g.moveTo(cx + Math.cos(an) * R * 0.66, cy + Math.sin(an) * R * 0.66); g.lineTo(cx + Math.cos(an) * (R + 8), cy + Math.sin(an) * (R + 8)); g.stroke();
    g.lineCap = 'butt';
    // centre readout
    g.textAlign = 'center';
    g.font = MONO9; g.fillStyle = T.vizLabel;
    g.fillText(tr('c.viz.correctionCaps'), cx, cy - R * 0.42);
    g.font = '600 20px "Geist Mono Variable", monospace'; g.fillStyle = on ? (Math.abs(corr) > 1 ? accent : T.fg3) : T.fg4;
    g.fillText(on ? fmtC(corr) : 'BYPASS', cx, cy - R * 0.42 + 24);
    // legend
    g.font = MONO9; g.textAlign = 'left';
    let lx = mx0 + 10; const ly = 16;
    const leg = (draw: () => void, t: string) => { draw(); g.fillStyle = T.fg4; g.fillText(t, lx + 13, ly + 3); lx += g.measureText(t).width + 26; };
    leg(() => { g.fillStyle = T.amber; g.fillRect(lx, ly - 1, 9, 2.4); }, tr('c.viz.input'));
    leg(() => { g.fillStyle = accent; g.fillRect(lx, ly - 2, 9, 5); }, tr('c.viz.correction'));
    leg(() => { g.fillStyle = T.ink; g.beginPath(); g.arc(lx + 4.5, ly, 3, 0, Math.PI * 2); g.fill(); }, tr('c.viz.output'));
    g.textAlign = 'right'; g.fillStyle = T.fg4;
    g.fillText(`${NOTE_NAMES[key]} ${SCALE_NAMES[scale] ? tr(SCALE_NAMES[scale]) : ''}`, W - 10, ly + 3);

    // ---- note ribbon (bottom): the 12 notes, scale lit, target highlighted
    const ry = H - ribbonH - 2, pad = 10, gap = 4;
    const pw = (W - 2 * pad - 11 * gap) / 12;
    const tpc = Number.isNaN(this.target) ? -1 : ((this.target % 12) + 12) % 12;
    for (let k = 0; k < 12; k++) {
      const pcN = (key + k) % 12; // start the ribbon at the key's root
      const x = pad + k * (pw + gap);
      const inS = (mask & (1 << k)) !== 0;
      const isT = pcN === tpc && conf > 0.05 && on;
      g.beginPath(); g.roundRect(x, ry, pw, ribbonH - 6, 4);
      g.fillStyle = isT ? accent : inS ? rgba(accent, 0.13) : rgba(T.fg3, 0.05); g.fill();
      if (inS && !isT) { g.strokeStyle = rgba(accent, 0.35); g.lineWidth = 1; g.stroke(); }
      g.font = '600 11px "Geist Variable", Geist, sans-serif'; g.textAlign = 'center';
      g.fillStyle = isT ? T.onBand : inS ? T.ink : rgba(T.fg3, 0.45);
      g.fillText(NOTE_NAMES[pcN], x + pw / 2, ry + (ribbonH - 6) / 2 + 4);
      if (k === 0) { g.fillStyle = isT ? T.onBand : accent; g.fillRect(x + pw / 2 - 6, ry + ribbonH - 10, 12, 2); }
    }
    g.textAlign = 'left';
  }
}

const fmtC = (c: number) => (Math.abs(c) < 0.5 ? '0' : (c > 0 ? '+' : '−') + Math.abs(c).toFixed(0)) + ' c';
