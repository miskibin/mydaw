// Reverb display: the space and its decay.
// Left: the room the network sounds like, drawn to scale (equivalent size from the space's delay lines
// and the decay, lib/reverbSpace.ts) with the source, the listener and the first reflections off its
// walls; a Plate is drawn as a plate. Right: the impulse response's outline — pre-delay, early
// reflections in their real span, the late tail per band (bass / mid / treble) down to −60 dB — with
// the live output after the last hit traced over it. PRE and the mid tail's end are drag handles
// (FxWindow `verbDown`); everything static is cached per (params, size, hover) — `Layer` adds DPR, fonts and theme.
import type { Fx } from '../../api/types';
import type { FxFrame } from '../../api/telemetry';
import { clamp } from '../../lib/format';
import { t as tr } from '../../lib/i18n';
import { envelope, PLATE, roomDims, roomM, timeSpan } from '../../lib/reverbSpace';
import { Layer } from '../../ui/frameLoop';
import { MONO10, MONO11, MONO9, rgba, T, type Viz } from './common';

const LIVE = '#4fb8cc', BASS = '#7c8cf8', TREBLE = '#69c3d6';
const DB_MIN = -66;
/** Rooms are drawn on one scale, so a hall is visibly bigger than a room: px ∝ (m / M_FULL)^0.7. */
const M_FULL = 45;

export type VerbHandle = 'pre' | 'tail';

/** Where things are on a canvas of W × H (shared with the window's hit testing). */
export function verbLayout(W: number, H: number, fx: Fx) {
  const p = fx.params, e = envelope(p), span = timeSpan(p);
  const roomW = Math.round(clamp(W * 0.4, 180, 300));
  const ex0 = roomW + 34, ex1 = W - 14, ey0 = 30, ey1 = H - 24;
  const tx = (t: number) => ex0 + clamp(t / span, 0, 1) * (ex1 - ex0);
  const xt = (x: number) => (x - ex0) / (ex1 - ex0) * span;
  const ydb = (db: number) => ey0 + clamp(-db / -DB_MIN, 0, 1) * (ey1 - ey0);
  return { e, span, roomW, ex0, ex1, ey0, ey1, tx, xt, ydb, preX: tx(e.pre), tailX: tx(e.onset + e.tMid), tailY: ydb(e.lateDb - 60) };
}

/** The handle under (x, y), if any. */
export function verbHit(fx: Fx, x: number, y: number, W: number, H: number): VerbHandle | null {
  const L = verbLayout(W, H, fx);
  if (Math.hypot(x - L.tailX, y - L.tailY) < 10) return 'tail';
  if (Math.abs(x - L.preX) < 6 && y >= L.ey0 - 14 && y <= L.ey1) return 'pre';
  return null;
}

export class ReverbViz implements Viz {
  cursor = 'default';
  hover: VerbHandle | null = null;
  /** The handle being dragged (drawn highlighted). */
  drag: VerbHandle | null = null;
  private stat = new Layer();
  /** Output level (dB) per analysis frame since the last hit: [seconds after it, dB]. */
  private trace: [number, number][] = [];
  private hitAt = -1;
  private floorDb = -120;
  private lastT = 0;

  update(_fx: Fx, a: FxFrame | null, now: number): boolean {
    const t = now / 1000;
    const db = a && a.outPeak > 1e-6 ? 20 * Math.log10(a.outPeak) : -120;
    // a hit: the level jumps well above where it was (and is audible); the trace restarts there
    if (db > -50 && db > this.floorDb + 15 && (this.hitAt < 0 || t - this.hitAt > 0.15)) { this.hitAt = t; this.trace = []; }
    this.floorDb = Math.min(db, this.floorDb + (t - this.lastT) * 40); // the floor follows the level down at once, up slowly
    this.lastT = t;
    if (this.hitAt < 0) return false;
    const since = t - this.hitAt;
    if (since > 10) { this.hitAt = -1; this.trace = []; return true; }
    if (!this.trace.length || since - this.trace[this.trace.length - 1][0] > 0.012) this.trace.push([since, db]);
    return true;
  }

