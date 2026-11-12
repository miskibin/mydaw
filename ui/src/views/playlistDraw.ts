// Canvas rendering of the playlist: lanes, grid, clips (pattern notes / waveforms), the
// recording clip. Only the visible window is drawn; everything is in grid coordinates.
import type { Automation, Channel, Clip, Comp, Id, Note, Pattern, Project, Source, Take, Track } from '../api/types';
import { keyOf } from '../lib/pattern';
import { valueAt } from '../lib/automation';
import { handleAt, HANDLE_MIN_W, type EnvHit } from '../lib/envEdit';
import type { LaneOverlaps } from '../lib/overlaps';
import { boundaries, takeEnd, toComp } from '../lib/comp';
import { mix } from '../lib/color';
import { rgba, T } from '../ui/theme';
import { clamp, dbg, mod } from '../lib/format';
import { snapRes } from '../lib/snap';
import { getClipPeaks, getPeaks, peakRange } from '../state/peaks';
import { prefs } from '../state/prefs';
import { drawGlyph, guessIcon, patternIcon, trackIcon } from '../ui/instIcons';

export interface View {
  offX: number; offY: number; W: number; H: number;
  /** Grid point drawn at the canvas origin when it is not (`offX`, `offY`): a region of a larger
   *  canvas is then painted with the very transform of a full paint (identical antialiasing). */
  orgX?: number; orgY?: number;
  bw: number; snap: number; spb: number;
  tops: number[]; sel: Set<number>; editable: boolean; recArmedBg: boolean;
  /** the automation clip under the pointer and the point / handle there */
  env?: { clip: number; hover: EnvHit | null } | null;
  /** selected envelope points of one automation clip */
  autoSel?: { clip: number; idx: number[] } | null;
  /** per track: the comp whose take lanes are expanded below it (null = none) */
  rows?: ({ clip: Clip; comp: Comp } | null)[];
  laneH?: number;
  /** comp id → auditioned take */
  audition?: Record<string, number>;
  /** clip id → stretch status: being rendered (`''`) or failed (message) */
  stretching?: Record<string, string>;
  /** comp time selection (comp beats) */
  compSel?: { clipId: number; from: number; to: number } | null;
  /** comp boundary under the pointer: clip id + boundary index */
  bndHover?: { clipId: number; i: number } | null;
  /** arrangement version (`arrVersion`): the lookup tables of `drawGrid` are kept while it holds
   *  (the grid is drawn in several strips per frame when it scrolls) */
  ver?: number;
}

interface Lookups { ver: number; pats: Map<Id, Pattern>; chans: Map<Id, Channel>; sigs: Map<Id, string>; srcs: Map<Id, Source>; autos: Map<Id, Automation>; comps: Map<Id, Comp>; patIcons: Map<number, string | null> }
let lookups: Lookups | null = null;
function lookupsOf(p: Project, ver: number | undefined): Lookups {
  if (lookups && ver != null && lookups.ver === ver) return lookups;
  const chans = new Map((p.channels ?? []).map(c => [c.id, c]));
  lookups = {
    ver: ver ?? -1, chans,
    pats: new Map(p.patterns.map(x => [x.id, x])),
    sigs: new Map(p.patterns.map(x => [x.id, patSig(x, chans)])),
    srcs: new Map(p.sources.map(x => [x.id, x])),
    autos: new Map((p.automations ?? []).map(x => [x.id, x])),
    comps: new Map((p.comps ?? []).map(x => [x.id, x])),
    patIcons: new Map(),
  };
  return lookups;
}

const FONT = '500 10px "Geist Variable", Geist, system-ui, sans-serif';
const MONO9 = '500 9px "Geist Mono Variable", "Geist Mono", monospace';

