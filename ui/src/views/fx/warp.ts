// Warp: the map editor. A strip of 16 slots per bank (Time / Volume) on top; below, the edited
// slot's map over one bar. Time maps: up = now, down = up to 8 beats back; the slope is the
// speed (the dashed safety line is "stopped", the grey guides are "reverse at normal speed").
// Volume maps: gain. The playing position moves across live; the strip shows which slot plays
// (filled), which is queued (outlined) and which is being edited (white frame).
import type { AutoPoint, CurveShape, Fx, WarpMaps } from '../../api/types';
import type { FxFrame } from '../../api/telemetry';
import { clamp } from '../../lib/format';
import { t as tr, tName } from '../../lib/i18n';
import { valueAt } from '../../lib/automation';
import { BAR, DEPTH, speedAt, speedText } from '../../lib/warp';
import { addPoint, deletePoints, dragTension, handleAt, hitEnv, movePoints, setShape, type EnvGeom } from '../../lib/envEdit';
import * as A from '../../state/actions';
import { commit, Gesture } from '../../state/edit';
import { findFx, plain, S } from '../../state/store';
import { openMenu, say, type MenuItem } from '../../state/ui';
import { paramMenu } from '../../state/automation';
import { dblCheck, track } from '../../ui/drag';
import { FXMETA } from '../../ui/icons';
import { MONO10, MONO9, rgba, SANS10, T, type Viz } from './common';

export type Bank = 'time' | 'vol';
const SLOTS = 16;
const VOL_COLOR = '#f0a24a';
/** Grid of the map: 1/16 note (a quarter beat). Time values snap to the same depth grid. */
const SNAP_T = 0.25, SNAP_Y = 0.25 / DEPTH;

/** Geometry shared by drawing and hit-testing. */
export function warpGeom(W: number, H: number) {
  const stripY = 6, stripH = 26, tabW = 66, gap = 6;
  const slotsX = 8 + 2 * tabW + 10, slotW = (W - slotsX - 8 - gap * 0) / SLOTS;
  const x0 = 46, y0 = stripY + stripH + 26, x1 = W - 12, y1 = H - 20;
  return { stripY, stripH, tabW, slotsX, slotW, x0, y0, x1, y1, w: x1 - x0, h: y1 - y0 };
}

function envGeom(W: number, H: number): EnvGeom {
  const g = warpGeom(W, H);
  return {
    X: t => g.x0 + t / BAR * g.w, T: x => (x - g.x0) / g.w * BAR,
    Y: v => g.y1 - v * g.h, V: y => clamp((g.y1 - y) / g.h, 0, 1),
  };
}

type Hit = { kind: 'tab'; bank: Bank } | { kind: 'slot'; k: number } | { kind: 'point'; i: number } | { kind: 'handle'; i: number } | { kind: 'plot' } | null;

export class WarpViz implements Viz {
  cursor = 'default';
  bank: Bank = 'time';
  edit: Record<Bank, number> = { time: 0, vol: 0 };
  hover: Hit = null;
  hoverX: number | null = null;
  dirty = true;
  private m = new Float32Array(12);
  private seen = -1;
  update(_fx: Fx, a: FxFrame | null) {
    let changed = this.dirty;
    this.dirty = false;
    if (a && a.seq !== this.seen) {
      this.seen = a.seq;
      for (let i = 0; i < 12; i++) if (this.m[i] !== a.meters[i]) { this.m[i] = a.meters[i]; changed = true; }
    } else if (!a && this.m[0] !== -1) { this.m.fill(0); this.m[0] = -1; changed = true; }
    return changed;
  }
  /** Slot of `bank` the engine plays / has queued (−1 none); falls back to the parameter. */
  playing(fx: Fx, bank: Bank): number { return this.seen >= 0 && this.m[0] >= 0 ? Math.round(this.m[bank === 'time' ? 1 : 2]) : Math.round(fx.params[bank === 'time' ? 'tslot' : 'vslot'] ?? 0); }
  queued(bank: Bank): number { return this.seen >= 0 ? Math.round(this.m[bank === 'time' ? 3 : 4]) : -1; }