  draw(g: CanvasRenderingContext2D, W: number, H: number, fx: Fx) {
    const p = fx.params, key = [W, H, fx.bypass, this.hover, this.drag, ...['size', 'pre', 'space', 'tone', 'bass', 'early'].map(k => p[k])].join('|');
    g.drawImage(this.stat.get(W, H, key, c => drawStatic(c, W, H, fx, this.hover ?? this.drag)) as CanvasImageSource, 0, 0, W, H);
    // the live output after the last hit, peak-normalised to the tail's start
    const L = verbLayout(W, H, fx);
    if (this.trace.length > 1) {
      const peak = Math.max(...this.trace.map(q => q[1])), off = L.e.lateDb - peak;
      g.save(); g.beginPath(); g.rect(L.ex0, L.ey0, L.ex1 - L.ex0, L.ey1 - L.ey0); g.clip();
      g.beginPath();
      this.trace.forEach(([s, db], i) => { const x = L.tx(L.e.pre + s), y = L.ydb(db + off); if (i) g.lineTo(x, y); else g.moveTo(x, y); });
      g.strokeStyle = rgba(LIVE, 0.85); g.lineWidth = 1.4; g.stroke();
      g.restore();
      g.font = MONO10; g.fillStyle = LIVE; g.textAlign = 'right'; g.fillText(tr('c.verb.lastHit'), L.ex1, L.ey0 - 4);
    }
  }
}

function drawStatic(g: CanvasRenderingContext2D, W: number, H: number, fx: Fx, hot: VerbHandle | null) {
  const L = verbLayout(W, H, fx), e = L.e;
  g.fillStyle = T.viz; g.fillRect(0, 0, W, H);
  g.globalAlpha = fx.bypass ? 0.45 : 1;
  drawRoom(g, 0, 0, L.roomW, H, fx);
  // ---- envelope
  const { ex0, ex1, ey0, ey1, tx, ydb } = L;
  g.fillStyle = T.vizPanel; g.fillRect(ex0, ey0, ex1 - ex0, ey1 - ey0);
  g.lineWidth = 1; g.font = MONO9; g.fillStyle = T.vizLabel; g.textAlign = 'right';
  for (let db = 0; db >= -60; db -= 12) {
    const y = Math.round(ydb(db)) + 0.5;
    g.strokeStyle = db === -60 ? T.vizGrid2 : T.vizGrid; g.beginPath(); g.moveTo(ex0, y); g.lineTo(ex1, y); g.stroke();
    g.fillText(String(db), ex0 - 4, y + 3);
  }
  const step = L.span > 4 ? 1 : L.span > 1.6 ? 0.5 : L.span > 0.8 ? 0.25 : 0.1;
  g.textAlign = 'center';
  for (let t = 0; t <= L.span + 1e-6; t += step) {
    const x = Math.round(tx(t)) + 0.5;
    g.strokeStyle = T.vizGrid; g.beginPath(); g.moveTo(x, ey0); g.lineTo(x, ey1); g.stroke();
    g.fillText((Math.round(t * 100) / 100) + ' s', x, ey1 + 13);
  }
  // pre-delay: nothing comes back yet
  if (e.pre > 0) { g.fillStyle = rgba(T.amber, 0.08); g.fillRect(ex0, ey0, L.preX - ex0, ey1 - ey0); }
  // early reflections
  g.strokeStyle = rgba(T.ink, 0.45); g.lineWidth = 1;
  for (const r of e.early) { const x = Math.round(tx(r.t)) + 0.5; g.beginPath(); g.moveTo(x, ey1); g.lineTo(x, ydb(r.db)); g.stroke(); }
  // late tail per band: fade in over the reflections, then −60 dB in that band's decay
  const tail = (t60: number, col: string, w: number, dash: number[]) => {
    g.beginPath(); g.moveTo(L.preX, ey1);
    g.lineTo(tx(e.onset), ydb(e.lateDb));
    const tEnd = e.onset + t60 * (e.lateDb - DB_MIN) / 60;
    g.lineTo(tx(tEnd), ydb(DB_MIN));
    g.strokeStyle = col; g.lineWidth = w; g.setLineDash(dash); g.stroke(); g.setLineDash([]);
  };
  tail(e.tLow, rgba(BASS, 0.8), 1.2, [4, 3]);
  tail(e.tHigh, rgba(TREBLE, 0.8), 1.2, [2, 3]);
  // mid: filled under, the main line
  g.beginPath(); g.moveTo(L.preX, ey1); g.lineTo(tx(e.onset), ydb(e.lateDb)); g.lineTo(tx(e.onset + e.tMid * (e.lateDb - DB_MIN) / 60), ey1); g.closePath();
  g.fillStyle = rgba(T.amber, 0.1); g.fill();
  tail(e.tMid, T.amber, 1.8, []);
  // handles: PRE (vertical line with a tab) and the mid tail at −60 dB
  const preHot = hot === 'pre', tailHot = hot === 'tail';
  g.strokeStyle = preHot ? T.ink : rgba(T.amber, 0.9); g.lineWidth = preHot ? 2 : 1.2;
  g.beginPath(); g.moveTo(Math.round(L.preX) + 0.5, ey0 - 10); g.lineTo(Math.round(L.preX) + 0.5, ey1); g.stroke();
  g.font = MONO10; g.textAlign = L.preX > ex1 - 70 ? 'right' : 'left'; g.fillStyle = preHot ? T.ink : T.amber;
  g.fillText('PRE ' + Math.round(e.pre * 1000) + ' ms', L.preX + (g.textAlign === 'right' ? -4 : 4), ey0 - 4);
  g.beginPath(); g.arc(L.tailX, L.tailY, tailHot ? 6 : 4.5, 0, Math.PI * 2);
  g.fillStyle = tailHot ? T.ink : T.amber; g.fill(); g.strokeStyle = T.viz; g.lineWidth = 1.5; g.stroke();
  // decay readouts: mid at the handle, bass and treble in the legend
  g.font = MONO11; g.fillStyle = tailHot ? T.ink : T.amber; g.textAlign = L.tailX > ex1 - 60 ? 'right' : 'left';
  g.fillText(e.tMid.toFixed(2) + ' s', L.tailX + (g.textAlign === 'right' ? -9 : 9), L.tailY - 7);
  g.font = MONO10; g.textAlign = 'left';
  let lx = ex0;
  for (const [lab, col] of [[tr('c.verb.bass', { s: e.tLow.toFixed(2) }), BASS], [tr('c.verb.mid', { s: e.tMid.toFixed(2) }), T.amber], [tr('c.verb.treble', { s: e.tHigh.toFixed(2) }), TREBLE]] as [string, string][]) {
    g.fillStyle = col; g.fillRect(lx, 9, 8, 2); g.fillStyle = T.fg4; g.fillText(lab, lx + 11, 13); lx += g.measureText(lab).width + 24;
  }
  g.globalAlpha = 1;
}