const patCache = new Map<string, Float32Array>();
/** Channels that play more than one key in the pattern (a bass line, chords): the others are one-note parts, i.e. drums. */
function melodic(p: Pattern, chans: Map<Id, Channel>): Set<Id> {
  const first = new Map<Id, number>(), multi = new Set<Id>();
  for (const n of p.notes) {
    const k = keyOf(n, chans.get(n.ch)), f = first.get(n.ch);
    if (f == null) first.set(n.ch, k); else if (f !== k) multi.add(n.ch);
  }
  return multi;
}
/** Row of a note in the clip preview: one row per key of a melodic channel, one row per channel for the rest. */
const rowKey = (n: Note, chans: Map<Id, Channel>, multi: Set<Id>) => (multi.has(n.ch) ? 'p' + keyOf(n, chans.get(n.ch)) : 'c' + n.ch);
const patSig = (p: Pattern, chans: Map<Id, Channel>) => { const multi = melodic(p, chans); return p.notes.map(n => rowKey(n, chans, multi) + '@' + n.step + ':' + n.len).join(','); };
let curChans = new Map<Id, Channel>();
/** Note rectangles [beat, y, lenBeats, h]* of a pattern clip body, cached by geometry (zoom-independent). */
function patRects(p: Pattern, sig: string, offset: number, len: number, h: number): Float32Array {
  const key = p.id + '|' + offset + '|' + len + '|' + h + '|' + p.len + '|' + sig;
  let r = patCache.get(key);
  if (r) return r;
  // notes past the pattern end are kept in the project but hidden (and silent)
  const notes = p.notes.filter(n => n.step < p.len * 4 - 1e-4);
  const order = [...curChans.keys()], multi = melodic(p, curChans);
  const drums = [...new Set(notes.map(n => rowKey(n, curChans, multi)).filter(k => k[0] === 'c'))].sort((a, b) => order.indexOf(+a.slice(1)) - order.indexOf(+b.slice(1)));
  const pitches = [...new Set(notes.map(n => rowKey(n, curChans, multi)).filter(k => k[0] === 'p'))].sort((a, b) => +b.slice(1) - +a.slice(1));
  const rows = [...drums.reverse(), ...pitches];
  const nr = Math.max(rows.length, 3), rh = Math.max(1.5, Math.min(5, (h - 6) / nr));
  const top = (h - nr * rh) / 2, out: number[] = [];
  for (let k = Math.floor(offset / p.len) - 1; k <= Math.ceil((offset + len) / p.len); k++) {
    for (const n of notes) {
      const b = k * p.len + n.step / 4 - offset;
      if (b < 0 || b >= len) continue;
      const ri = rows.indexOf(rowKey(n, curChans, multi));
      out.push(b, top + ri * rh, Math.min(n.len / 4, len - b), Math.max(1, rh - 1));
    }
    if (out.length > 6000) break;
  }
  if (patCache.size > 600) patCache.clear();
  r = Float32Array.from(out);
  patCache.set(key, r);
  return r;
}

export function drawGrid(g: CanvasRenderingContext2D, v: View, p: Project, rec: null | { track: number; clipStart: number; seconds: number; peaks: Float32Array; nPeaks: number; pass?: number }) {
  const { offX, offY, W, H, bw, tops } = v;
  g.save();
  g.translate(-(v.orgX ?? offX), -(v.orgY ?? offY));
  const x0 = offX, x1 = offX + W, y0 = offY, y1 = offY + H;
  // lanes
  p.tracks.forEach((t: Track, i) => {
    const a = tops[i], b = tops[i + 1];
    if (b < y0 || a > y1) return;
    g.fillStyle = t.arm && v.recArmedBg ? T.laneArm : T.grid;
    g.fillRect(x0, a, W, b - a - 1);
    const row = v.rows?.[i], lh = v.laneH ?? 30;
    if (row) {
      // take lanes: a recessed band under the track, one row per take
      g.fillStyle = T.laneAlt; g.fillRect(x0, a + t.h, W, b - a - t.h - 1);
      g.fillStyle = rgba(T.bg0, 0.6);
      for (let k = 0; k <= row.comp.takes.length; k++) g.fillRect(x0, a + t.h + k * lh - 1, W, 1);
    }
    g.fillStyle = T.bg0;
    g.fillRect(x0, b - 1, W, 1);
  });
  const gridBottom = Math.min(y1, tops[tops.length - 1]);
  // 2-bar shading
  const b0 = Math.floor(x0 / bw), b1 = Math.ceil(x1 / bw);
  g.fillStyle = T.gridShade;
  for (let b = Math.floor(b0 / 8) * 8; b <= b1; b += 8) g.fillRect(b * bw, y0, 4 * bw, gridBottom - y0);
  // sub-beat lines follow the active snap
  const sr = snapRes(v.snap, bw), u = sr > 0 && sr < 1 ? sr : 0.25;
  const stepCol = sr > 0 && sr < 1 && sr * bw >= 6 ? T.gridBeat : bw >= 36 ? T.gridBeat : null;
  if (stepCol) {
    g.fillStyle = stepCol;
    for (let k = Math.floor(b0 / u); k * u <= b1; k++) g.fillRect(Math.round(k * u * bw), y0, 1, gridBottom - y0);
  }
  g.fillStyle = T.gridBeat;
  for (let b = b0; b <= b1; b++) if (b % 4) g.fillRect(Math.round(b * bw), y0, 1, gridBottom - y0);
  g.fillStyle = T.gridBar;
  for (let b = Math.floor(b0 / 4) * 4; b <= b1; b += 4) g.fillRect(Math.round(b * bw), y0, 1, gridBottom - y0);
  // (the loop range is shaded on the playlist's overlay canvas: moving it repaints no clip)
  // clips
  const { pats, chans, sigs, srcs, autos, comps, patIcons } = lookupsOf(p, v.ver);
  curChans = chans;
  const iconOf = (c: Clip, tr: Track, src: Pattern | Source | Automation): string | null => {
    if (c.kind === 'auto' || !prefs().clipIcons) return null;
    if (c.kind === 'pat') { let i = patIcons.get(c.ref); if (i === undefined) { i = patternIcon(src as Pattern, curChans); patIcons.set(c.ref, i); } return i; }
    return trackIcon(tr) ?? guessIcon(src.name);
  };
  const anySolo = p.tracks.some(t => t.solo);
  for (const c of p.clips) {
    const tr = p.tracks[c.track]; if (!tr) continue;
    const silent = tr.muted || (anySolo && !tr.solo);
    const cx = c.start * bw, cw = Math.max(3, c.len * bw - 1), cy = tops[c.track] + 1, ch = tr.h - 2;
    if (cx > x1 || cx + cw < x0 || cy > y1 || cy + ch < y0) continue;
    g.globalAlpha = silent ? 0.35 : 1;
    if (c.kind === 'comp') { const cp = comps.get(c.ref); if (cp) drawComp(g, v, c, cp, tr.color, cx, cy, cw, ch, x0, x1); continue; }
    const src = c.kind === 'pat' ? pats.get(c.ref) : c.kind === 'auto' ? autos.get(c.ref) : srcs.get(c.ref);
    if (!src) continue;
    drawClip(g, v, c, src, sigs.get(c.ref) ?? '', cx, cy, cw, ch, x0, x1, iconOf(c, tr, src));
  }
  g.globalAlpha = 1;
  p.tracks.forEach((t, i) => { const row = v.rows?.[i]; if (row) drawTakeLanes(g, v, row.clip, row.comp, t, tops[i] + t.h, x0, x1, y0, y1); });
  if (rec && p.tracks[rec.track]) drawRec(g, v, rec, p.tracks[rec.track].h);
  g.restore();
}

