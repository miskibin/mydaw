// Canvas rendering of the playlist: lanes, grid, clips (pattern notes / waveforms), the
// recording clip. Only the visible window is drawn; everything is in grid coordinates.
import type { Clip, Pattern, Project, Source, Track } from '../api/types';
import { mix } from '../lib/color';
import { clamp, dbg, mod } from '../lib/format';
import { snapRes } from '../lib/snap';
import { getPeaks, peakRange } from '../state/peaks';

export interface View {
  offX: number; offY: number; W: number; H: number;
  bw: number; snap: number; spb: number;
  tops: number[]; sel: Set<number>; editable: boolean; recArmedBg: boolean;
}

const FONT = '500 10px "Geist Variable", Geist, system-ui, sans-serif';
const MONO9 = '500 9px "Geist Mono Variable", "Geist Mono", monospace';

const patCache = new Map<string, Float32Array>();
/** Note rectangles [x,y,w,h]* of a pattern clip body, cached by geometry. */
function patRects(p: Pattern, offset: number, len: number, bw: number, h: number): Float32Array {
  const key = p.id + '|' + offset + '|' + len + '|' + bw + '|' + h + '|' + p.len + '|' + p.notes.length + '|' + (p.notes[0]?.step ?? '');
  let r = patCache.get(key);
  if (r) return r;
  const drums = ['hat', 'clap', 'kick'];
  const pitches = [...new Set(p.notes.filter(n => n.p != null).map(n => n.p!))].sort((a, b) => b - a);
  const rows = [...drums.filter(d => p.notes.some(n => n.inst === d)), ...pitches.map(x => 'p' + x)];
  const nr = Math.max(rows.length, 3), rh = Math.max(1.5, Math.min(5, (h - 6) / nr));
  const top = (h - nr * rh) / 2, W = len * bw, out: number[] = [];
  for (let k = Math.floor(offset / p.len) - 1; k <= Math.ceil((offset + len) / p.len); k++) {
    for (const n of p.notes) {
      const b = k * p.len + n.step / 4 - offset;
      if (b < 0 || b >= len) continue;
      const ri = rows.indexOf(n.p != null ? 'p' + n.p : n.inst);
      out.push(b * bw, top + ri * rh, Math.max(1.5, Math.min(n.len / 4 * bw - 1, W - b * bw)), Math.max(1, rh - 1));
    }
    if (out.length > 6000) break;
  }
  if (patCache.size > 600) patCache.clear();
  r = Float32Array.from(out);
  patCache.set(key, r);
  return r;
}

export function drawGrid(g: CanvasRenderingContext2D, v: View, p: Project, rec: null | { track: number; clipStart: number; seconds: number; peaks: Float32Array; nPeaks: number }) {
  const { offX, offY, W, H, bw, tops } = v;
  g.save();
  g.translate(-offX, -offY);
  const x0 = offX, x1 = offX + W, y0 = offY, y1 = offY + H;
  // lanes
  p.tracks.forEach((t: Track, i) => {
    const a = tops[i], b = tops[i + 1];
    if (b < y0 || a > y1) return;
    g.fillStyle = t.arm && v.recArmedBg ? '#16121a' : '#111419';
    g.fillRect(x0, a, W, b - a - 1);
    g.fillStyle = '#0b0d10';
    g.fillRect(x0, b - 1, W, 1);
  });
  const gridBottom = Math.min(y1, tops[tops.length - 1]);
  // 2-bar shading
  const b0 = Math.floor(x0 / bw), b1 = Math.ceil(x1 / bw);
  g.fillStyle = 'rgba(255,255,255,.018)';
  for (let b = Math.floor(b0 / 8) * 8; b <= b1; b += 8) g.fillRect(b * bw, y0, 4 * bw, gridBottom - y0);
  // sub-beat lines follow the active snap
  const sr = snapRes(v.snap, bw), u = sr > 0 && sr < 1 ? sr : 0.25;
  const stepCol = sr > 0 && sr < 1 && sr * bw >= 6 ? '#1b2129' : bw >= 36 ? '#161b21' : null;
  if (stepCol) {
    g.fillStyle = stepCol;
    for (let k = Math.floor(b0 / u); k * u <= b1; k++) g.fillRect(Math.round(k * u * bw), y0, 1, gridBottom - y0);
  }
  g.fillStyle = '#1a1f27';
  for (let b = b0; b <= b1; b++) if (b % 4) g.fillRect(Math.round(b * bw), y0, 1, gridBottom - y0);
  g.fillStyle = '#2a313b';
  for (let b = Math.floor(b0 / 4) * 4; b <= b1; b += 4) g.fillRect(Math.round(b * bw), y0, 1, gridBottom - y0);
  // loop
  if (p.loop) { g.fillStyle = 'rgba(78,192,138,.05)'; g.fillRect(p.loop.a * bw, y0, (p.loop.b - p.loop.a) * bw, gridBottom - y0); }
  // clips
  const pats = new Map(p.patterns.map(x => [x.id, x]));
  const srcs = new Map(p.sources.map(x => [x.id, x]));
  for (const c of p.clips) {
    const tr = p.tracks[c.track]; if (!tr) continue;
    const cx = c.start * bw, cw = Math.max(3, c.len * bw - 1), cy = tops[c.track] + 1, ch = tr.h - 2;
    if (cx > x1 || cx + cw < x0 || cy > y1 || cy + ch < y0) continue;
    const src = c.kind === 'pat' ? pats.get(c.ref) : srcs.get(c.ref);
    if (!src) continue;
    drawClip(g, v, c, src, cx, cy, cw, ch, x0, x1);
  }
  if (rec && p.tracks[rec.track]) drawRec(g, v, rec, p.tracks[rec.track].h);
  g.restore();
}