  points(fx: Fx): AutoPoint[] { return fx.maps?.[this.bank][this.edit[this.bank]]?.points ?? []; }

  hit(fx: Fx, x: number, y: number, W: number, H: number): Hit {
    const g = warpGeom(W, H);
    if (y >= g.stripY && y <= g.stripY + g.stripH) {
      if (x >= 8 && x < 8 + g.tabW) return { kind: 'tab', bank: 'time' };
      if (x >= 8 + g.tabW && x < 8 + 2 * g.tabW) return { kind: 'tab', bank: 'vol' };
      if (x >= g.slotsX) { const k = Math.floor((x - g.slotsX) / g.slotW); if (k >= 0 && k < SLOTS) return { kind: 'slot', k }; }
      return null;
    }
    if (x < g.x0 - 8 || x > g.x1 + 8 || y < g.y0 - 8 || y > g.y1 + 8) return null;
    const h = hitEnv(this.points(fx), envGeom(W, H), x, y);
    return h ?? { kind: 'plot' };
  }

  draw(gc: CanvasRenderingContext2D, W: number, H: number, fx: Fx) {
    const g = warpGeom(W, H), G = envGeom(W, H), bank = this.bank, col = bank === 'time' ? FXMETA.warp.color : VOL_COLOR;
    const pts = this.points(fx), slot = fx.maps?.[bank][this.edit[bank]];
    gc.fillStyle = T.viz; gc.fillRect(0, 0, W, H);
    // ---- slot strip
    gc.font = SANS10; gc.textAlign = 'center';
    (['time', 'vol'] as Bank[]).forEach((b, i) => {
      const x = 8 + i * g.tabW, on = b === bank, c = b === 'time' ? FXMETA.warp.color : VOL_COLOR;
      gc.fillStyle = on ? rgba(c, 0.25) : T.vizPanel; gc.fillRect(x, g.stripY, g.tabW - 4, g.stripH);
      gc.strokeStyle = on ? c : T.vizAxis; gc.strokeRect(x + 0.5, g.stripY + 0.5, g.tabW - 5, g.stripH - 1);
      gc.fillStyle = on ? T.ink : T.vizLabel; gc.fillText(tr(b === 'time' ? 'c.fx.warp.time' : 'c.fx.warp.volume'), x + (g.tabW - 4) / 2, g.stripY + 17);
    });
    const play = this.playing(fx, bank), queued = this.queued(bank);
    for (let k = 0; k < SLOTS; k++) {
      const x = g.slotsX + k * g.slotW, w = g.slotW - 3, y = g.stripY;
      gc.fillStyle = k === play ? rgba(col, fx.bypass ? 0.35 : 0.85) : T.vizPanel; gc.fillRect(x, y, w, g.stripH);
      if (k === queued) { gc.strokeStyle = col; gc.lineWidth = 2; gc.strokeRect(x + 1, y + 1, w - 2, g.stripH - 2); gc.lineWidth = 1; }
      if (k === this.edit[bank]) { gc.strokeStyle = T.ink; gc.lineWidth = 1.5; gc.strokeRect(x + 0.75, y + 0.75, w - 1.5, g.stripH - 1.5); gc.lineWidth = 1; }
      if (this.hover?.kind === 'slot' && this.hover.k === k) { gc.fillStyle = rgba(T.ink, 0.08); gc.fillRect(x, y, w, g.stripH); }
      gc.fillStyle = k === play ? T.onFill : T.vizLabel; gc.font = MONO10; gc.fillText(String(k + 1), x + w / 2, y + 17);
    }
    gc.textAlign = 'left';
    // ---- header: slot name, state, speed under the pointer
    gc.font = SANS10; gc.fillStyle = T.ink;
    const running = this.seen >= 0 && this.m[0] >= 0;
    const state = this.edit[bank] === play ? tr(running ? 'c.fx.warp.playing' : 'c.fx.warp.active') : this.edit[bank] === queued ? tr('c.fx.warp.queued') : tr('c.fx.warp.editing');
    // "*": a factory slot that was edited (the slot menu restores it)
    const fac = S.catalog?.warp?.[bank][this.edit[bank]];
    const edited = !!slot && !!fac && fac.name === slot.name && JSON.stringify(fac.points) !== JSON.stringify(slot.points);
    gc.fillText((slot ? tName(slot.name) : '') + (edited ? ' *' : '') + ' · ' + state, g.x0, g.y0 - 9);
    if (bank === 'time' && this.hoverX != null && pts.length) {
      gc.textAlign = 'right'; gc.fillStyle = col; gc.font = MONO10;
      gc.fillText(speedText(speedAt(pts, this.hoverX)), g.x1, g.y0 - 9); gc.textAlign = 'left';
    }
    // ---- grid
    gc.lineWidth = 1; gc.font = MONO9;
    for (let k = 0; k <= 16; k++) {
      const x = Math.round(G.X(k * SNAP_T)) + 0.5;
      gc.strokeStyle = k % 4 === 0 ? T.vizGrid2 : T.vizGrid; gc.beginPath(); gc.moveTo(x, g.y0); gc.lineTo(x, g.y1); gc.stroke();
      if (k % 4 === 0 && k < 16) { gc.fillStyle = T.vizLabel; gc.fillText(String(k / 4 + 1), x + 3, H - 6); }
    }
    if (bank === 'time') {
      for (let b = 0; b <= DEPTH; b++) {
        const y = Math.round(G.Y(1 - b / DEPTH)) + 0.5;
        gc.strokeStyle = b === 4 ? T.vizGrid2 : T.vizGrid; gc.beginPath(); gc.moveTo(g.x0, y); gc.lineTo(g.x1, y); gc.stroke();
        gc.fillStyle = T.vizLabel; gc.textAlign = 'right'; gc.fillText(b === 0 ? tr('c.fx.warp.now') : '−' + b, g.x0 - 5, y + 3); gc.textAlign = 'left';
      }
      // reverse guides (slope −2 beats per beat) and the safety line (slope −1: stopped)
      gc.save(); gc.beginPath(); gc.rect(g.x0, g.y0, g.w, g.h); gc.clip();
      gc.strokeStyle = rgba(T.vizLabel, 0.35);
      for (let k = 0; k < BAR; k++) { gc.beginPath(); gc.moveTo(G.X(k), G.Y(1)); gc.lineTo(G.X(k + 4), G.Y(1 - 8 / DEPTH)); gc.stroke(); }
      gc.strokeStyle = rgba(T.ink, 0.55); gc.setLineDash([4, 3]);
      gc.beginPath(); gc.moveTo(G.X(0), G.Y(1)); gc.lineTo(G.X(BAR), G.Y(1 - BAR / DEPTH)); gc.stroke(); gc.setLineDash([]);
      // what the buffer does not hold yet (after a start or a playhead jump)
      const sec = this.m[10], have = sec * S.project.bpm / 60;
      if (this.m[0] >= 0 && have < DEPTH) {
        const y = G.Y(1 - Math.max(0, have) / DEPTH);
        gc.fillStyle = rgba(T.vizLabel, 0.12); gc.fillRect(g.x0, y, g.w, g.y1 - y);
        gc.fillStyle = T.vizLabel; gc.fillText(tr('c.fx.warp.notBuffered'), g.x0 + 4, Math.min(g.y1 - 4, y + 12));
      }
      gc.restore();
    } else {
      for (const v of [0, 0.25, 0.5, 0.75, 1]) {
        const y = Math.round(G.Y(v)) + 0.5;
        gc.strokeStyle = v === 0.5 ? T.vizGrid2 : T.vizGrid; gc.beginPath(); gc.moveTo(g.x0, y); gc.lineTo(g.x1, y); gc.stroke();
        gc.fillStyle = T.vizLabel; gc.textAlign = 'right'; gc.fillText(Math.round(v * 100) + '%', g.x0 - 5, y + 3); gc.textAlign = 'left';
      }
    }
    gc.strokeStyle = T.vizAxis; gc.strokeRect(g.x0 + 0.5, g.y0 + 0.5, g.w - 1, g.h - 1);
    // ---- the map
    if (pts.length) {
      const n = Math.max(64, Math.round(g.w));
      gc.beginPath();
      for (let i = 0; i <= n; i++) {
        const t = i / n * BAR, v = valueAt(pts, t) ?? 1, x = G.X(t), y = G.Y(v);
        // vertical jumps: a point pair at one time draws straight down/up
        if (i) gc.lineTo(x, y); else gc.moveTo(x, y);
      }
      if (bank === 'vol') {
        gc.lineTo(G.X(BAR), g.y1); gc.lineTo(G.X(0), g.y1); gc.closePath();
        gc.fillStyle = rgba(col, 0.18); gc.fill();
      }
      gc.strokeStyle = fx.bypass ? T.vizLabel : col; gc.lineWidth = 2; gc.stroke(); gc.lineWidth = 1;
      // handles, then points
      for (let i = 1; i < pts.length; i++) {
        if (G.X(pts[i].t) - G.X(pts[i - 1].t) < 18) continue;
        const h = handleAt(pts, i); if (!h) continue;
        const hx = G.X(h.t), hy = G.Y(h.v), on = this.hover?.kind === 'handle' && this.hover.i === i;
        gc.fillStyle = on ? col : T.viz; gc.strokeStyle = col;
        gc.beginPath(); gc.arc(hx, hy, on ? 4.5 : 3.5, 0, Math.PI * 2); gc.fill(); gc.stroke();
      }
      pts.forEach((q, i) => {
        const on = this.hover?.kind === 'point' && this.hover.i === i;
        gc.fillStyle = on ? T.ink : col; gc.beginPath(); gc.arc(G.X(q.t), G.Y(q.v), on ? 5.5 : 4.5, 0, Math.PI * 2); gc.fill();
      });
    }
    // ---- playing position (when the edited slot is the one playing)
    const x = this.m[0];
    if (x >= 0 && this.seen >= 0) {
      const px = Math.round(G.X(clamp(x, 0, BAR))) + 0.5, mine = this.edit[bank] === play;
      gc.strokeStyle = mine ? T.playhead : rgba(T.playhead, 0.35); gc.lineWidth = mine ? 1.5 : 1;
      gc.beginPath(); gc.moveTo(px, g.y0); gc.lineTo(px, g.y1); gc.stroke(); gc.lineWidth = 1;
      if (mine && pts.length) {
        const v = valueAt(pts, x) ?? 1;
        gc.fillStyle = T.playhead; gc.beginPath(); gc.arc(px, G.Y(v), 3.5, 0, Math.PI * 2); gc.fill();
      }
      const hold = Math.round(this.m[9]);
      if (hold) {
        gc.font = SANS10; gc.fillStyle = T.amber; gc.textAlign = 'right';
        gc.fillText(tr(hold === 2 ? 'c.fx.warp.holdLooping' : 'c.fx.warp.holdRecording'), g.x1 - 4, g.y0 + 14); gc.textAlign = 'left';
      }
    }
  }
}