// ---------- take lanes & comps ----------

/** Waveform of `take` between timeline px `xa`..`xb`, centred on `mid`. */
function takeWave(g: CanvasRenderingContext2D, v: View, clip: Clip, t: Take, xa: number, xb: number, mid: number, half: number, gain: number) {
  const pk = getPeaks(t.ref); if (!pk) { g.fillRect(xa, mid, xb - xa, 1); return; }
  const rate = 100 * v.spb, src = (x: number) => (toComp(clip, x / v.bw) - t.start + t.offset) * rate;
  g.beginPath();
  for (let x = Math.floor(xa); x < xb; x++) {
    const i0 = Math.floor(src(x)), i1 = Math.floor(src(x + 1));
    if (i0 * 2 >= pk.length) break;
    if (i1 < 0) continue;
    const [mn, mx] = peakRange(pk, i0, i1);
    const ya = mid - clamp(mx * gain, 0, 1) * half, yb = mid - clamp(mn * gain, -1, 0) * half;
    g.rect(x, ya, 1, Math.max(0.6, yb - ya));
  }
  g.fill();
}

/** Crossfade (s) at a boundary: the region starting there, else the one ending there. */
const xfadeOf = (cp: Comp, b: { left: number; right: number }) => {
  const r = b.right >= 0 ? cp.regions[b.right] : cp.regions[b.left];
  return r?.xfade ?? cp.xfade;
};