/** The space: an isometric room to scale with source, listener and first reflections, or a plate. */
function drawRoom(g: CanvasRenderingContext2D, x0: number, y0: number, w: number, h: number, fx: Fx) {
  const p = fx.params, e = envelope(p), m = roomM(p);
  g.fillStyle = T.vizPanel; g.fillRect(x0 + 8, y0 + 8, w - 16, h - 16);
  const name = e.sp.name; // the Space selector's own (engine) names
  g.font = MONO11; g.fillStyle = T.ink; g.textAlign = 'left';
  g.fillText(name, x0 + 16, y0 + 25);
  const cx = x0 + w / 2, cy = y0 + h / 2 + 10, aw = w - 40, ah = h - 84;
  /** On-screen share of the panel: the biggest room fills it, smaller ones shrink (∝ size^0.7, so a small room stays readable). */
  const share = Math.pow(Math.min(1, m / M_FULL), 0.7);
  const c30 = Math.cos(Math.PI / 6), s30 = 0.5;
  if (e.space === PLATE) {
    // a steel plate on its edge in a frame: its size follows the decay like the network's
    const pw = Math.min(aw / c30, (ah - 10) * 1.2) * Math.max(0.45, share), ph = pw * 0.5;
    const P = (u: number, v: number): [number, number] => [cx + (u - 0.5) * pw * c30, cy + (u - 0.5) * pw * s30 - (v - 0.5) * ph];
    g.beginPath(); [[0, 0], [1, 0], [1, 1], [0, 1]].forEach(([u, v], i) => { const q = P(u, v); if (i) g.lineTo(q[0], q[1]); else g.moveTo(q[0], q[1]); }); g.closePath();
    g.fillStyle = rgba(T.amber, 0.12); g.fill(); g.strokeStyle = rgba(T.amber, 0.8); g.lineWidth = 1.4; g.stroke();
    g.strokeStyle = rgba(T.ink, 0.35); g.lineWidth = 1;
    for (const u of [0.3, 0.7]) { const a = P(u, 0.45), b = P(u, 0.55); g.beginPath(); g.arc((a[0] + b[0]) / 2, (a[1] + b[1]) / 2, 3, 0, Math.PI * 2); g.stroke(); }
    g.font = MONO10; g.fillStyle = T.fg4; g.fillText(tr('c.verb.plateNote'), x0 + 16, y0 + 40, w - 32);
    return;
  }
  const [Lm, Wm, Hm] = roomDims(p);
  // px per metre: fit the box's projection into the panel, then shrink by the room's share
  const bw = (Lm + Wm) * c30, bh = (Lm + Wm) * s30 + Hm;
  const k = Math.min(aw / bw, ah / bh) * share;
  // isometric: length to the lower right, width to the lower left, height up; centred on the floor's middle
  const P = (x: number, y: number, z: number): [number, number] => [cx + (x - y) * c30 * k, cy + (x + y - (Lm + Wm) / 2) * s30 * k - z * k + Hm * k * 0.35];
  const poly = (pts: [number, number, number][], fill: string | null, stroke: string | null) => {
    g.beginPath(); pts.forEach((q, i) => { const s = P(...q); if (i) g.lineTo(s[0], s[1]); else g.moveTo(s[0], s[1]); }); g.closePath();
    if (fill) { g.fillStyle = fill; g.fill(); } if (stroke) { g.strokeStyle = stroke; g.stroke(); }
  };
  g.lineWidth = 1;
  // back walls and floor (the near walls are left open so the inside shows)
  poly([[0, 0, 0], [Lm, 0, 0], [Lm, 0, Hm], [0, 0, Hm]], rgba(T.ink, 0.04), rgba(T.ink, 0.35));
  poly([[0, 0, 0], [0, Wm, 0], [0, Wm, Hm], [0, 0, Hm]], rgba(T.ink, 0.06), rgba(T.ink, 0.35));
  poly([[0, 0, 0], [Lm, 0, 0], [Lm, Wm, 0], [0, Wm, 0]], rgba(T.amber, 0.06), rgba(T.amber, 0.6));
  // near edges, dashed
  g.setLineDash([3, 3]); g.strokeStyle = rgba(T.ink, 0.25);
  for (const [a, b] of [[[Lm, Wm, 0], [Lm, Wm, Hm]], [[Lm, 0, Hm], [Lm, Wm, Hm]], [[0, Wm, Hm], [Lm, Wm, Hm]]] as [number, number, number][][]) {
    const s = P(...a), t = P(...b); g.beginPath(); g.moveTo(s[0], s[1]); g.lineTo(t[0], t[1]); g.stroke();
  }
  g.setLineDash([]);
  // source and listener, the direct path and the six first reflections (image sources), brighter with Early
  const S: [number, number, number] = [Lm * 0.28, Wm * 0.5, Math.min(1.6, Hm * 0.4)], R: [number, number, number] = [Lm * 0.72, Wm * 0.42, Math.min(1.6, Hm * 0.4)];
  const erA = clamp(e.sp.erGain * (p.early ?? 50) / 50, 0, 1.5) / 1.5;
  // reflections off the surfaces that are drawn: the two back walls, the floor and the ceiling
  const walls: [number, number][] = [[0, 0], [1, 0], [2, 0], [2, Hm]];
  if (erA > 0) {
    g.strokeStyle = rgba(T.ink, +(0.12 + 0.5 * erA).toFixed(2)); g.lineWidth = 1;
    for (const [ax, at] of walls) {
      const img = [...R] as [number, number, number]; img[ax] = 2 * at - R[ax];
      const u = (at - S[ax]) / (img[ax] - S[ax]);
      const hit: [number, number, number] = [S[0] + (img[0] - S[0]) * u, S[1] + (img[1] - S[1]) * u, S[2] + (img[2] - S[2]) * u];
      const a = P(...S), b = P(...hit), c = P(...R);
      g.beginPath(); g.moveTo(a[0], a[1]); g.lineTo(b[0], b[1]); g.lineTo(c[0], c[1]); g.stroke();
    }
  }
  const a = P(...S), b = P(...R);
  g.strokeStyle = rgba(T.ink, 0.5); g.setLineDash([2, 3]); g.beginPath(); g.moveTo(a[0], a[1]); g.lineTo(b[0], b[1]); g.stroke(); g.setLineDash([]);
  g.fillStyle = T.amber; g.beginPath(); g.arc(a[0], a[1], 3.5, 0, Math.PI * 2); g.fill();
  g.fillStyle = LIVE; g.beginPath(); g.arc(b[0], b[1], 3.5, 0, Math.PI * 2); g.fill();
  // dimensions and a scale bar
  g.font = MONO10; g.fillStyle = T.fg4; g.textAlign = 'left';
  g.fillText('≈ ' + Math.round(Lm) + ' × ' + Math.round(Wm) + ' × ' + Math.round(Hm) + ' m', x0 + 16, y0 + 40);
  const bar = m > 20 ? 10 : 5, bx = x0 + 16, by = y0 + h - 18;
  g.strokeStyle = T.fg4; g.beginPath(); g.moveTo(bx, by); g.lineTo(bx + bar * k, by); g.moveTo(bx, by - 3); g.lineTo(bx, by + 3); g.moveTo(bx + bar * k, by - 3); g.lineTo(bx + bar * k, by + 3); g.stroke();
  g.fillText(bar + ' m', bx + bar * k + 5, by + 3);
}