// ---------------------------------------------------------------- editing

const SHAPES: [CurveShape, string][] = [['curve', 'a.pl.shape.curve'], ['double', 'a.pl.shape.double'], ['hold', 'a.pl.shape.hold'], ['stairs', 'a.pl.shape.stairs'],
  ['pulse', 'a.pl.shape.pulse'], ['wave', 'a.pl.shape.wave'], ['halfSine', 'a.pl.shape.halfSine']];

function curMaps(fxId: number): { f: Fx; maps: WarpMaps } | null {
  const f = findFx(fxId)?.f; if (!f || !f.maps) return null;
  return { f, maps: structuredClone(plain(f.maps)) as WarpMaps };
}
/** The op that writes `points` into slot `k` of `bank`. */
function slotOp(fxId: number, bank: Bank, k: number, points: AutoPoint[], name?: string) {
  const c = curMaps(fxId); if (!c || !c.maps[bank][k]) return null;
  c.maps[bank][k] = { name: name ?? c.maps[bank][k].name, points };
  return A.fxOp({ ...plain(c.f), maps: c.maps });
}
const snapT = (t: number, free: boolean) => clamp(free ? t : Math.round(t / SNAP_T) * SNAP_T, 0, BAR);
const snapV = (v: number, bank: Bank, free: boolean) => clamp(free || bank === 'vol' ? v : Math.round(v / SNAP_Y) * SNAP_Y, 0, 1);
function valText(bank: Bank, v: number) { return bank === 'time' ? (v >= 0.9999 ? tr('c.fx.warp.now') : '−' + ((1 - v) * DEPTH).toFixed(2) + ' ' + tr('c.fx.warp.beatsShort')) : Math.round(v * 100) + '%'; }