/** The comp clip on the track lane: each region's take waveform, boundaries as thin markers. */
function drawComp(g: CanvasRenderingContext2D, v: View, c: Clip, cp: Comp, color: string, cx: number, cy: number, cw: number, ch: number, vx0: number, vx1: number) {
  const sel = v.sel.has(c.id), bh = ch - 15, by = cy + 15, mid = by + bh / 2;
  const X = (cb: number) => (cb - c.offset + c.start) * v.bw;
  const aud = v.audition?.[cp.id], audT = aud != null ? cp.takes.find(t => t.id === aud) : undefined;
  const alpha = c.muted ? 0.38 : 1;
  g.save();
  g.globalAlpha = alpha;
  g.beginPath(); g.roundRect(cx, cy, cw, ch, 3); g.clip();
  g.fillStyle = mix(color, T.clipBodyPct, T.clipBase); g.fillRect(cx, cy, cw, ch);
  g.fillStyle = sel ? mix(color, T.clipHeadSelPct, T.clipHeadSelTo) : mix(color, T.clipHeadPct, T.clipBase); g.fillRect(cx, cy, cw, 15);
  const ink = mix(color, T.clipInkPct, T.clipInkTo);
  const vis0 = Math.max(cx, vx0), vis1 = Math.min(cx + cw, vx1);
  const gain = dbg(c.gain), half = bh / 2 * 0.92;
  const regions = audT ? [{ take: audT.id, from: audT.start, to: takeEnd(audT) }] : cp.regions;
  g.fillStyle = ink;
  for (const r of regions) {
    const t = cp.takes.find(x => x.id === r.take); if (!t) continue;
    const xa = Math.max(vis0, X(r.from)), xb = Math.min(vis1, X(r.to));
    if (xb > xa) takeWave(g, v, c, t, xa, xb, mid, half, gain);
  }
  // which take plays where
  if (!audT && bh >= 24) {
    g.font = MONO9;
    for (const r of regions) {
      // at the region start, not pinned to the view edge: the grid is drawn in strips (playlistTile.ts)
      const xa = X(r.from), xb = X(r.to); if (xb - xa < 28 || xa + 26 < vis0 || xa > vis1) continue;
      const k = cp.takes.findIndex(t => t.id === r.take) + 1, lx = xa + 3;
      g.fillStyle = mix(color, 30, T.clipBase); g.beginPath(); g.roundRect(lx, cy + ch - 13, 20, 11, 2); g.fill();
      g.fillStyle = ink; g.fillText('T' + k, lx + 3, cy + ch - 4.5);
    }
  }
  // boundaries: thin markers, the crossfade width shaded, a tick in the body top
  if (!audT) boundaries(regions).forEach((b, i) => {
    const x = Math.round(X(b.at)); if (x <= cx + 1 || x >= cx + cw - 1 || x < vis0 - 4 || x > vis1 + 4) return;
    const hot = v.bndHover?.clipId === c.id && v.bndHover.i === i;
    const xw = xfadeOf(cp, b) / v.spb * v.bw;
    if (xw >= 3) { g.fillStyle = rgba(T.ink, hot ? 0.2 : 0.1); g.fillRect(x - xw / 2, by, xw, bh); }
    g.fillStyle = hot ? T.ink : rgba(T.ink, 0.5); g.fillRect(x, by, 1, bh);
    g.beginPath(); g.moveTo(x - 3, by); g.lineTo(x + 4, by); g.lineTo(x + 0.5, by + 4); g.closePath(); g.fill();
  });
  if (cw > 14) {
    g.fillStyle = sel ? T.clipTextSel : T.clipText; g.font = FONT;
    let tx = cx + 5;
    if (c.muted) { g.font = '600 9px "Geist Mono Variable", monospace'; g.fillText('M', tx, cy + 11); tx += 10; g.font = FONT; }
    const label = audT ? cp.name + ' · ▶ ' + audT.name : cp.name;
    g.fillText(label, tx, cy + 11);
    const w = g.measureText(label).width;
    g.font = MONO9; g.globalAlpha = alpha * 0.65;
    g.fillText(cp.takes.length + ' tk' + (c.gain ? ' · ' + (c.gain >= 0 ? '+' : '') + c.gain.toFixed(1) + ' dB' : ''), tx + w + 6, cy + 11);
    g.globalAlpha = alpha;
  }
  const s = v.compSel; if (s && s.clipId === c.id) { g.fillStyle = rgba(T.blue, 0.16); g.fillRect(X(s.from), by, X(s.to) - X(s.from), bh); }
  g.restore();
  g.strokeStyle = sel ? T.ink : audT ? T.green : mix(color, 55, T.clipBase);
  g.lineWidth = 1;
  g.globalAlpha = alpha;
  g.beginPath(); g.roundRect(cx + 0.5, cy + 0.5, cw - 1, ch - 1, 3); g.stroke();
  g.globalAlpha = 1;
}

/**
 * One lane per take under the track (Ableton-style): the parts the comp plays are drawn in
 * the track colour, the rest desaturated; outside the clip window everything is dimmed.
 */