function drawClip(g: CanvasRenderingContext2D, v: View, c: Clip, src: Pattern | Source, cx: number, cy: number, cw: number, ch: number, vx0: number, vx1: number) {
  const sel = v.sel.has(c.id), color = src.color, bh = ch - 15;
  g.save();
  if (c.muted) g.globalAlpha = 0.38;
  g.beginPath(); g.roundRect(cx, cy, cw, ch, 3); g.clip();
  g.fillStyle = mix(color, 14, '#101318'); g.fillRect(cx, cy, cw, ch);
  // header
  g.fillStyle = sel ? mix(color, 75, '#ffffff') : mix(color, 42, '#12161c'); g.fillRect(cx, cy, cw, 15);
  const ink = mix(color, 80, '#ffffff');
  const isAudio = c.kind === 'audio';
  const vis0 = Math.max(cx, vx0), vis1 = Math.min(cx + cw, vx1);
  if (cw > 14) {
    g.fillStyle = sel ? '#0b1220' : '#f2f4f7';
    let tx = cx + 5;
    if (c.muted) { g.font = '600 9px "Geist Mono Variable", monospace'; g.fillText('M', tx, cy + 11); tx += 10; }
    g.font = FONT; g.fillText(src.name, tx, cy + 11);
    if (isAudio && c.gain) {
      const w = g.measureText(src.name).width;
      g.font = MONO9; g.globalAlpha *= 0.65; g.fillText((c.gain >= 0 ? '+' : '') + c.gain.toFixed(1) + ' dB', tx + w + 5, cy + 11);
      g.globalAlpha = c.muted ? 0.38 : 1;
    }
  }
  const by = cy + 15;
  if (!isAudio) {
    const pat = src as Pattern;
    const r = patRects(pat, mod(c.offset, pat.len), c.len, v.bw, bh);
    g.fillStyle = ink; g.beginPath();
    for (let i = 0; i < r.length; i += 4) {
      const x = cx + r[i]; if (x > vis1 || x + r[i + 2] < vis0) continue;
      g.rect(x, by + r[i + 1], r[i + 2], r[i + 3]);
    }
    g.fill();
  } else {
    const pk = getPeaks(c.ref, c.norm, c.rev);
    const mid = by + bh / 2, gain = dbg(c.gain), rate = 100 * v.spb;
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
    } else { g.fillStyle = mix(color, 30, '#101318'); g.fillRect(cx, mid, cw, 1); }
    const fi = c.fadeIn / v.spb * v.bw, fo = c.fadeOut / v.spb * v.bw;
    if (fi || fo) {
      g.fillStyle = 'rgba(0,0,0,.5)'; g.beginPath();
      if (fi) { g.moveTo(cx, by + bh); g.lineTo(cx + fi, by); g.lineTo(cx, by); g.closePath(); }
      if (fo) { g.moveTo(cx + cw - fo, by); g.lineTo(cx + cw, by + bh); g.lineTo(cx + cw, by); g.closePath(); }
      g.fill();
      g.strokeStyle = 'rgba(242,244,247,.7)'; g.lineWidth = 1; g.beginPath();
      if (fi) { g.moveTo(cx, by + bh); g.lineTo(cx + fi, by); }
      if (fo) { g.moveTo(cx + cw - fo, by); g.lineTo(cx + cw, by + bh); }
      g.stroke();
    }
    if (v.editable && cw > 36) {
      g.fillStyle = 'rgba(242,244,247,.85)';
      g.beginPath(); g.roundRect(cx + fi - 1, by, 9, 9, [0, 0, 3, 0]); g.fill();
      g.beginPath(); g.roundRect(cx + cw - fo - 8, by, 9, 9, [0, 0, 0, 3]); g.fill();
    }
  }
  g.restore();
  // ring
  g.strokeStyle = sel ? '#f2f4f7' : mix(color, 55, '#101318');
  g.lineWidth = 1;
  if (c.muted) g.globalAlpha = 0.38;
  g.beginPath(); g.roundRect(cx + 0.5, cy + 0.5, cw - 1, ch - 1, 3); g.stroke();
  g.globalAlpha = 1;
}