/** Pointer down on the Warp canvas. `bump` asks for a redraw. */
export function warpDown(e: PointerEvent, cv: HTMLCanvasElement, fxId: number, viz: WarpViz, bump: () => void) {
  const f = findFx(fxId)?.f; if (!f) return;
  const r = cv.getBoundingClientRect(), x = e.clientX - r.left, y = e.clientY - r.top, W = r.width, H = r.height;
  const G = envGeom(W, H), bank = viz.bank, k = viz.edit[bank], h = viz.hit(f, x, y, W, H);
  if (!h) return;
  e.preventDefault();
  if (h.kind === 'tab') { viz.bank = h.bank; viz.dirty = true; bump(); return; }
  if (h.kind === 'slot') {
    const name = tName(f.maps?.[bank][h.k]?.name ?? '');
    if (e.button === 2) { slotMenu(e.clientX, e.clientY, fxId, viz, h.k, bump); return; }
    viz.edit[bank] = h.k; viz.dirty = true; bump();
    // a click also plays it (one undo step), like picking a pattern
    const pid = bank === 'time' ? 'tslot' : 'vslot';
    if (Math.round(f.params[pid] ?? 0) !== h.k) commit([A.fxOp({ ...plain(f), params: { ...plain(f.params), [pid]: h.k } })], tr(bank === 'time' ? 'c.fx.warp.timeSlot' : 'c.fx.warp.volSlot'));
    say(tr(bank === 'time' ? 'c.fx.warp.timeSlot' : 'c.fx.warp.volSlot') + ' ' + (h.k + 1) + ' · ' + name);
    return;
  }
  const pts = viz.points(f); if (!pts.length) return;
  if (h.kind === 'point') {
    if (e.button === 2) { pointMenu(e.clientX, e.clientY, fxId, viz, h.i); return; }
    if (e.altKey || dblCheck('warp' + fxId + bank + k + 'p' + h.i)) {
      const left = deletePoints(pts, new Set([h.i]));
      if (left) { const op = slotOp(fxId, bank, k, left); if (op) commit([op], tr('c.fx.warp.deletePoint')); }
      return;
    }
    dragPoints(e, fxId, viz, pts, h.i, G);
    return;
  }
  if (h.kind === 'handle') {
    if (dblCheck('warp' + fxId + bank + k + 'h' + h.i)) {
      const op = slotOp(fxId, bank, k, pts.map((q, j) => (j === h.i ? { ...q, tension: undefined } : q)).map(stripZero)); if (op) commit([op], tr('a.pl.bend'));
      return;
    }
    const A0 = pts[h.i - 1], B0 = pts[h.i], k0 = B0.tension ?? 0, sy = e.clientY;
    const g = new Gesture(tr('a.pl.bend'));
    track(ev => {
      const kk = dragTension(k0, ev.clientY - sy, A0, B0, ev.ctrlKey || ev.metaKey);
      const op = slotOp(fxId, bank, k, pts.map((q, j) => (j === h.i ? stripZero({ ...q, tension: kk }) : q))); if (op) g.update([op]);
    }, () => g.end(), 'ns-resize');
    return;
  }
  // empty plot: a new point that follows the pointer until release
  const t = snapT(G.T(x), e.altKey), v = snapV(G.V(y), bank, e.altKey);
  const add = addPoint(pts, t, v);
  dragPoints(e, fxId, viz, add.points, add.i, G, true);
}