function drawTakeLanes(g: CanvasRenderingContext2D, v: View, clip: Clip, cp: Comp, tr: Track, top: number, vx0: number, vx1: number, vy0: number, vy1: number) {
  const lh = v.laneH ?? 30, X = (cb: number) => (cb - clip.offset + clip.start) * v.bw;
  const winA = X(clip.offset), winB = X(clip.offset + clip.len);
  const aud = v.audition?.[cp.id], s = v.compSel && v.compSel.clipId === clip.id ? v.compSel : null;
  const used = mix(tr.color, T.clipBodyPct + 16, T.clipBase), usedInk = mix(tr.color, T.clipInkPct, T.clipInkTo);
  const idle = mix(T.fg5, 9, T.clipBase), idleInk = mix(T.fg4, 55, T.clipBase);
  cp.takes.forEach((t, k) => {
    const y = top + k * lh + 2, h = lh - 5;
    if (y > vy1 || y + h < vy0) return;
    const xa = X(t.start), w = Math.max(3, X(takeEnd(t)) - xa - 1);
    if (xa > vx1 || xa + w < vx0) return;
    const vis0 = Math.max(xa, vx0), vis1 = Math.min(xa + w, vx1), mid = y + h / 2, half = h / 2 * 0.86;
    g.save();
    g.beginPath(); g.roundRect(xa, y, w, h, 3); g.clip();
    g.fillStyle = idle; g.fillRect(xa, y, w, h);
    g.fillStyle = idleInk; takeWave(g, v, clip, t, vis0, vis1, mid, half, 1);
    for (const r of cp.regions) {
      if (r.take !== t.id) continue;
      const ra = Math.max(vis0, X(r.from)), rb = Math.min(vis1, X(r.to)); if (rb <= ra) continue;
      g.fillStyle = used; g.fillRect(ra, y, rb - ra, h);
      g.fillStyle = tr.color; g.fillRect(ra, y, rb - ra, 2);
      g.fillStyle = usedInk; takeWave(g, v, clip, t, ra, rb, mid, half, 1);
    }
    // outside the clip window the take is not heard at all
    g.fillStyle = rgba(T.grid, 0.6);
    if (winA > xa) g.fillRect(xa, y, winA - xa, h);
    if (winB < xa + w) g.fillRect(winB, y, xa + w - winB, h);
    if (s) { g.fillStyle = rgba(T.blue, 0.14); g.fillRect(X(s.from), y, X(s.to) - X(s.from), h); }
    g.restore();
    g.strokeStyle = aud === t.id ? T.green : rgba(T.ink, 0.12); g.lineWidth = 1;
    g.beginPath(); g.roundRect(xa + 0.5, y + 0.5, w - 1, h - 1, 3); g.stroke();
  });
  if (s) {
    const H = cp.takes.length * lh - 1;
    g.fillStyle = rgba(T.blue, 0.75);
    g.fillRect(Math.round(X(s.from)), top, 1, H); g.fillRect(Math.round(X(s.to)) - 1, top, 1, H);
  }
}


/**
 * Fade overlays of the visible audio clips. Drawn on top of the cached grid every time a fade
 * moves, so a fade drag never re-renders waveforms (cost independent of clip length).
 */
export function drawFades(g: CanvasRenderingContext2D, v: View, p: Project) {
  const { offX, offY, W, H, bw, tops } = v;
  g.save(); g.translate(-offX, -offY);
  const x0 = offX, x1 = offX + W, y0 = offY, y1 = offY + H;
  for (const c of p.clips) {
    if (c.kind !== 'audio' && c.kind !== 'comp') continue;
    const tr = p.tracks[c.track]; if (!tr) continue;
    const cx = c.start * bw, cw = Math.max(3, c.len * bw - 1), cy = tops[c.track] + 1, ch = tr.h - 2;
    if (cx > x1 || cx + cw < x0 || cy > y1 || cy + ch < y0) continue;
    const by = cy + 15, bh = ch - 15;
    const fi = Math.min(cw, c.fadeIn / v.spb * bw), fo = Math.min(cw, c.fadeOut / v.spb * bw);
    g.save();
    if (c.muted) g.globalAlpha = 0.38;
    g.beginPath(); g.roundRect(cx, cy, cw, ch, 3); g.clip();
    if (fi || fo) {
      g.fillStyle = T.shade; g.beginPath();
      if (fi) { g.moveTo(cx, by + bh); g.lineTo(cx + fi, by); g.lineTo(cx, by); g.closePath(); }
      if (fo) { g.moveTo(cx + cw - fo, by); g.lineTo(cx + cw, by + bh); g.lineTo(cx + cw, by); g.closePath(); }
      g.fill();
      g.strokeStyle = rgba(T.ink, 0.7); g.lineWidth = 1; g.beginPath();
      if (fi) { g.moveTo(cx, by + bh); g.lineTo(cx + fi, by); }
      if (fo) { g.moveTo(cx + cw - fo, by); g.lineTo(cx + cw, by + bh); }
      g.stroke();
    }
    if (v.editable && cw > 36) {
      g.fillStyle = rgba(T.ink, 0.85);
      g.beginPath(); g.roundRect(cx + fi - 1, by, 9, 9, [0, 0, 3, 0]); g.fill();
      g.beginPath(); g.roundRect(cx + cw - fo - 8, by, 9, 9, [0, 0, 0, 3]); g.fill();
    }
    g.restore();
  }
  g.restore();
}

/**
 * Overlapping clips, on the overlay: an amber hatch along the header where clips of a track
 * play at once, and "×N" on a clip that hides N − 1 clips entirely (a stacked duplicate).
 */