function drawRec(g: CanvasRenderingContext2D, v: View, r: { track: number; clipStart: number; seconds: number; peaks: Float32Array; nPeaks: number }, trackH: number) {
  const h = trackH - 2, bh = h - 15, lenB = Math.max(0, r.seconds / v.spb);
  const x = r.clipStart * v.bw, y = v.tops[r.track] + 1, w = Math.max(2, lenB * v.bw);
  g.save();
  g.beginPath(); g.roundRect(x, y, w, h, 3); g.clip();
  g.fillStyle = 'rgba(224,82,99,.14)'; g.fillRect(x, y, w, h);
  g.fillStyle = '#e05263'; g.fillRect(x, y, w, 15);
  g.fillStyle = '#12151a'; g.font = '600 10px "Geist Variable", sans-serif';
  g.fillText('● REC ' + (lenB > 0 ? (lenB * v.spb).toFixed(1) + ' s' : 'count-in'), x + 5, y + 11);
  if (r.nPeaks) {
    const rate = 100 * v.spb, mid = y + 15 + bh / 2;
    g.fillStyle = '#f08a96'; g.beginPath();
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
  g.strokeStyle = '#e05263'; g.lineWidth = 1; g.beginPath(); g.roundRect(x + 0.5, y + 0.5, w - 1, h - 1, 3); g.stroke();
}

export function drawRuler(g: CanvasRenderingContext2D, offX: number, W: number, bw: number, loop: { a: number; b: number } | null, start: number) {
  g.fillStyle = '#171b21'; g.fillRect(0, 0, W, 26);
  g.save(); g.translate(-offX, 0);
  const x0 = offX, x1 = offX + W;
  if (loop) {
    g.fillStyle = 'rgba(78,192,138,.16)'; g.fillRect(loop.a * bw, 0, (loop.b - loop.a) * bw, 26);
    g.fillStyle = '#4ec08a'; g.fillRect(Math.round(loop.a * bw), 0, 1, 26); g.fillRect(Math.round(loop.b * bw) - 1, 0, 1, 26);
  }
  // beat ticks along the bottom
  g.fillStyle = '#3d4653';
  for (let b = Math.floor(x0 / bw); b * bw <= x1; b++) g.fillRect(Math.round(b * bw), 21, 1, 5);
  const barStep = bw * 4 < 36 ? 4 : bw * 4 < 64 ? 2 : 1;
  g.font = '500 10px "Geist Mono Variable", "Geist Mono", monospace';
  for (let bar = Math.floor(x0 / (4 * bw) / barStep) * barStep; bar * 4 * bw <= x1; bar += barStep) {
    const x = Math.round(bar * 4 * bw);
    g.fillStyle = '#4a5360'; g.fillRect(x, 0, 1, 26);
    g.fillStyle = '#9aa2ad'; g.fillText(String(bar + 1), x + 4, 15);
  }
  // start marker
  const sx = start * bw;
  g.fillStyle = '#4ec08a'; g.beginPath(); g.moveTo(sx - 5, 0); g.lineTo(sx + 5, 0); g.lineTo(sx, 7); g.closePath(); g.fill();
  g.restore();
  g.fillStyle = '#0a0c0f'; g.fillRect(0, 25, W, 1);
}