const stripZero = (q: AutoPoint): AutoPoint => { const o = { ...q }; if (!o.tension) delete o.tension; return o; };

function dragPoints(e: PointerEvent, fxId: number, viz: WarpViz, pts0: AutoPoint[], i: number, G: EnvGeom, added = false) {
  const bank = viz.bank, k = viz.edit[bank], q0 = pts0[i], sx = e.clientX, sy = e.clientY;
  const g = new Gesture(tr(added ? 'c.fx.warp.addPoint' : 'c.fx.warp.movePoint'));
  const at = (ev: PointerEvent) => {
    const free = ev.altKey;
    let t = snapT(G.T(G.X(q0.t) + ev.clientX - sx), free), v = snapV(G.V(G.Y(q0.v) + ev.clientY - sy), bank, free);
    if (ev.shiftKey) v = q0.v; // Shift: keep the value
    if (ev.ctrlKey || ev.metaKey) t = q0.t; // Ctrl: keep the time
    const m = movePoints(pts0, new Set([i]), t - q0.t, v - q0.v);
    const op = slotOp(fxId, bank, k, m.points); if (op) g.update([op]);
    const p = m.points[i];
    say(valText(bank, p.v) + ' · ' + tr('c.fx.warp.atBeat', { b: (p.t + 1).toFixed(2) }));
  };
  at(e);
  track(at, () => g.end(), 'grabbing');
  viz.dirty = true;
}