export function drawOverlaps(g: CanvasRenderingContext2D, v: View, p: Project, o: LaneOverlaps) {
  if (!o.spans.length && !o.covers.size) return;
  const { offX, offY, W, H, bw, tops } = v;
  const x0 = offX, x1 = offX + W, y0 = offY, y1 = offY + H;
  g.save(); g.translate(-offX, -offY);
  g.lineWidth = 1;
  for (const s of o.spans) {
    const tr = p.tracks[s.track], top = tops[s.track]; if (!tr || top == null) continue;
    const sx = s.a * bw, ex = s.b * bw, cy = top + 1;
    if (ex < x0 || sx > x1 || cy + 15 < y0 || cy > y1) continue;
    const w = Math.max(2, ex - sx);
    g.save();
    g.beginPath(); g.rect(sx, cy, w, 15); g.clip();
    g.strokeStyle = rgba(T.amber, 0.55); g.beginPath();
    for (let x = sx - 15; x < sx + w; x += 5) { g.moveTo(x, cy + 15); g.lineTo(x + 15, cy); }
    g.stroke();
    g.fillStyle = rgba(T.amber, 0.9); g.fillRect(sx, cy, w, 2);
    g.restore();
  }
  g.font = MONO9; g.textBaseline = 'middle'; g.textAlign = 'center';
  for (const c of p.clips) {
    const n = o.covers.get(c.id); if (!n) continue;
    const tr = p.tracks[c.track], top = tops[c.track]; if (!tr || top == null) continue;
    const cx = c.start * bw, cw = Math.max(3, c.len * bw - 1), cy = top + 1;
    if (cx > x1 || cx + cw < x0 || cy > y1 || cy + 15 < y0) continue;
    const label = '\u00d7' + (n + 1), bwid = 6 + label.length * 6;
    if (cw < bwid + 4) continue; // too narrow: the hatch above still shows the stack
    const bx = cx + cw - bwid - 2;
    g.fillStyle = T.amber; g.beginPath(); g.roundRect(bx, cy + 2, bwid, 11, 3); g.fill();
    g.fillStyle = T.onFill; g.fillText(label, bx + bwid / 2, cy + 8);
  }
  g.restore();
}

function drawClip(g: CanvasRenderingContext2D, v: View, c: Clip, src: Pattern | Source | Automation, sig: string, cx: number, cy: number, cw: number, ch: number, vx0: number, vx1: number, icon: string | null = null) {
  const sel = v.sel.has(c.id), color = src.color, bh = ch - 15, prevAlpha = g.globalAlpha;
  g.save();
  if (c.muted) g.globalAlpha = 0.38;
  g.beginPath(); g.roundRect(cx, cy, cw, ch, 3); g.clip();
  g.fillStyle = mix(color, T.clipBodyPct, T.clipBase); g.fillRect(cx, cy, cw, ch);
  // header
  g.fillStyle = sel ? mix(color, T.clipHeadSelPct, T.clipHeadSelTo) : mix(color, T.clipHeadPct, T.clipBase); g.fillRect(cx, cy, cw, 15);
  const ink = mix(color, T.clipInkPct, T.clipInkTo);
  const isAudio = c.kind === 'audio';
  const vis0 = Math.max(cx, vx0), vis1 = Math.min(cx + cw, vx1);
  if (cw > 14) {
    g.fillStyle = sel ? T.clipTextSel : T.clipText;
    let tx = cx + 5;
    if (c.muted) { g.font = '600 9px "Geist Mono Variable", monospace'; g.fillText('M', tx, cy + 11); tx += 10; }
    if (icon && cw > 30) { drawGlyph(g, icon, tx, cy + 2.5, 10); tx += 13; }
    g.font = FONT; g.fillText(src.name, tx, cy + 11);
    let ax = tx + g.measureText(src.name).width + 5;
    const st = isAudio ? c.stretch ?? 1 : 1;
    if (st !== 1) {
      // the stretch factor; '…' while the pitch-preserving render is not ready (the clip plays
      // resampled meanwhile), '!' when it failed; 'rs' = resampled on purpose (pitch follows)
      const status = v.stretching?.[c.id];
      const label = '×' + st.toFixed(2) + (c.resample ? ' rs' : status === '' ? ' …' : status != null ? ' !' : '');
      g.font = MONO9;
      if (status) g.fillStyle = T.red;
      g.fillText(label, ax, cy + 11);
      g.fillStyle = sel ? T.clipTextSel : T.clipText;
      ax += g.measureText(label).width + 5;
    }
    if (isAudio && c.gain) {
      g.font = MONO9; g.globalAlpha *= 0.65; g.fillText((c.gain >= 0 ? '+' : '') + c.gain.toFixed(1) + ' dB', ax, cy + 11);
      g.globalAlpha = c.muted ? 0.38 : prevAlpha;
    }
  }
  const by = cy + 15;
  if (c.kind === 'auto') {
    drawEnvelope(g, v, c, src as Automation, cx, by, cw, bh, vis0, vis1, color, ink);
  } else if (!isAudio) {
    const pat = src as Pattern;
    const r = patRects(pat, sig, mod(c.offset, pat.len), c.len, bh), bw = v.bw;
    g.fillStyle = ink; g.beginPath();
    for (let i = 0; i < r.length; i += 4) {
      const x = cx + r[i] * bw, w = Math.max(1.5, r[i + 2] * bw - (r[i + 2] * bw < (c.len - r[i]) * bw ? 1 : 0));
      if (x > vis1 || x + w < vis0) continue;
      g.rect(x, by + r[i + 1], w, r[i + 3]);
    }
    g.fill();
  } else {
    const pk = getClipPeaks(c);
    // peaks are in source time: a stretched clip shows them stretched
    const mid = by + bh / 2, gain = dbg(c.gain), rate = 100 * v.spb / (c.stretch ?? 1);
    if (pk) {
      g.fillStyle = ink; g.beginPath();
      const xs = Math.floor(vis0 - cx), xe = Math.ceil(vis1 - cx);
      for (let x = Math.max(0, xs); x < xe; x++) {
        const i0 = Math.floor((c.offset + x / v.bw) * rate), i1 = Math.floor((c.offset + (x + 1) / v.bw) * rate);
        if (i0 * 2 >= pk.length) break;
        const [mn, mx] = peakRange(pk, i0, i1);
        const ya = mid - clamp(mx * gain, 0, 1) * bh / 2 * 0.92, yb = mid - clamp(mn * gain, -1, 0) * bh / 2 * 0.92;
        g.rect(cx + x, ya, 1, Math.max(0.6, yb - ya));
      }
      g.fill();
    } else { g.fillStyle = mix(color, 30, T.clipBase); g.fillRect(cx, mid, cw, 1); }
  }
  g.restore();
  // ring
  g.strokeStyle = sel ? T.ink : mix(color, 55, T.clipBase);
  g.lineWidth = 1;
  if (c.muted) g.globalAlpha = 0.38;
  g.beginPath(); g.roundRect(cx + 0.5, cy + 0.5, cw - 1, ch - 1, 3); g.stroke();
  g.globalAlpha = 1;
}

/** Automation envelope inside a clip: filled area below the curve (sampled per pixel, exactly as
 *  it plays), its points, and on the clip under the pointer or with selected points, the tension
 *  handles between them. */
function drawEnvelope(g: CanvasRenderingContext2D, v: View, c: Clip, a: Automation, cx: number, by: number, cw: number, bh: number, vis0: number, vis1: number, color: string, ink: string) {
  const pts = a.points, pad = 3, Y = (val: number) => by + pad + (1 - val) * (bh - 2 * pad);
  const X = (t: number) => cx + (t - c.offset) * v.bw, Tt = (x: number) => c.offset + (x - cx) / v.bw;
  const val = (t: number) => valueAt(pts, t) ?? 0.5;
  const x0 = Math.floor(Math.max(vis0, cx)), x1 = Math.ceil(Math.min(vis1, cx + cw));
  if (x1 <= x0) return;
  // the curve: one vertex per pixel, plus both sides of every point (holds and stairs jump there)
  const line: [number, number][] = [];
  let k = 0; while (k < pts.length && X(pts[k].t) < x0) k++;
  for (let x = x0; x <= x1; x++) {
    while (k < pts.length && X(pts[k].t) <= x) { const px = X(pts[k].t); line.push([px, Y(val(pts[k].t - 1e-9))], [px, Y(pts[k].v)]); k++; }
    line.push([x, Y(val(Tt(x)))]);
  }
  g.beginPath(); g.moveTo(x0, by + bh);
  for (const [x, y] of line) g.lineTo(x, y);
  g.lineTo(x1, by + bh); g.closePath();
  g.fillStyle = rgba(color, 0.22); g.fill();
  g.beginPath(); line.forEach(([x, y], i) => (i ? g.lineTo(x, y) : g.moveTo(x, y)));
  g.strokeStyle = ink; g.lineWidth = 1.5; g.stroke();
  const focus = v.env?.clip === c.id ? v.env : null;
  const sel = v.autoSel?.clip === c.id ? new Set(v.autoSel.idx) : null;
  const hov = focus?.hover;
  if (bh < 14) return; // too low to grab anything: the curve alone
  // handles of the focused clip: hollow dots halfway along each bendable segment
  if (focus || sel) {
    for (let i = 1; i < pts.length; i++) {
      if (X(pts[i].t) - X(pts[i - 1].t) < HANDLE_MIN_W) continue;
      const h = handleAt(pts, i); if (!h) continue;
      const hx = X(h.t); if (hx < vis0 - 5 || hx > vis1 + 5) continue;
      const on = hov?.kind === 'handle' && hov.i === i;
      g.beginPath(); g.arc(hx, Y(h.v), on ? 4 : 3, 0, Math.PI * 2);
      g.fillStyle = on ? T.ink : T.clipBase; g.fill();
      g.strokeStyle = on ? T.ink : ink; g.lineWidth = 1.25; g.stroke();
    }
  }
  // points: always where there is room for them, bigger on the focused clip
  if (!focus && !sel && cw / Math.max(1, pts.length) <= 8) return;
  pts.forEach((q, i) => {
    const x = X(q.t); if (x < vis0 - 5 || x > vis1 + 5) return;
    const on = hov?.kind === 'point' && hov.i === i, picked = !!sel?.has(i);
    g.beginPath(); g.arc(x, Y(q.v), on || picked ? 4.5 : focus ? 3.5 : 2.5, 0, Math.PI * 2);
    g.fillStyle = picked ? T.amber : on ? T.ink : ink; g.fill();
    if (focus || picked) { g.strokeStyle = T.clipBase; g.lineWidth = 1.25; g.stroke(); }
  });
}