function pointMenu(cx: number, cy: number, fxId: number, viz: WarpViz, i: number) {
  const f = findFx(fxId)?.f; if (!f) return;
  const bank = viz.bank, k = viz.edit[bank], pts = viz.points(f), q = pts[i];
  const put = (np: AutoPoint[], lab: string) => { const op = slotOp(fxId, bank, k, np); if (op) commit([op], lab); };
  const items: Parameters<typeof openMenu>[2] = [{ t: 'title', label: valText(bank, q.v) + ' · ' + tr('c.fx.warp.atBeat', { b: (q.t + 1).toFixed(2) }) }];
  if (i > 0) {
    items.push({ t: 'sec', label: tr('a.pl.curveType') });
    for (const [s, key] of SHAPES) items.push({ t: 'item', label: tr(key as Parameters<typeof tr>[0]), checked: (q.shape ?? 'curve') === s, act: () => put(setShape(pts, [i], s), tr('a.pl.curveType')) });
    if (bank === 'time') {
      // the segment ending here at a musical speed: slope = 2^(n/12) − 1 beats per beat
      const a = pts[i - 1], dt = q.t - a.t;
      const at = (n: number) => () => { const v = clamp(a.v + (Math.pow(2, n / 12) - 1) * dt / DEPTH, 0, 1); put(pts.map((p, j) => (j === i ? { ...p, v, shape: undefined, tension: undefined } : p)).map(stripZero).map(clean), tr('c.fx.warp.speed')); };
      if (dt > 0) {
        items.push({ t: 'sec', label: tr('c.fx.warp.segSpeed') });
        for (const [n, lab] of [[0, '×1'], [-12, '×0.5 (−12 st)'], [-7, '−7 st'], [-1, '−1 st'], [1, '+1 st']] as [number, string][]) items.push({ t: 'item', label: lab, act: at(n) });
        items.push({ t: 'item', label: tr('c.fx.warp.stopHere'), act: () => put(pts.map((p, j) => (j === i ? clean({ ...p, v: clamp(a.v - dt / DEPTH, 0, 1), shape: undefined, tension: undefined }) : p)), tr('c.fx.warp.speed')) });
        items.push({ t: 'item', label: tr('c.fx.warp.reverseHere'), act: () => put(pts.map((p, j) => (j === i ? clean({ ...p, v: clamp(a.v - 2 * dt / DEPTH, 0, 1), shape: undefined, tension: undefined }) : p)), tr('c.fx.warp.speed')) });
      }
    }
  }
  items.push({ t: 'sep' }, { t: 'item', label: tr('c.fx.warp.deletePoint'), danger: true, disabled: pts.length < 2, act: () => { const left = deletePoints(pts, new Set([i])); if (left) put(left, tr('c.fx.warp.deletePoint')); } });
  openMenu(cx, cy, items, 60 + items.length * 26);
}
const clean = (q: AutoPoint): AutoPoint => { const o = { ...q } as AutoPoint; if (!o.shape) delete o.shape; if (!o.tension) delete o.tension; return o; };

let clip: { bank: Bank; slot: { name: string; points: AutoPoint[] } } | null = null;
function slotMenu(cx: number, cy: number, fxId: number, viz: WarpViz, k: number, bump: () => void) {
  const f = findFx(fxId)?.f; if (!f || !f.maps) return;
  const bank = viz.bank, s = f.maps[bank][k], fac = S.catalog?.warp?.[bank][k];
  const put = (slot: { name: string; points: AutoPoint[] }, lab: string) => { const op = slotOp(fxId, bank, k, structuredClone(plain(slot.points)), slot.name); if (op) commit([op], lab); viz.edit[bank] = k; viz.dirty = true; bump(); };
  // the library: ready-made maps of this bank, one submenu per category
  const lib = S.catalog?.warpLibrary?.[bank] ?? [];
  const cats = [...new Set(lib.map(m => m.cat))];
  const libItems: MenuItem[] = cats.map(c => ({ t: 'sub', label: tName(c), items: lib.filter(m => m.cat === c).map(m => ({ t: 'item', label: tName(m.name), act: () => {
    put({ name: m.name, points: m.points }, tr('c.fx.warp.loadMap'));
    say(tr(bank === 'time' ? 'c.fx.warp.timeSlot' : 'c.fx.warp.volSlot') + ' ' + (k + 1) + ' · ' + tName(m.name));
  } }) as MenuItem) }));
  openMenu(cx, cy, [
    { t: 'title', label: (k + 1) + ' · ' + tName(s?.name ?? '') },
    ...(libItems.length ? [{ t: 'sec', label: tr('c.fx.warp.library') } as MenuItem, ...libItems, { t: 'sep' } as MenuItem] : []),
    { t: 'item', label: tr('c.fx.warp.copySlot'), act: () => { clip = { bank, slot: structuredClone(plain(s)) }; } },
    { t: 'item', label: tr('c.fx.warp.pasteSlot'), disabled: !clip, act: () => { if (clip) put(clip.slot, tr('c.fx.warp.pasteSlot')); } },
    { t: 'item', label: tr('c.fx.warp.clearSlot'), act: () => put({ name: s?.name ?? '', points: [{ t: 0, v: 1 }, { t: BAR, v: 1 }] }, tr('c.fx.warp.clearSlot')) },
    { t: 'item', label: tr('c.fx.warp.restoreSlot'), disabled: !fac, act: () => { if (fac) put(fac, tr('c.fx.warp.restoreSlot')); } },
    { t: 'sep' },
    // switching slots over the song: an automation clip of the slot parameter (hold points)
    { t: 'item', label: tr('c.fx.warp.automateSlots'), act: () => queueMicrotask(() => paramMenu(cx, cy, { kind: 'fx', fxId, param: bank === 'time' ? 'tslot' : 'vslot' }, null, FXMETA.warp.color)) },
  ], 230 + (libItems.length + 2) * 26);
}

/** Pointer move without buttons: hover state and cursor. */
export function warpHover(e: PointerEvent, cv: HTMLCanvasElement, fxId: number, viz: WarpViz) {
  const f = findFx(fxId)?.f; if (!f) return false;
  const r = cv.getBoundingClientRect(), x = e.clientX - r.left, y = e.clientY - r.top;
  const h = viz.hit(f, x, y, r.width, r.height), g = warpGeom(r.width, r.height);
  const hx = h && (h.kind === 'plot' || h.kind === 'point' || h.kind === 'handle') ? clamp((x - g.x0) / g.w * BAR, 0, BAR) : null;
  const same = JSON.stringify(h) === JSON.stringify(viz.hover) && hx === viz.hoverX;
  viz.hover = h; viz.hoverX = hx;
  cv.style.cursor = !h ? 'default' : h.kind === 'point' ? 'grab' : h.kind === 'handle' ? 'ns-resize' : h.kind === 'plot' ? 'crosshair' : 'pointer';
  if (h?.kind === 'slot' && !same) say((h.k + 1) + ' · ' + tName(f.maps?.[viz.bank][h.k]?.name ?? ''));
  return !same;
}