function drawRec(g: CanvasRenderingContext2D, v: View, r: { track: number; clipStart: number; seconds: number; peaks: Float32Array; nPeaks: number; pass?: number }, trackH: number) {
  const h = trackH - 2, bh = h - 15, lenB = Math.max(0, r.seconds / v.spb);
  const x = r.clipStart * v.bw, y = v.tops[r.track] + 1, w = Math.max(2, lenB * v.bw);
  g.save();
  g.beginPath(); g.roundRect(x, y, w, h, 3); g.clip();
  g.fillStyle = rgba(T.red, 0.14); g.fillRect(x, y, w, h);
  g.fillStyle = T.red; g.fillRect(x, y, w, 15);
  g.fillStyle = T.onFill; g.font = '600 10px "Geist Variable", sans-serif';
  g.fillText('● REC ' + (r.pass ? 'take ' + (r.pass + 1) + ' · ' : '') + (lenB > 0 ? (lenB * v.spb).toFixed(1) + ' s' : 'count-in'), x + 5, y + 11);
  if (r.nPeaks) {
    const rate = 100 * v.spb, mid = y + 15 + bh / 2;
    g.fillStyle = rgba(T.red, 0.8); g.beginPath();
    const vx0 = Math.max(0, v.offX - x), vx1 = Math.min(w, v.offX + v.W - x);
    for (let px = Math.floor(vx0); px < vx1; px++) {
      const i0 = Math.floor(px / v.bw * rate), i1 = Math.floor((px + 1) / v.bw * rate);
      if (i0 >= r.nPeaks) break;
      const [mn, mx] = peakRange(r.peaks.subarray(0, r.nPeaks * 2), i0, i1);
      const ya = mid - clamp(mx, 0, 1) * bh / 2 * 0.92, yb = mid - clamp(mn, -1, 0) * bh / 2 * 0.92;
      g.rect(x + px, ya, 1, Math.max(0.6, yb - ya));
    }
    g.fill();
  }
  g.restore();
  g.strokeStyle = T.red; g.lineWidth = 1; g.beginPath(); g.roundRect(x + 0.5, y + 0.5, w - 1, h - 1, 3); g.stroke();
}

export function drawRuler(g: CanvasRenderingContext2D, offX: number, W: number, bw: number, loop: { a: number; b: number } | null, start: number) {
  g.fillStyle = T.rulerBg; g.fillRect(0, 0, W, 26);
  g.save(); g.translate(-offX, 0);
  const x0 = offX, x1 = offX + W;
  if (loop) {
    g.fillStyle = rgba(T.green, 0.16); g.fillRect(loop.a * bw, 0, (loop.b - loop.a) * bw, 26);
    g.fillStyle = T.green; g.fillRect(Math.round(loop.a * bw), 0, 1, 26); g.fillRect(Math.round(loop.b * bw) - 1, 0, 1, 26);
  }
  // beat ticks along the bottom
  g.fillStyle = T.fg6;
  for (let b = Math.floor(x0 / bw); b * bw <= x1; b++) g.fillRect(Math.round(b * bw), 21, 1, 5);
  const barStep = bw * 4 < 36 ? 4 : bw * 4 < 64 ? 2 : 1;
  g.font = '500 10px "Geist Mono Variable", "Geist Mono", monospace';
  for (let bar = Math.floor(x0 / (4 * bw) / barStep) * barStep; bar * 4 * bw <= x1; bar += barStep) {
    const x = Math.round(bar * 4 * bw);
    g.fillStyle = T.fg6; g.fillRect(x, 0, 1, 26);
    g.fillStyle = T.rulerText; g.fillText(String(bar + 1), x + 4, 15);
  }
  // start marker
  const sx = start * bw;
  g.fillStyle = T.green; g.beginPath(); g.moveTo(sx - 5, 0); g.lineTo(sx + 5, 0); g.lineTo(sx, 7); g.closePath(); g.fill();
  g.restore();
  g.fillStyle = T.bg0; g.fillRect(0, 25, W, 1);
}
