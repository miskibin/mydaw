import { prefs } from '../state/prefs';
import { t as tl } from '../lib/i18n';
import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from 'solid-js';
import { unwrap } from 'solid-js/store';
import { InstIcon, trackIcon } from '../ui/instIcons';
import { STRETCH_MAX, STRETCH_MIN, VOL_UNITY, type Automation, type AutoPoint, type Channel, type Clip, type Comp, type CurveShape, type Id, type Op, type Take, type Track } from '../api/types';
import { boundaries, boundaryXfade, moveBoundary, setBoundaryXfade, takeEnd, toComp, toTimeline, unusedTakes, usedBeats, writeRange } from '../lib/comp';
import * as CA from '../state/comp';
import { compById, LANE_H, takeRow, type TakeRow } from '../state/comp';
import { clampMove, moveClips, resizeClip, resizeClips, stretchClip, stretchClips, TOTAL } from '../lib/clipOps';
import { clamp, fmtPos, volText } from '../lib/format';
import { snapFloor, snapRes, snapRound } from '../lib/snap';
import * as A from '../state/actions';
import { commit, Gesture, wheelEdit } from '../state/edit';
import { peaksVersion } from '../state/peaks';
import { allocId, anyTrackSolo, arrVersion, clipSrcLen, onArrangementChange, plain, S, srcLenBeats, srcOf } from '../state/store';
import { curBeat, recState, tele } from '../state/telemetry';
import { closeMenu, openMenu, openRename, say, setU, U, type MenuItem } from '../state/ui';
import { dblCheck, dragCursor, edgeScroller, panDrag, track } from '../ui/drag';
import { fitSpan, zoomScroll, zoomToRange } from '../lib/viewNav';
import { barsText } from '../lib/pattern';
import { addFrameTask, fitCanvas, fonts } from '../ui/frameLoop';
import { drawFades, drawGrid, drawOverlaps, type View } from './playlistDraw';
import { drawRuler, rulerCursor, rulerDown as rulerPress, RULER_H, type RulerHost } from '../ui/timeRuler';
import { ZoomScrollbar } from '../ui/ZoomScrollbar';
import type { ZRange } from '../lib/zoomBar';
import { laneOverlaps } from '../lib/overlaps';
import { damageRects, followScroll, planTile, snapPx, type Rect, type Tile } from './playlistTile';
import { autoDisplay, isAutomated, paramMenu } from '../state/automation';
import { autoValueText, parseAutoValue, cyclesOf, stairsOf } from '../lib/automation';
import { addPoint, deletePoints, dragTension, hitEnv, movePoints, pointsInBox, setShape, type EnvHit } from '../lib/envEdit';
import { autoById } from '../state/store';
import { openPattern } from '../state/pattern';
import { rgba, T, theme } from '../ui/theme';


/** Shared geometry, so the browser and OS drops can find a beat/track under the pointer. */
export const geom = { grid: null as HTMLDivElement | null, scroll: null as HTMLDivElement | null, tops: [0] as number[] };

function trackAtY(y: number): number {
  const t = geom.tops; if (y < 0) return -1;
  for (let i = 0; i < t.length - 1; i++) if (y < t[i + 1]) return i;
  return -1;
}
function trackAtYClamped(y: number): number {
  const n = S.project.tracks.length; if (y < 0) return 0;
  const i = trackAtY(y); return i < 0 ? n - 1 : i;
}
export function gridPoint(clientX: number, clientY: number) {
  const r = geom.grid!.getBoundingClientRect(), x = clientX - r.left, y = clientY - r.top;
  const sr = geom.scroll!.getBoundingClientRect();
  const inside = clientX >= Math.max(r.left, sr.left + U.headW) && clientX <= Math.min(r.right, sr.right) && clientY >= Math.max(r.top, sr.top + 26) && clientY <= Math.min(r.bottom, sr.bottom);
  return { beat: x / U.bw, track: trackAtY(y), x, y, inside };
}
const unit = (alt: boolean) => A.snapUnit(alt);

/** Playlist zoom presets (Shift+1 / 2 / 3), px per beat: detail, the default, overview. */
const ZOOM_PRESETS = [64, 22, 8];
/** The least a "fit the song" zoom shows (beats): a few bars, so an empty song is not zoomed in to a beat. */
const FIT_MIN_BEATS = 16;
let zoomImpl: { step(f: number): void; preset(i: number): void; fit(): void } | null = null;
/** The playlist's zoom keys (App): Page Up / Down around the pointer, Shift+1–3 presets, Shift+4 fit the song. */
export const playlistZoom = {
  step: (f: number) => zoomImpl?.step(f),
  preset: (i: number) => zoomImpl?.preset(i),
  fit: () => zoomImpl?.fit(),
};

/** Place a pattern/source clip at a client point (browser drag). */
export function dropClipAt(clientX: number, clientY: number, kind: Clip['kind'], ref: Id, alt: boolean): boolean {
  const p = gridPoint(clientX, clientY);
  if (!p.inside || p.track < 0) return false;
  const len = srcLenBeats(kind, ref), start = clamp(snapFloor(p.beat, unit(alt)), 0, TOTAL - Math.min(len, TOTAL));
  const nc: Clip = { id: allocId(), track: p.track, start, len: Math.min(len, TOTAL - start), offset: 0, kind, ref, muted: false, gain: 0, fadeIn: 0, fadeOut: 0, pan: 0, norm: false, rev: false };
  commit([{ op: 'putClips', clips: [nc] }], tl('a.pl.insertClip'));
  setU('sel', [nc.id]);
  return true;
}

/** True when `b` changes nothing of `a` that the grid draws (a fader or routing move repaints no clip). */
function sameDrawnTrack(a: Track | undefined, b: Track): boolean {
  return !!a && a.name === b.name && a.color === b.color && a.muted === b.muted && a.solo === b.solo && a.arm === b.arm && a.h === b.h && (a.icon ?? null) === (b.icon ?? null);
}
/** Same for a channel: pattern clips show its notes' rows (kind, root) and an icon (name, instrument), not its knobs. */
function sameDrawnChannel(a: Channel | undefined, b: Channel): boolean {
  return !!a && a.name === b.name && a.kind === b.kind && a.root === b.root;
}

/** `cb`: a comp region boundary (`bnd` = its index in `boundaries(comp.regions)`). */
interface Hit { c: Clip; edge: 'l' | 'r' | 'fi' | 'fo' | 'cb' | 'menu' | null; bnd?: number }
/** A take lane under a track. `cb` = comp beat under the pointer. */
interface LaneHit extends TakeRow { ti: number; k: number; take: Take; cb: number }

export function Playlist() {
  let scrollEl!: HTMLDivElement, gridEl!: HTMLDivElement, gridCv!: HTMLCanvasElement, fadeCv!: HTMLCanvasElement, rulerCv!: HTMLCanvasElement;
  let phGrid!: HTMLDivElement, phRuler!: HTMLDivElement;
  const [vw, setVw] = createSignal(800), [vh, setVh] = createSignal(400);
  const [scroll, setScroll] = createSignal({ x: 0, y: 0 });

  /** per track: the comp whose take lanes are open below it */
  const rows = createMemo(() => S.project.tracks.map((_, i) => takeRow(i)));
  const tops = createMemo(() => {
    const t: number[] = [], rs = rows(); let y = 0;
    S.project.tracks.forEach((tr, i) => { t.push(y); y += tr.h + (rs[i]?.comp.takes.length ?? 0) * LANE_H; });
    t.push(y); return t;
  });
  const [bndHover, setBndHover] = createSignal<{ clipId: Id; i: number } | null>(null);
  createEffect(() => { geom.tops = tops(); });
  // recording starts: make sure the armed track is on screen (a tall mixer can hide it)
  createEffect(on(() => S.transport.rec && U.playing, rec => {
    if (!rec || !scrollEl) return;
    const i = S.project.tracks.findIndex(t => t.arm); if (i < 0) return;
    const y0 = tops()[i], y1 = y0 + S.project.tracks[i].h, top = scrollEl.scrollTop, vis = scrollEl.clientHeight - 30;
    if (y0 < top || y1 > top + vis) scrollEl.scrollTop = Math.max(0, y0 - 10);
  }, { defer: true }));
  const gridW = () => TOTAL * U.bw;
  const gridH = () => tops()[tops().length - 1];
  const cw = () => Math.max(1, Math.min(vw() - U.headW, gridW()));
  const ch = () => Math.max(1, Math.min(vh() - 26, gridH()));
  const editable = () => U.tool === 'draw' || U.tool === 'paint' || U.tool === 'select';
  const toolCursor = () => ({ draw: 'crosshair', paint: 'copy', erase: 'not-allowed', mute: 'pointer', slice: 'col-resize', select: 'crosshair' })[U.tool];

  let dirtyGrid = true, dirtyFade = true, dirtyRuler = true, dirtyOv = true, lastRecV = -1, lastPhX = -1, fontEpoch = 0, themeEpoch = -1, lastDpr = 0;
  /** Where the grid canvases sit in grid coordinates (see playlistTile.ts); null = redraw all. */
  let tile: Tile | null = null, rulerTile: Tile | null = null;
  /**
   * Parts of the grid to repaint (grid coordinates) when not all of it is dirty: the old and new
   * place of each clip an edit moved, the clips whose selection changed. A drag on a big song
   * repaints a few clips per frame instead of the whole view with every waveform in it.
   */
  let damage: Rect[] = [];
  /**
   * Zoom on a view too heavy to repaint within a frame: the tile (drawn at `tileBw` px per beat) is
   * stretched to the new zoom on the compositor and repainted sharp once the zoom has settled, or
   * as soon as the stretched tile no longer covers the view or is off by more than 2×. Without it
   * every wheel step waited for a full paint of all the waveforms in view (a zoom at ~20 fps on a
   * big song). Where a full paint fits in a frame nothing changes: every step is painted sharp.
   */
  let zoomed = false, stretched = false, tileBw = 0, zoomAt = 0;
  /** Frame intervals after the last full paints (ms): their median says whether a full paint fits in a frame. */
  const paintGaps: number[] = [];
  let lastDrawAt = 0, paintedFull = false;
  const heavyPaint = () => paintGaps.length >= 2 && [...paintGaps].sort((a, b) => a - b)[paintGaps.length >> 1] > 24;
  const ZOOM_SETTLE_MS = 90;
  /** Bumped when the clip lookups of `drawGrid` (patterns, sources, …) may be stale: not for a clip-only edit. */
  let lookVer = 0;
  /** The automation clip under the pointer, and the point / tension handle there. */
  const [envHover, setEnvHover] = createSignal<{ clip: Id; hover: EnvHit | null } | null>(null);
  // what the grid canvas depends on (not mixer state), split so a zoom does not re-walk every clip.
  // The view's size is not here either: `planTile` asks for a full paint only when the tile's size changes.
  // Scrolling is not here: the canvases scroll with the content and redraw only strips (`draw`)
  createEffect(() => { U.snap; U.tool; S.transport.rec; S.project.bpm; peaksVersion(); dirtyGrid = true; });
  // a zoom step is handled in `draw`: a full paint, or the tile stretched until the zoom settles
  createEffect(on(() => U.bw, () => { zoomed = true; }, { defer: true }));
  // the loop range is shaded on the overlay: moving it repaints no clip
  createEffect(() => { S.project.loop?.a; S.project.loop?.b; dirtyFade = true; });
  createEffect(() => { for (const t of S.project.tracks) { t.h; t.arm; t.color; } dirtyGrid = true; });
  /** The clip's box in grid coordinates (with its ring), or null when it is drawn beyond it (comps open take lanes). */
  const clipBox = (c: Clip, P: { tracks: Track[] }): Rect | null => {
    if (c.kind === 'comp') return null;
    const tr = P.tracks[c.track], t = tops(); if (!tr || t[c.track] == null) return null;
    // exactly the rows the clip is drawn on (top + 1 … top + h − 1): rounded out to device pixels the
    // box still ends inside its own lane, so a repaint never cuts through a clip of the lane above or
    // below (which chained, clip by clip, into a repaint of the whole view)
    return { x: c.start * U.bw - 2, y: t[c.track] + 1, w: Math.max(3, c.len * U.bw - 1) + 4, h: tr.h - 2 };
  };
  const damageClip = (c: Clip | undefined, P: { tracks: Track[] }) => {
    if (!c) return;
    const r = clipBox(c, P);
    if (r) damage.push(r); else dirtyGrid = true;
  };
  // selection: only the clips whose state changed are repainted
  let drawnSel = new Set<Id>();
  createEffect(() => {
    const next = new Set<Id>(U.sel);
    const changed: Id[] = [];
    for (const id of next) if (!drawnSel.has(id)) changed.push(id);
    for (const id of drawnSel) if (!next.has(id)) changed.push(id);
    drawnSel = next;
    if (!changed.length) return;
    if (changed.length > 64) { dirtyGrid = true; return; }
    const P = unwrap(S.project), ids = new Set(changed);
    for (const c of P.clips) if (ids.has(c.id)) damageClip(c, P);
  });
  /**
   * Patterns edited since the last frame → their length before the edit. A note edit repaints only
   * the clips that show the pattern (their boxes change only when its length does: checked in `draw`,
   * after the edit is applied) instead of every clip in view on each frame of a piano-roll drag.
   */
  const editedPats = new Map<Id, number>();
  // clip edits: the old and the new box of each clip; pattern edits: the clips of that pattern;
  // edits of fields the grid does not draw (a fader, a channel's knobs): nothing; anything else redraws the view
  onCleanup(onArrangementChange((ops, before) => {
    if (!ops) { dirtyGrid = dirtyFade = dirtyOv = true; lookVer++; editedPats.clear(); return; }
    let byId: Map<Id, Clip> | null = null;
    const old = (id: Id) => (byId ??= new Map(before.clips.map(c => [c.id, c]))).get(id);
    for (const op of ops) {
      if (op.op === 'putClips') { dirtyFade = dirtyOv = true; for (const c of op.clips) { damageClip(old(c.id), before); damageClip(c, before); } }
      else if (op.op === 'removeClips') { dirtyFade = dirtyOv = true; for (const id of op.ids) damageClip(old(id), before); }
      else if (op.op === 'putPattern') {
        lookVer++;
        const was = before.patterns.find(x => x.id === op.pattern.id);
        if (!was) continue; // a new pattern: no clip shows it yet
        if (!editedPats.has(was.id)) editedPats.set(was.id, was.len);
        for (const c of before.clips) if (c.kind === 'pat' && c.ref === was.id) damageClip(c, before);
      }
      else if (op.op === 'putTrack' && sameDrawnTrack(before.tracks.find(x => x.id === op.track.id), op.track)) continue;
      else if (op.op === 'putChannel' && sameDrawnChannel(before.channels?.find(x => x.id === op.channel.id), op.channel)) continue;
      else { dirtyGrid = dirtyFade = dirtyOv = true; lookVer++; }
    }
  }));
  createEffect(on(() => prefs().clipIcons, () => { dirtyGrid = true; }, { defer: true }));
  createEffect(() => {
    for (const k in S.audition) void S.audition[k];
    for (const k in S.stretching) void S.stretching[k];
    U.compSel?.from; U.compSel?.to; U.compSel?.clipId; rows(); bndHover(); dirtyGrid = true;
  });
  // envelope focus and point selection: only the automation clips involved are repainted
  let envDrawn: Id[] = [];
  createEffect(() => {
    const hv = envHover(), sel = U.autoSel, ids: Id[] = [];
    if (hv) ids.push(hv.clip);
    if (sel) { ids.push(sel.clip); void [...sel.idx]; }
    const P = unwrap(S.project), all = new Set([...envDrawn, ...ids]);
    for (const c of P.clips) if (all.has(c.id)) damageClip(c, P);
    envDrawn = ids;
  });
  createEffect(() => { U.bw; U.start; S.project.loop?.a; S.project.loop?.b; cw(); dirtyRuler = true; });

  /** The grid view of one frame; `at` places it on a region of the tile (tile-local CSS px). */
  const frameView = (): View => ({
    offX: 0, offY: 0, W: 0, H: 0, bw: U.bw, snap: U.snap, spb: 60 / S.project.bpm, tops: tops(), sel: new Set(U.sel), editable: editable(), recArmedBg: S.transport.rec, env: envHover(), autoSel: U.autoSel ? { clip: U.autoSel.clip, idx: [...U.autoSel.idx] } : null,
    rows: rows().map(r => r && { clip: unwrap(r.clip), comp: unwrap(r.comp) }), laneH: LANE_H, audition: { ...S.audition }, stretching: { ...S.stretching }, compSel: U.compSel ? { ...U.compSel } : null, bndHover: bndHover(), ver: lookVer,
  });
  const at = (v: View, t: Tile, r: Rect): View => ({ ...v, offX: t.x + r.x, offY: t.y + r.y, W: r.w, H: r.h });
  /** Draws the grid into `r` of the grid canvas only (clipped to whole device pixels: nothing is painted twice). */
  const paintGrid = (g: CanvasRenderingContext2D, v: View, t: Tile, r: Rect, dpr: number, rec: typeof recState | null) => {
    // the transform of a full paint, clipped to the region: a region repaint matches it pixel for pixel
    g.save();
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    g.beginPath(); g.rect(r.x, r.y, r.w, r.h); g.clip();
    g.fillStyle = T.grid; g.fillRect(r.x, r.y, r.w, r.h);
    drawGrid(g, { ...at(v, t, r), orgX: t.x, orgY: t.y }, unwrap(S.project), rec);
    g.restore();
  };
  /**
   * Grows damaged boxes over every clip they touch (until none cuts through one): a repaint edge
   * through a clip's rounded corner antialiases a few levels off a full paint, an edge on the
   * lane background never does.
   */
  const closeOverClips = (rs: Rect[]): Rect[] => {
    const P = unwrap(S.project), t = tops(), bw = U.bw;
    const out = rs.map(r => ({ ...r }));
    for (let pass = 0; pass < 4; pass++) {
      let grew = false;
      for (const c of P.clips) {
        const tr = P.tracks[c.track]; if (!tr || t[c.track] == null) continue;
        const x0 = c.start * bw - 2, x1 = x0 + Math.max(3, c.len * bw - 1) + 4, y0 = t[c.track] + 1, y1 = y0 + tr.h - 2;
        for (const r of out) {
          if (x1 <= r.x || x0 >= r.x + r.w || y1 <= r.y || y0 >= r.y + r.h) continue;
          if (x0 >= r.x && x1 <= r.x + r.w && y0 >= r.y && y1 <= r.y + r.h) continue;
          const nx = Math.min(r.x, x0), ny = Math.min(r.y, y0);
          r.w = Math.max(r.x + r.w, x1) - nx; r.h = Math.max(r.y + r.h, y1) - ny; r.x = nx; r.y = ny;
          grew = true;
        }
      }
      if (!grew) break;
    }
    return out;
  };
  /** Follow playback: scrolls before anything is drawn, so grid, ruler and playhead change in one frame. */
  const follow = (b: number) => {
    if (!U.follow || followHold || !tele.frame.playing || b < 0) return;
    const sl = scrollEl.scrollLeft, to = followScroll(b * U.bw, sl, cw());
    if (to === sl) return;
    scrollEl.scrollLeft = to;
    // the scroll event comes a frame later: the overview follows now
    setScroll({ x: scrollEl.scrollLeft, y: scrollEl.scrollTop });
  };

  const draw = (now: number) => {
    if (paintedFull && lastDrawAt) { paintGaps.push(now - lastDrawAt); if (paintGaps.length > 5) paintGaps.shift(); }
    lastDrawAt = now; paintedFull = false;
    const recActive = recState.active && tele.frame.playing;
    if (recActive && recState.version !== lastRecV) { dirtyGrid = true; lastRecV = recState.version; }
    else if (!recActive && lastRecV !== -1) { dirtyGrid = true; lastRecV = -1; }
    const dpr = window.devicePixelRatio || 1;
    if (fontEpoch !== fonts.epoch || themeEpoch !== theme.epoch || dpr !== lastDpr) { fontEpoch = fonts.epoch; themeEpoch = theme.epoch; lastDpr = dpr; dirtyGrid = dirtyRuler = dirtyOv = true; }
    if (editedPats.size) {
      const P = unwrap(S.project);
      for (const [id, len] of editedPats) {
        const p = P.patterns.find(x => x.id === id);
        if (!p) { dirtyGrid = dirtyFade = dirtyOv = true; continue; }
        if (p.len === len) continue;
        // its clips followed the new length: their new boxes, the fades and the overview too
        dirtyFade = dirtyOv = true;
        for (const c of P.clips) if (c.kind === 'pat' && c.ref === id) damageClip(c, P);
      }
      editedPats.clear();
    }
    const beat = curBeat();
    follow(beat);
    const sx = scrollEl.scrollLeft, sy = scrollEl.scrollTop, W = cw(), H = ch();
    if (zoomed) {
      zoomed = false; zoomAt = now;
      if (tile && tileBw && !dirtyGrid && heavyPaint()) stretched = true; else dirtyGrid = true;
    }
    if (stretched && tile) {
      const k = U.bw / tileBw, x0 = tile.x * k, x1 = (tile.x + tile.w) * k;
      const covers = x0 <= sx + 0.5 && (x1 >= sx + W - 0.5 || x1 >= gridW() - 1) && tile.y <= sy + 0.5 && (tile.y + tile.h >= sy + H - 0.5 || tile.y + tile.h >= gridH() - 1);
      if (dirtyGrid || damage.length || !covers || k > 2 || k < 0.5 || now - zoomAt > ZOOM_SETTLE_MS) { stretched = false; dirtyGrid = true; }
      else {
        // the same pixels at the new zoom; ruler, overview and playhead are cheap and stay exact
        gridCv.style.transform = fadeCv.style.transform = `translate(${x0}px, ${tile.y}px) scaleX(${k})`;
        const rt: Tile = { x: snapPx(Math.max(0, sx - 96), dpr), y: 0, w: Math.min(gridW(), W + 192), h: 26 };
        paintRuler(rt, dpr);
        if (dirtyOv) { dirtyOv = false; bumpOv(); }
        placePlayhead(beat);
        return;
      }
    }
    const plan = planTile(dirtyGrid ? null : tile, sx, sy, W, H, gridW(), gridH(), dpr);
    const t = plan.tile, moved = !tile || t.x !== tile.x || t.y !== tile.y || t.w !== tile.w || t.h !== tile.h;
    const rects = plan.full ? [] : damageRects(damage.length ? closeOverClips(damage) : damage, t, dpr);
    const full = plan.full || rects === null;
    damage = [];
    if (full || moved || rects!.length) {
      const rec = recActive ? recState : null, v = frameView();
      if (full) fitCanvas(gridCv, t.w, t.h);
      const g = gridCv.getContext('2d')!;
      if (full) { paintGrid(g, v, t, { x: 0, y: 0, w: t.w, h: t.h }, dpr, rec); tileBw = U.bw; paintedFull = true; }
      else {
        if (moved) {
          // shift what is already drawn, then draw only the exposed strips
          g.save();
          g.setTransform(1, 0, 0, 1, 0, 0);
          g.globalCompositeOperation = 'copy';
          g.drawImage(gridCv, Math.round(plan.dx * dpr), Math.round(plan.dy * dpr));
          g.restore();
          for (const r of plan.strips) paintGrid(g, v, t, r, dpr, rec);
        }
        for (const r of rects!) paintGrid(g, v, t, r, dpr, rec);
      }
      dirtyGrid = false;
      if (full || moved) {
        gridCv.style.width = fadeCv.style.width = t.w + 'px';
        gridCv.style.height = fadeCv.style.height = t.h + 'px';
        gridCv.style.transform = fadeCv.style.transform = `translate(${t.x}px, ${t.y}px)`;
        dirtyFade = true;
      }
      tile = t;
    }
    // fades and the loop range: an overlay, so dragging them repaints no clip
    if (dirtyFade) {
      dirtyFade = false;
      fitCanvas(fadeCv, t.w, t.h);
      const g = fadeCv.getContext('2d')!;
      g.setTransform(1, 0, 0, 1, 0, 0);
      g.clearRect(0, 0, fadeCv.width, fadeCv.height);
      g.setTransform(dpr, 0, 0, dpr, 0, 0);
      const P = unwrap(S.project);
      if (P.loop) { g.fillStyle = rgba(T.green, 0.05); g.fillRect(P.loop.a * U.bw - t.x, 0, (P.loop.b - P.loop.a) * U.bw, Math.min(t.h, gridH() - t.y)); }
      const fv = at(frameView(), t, { x: 0, y: 0, w: t.w, h: t.h });
      drawFades(g, fv, P);
      // stacked clips: recomputed with the overlay, i.e. on clip edits and tile moves, never per frame
      drawOverlaps(g, fv, P, laneOverlaps(P.clips));
    }
    // the ruler is cheap: redrawn whole whenever its tile moves
    if (dirtyRuler || !rulerTile || rulerTile.x !== t.x || rulerTile.w !== t.w) paintRuler(t, dpr);
    if (dirtyOv) { dirtyOv = false; bumpOv(); }
    placePlayhead(beat);
  };
  const paintRuler = (t: Tile, dpr: number) => {
    dirtyRuler = false;
    rulerTile = t;
    fitCanvas(rulerCv, t.w, RULER_H);
    const g = rulerCv.getContext('2d')!;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    drawRuler(g, t.x, t.w, U.bw, S.project.loop, U.start);
    rulerCv.style.width = t.w + 'px';
    rulerCv.style.transform = `translateX(${t.x}px)`;
  };
  /** The playhead lives in grid coordinates like the canvases: it scrolls with them. */
  const placePlayhead = (beat: number) => {
    const x = Math.max(0, beat) * U.bw;
    if (x !== lastPhX) {
      lastPhX = x;
      phGrid.style.transform = phRuler.style.transform = `translateX(${x}px)`;
    }
  };

  onMount(() => {
    geom.grid = gridEl; geom.scroll = scrollEl;
    const ro = new ResizeObserver(() => { setVw(scrollEl.clientWidth); setVh(scrollEl.clientHeight); });
    ro.observe(scrollEl);
    const onScroll = () => setScroll({ x: scrollEl.scrollLeft, y: scrollEl.scrollTop });
    scrollEl.addEventListener('scroll', onScroll, { passive: true });
    scrollEl.addEventListener('wheel', onWheel, { passive: false });
    gridCv.style.transformOrigin = fadeCv.style.transformOrigin = '0 0';
    const stop = addFrameTask(draw);
    onCleanup(() => { ro.disconnect(); stop(); scrollEl.removeEventListener('scroll', onScroll); scrollEl.removeEventListener('wheel', onWheel); });
  });

  // the user moved the view while playing: stop following until the next Play (no tug of war)
  let followHold = false;
  const holdFollow = () => {
    if (!U.follow || !U.playing || followHold) return;
    followHold = true; say(tl('a.pl.followPaused'));
  };
  createEffect(on(() => U.playing, p => { if (p) followHold = false; }));
  createEffect(on(() => U.follow, f => { if (f) followHold = false; }, { defer: true }));
  // ---------- wheel ----------
  const onWheel = (e: WheelEvent) => {
    if (!e.ctrlKey && !e.metaKey && !e.altKey && (e.shiftKey || Math.abs(e.deltaX) > Math.abs(e.deltaY))) holdFollow();
    if (e.ctrlKey || e.metaKey) {
      e.preventDefault();
      const r = scrollEl.getBoundingClientRect(), mx = e.clientX - r.left - U.headW;
      const beat = (scrollEl.scrollLeft + mx) / U.bw;
      const bw = clamp(U.bw * (e.deltaY < 0 ? 1.15 : 1 / 1.15), A.BW_MIN, A.BW_MAX);
      setU('bw', bw);
      scrollEl.scrollLeft = beat * bw - mx;
    } else if (e.altKey) {
      e.preventDefault();
      const d = e.deltaY < 0 ? 6 : -6;
      wheelEdit('heights', tl('a.pl.trackHeights'), S.project.tracks.map(t => ({ op: 'putTrack', track: { ...plain(t), h: clamp(t.h + d, 28, 220) } }) as Op));
    }
  };

  // ---------- view navigation: pan, edge auto-scroll, zoom ----------
  /** The clip area on screen (client px): the view without the track headers, the ruler and the scrollbar. */
  const viewRect = () => {
    const r = scrollEl.getBoundingClientRect();
    return { left: r.left + U.headW, top: r.top + 26, right: r.left + scrollEl.clientWidth, bottom: r.top + scrollEl.clientHeight };
  };
  const syncScroll = () => setScroll({ x: scrollEl.scrollLeft, y: scrollEl.scrollTop });
  /** Scrolls by (dx, dy) px; false when the view could not move (at its ends). */
  const scrollByPx = (dx: number, dy: number) => {
    const x0 = scrollEl.scrollLeft, y0 = scrollEl.scrollTop;
    scrollEl.scrollLeft = x0 + dx; scrollEl.scrollTop = y0 + dy;
    const moved = scrollEl.scrollLeft !== x0 || scrollEl.scrollTop !== y0;
    if (moved) syncScroll();
    return moved;
  };
  /**
   * `track` with edge auto-scroll: dragged near or past the clip area's edge, the view scrolls and
   * `move` runs again with the last pointer event. A `move` must read positions in grid
   * coordinates (the grid's live rect), so what is dragged keeps up with the view.
   */
  const trackEdge = (move: (e: PointerEvent) => void, up: ((e: PointerEvent) => void) | null, cursor: string, axes?: { x?: boolean; y?: boolean }) => {
    const es = edgeScroller(viewRect, scrollByPx, move, axes);
    track(ev => { move(ev); es.move(ev); }, ev => { es.stop(); up?.(ev); }, cursor);
  };
  const gridLeft = () => gridEl.getBoundingClientRect().left;
  /** Middle-button drag anywhere in the playlist pans it in both directions (FL). */
  const panDown = (e: PointerEvent) => {
    if (panDrag(e, () => ({ x: scrollEl.scrollLeft, y: scrollEl.scrollTop }), (x, y) => { scrollEl.scrollLeft = x; scrollEl.scrollTop = y; syncScroll(); })) { holdFollow(); closeMenu(); }
  };
  /** The last pointer position over the playlist (Page Up / Down zoom around it). */
  let lastPtr: { x: number; y: number } | null = null;
  /** Views to go back to with a plain Ctrl+right-click (saved before a range zoom, a preset or a fit). */
  const views: { bw: number; x: number; y: number }[] = [];
  const pushView = () => { views.push({ bw: U.bw, x: scrollEl.scrollLeft, y: scrollEl.scrollTop }); if (views.length > 32) views.shift(); };
  const setView = (bw: number, x: number, y = scrollEl.scrollTop) => {
    setU('bw', clamp(bw, A.BW_MIN, A.BW_MAX));
    scrollEl.scrollLeft = x; scrollEl.scrollTop = y;
    syncScroll();
  };
  const viewW = () => Math.max(1, scrollEl.clientWidth - U.headW);
  /** Zoom to `bw` around the pointer when it is over the clips, else around the middle of the view. */
  const zoomTo = (bw: number) => {
    const nb = clamp(bw, A.BW_MIN, A.BW_MAX); if (nb === U.bw) return;
    const r = viewRect(), p = lastPtr;
    const anchor = p && p.x >= r.left && p.x <= r.right && p.y >= r.top && p.y <= r.bottom ? p.x - r.left : viewW() / 2;
    setView(nb, zoomScroll(scrollEl.scrollLeft, anchor, U.bw, nb));
  };
  const barsInView = () => barsText(Math.max(1, Math.round(viewW() / U.bw / 4)));
  zoomImpl = {
    step: f => zoomTo(U.bw * f),
    preset: i => { const bw = ZOOM_PRESETS[i]; if (bw == null) return; if (bw !== U.bw) { pushView(); zoomTo(bw); } say(tl('a.pl.zoomPreset', { n: i + 1, bars: barsInView() })); },
    fit: () => {
      const span = fitSpan(songEnd(), 4, FIT_MIN_BEATS);
      pushView();
      setView(zoomToRange(0, span, viewW(), A.BW_MIN, A.BW_MAX).k, 0);
      say(tl('a.pl.zoomFit', { bars: barsText(Math.round(span / 4)) }));
    },
  };
  onCleanup(() => { zoomImpl = null; });
  const [zbox, setZbox] = createSignal<{ x0: number; y0: number; x1: number; y1: number } | null>(null);
  /** Ctrl + right-drag: a box; on release the view zooms horizontally to it. A plain Ctrl+right-click goes back to the previous view. */
  function zoomBoxDrag(e: PointerEvent) {
    e.preventDefault(); closeMenu();
    const p0 = pt(e);
    let moved = false;
    trackEdge(ev => {
      const q = pt(ev);
      if (!moved && Math.abs(q.x - p0.x) < 4) return;
      moved = true;
      setZbox({ x0: p0.x, y0: p0.y, x1: q.x, y1: q.y });
    }, ev => {
      setZbox(null);
      if (!moved) {
        const v = views.pop();
        if (!v) { say(tl('a.pl.zoomNoBack')); return; }
        setView(v.bw, v.x, v.y);
        say(tl('a.pl.zoomBack'));
        return;
      }
      const q = pt(ev), a = clamp(Math.min(p0.beat, q.beat), 0, TOTAL), b = clamp(Math.max(p0.beat, q.beat), 0, TOTAL);
      if (b - a < 1e-6) return;
      pushView();
      const z = zoomToRange(a, b, viewW(), A.BW_MIN, A.BW_MAX);
      setView(z.k, z.scroll);
      say(tl('a.pl.zoomRange', { a: fmtPos(a), b: fmtPos(b) }));
    }, 'zoom-in');
  }

  // ---------- hit testing ----------
  const pt = (e: { clientX: number; clientY: number }) => gridPoint(e.clientX, e.clientY);
  function hit(x: number, y: number): Hit | null {
    const ti = trackAtY(y); if (ti < 0) return null;
    const P = unwrap(S.project), t = tops(), tr = P.tracks[ti], bw = U.bw, spb = 60 / P.bpm;
    const cs = P.clips;
    for (let i = cs.length - 1; i >= 0; i--) {
      const c = cs[i]; if (c.track !== ti) continue;
      const cx = c.start * bw, cwid = Math.max(3, c.len * bw - 1), cy = t[ti] + 1;
      if (x < cx || x > cx + cwid || y < cy || y > cy + tr.h - 2) continue;
      let edge: Hit['edge'] = null;
      if ((c.kind === 'audio' || c.kind === 'comp') && editable() && cwid > 36 && y - cy >= 15 && y - cy <= 25) {
        const fi = cx + c.fadeIn / spb * bw, fo = cx + cwid - c.fadeOut / spb * bw;
        if (x >= fi - 1 && x <= fi + 8) edge = 'fi';
        else if (x >= fo - 8 && x <= fo + 1) edge = 'fo';
      }
      if (!edge && c.kind === 'comp' && editable() && y - cy >= 15) {
        const cp = compById(c.ref);
        if (cp && S.audition[cp.id] == null) {
          const bs = boundaries(cp.regions);
          for (let i = 0; i < bs.length; i++) {
            const bx = toTimeline(c, bs[i].at) * bw;
            if (Math.abs(x - bx) <= 4 && bx > cx + 3 && bx < cx + cwid - 3) return { c, edge: 'cb', bnd: i };
          }
        }
      }
      // narrow clips keep a body to grab: the trim zones shrink, and vanish on a sliver
      const ez = cwid < 14 ? 0 : Math.min(6, cwid / 4);
      if (!edge) edge = x - cx < ez ? 'l' : cx + cwid - x < ez ? 'r' : null;
      // the icon in the clip's top-left corner opens its menu (FL), with either button
      if (!edge && cwid >= 36 && x - cx < 20 && y - cy < 15) edge = 'menu';
      return { c, edge };
    }
    return null;
  }

  /** The take lane under a grid point, if any. */
  function laneHit(x: number, y: number): LaneHit | null {
    const ti = trackAtY(y); if (ti < 0) return null;
    const row = rows()[ti]; if (!row) return null;
    const dy = y - tops()[ti] - S.project.tracks[ti].h; if (dy < 0) return null;
    const k = Math.floor(dy / LANE_H), take = row.comp.takes[k]; if (!take) return null;
    return { ...row, ti, k, take, cb: toComp(row.clip, x / U.bw) };
  }
  const hover = (e: PointerEvent) => {
    if (e.buttons) return;
    const p = pt(e), L = laneHit(p.x, p.y), h = L ? null : hit(p.x, p.y);
    if (L) { CA.compHover.clipId = L.clip.id; CA.compHover.cb = L.cb; }
    else if (h?.c.kind === 'comp') { CA.compHover.clipId = h.c.id; CA.compHover.cb = toComp(h.c, p.beat); }
    else CA.compHover.clipId = null;
    const bh = h?.edge === 'cb' ? { clipId: h.c.id, i: h.bnd! } : null, cur0 = bndHover();
    if ((bh?.clipId ?? -1) !== (cur0?.clipId ?? -1) || (bh?.i ?? -1) !== (cur0?.i ?? -1)) setBndHover(bh);
    if (L) { const c = editable() ? 'text' : toolCursor(); if (gridCv.style.cursor !== c) gridCv.style.cursor = c; return; }
    // an automation clip: its points and tension handles are live under the pointer (no edit mode)
    const prevEnv = envHover();
    if (h?.c.kind === 'auto' && editable()) {
      const hv = envAt(h.c, p.x, p.y);
      if (!prevEnv || prevEnv.clip !== h.c.id || !sameHit(prevEnv.hover, hv)) setEnvHover({ clip: h.c.id, hover: hv });
      if (!prevEnv || prevEnv.clip !== h.c.id) say(tl('a.pl.autoEditHint'));
      if (hv) { const c = hv.kind === 'point' ? 'move' : 'ns-resize'; if (gridCv.style.cursor !== c) gridCv.style.cursor = c; return; }
    } else if (prevEnv) setEnvHover(null);
    let cur = toolCursor();
    if (h) {
      if (h.edge === 'menu') cur = 'pointer';
      else if (h.edge === 'fi' || h.edge === 'fo' || h.edge === 'cb') cur = 'col-resize';
      else if (h.edge) cur = editable() ? 'ew-resize' : toolCursor();
      else cur = editable() ? 'grab' : toolCursor();
    }
    if (gridCv.style.cursor !== cur) gridCv.style.cursor = cur;
  };

  // ---------- pointer interactions ----------
  const gridDown = (e: PointerEvent) => {
    if (e.button === 1) return; // the scroll view pans
    holdFollow();
    const modK = e.ctrlKey || e.metaKey;
    if (e.button === 2 && modK) return zoomBoxDrag(e);
    const p = pt(e), h = hit(p.x, p.y), id = h?.c.id ?? null;
    if (h?.c.kind === 'auto' && editable() && (e.button === 0 || e.button === 2) && envDown(e, h.c, p.x, p.y)) return;
    if (U.autoSel && (!h || h.c.id !== U.autoSel.clip)) setU('autoSel', null);
    const L = laneHit(p.x, p.y);
    if (L) {
      e.preventDefault(); closeMenu();
      if (e.button === 2) { const sx = e.clientX, sy = e.clientY; track(() => {}, () => openTakeMenu(sx, sy, L, L.take)); return; }
      if (e.button === 0) return swipeDrag(e, L);
      return;
    }
    if (U.compSel && e.button === 0) setU('compSel', null);
    if (e.button === 2 && h?.edge === 'menu') { e.preventDefault(); closeMenu(); return openClipMenu(e.clientX, e.clientY, h.c, p); }
    if (e.button === 2) return rightDown(e, h, p);
    if (e.button !== 0) return;
    e.preventDefault(); closeMenu();
    const tool = U.tool;
    if (tool === 'erase') return eraseDrag(id);
    if (tool === 'mute') return muteDrag(id);
    if (tool === 'slice') { if (id) A.sliceAt(id, snapRound(p.beat, unit(e.altKey)), false); return; }
    if (modK || (tool === 'select' && id == null)) return marqueeDrag(e, p, id);
    if (h) {
      const clip = h.c;
      if (clip.kind !== 'comp') setU('cur', { kind: clip.kind, ref: clip.ref });
      if (h.edge === 'menu') { if (!U.sel.includes(clip.id)) setU('sel', [clip.id]); return openClipMenu(e.clientX, e.clientY, clip, p); }
      if (h.edge === 'fi' || h.edge === 'fo') return fadeDrag(e, clip, h.edge);
      if (h.edge === 'cb') return boundaryDrag(e, clip, h.bnd!);
      if (h.edge === 'l' || h.edge === 'r') return resizeDrag(e, clip, h.edge);
      return moveDrag(e, clip, e.shiftKey);
    }
    if (p.track < 0) { setU('sel', []); return; }
    const cur = U.cur; if (!cur || !srcOf({ kind: cur.kind, ref: cur.ref } as Clip)) return;
    const len = srcLenBeats(cur.kind, cur.ref);
    const start = clamp(snapFloor(p.beat, unit(e.altKey)), 0, TOTAL - Math.min(len, TOTAL));
    const nc: Clip = { id: allocId(), track: p.track, start, len: Math.min(len, TOTAL - start), offset: 0, kind: cur.kind, ref: cur.ref, muted: false, gain: 0, fadeIn: 0, fadeOut: 0, pan: 0, norm: false, rev: false };
    const g = new Gesture(tool === 'paint' ? tl('a.pl.paintClips') : tl('a.pl.insertClip'));
    g.update([{ op: 'putClips', clips: [nc] }]);
    setU('sel', [nc.id]);
    if (tool === 'paint') return paintDrag(g, nc);
    moveDrag(e, nc, false, g);
  };

  function moveDrag(e: PointerEvent, clip: Clip, clone: boolean, fresh?: Gesture) {
    const g = fresh ?? new Gesture(clone ? tl('a.pl.copyClips') : tl('a.pl.moveClips'));
    const ids = U.sel.includes(clip.id) ? [...U.sel] : [clip.id];
    const idSet = new Set(ids);
    let orig = plain(S.project.clips.filter(c => idSet.has(c.id)));
    let lead = clip.id;
    setU('sel', ids);
    // a clip with others hidden under it ("×N"): say so before the drag reveals them
    const hidden = laneOverlaps(unwrap(S.project).clips.filter(c => c.track === clip.track)).covers.get(clip.id);
    if (hidden) say(tl('a.pl.stacked', { n: hidden + 1 }));
    const sx = e.clientX, sy = e.clientY, n = S.project.tracks.length, gx0 = sx - gridLeft();
    const t0 = trackAtYClamped(sy - gridEl.getBoundingClientRect().top);
    let moved = false, lastD = NaN, lastDt = NaN;
    trackEdge(ev => {
      if (!moved && Math.abs(ev.clientX - sx) + Math.abs(ev.clientY - sy) < 3) return;
      if (!moved && clone) {
        const copies = orig.map(c => ({ ...c, id: allocId() }));
        lead = copies[orig.findIndex(c => c.id === clip.id)].id;
        orig = copies;
        g.update([{ op: 'putClips', clips: copies }]);
        setU('sel', copies.map(c => c.id));
      }
      moved = true;
      const o = orig.find(c => c.id === lead)!;
      const raw = (ev.clientX - gridLeft() - gx0) / U.bw;
      const d0 = snapRound(o.start + raw, unit(ev.altKey)) - o.start;
      const dt0 = trackAtYClamped(ev.clientY - gridEl.getBoundingClientRect().top) - t0;
      const [d, dt] = clampMove(orig, d0, dt0, n);
      // most pointer moves land on the same snapped place: nothing to apply, send or repaint
      if (d === lastD && dt === lastDt) return;
      lastD = d; lastDt = dt;
      g.update([{ op: 'putClips', clips: moveClips(orig, d, dt) }]);
      const src = srcOf(o);
      say((clone ? tl('a.pl.copyPrefix') : '') + (src ? src.name : '') + ' → ' + fmtPos(o.start + d) + ' · ' + (S.project.tracks[o.track + dt]?.name ?? ''));
    }, () => g.end(), 'grabbing');
  }

  function resizeDrag(e: PointerEvent, clip: Clip, edge: 'l' | 'r') {
    if (U.stretchMode) {
      if (clip.kind === 'audio') return stretchDrag(e, clip, edge);
      // only audio stretches: anything else trims, and says why
      say(tl(clip.kind === 'comp' ? 'a.pl.stretchComp' : 'a.pl.stretchNotAudio'));
    }
    const g = new Gesture(tl('a.pl.resizeClip'));
    const o = plain(clip), sx = e.clientX, gx0 = sx - gridLeft();
    const srcLenOf = (c: Clip) => (c.kind === 'audio' || c.kind === 'comp' ? clipSrcLen(c) : Infinity);
    const minL = Math.max(1 / 16, snapRes(U.snap, U.bw) || 1 / 16);
    // the edge of a selected clip trims the whole selection by the same amount (FL / Ableton)
    const others = U.sel.includes(o.id) ? S.project.clips.filter(c => c.id !== o.id && U.sel.includes(c.id)).map(plain) : [];
    let last = [o, ...others].map(c => c.start + ':' + c.len).join('|'), moved = false;
    trackEdge(ev => {
      if (!moved && Math.abs(ev.clientX - sx) < 3) return; // a click on an edge is not a trim
      moved = true;
      const raw = (ev.clientX - gridLeft() - gx0) / U.bw, edge0 = edge === 'r' ? o.start + o.len : o.start;
      const nc = resizeClip(o, edge, snapRound(edge0 + raw, unit(ev.altKey)), minL, srcLenOf(o));
      const delta = (edge === 'r' ? nc.start + nc.len : nc.start) - edge0;
      const all = [nc, ...resizeClips(others, edge, delta, minL, srcLenOf)];
      const key = all.map(c => c.start + ':' + c.len).join('|');
      if (key === last) return;
      last = key;
      g.update([{ op: 'putClips', clips: all }]);
      say(tl('a.pl.startLen', { pos: fmtPos(nc.start), len: nc.len.toFixed(2) }));
    }, () => g.end(), 'ew-resize', { x: true });
  }

  /** Stretch mode: the edge of an audio clip makes it play longer or shorter (the selected audio
   *  clips by the same factor); the other edge stays. */
  function stretchDrag(e: PointerEvent, clip: Clip, edge: 'l' | 'r') {
    const g = new Gesture(tl('a.pl.stretchClip'));
    const o = plain(clip), sx = e.clientX, gx0 = sx - gridLeft();
    const minL = Math.max(1 / 16, snapRes(U.snap, U.bw) || 1 / 16);
    const others = U.sel.includes(o.id) ? S.project.clips.filter(c => c.id !== o.id && c.kind === 'audio' && U.sel.includes(c.id)).map(plain) : [];
    let last = '', moved = false;
    trackEdge(ev => {
      if (!moved && Math.abs(ev.clientX - sx) < 3) return; // a click on an edge is not a stretch
      moved = true;
      const raw = (ev.clientX - gridLeft() - gx0) / U.bw, edge0 = edge === 'r' ? o.start + o.len : o.start;
      const target = snapRound(edge0 + raw, unit(ev.altKey));
      const nc = stretchClip(o, edge, target, minL);
      const all = [nc, ...stretchClips(others, edge, nc.len / o.len, minL)];
      const key = all.map(c => c.start + ':' + c.len).join('|');
      if (key === last) return;
      last = key;
      g.update([{ op: 'putClips', clips: all }]);
      // the pointer went past what the stretch range (or the song) allows
      const want = edge === 'r' ? target - o.start : o.start + o.len - target;
      const capped = Math.abs(want - nc.len) > 1e-6 && (nc.stretch === STRETCH_MAX || nc.stretch === STRETCH_MIN);
      say(tl('a.pl.stretchLen', { k: (nc.stretch ?? 1).toFixed(2), len: nc.len.toFixed(2) }) + (capped ? ' · ' + tl('a.pl.stretchLimit') : ''));
    }, () => g.end(), 'ew-resize', { x: true });
  }

  function fadeDrag(e: PointerEvent, clip: Clip, which: 'fi' | 'fo') {
    const g = new Gesture(which === 'fi' ? 'Fade in' : 'Fade out');
    const o = plain(clip), sx = e.clientX, spb = 60 / S.project.bpm;
    // a fade handle of a selected clip fades every selected audio clip by the same amount
    const all = [o, ...(U.sel.includes(o.id) ? S.project.clips.filter(c => c.id !== o.id && U.sel.includes(c.id) && (c.kind === 'audio' || c.kind === 'comp')).map(plain) : [])];
    let moved = false;
    track(ev => {
      if (!moved && Math.abs(ev.clientX - sx) < 3) return; // a click on the handle is not a fade
      moved = true;
      const d = (ev.clientX - sx) / U.bw * spb;
      const out = all.map(c => { const max = c.len * spb; return which === 'fi' ? { ...c, fadeIn: clamp(c.fadeIn + d, 0, max - c.fadeOut) } : { ...c, fadeOut: clamp(c.fadeOut - d, 0, max - c.fadeIn) }; });
      const nc = out[0];
      g.update([{ op: 'putClips', clips: out }]);
      say((which === 'fi' ? 'Fade in ' : 'Fade out ') + Math.round((which === 'fi' ? nc.fadeIn : nc.fadeOut) * 1000) + ' ms');
    }, () => g.end(), 'ew-resize');
  }

  function paintDrag(g: Gesture, first: Clip) {
    let lastEnd = first.start + first.len;
    const len = first.len;
    trackEdge(ev => {
      const b = pt(ev).beat, add: Clip[] = [];
      while (b >= lastEnd && lastEnd + len <= TOTAL) { add.push({ ...first, id: allocId(), start: lastEnd }); lastEnd += len; }
      if (add.length) { g.update([{ op: 'putClips', clips: add }]); setU('sel', [...U.sel, ...add.map(c => c.id)]); }
    }, () => g.end(), 'copy', { x: true });
  }

  function eraseDrag(id: Id | null) {
    const g = new Gesture(tl('a.pl.deleteClips'));
    const del = (cid: Id | null | undefined) => {
      if (cid == null || !S.project.clips.some(c => c.id === cid)) return;
      g.update([{ op: 'removeClips', ids: [cid] }]);
      if (U.sel.includes(cid)) setU('sel', U.sel.filter(x => x !== cid));
    };
    del(id);
    track(ev => { const p = pt(ev); del(hit(p.x, p.y)?.c.id); }, () => g.end(), 'not-allowed');
  }

  function muteDrag(id: Id | null) {
    if (id == null) return;
    const g = new Gesture(tl('a.pl.muteClips'));
    const val = !S.project.clips.find(c => c.id === id)!.muted;
    const set = (cid: Id | undefined) => {
      const c = S.project.clips.find(x => x.id === cid);
      if (c && c.muted !== val) g.update([{ op: 'putClips', clips: [{ ...plain(c), muted: val }] }]);
    };
    set(id);
    track(ev => { const p = pt(ev); set(hit(p.x, p.y)?.c.id); }, () => g.end());
  }

  function marqueeDrag(e: PointerEvent, p: { x: number; y: number }, clickId: Id | null) {
    const initial = [...U.sel];
    const base = e.shiftKey ? initial : [];
    const x0 = p.x, y0 = p.y;
    // Over a clip, wait for drag movement so a plain Ctrl+click can still toggle it.
    let dragging = clickId == null;
    if (dragging) setU({ marquee: { x0, y0, x1: x0, y1: y0 }, sel: base });
    const update = (ev: PointerEvent) => {
      if (!dragging && Math.abs(ev.clientX - e.clientX) + Math.abs(ev.clientY - e.clientY) < 3) return;
      dragging = true;
      const q = pt(ev), bw = U.bw, t = tops();
      const ax = Math.min(x0, q.x), bx = Math.max(x0, q.x), ay = Math.min(y0, q.y), by = Math.max(y0, q.y);
      const clips = unwrap(S.project).clips;
      const hits = clips.filter(c => c.start * bw < bx && (c.start + c.len) * bw > ax && t[c.track] < by && t[c.track + 1] > ay).map(c => c.id);
      const sel = [...new Set([...base, ...hits])], cur = U.sel;
      // the box moves every frame, the selection only when it crosses a clip
      const same = sel.length === cur.length && sel.every((id, i) => id === cur[i]);
      setU(same ? { marquee: { x0, y0, x1: q.x, y1: q.y } } : { marquee: { x0, y0, x1: q.x, y1: q.y }, sel });
    };
    trackEdge(update, ev => {
      if (ev.type === 'pointerup' && clickId != null && !dragging) {
        update(ev);
        if (!dragging) setU('sel', initial.includes(clickId) ? initial.filter(id => id !== clickId) : [...initial, clickId]);
      }
      setU('marquee', null);
      say(tl('a.pl.selected', { n: U.sel.length }));
    }, 'crosshair');
  }

  function rightDown(e: PointerEvent, h: Hit | null, p: { beat: number; track: number }) {
    e.preventDefault();
    const sx = e.clientX, sy = e.clientY;
    let g: Gesture | null = null;
    const del = (cid: Id | undefined) => {
      if (cid == null || !g || !S.project.clips.some(c => c.id === cid)) return;
      g.update([{ op: 'removeClips', ids: [cid] }]);
      if (U.sel.includes(cid)) setU('sel', U.sel.filter(x => x !== cid));
    };
    // FL: a right-click on a clip deletes it at once and a right-drag deletes what it crosses (one
    // undo step). The clip's menu is on its icon; empty space and a comp boundary keep their menus.
    const start = () => { g = new Gesture(tl('a.pl.deleteClips')); dragCursor('not-allowed'); del(h?.c.id); };
    if (h && h.edge !== 'cb') start();
    track(ev => {
      if (!g && Math.hypot(ev.clientX - sx, ev.clientY - sy) > 5) start();
      if (g) { const q = pt(ev); del(hit(q.x, q.y)?.c.id); }
    }, () => {
      dragCursor(null);
      if (g) { g.end(); return; }
      if (h?.edge === 'cb') openBoundaryMenu(sx, sy, h.c, h.bnd!);
      else openGridMenu(sx, sy, p);
    });
  }

  const gridDbl = (e: MouseEvent) => {
    const p = pt(e), h = hit(p.x, p.y); if (!h) return;
    if (h.c.kind === 'comp') { if (!U.sel.includes(h.c.id)) setU('sel', [h.c.id]); CA.toggleTakes(h.c.track); return; }
    // the envelope handles its own double-clicks (add / delete a point, straighten); on the header: every point
    if (h.c.kind === 'auto') {
      const a = autoById(h.c.ref);
      if (a && p.y < envGeom(h.c).by) { setU('autoSel', { clip: h.c.id, idx: a.points.map((_, i) => i) }); say(tl('a.pl.ptsSelected', { n: a.points.length })); }
      return;
    }
    setU('cur', { kind: h.c.kind, ref: h.c.ref });
    if (h.c.kind === 'audio') A.openEditor(h.c.id);
    if (h.c.kind === 'pat') openPattern(h.c.ref);
  };

  // ---------- automation envelope editing (inline, after a double-click on an automation clip) ----------
  const envGeom = (c: Clip) => {
    const t = tops(), tr = S.project.tracks[c.track], cy = t[c.track] + 1, by = cy + 15, bh = tr.h - 2 - 15, pad = 3;
    return { by, bh, X: (tt: number) => (c.start + tt - c.offset) * U.bw, Y: (v: number) => by + pad + (1 - v) * (bh - 2 * pad), T: (x: number) => x / U.bw - c.start + c.offset, V: (y: number) => clamp(1 - (y - by - pad) / (bh - 2 * pad), 0, 1) };
  };
  const sameHit = (a: EnvHit | null, b: EnvHit | null) => a === b || (!!a && !!b && a.kind === b.kind && a.i === b.i);
  /** The point or handle of automation clip `c` under (x, y): none on its header or beyond its envelope. */
  const envAt = (c: Clip, x: number, y: number): EnvHit | null => {
    const a = autoById(c.ref); if (!a) return null;
    const G = envGeom(c); if (y < G.by || G.bh < 14) return null;
    return hitEnv(a.points, G, x, y);
  };
  const putEnv = (a: Automation, points: AutoPoint[]): Op => ({ op: 'putAutomation', automation: { ...plain(a), points } });
  const withTension = (q: AutoPoint, k: number): AutoPoint => { const { tension: _t, ...r } = q; return Math.abs(k) < 1e-3 ? r : { ...r, tension: k }; };
  const valText = (a: Automation, v: number) => autoValueText(unwrap(S.project), S.catalog, a.target, v);
  /** Straightens the segments ending at `idx` (tension back to 0). */
  const straighten = (a: Automation, idx: number[]) => {
    commit([putEnv(a, plain(a.points).map((q, i) => (idx.includes(i) ? withTension(q, 0) : q)))], tl('a.pl.straighten'));
    say(tl('a.pl.straighten'));
  };
  /**
   * A press on an automation clip's envelope (below its header). Points and handles work without
   * an edit mode, as in FL: drag a point (Shift locks the value, Ctrl the time, Alt skips the
   * snap); drag a handle to bend (Ctrl: finer); double-click a point to delete it, a handle to
   * straighten, empty envelope to add a point; Ctrl+drag selects points; right-click adds a point
   * (held: placed by the drag), opens a point's menu, or straightens a handle. False: not ours
   * (the clip moves as any clip).
   */
  const envDown = (e: PointerEvent, c: Clip, x: number, y: number): boolean => {
    const a = autoById(c.ref); if (!a) return false;
    const G = envGeom(c);
    if (y < G.by || G.bh < 14) return false;
    if (e.button === 0 && (U.tool === 'erase' || U.tool === 'mute' || U.tool === 'slice')) return false;
    const pts = plain(a.points), hit = hitEnv(pts, G, x, y), modK = e.ctrlKey || e.metaKey;
    e.preventDefault(); closeMenu();
    if (!U.sel.includes(c.id)) setU('sel', [c.id]);
    if (e.button === 2) {
      if (hit?.kind === 'point') { const sx = e.clientX, sy = e.clientY, i = hit.i; track(() => {}, () => openPointMenu(sx, sy, c, i)); return true; }
      if (hit?.kind === 'handle') { straighten(a, [hit.i]); return true; }
      addAndDrag(e, c, a, G, x, y);
      return true;
    }
    if (hit?.kind === 'point') {
      if (dblCheck('ap' + c.id + ':' + hit.i)) {
        const left = deletePoints(pts, new Set([hit.i]));
        if (!left) { say(tl('a.pl.autoMinPoint')); return true; }
        commit([putEnv(a, left)], tl('a.pl.deletePoint')); setU('autoSel', null); say(tl('a.pl.pointDeleted'));
        return true;
      }
      const cur = U.autoSel?.clip === c.id ? U.autoSel.idx : [];
      if (e.shiftKey && !modK) {
        // Shift+click: add to / take from the selection
        const next = cur.includes(hit.i) ? cur.filter(i => i !== hit.i) : [...cur, hit.i].sort((p, q) => p - q);
        setU('autoSel', next.length ? { clip: c.id, idx: next } : null);
        say(tl('a.pl.ptsSelected', { n: next.length }));
        return true;
      }
      const sel = cur.includes(hit.i) ? cur : [hit.i];
      setU('autoSel', { clip: c.id, idx: sel });
      pointDrag(e, c, a, G, pts, sel, hit.i, new Gesture(tl('a.pl.movePoint')));
      return true;
    }
    if (hit?.kind === 'handle') {
      if (dblCheck('ah' + c.id + ':' + hit.i)) { straighten(a, [hit.i]); return true; }
      tensionDrag(e, a, pts, hit.i);
      return true;
    }
    if (modK) { pointMarquee(e, c, a, G, e.shiftKey); return true; }
    if (dblCheck('ab' + c.id)) { addAndDrag(e, c, a, G, x, y); return true; }
    setU('autoSel', null);
    return false;
  };
  /** Envelope time at grid x, on the playlist's snap (Alt: free). */
  const envT = (c: Clip, G: ReturnType<typeof envGeom>, x: number, free: boolean) => snapRound(G.T(x) + c.start - c.offset, unit(free)) - c.start + c.offset;
  function pointDrag(e: PointerEvent, c: Clip, a: Automation, G: ReturnType<typeof envGeom>, pts0: AutoPoint[], sel: number[], lead: number, g: Gesture) {
    const set = new Set(sel), l = pts0[lead], sx = e.clientX, sy = e.clientY;
    let moved = false;
    track(ev => {
      if (!moved && Math.hypot(ev.clientX - sx, ev.clientY - sy) < 3) return;
      moved = true;
      const q = pt(ev);
      // the point under the pointer snaps; the rest of the selection follows by as much
      const dt = ev.ctrlKey || ev.metaKey ? 0 : envT(c, G, q.x, ev.altKey) - l.t;
      const dv = ev.shiftKey ? 0 : G.V(q.y) - l.v;
      const m = movePoints(pts0, set, dt, dv);
      g.update([putEnv(a, m.points)]);
      const p = m.points[lead];
      say(a.name + ' · ' + valText(a, p.v) + ' · ' + fmtPos(c.start + p.t - c.offset) + (sel.length > 1 ? ' · ' + tl('a.pl.ptsSelected', { n: sel.length }).split(' · ')[0] : ''));
    }, () => g.end(), 'grabbing');
  }
  function tensionDrag(e: PointerEvent, a: Automation, pts0: AutoPoint[], i: number) {
    const A0 = pts0[i - 1], B0 = pts0[i], k0 = B0.tension ?? 0, sy = e.clientY;
    const g = new Gesture(tl('a.pl.bend'));
    track(ev => {
      const k = dragTension(k0, ev.clientY - sy, A0, B0, ev.ctrlKey || ev.metaKey);
      g.update([putEnv(a, pts0.map((q, j) => (j === i ? withTension(q, k) : q)))]);
      say(B0.shape === 'stairs' ? tl('a.pl.steps', { n: stairsOf(k) }) : B0.shape === 'pulse' || B0.shape === 'wave' ? tl('a.pl.cycles', { n: cyclesOf(k) }) : tl('a.pl.bend') + ' · ' + Math.round(k * 100) + '%');
    }, () => g.end(), 'ns-resize');
  }
  /** Adds a point at the press and keeps it under the pointer until release (FL's right-click). */
  function addAndDrag(e: PointerEvent, c: Clip, a: Automation, G: ReturnType<typeof envGeom>, x: number, y: number) {
    const { points, i } = addPoint(plain(a.points), Math.max(0, envT(c, G, x, e.altKey)), G.V(y));
    const g = new Gesture(tl('a.pl.addPoint'));
    g.update([putEnv(a, points)]);
    setU('autoSel', { clip: c.id, idx: [i] });
    say(a.name + ' · ' + valText(a, points[i].v) + ' · ' + fmtPos(c.start + points[i].t - c.offset));
    pointDrag(e, c, a, G, points, [i], i, g);
  }
  /** Ctrl+drag on an envelope: a box that selects its points (Shift adds to the selection). */
  function pointMarquee(e: PointerEvent, c: Clip, a: Automation, G: ReturnType<typeof envGeom>, additive: boolean) {
    const p0 = pt(e), base = additive && U.autoSel?.clip === c.id ? U.autoSel.idx : [];
    setU({ marquee: { x0: p0.x, y0: p0.y, x1: p0.x, y1: p0.y }, autoSel: base.length ? { clip: c.id, idx: base } : null });
    track(ev => {
      const q = pt(ev);
      const idx = [...new Set([...base, ...pointsInBox(a.points, G, p0.x, p0.y, q.x, q.y)])].sort((m, n) => m - n);
      setU({ marquee: { x0: p0.x, y0: p0.y, x1: q.x, y1: q.y }, autoSel: idx.length ? { clip: c.id, idx } : null });
    }, () => {
      setU('marquee', null);
      const n = U.autoSel?.clip === c.id ? U.autoSel.idx.length : 0;
      say(n ? tl('a.pl.ptsSelected', { n }) : tl('a.pl.autoEditHint'));
    }, 'crosshair');
  }
  /** A point's menu: its value, the curve of the segment(s) ending at it, delete. Acts on the
   *  whole selection when the point is in it. */
  function openPointMenu(x: number, y: number, c: Clip, i: number) {
    const a = autoById(c.ref); if (!a || !a.points[i]) return;
    const sel = U.autoSel?.clip === c.id && U.autoSel.idx.includes(i) ? [...U.autoSel.idx] : [i];
    const segs = sel.filter(j => j > 0), q = a.points[i];
    const shapes: [CurveShape, string][] = [['curve', 'a.pl.shape.curve'], ['double', 'a.pl.shape.double'], ['hold', 'a.pl.shape.hold'], ['stairs', 'a.pl.shape.stairs'],
      ['pulse', 'a.pl.shape.pulse'], ['wave', 'a.pl.shape.wave'], ['halfSine', 'a.pl.shape.halfSine']];
    const shapeOf = (j: number) => a.points[j].shape ?? 'curve';
    const items: MenuItem[] = [
      { t: 'title', label: valText(a, q.v) + ' · ' + fmtPos(c.start + q.t - c.offset) + (sel.length > 1 ? ' · ×' + sel.length : '') },
      { t: 'item', label: tl('a.pl.typeValue'), act: () => openRename(x, y, valText(a, q.v), txt => {
        const v = parseAutoValue(unwrap(S.project), S.catalog, a.target, txt);
        if (v == null) { say(tl('a.pl.badValue', { v: txt })); return; }
        const cur = autoById(c.ref); if (!cur) return;
        commit([putEnv(cur, plain(cur.points).map((p, j) => (sel.includes(j) ? { ...p, v } : p)))], tl('a.pl.movePoint'));
        say(cur.name + ' · ' + valText(cur, v));
      }, tl('a.pl.valueTitle')) },
      { t: 'sep' },
      { t: 'sec', label: tl('a.pl.curveType') },
      ...shapes.map(([sh, key]): MenuItem => ({
        t: 'item', label: tl(key as Parameters<typeof tl>[0]), disabled: !segs.length, checked: segs.length > 0 && segs.every(j => shapeOf(j) === sh),
        act: () => { commit([putEnv(a, setShape(plain(a.points), segs, sh))], tl('a.pl.curveType') + ' · ' + tl(key as Parameters<typeof tl>[0])); },
      })),
      { t: 'item', label: tl('a.pl.straighten'), disabled: !segs.some(j => a.points[j].tension), act: () => straighten(a, segs) },
      { t: 'sep' },
      { t: 'item', label: tl('a.pl.deletePoint') + (sel.length > 1 ? ' ×' + sel.length : ''), kbd: 'Del', danger: true, act: () => { setU('autoSel', { clip: c.id, idx: sel }); A.deleteAutoPoints(); } },
    ];
    openMenu(x, y, items, 40 + items.length * 26);
  }

  // ---------- menus ----------
  // ---------- swipe comping ----------
  /** Dragging across a take lane writes that range of the take into the comp. */
  function swipeDrag(e: PointerEvent, L: LaneHit) {
    const clip = plain(L.clip), comp = plain(L.comp), take = plain(L.take);
    const lo = Math.max(take.start, clip.offset), hi = Math.min(takeEnd(take), clip.offset + clip.len);
    if (hi <= lo) { say(tl('a.pl.takeOutside', { name: take.name })); return; }
    const at = (ev: PointerEvent) => clamp(toComp(clip, snapRound(pt(ev).beat, unit(ev.altKey))), lo, hi);
    const a = at(e), sx = e.clientX;
    let g: Gesture | null = null;
    track(ev => {
      if (!g && Math.abs(ev.clientX - sx) < 3) return;
      const b = at(ev), from = Math.min(a, b), to = Math.max(a, b);
      if (to - from < 1e-6) return;
      if (!g) g = new Gesture('Comping');
      g.update([{ op: 'putComp', comp: { ...comp, regions: writeRange(comp, take.id, from, to) } }]);
      setU('compSel', { clipId: clip.id, from, to });
      say(take.name + ' · ' + fmtPos(toTimeline(clip, from)) + ' – ' + fmtPos(toTimeline(clip, to)) + tl('a.pl.takeHint'));
    }, () => {
      if (g) { g.end(); return; }
      setU('compSel', null);
      say(tl('a.pl.swipeHint'));
    }, 'text');
  }
  function boundaryDrag(e: PointerEvent, clip: Clip, i: number) {
    const cp = compById(clip.ref); if (!cp) return;
    const comp = plain(cp), c = plain(clip), b = boundaries(comp.regions)[i]; if (!b) return;
    const g = new Gesture(tl('a.pl.compBoundary')), sx = e.clientX;
    track(ev => {
      if (Math.abs(ev.clientX - sx) < 2) return;
      const cb = toComp(c, snapRound(pt(ev).beat, unit(ev.altKey)));
      const regions = moveBoundary(comp, b, cb, 1 / 32);
      g.update([{ op: 'putComp', comp: { ...comp, regions } }]);
      const r = b.left >= 0 ? regions[b.left].to : regions[b.right].from;
      say(tl('a.pl.compBoundaryAt', { pos: fmtPos(toTimeline(c, r)) }));
    }, () => g.end(), 'col-resize');
  }
  function openBoundaryMenu(x: number, y: number, clip: Clip, i: number) {
    const comp = compById(clip.ref); if (!comp) return;
    const b = boundaries(comp.regions)[i]; if (!b) return;
    const cur = boundaryXfade(comp, b), own = (b.right >= 0 ? comp.regions[b.right] : comp.regions[b.left])?.xfade;
    const set = (v: number | null, label: string) => { CA.putComp({ ...plain(comp), regions: setBoundaryXfade(comp, b, v) }, tl('a.pl.boundaryXfade')); say(label); };
    openMenu(x, y, [
      { t: 'title', label: tl('a.pl.boundaryTitle', { pos: fmtPos(toTimeline(clip, b.at)), ms: Math.round(cur * 1000) }) },
      { t: 'chips', label: 'Crossfade', options: XF.map(v => v * 1000 + ' ms'), active: XF.findIndex(v => Math.abs(v - cur) < 1e-6), pick: k => set(XF[k], tl('a.pl.boundaryXfadeMs', { ms: XF[k] * 1000 })) },
      { t: 'item', label: tl('a.pl.compDefault', { ms: Math.round(comp.xfade * 1000) }), disabled: own == null, act: () => set(null, tl('a.pl.boundaryXfadeDef')) },
    ], 130);
  }

  function openClipMenu(x: number, y: number, c: Clip, p: { beat: number }) {
    if (!U.sel.includes(c.id)) setU('sel', [c.id]);
    if (c.kind === 'comp') return openCompMenu(x, y, c, p);
    const src = srcOf(c), isPat = c.kind === 'pat';
    if (c.kind === 'auto') {
      const a = autoById(c.ref);
      openMenu(x, y, [
        { t: 'title', label: tl('a.pl.autoPrefix') + (a?.name ?? '') },
        { t: 'item', label: tl('a.pl.rename'), act: () => openRename(x, y, a?.name ?? '', v => A.setSrcProp(c, { name: v })) },
        { t: 'swatches', current: a?.color, pick: col => A.setSrcProp(c, { color: col }) },
        { t: 'sep' },
        { t: 'item', label: c.muted ? tl('a.pl.unmute') : tl('a.pl.mute'), kbd: 'Ctrl+M', act: A.toggleMuteSel },
        { t: 'item', label: tl('a.pl.duplicate'), kbd: 'Ctrl+B', act: A.duplicate },
        { t: 'item', label: tl('a.pl.splitHere'), act: () => A.sliceAt(c.id, snapRound(p.beat, unit(false))) },
        { t: 'item', label: tl('a.pl.selAllAuto'), act: () => setU('sel', S.project.clips.filter(v => v.kind === 'auto' && v.ref === c.ref).map(v => v.id)) },
        { t: 'sep' },
        { t: 'item', label: tl('a.pl.deleteClip'), kbd: 'Del', danger: true, act: () => A.delIds(U.sel.includes(c.id) ? [...U.sel] : [c.id]) },
        { t: 'item', label: tl('a.pl.deleteAutoAll'), danger: true, act: () => { commit([{ op: 'removeAutomation', id: c.ref }], tl('a.pl.deleteAuto')); setU('autoSel', null); } },
      ], 380);
      return;
    }
    const all = () => setU('sel', S.project.clips.filter(v => v.kind === c.kind && v.ref === c.ref).map(v => v.id));
    const items: MenuItem[] = [
      { t: 'title', label: (isPat ? 'Pattern · ' : 'Audio · ') + (src?.name ?? '') },
      { t: 'item', label: tl('a.pl.rename'), act: () => openRename(x, y, src?.name ?? '', v => A.setSrcProp(c, { name: v })) },
      { t: 'swatches', current: (src as { color?: string } | undefined)?.color, pick: col => A.setSrcProp(c, { color: col }) },
      { t: 'sec', label: tl('a.pl.secEdit') },
      { t: 'item', label: c.muted ? tl('a.pl.unmute') : tl('a.pl.mute'), kbd: 'Ctrl+M', act: A.toggleMuteSel },
      { t: 'item', label: tl('a.pl.duplicate'), kbd: 'Ctrl+B', act: A.duplicate },
      { t: 'sub', label: tl('a.pl.clipboard'), items: [
        { t: 'item', label: tl('a.pl.copy'), kbd: 'Ctrl+C', act: A.copy },
        { t: 'item', label: tl('a.pl.cut'), kbd: 'Ctrl+X', act: A.cut },
      ] },
      { t: 'sub', label: tl('a.pl.split'), items: [
        { t: 'item', label: tl('a.pl.splitHere'), act: () => A.sliceAt(c.id, snapRound(p.beat, unit(false))) },
        { t: 'item', label: tl('a.pl.splitAtHead'), act: () => A.sliceAt(c.id, curBeat()) },
      ] },
      ...(isPat ? [
        { t: 'sec', label: 'Pattern' } as MenuItem,
        { t: 'item', label: tl('a.pl.editPattern'), kbd: tl('a.pl.dblClick'), act: () => openPattern(c.ref) } as MenuItem,
        { t: 'item', label: tl('a.pl.makeUnique'), act: () => A.makeUnique(c) } as MenuItem,
      ] : [
        { t: 'sec', label: 'Audio' } as MenuItem,
        { t: 'gain', clipId: c.id } as MenuItem,
        ...(c.kind === 'audio' ? [
          { t: 'item', label: tl('a.pl.makeUnique'), title: tl('a.pl.makeUniqueAudioTip'), act: () => A.makeUnique(c) } as MenuItem,
          { t: 'item', label: tl('a.pl.editSample'), kbd: tl('a.pl.dblClick'), act: () => A.openEditor(c.id) } as MenuItem,
          { t: 'item', label: tl('a.pl.editPitch'), kbd: c.tune?.notes.length ? tl('a.pl.edited') : '', act: () => A.openPitchEditor(c.id) } as MenuItem,
          // how a stretch plays (the clip's stretch factor in the label: set it in Stretch mode)
          { t: 'chips', label: tl('a.pl.stretchMethod', { k: (c.stretch ?? 1).toFixed(2) }), options: [tl('a.pl.keepPitch'), tl('a.pl.resample')], active: c.resample ? 1 : 0, pick: (i: number) => A.setStretchMethod(c, i === 1) } as MenuItem,
          ...(S.stretching[c.id] ? [{ t: 'item', label: tl('a.pl.stretchFailed'), title: S.stretching[c.id], disabled: true, act: () => {} } as MenuItem] : []),
          { t: 'item', label: tl('a.pl.detectTempoKey'), act: () => A.openMusicAnalysis({ clipId: c.id }) } as MenuItem,
          { t: 'item', label: tl('a.pl.trimRegion'), title: tl('d.smp.trim.tip'), act: () => void A.trimClip(c.id) } as MenuItem,
          { t: 'sub', label: tl('a.pl.autoVocal'), items: [
            { t: 'item', label: tl('a.pl.autoVocalSing'), act: () => void A.autoVocal(c.id, 'natural'), title: tl('a.pl.autoVocalSingTip') },
            { t: 'item', label: tl('a.pl.autoVocalRap'), act: () => void A.autoVocal(c.id, 'hard'), title: tl('a.pl.autoVocalRapTip') },
          ] } as MenuItem,
        ] : []),
      ]),
      { t: 'sec', label: tl('a.pl.secMix') },
      { t: 'item', label: tl('a.pl.mixerChannel'), act: () => openRouteMenu(c.track, x, y) },
      { t: 'item', label: tl('a.pl.selAllSource'), act: all },
      { t: 'sep' },
      { t: 'item', label: tl('a.pl.delete'), kbd: 'Del', danger: true, act: () => A.delIds(U.sel.includes(c.id) ? [...U.sel] : [c.id]) },
    ];
    openMenu(x, y, items, 420);
  }
  function openCompMenu(x: number, y: number, c: Clip, p: { beat: number }) {
    const comp = compById(c.ref); if (!comp) return;
    const tr = S.project.tracks[c.track], open = !!(tr && U.takesOpen[tr.id]);
    const aud = S.audition[comp.id], unused = unusedTakes(comp).length;
    openMenu(x, y, [
      { t: 'title', label: comp.name + ' · ' + CA.takesLabel(comp.takes.length) },
      { t: 'item', label: open ? tl('a.pl.hideTakes') : tl('a.pl.showTakes'), kbd: tl('a.pl.dblClick'), act: () => CA.toggleTakes(c.track) },
      ...(aud != null ? [{ t: 'item', label: tl('a.pl.endSoloPrev'), act: () => CA.audition(comp.id, null) } as MenuItem] : []),
      { t: 'item', label: tl('a.pl.rename'), act: () => openRename(x, y, comp.name, v => CA.renameComp(comp, v)) },
      { t: 'chips', label: 'Crossfade', options: XF.map(v => v * 1000 + ' ms'), active: XF.findIndex(v => Math.abs(v - comp.xfade) < 1e-6), pick: k => CA.setXfade(comp, XF[k]) },
      { t: 'sep' },
      { t: 'item', label: c.muted ? tl('a.pl.unmute') : tl('a.pl.mute'), kbd: 'Ctrl+M', act: A.toggleMuteSel },
      { t: 'item', label: tl('a.pl.duplicate'), kbd: 'Ctrl+B', act: A.duplicate, title: tl('a.pl.dupCompTip') },
      { t: 'item', label: tl('a.pl.copy'), kbd: 'Ctrl+C', act: A.copy },
      { t: 'item', label: tl('a.pl.cut'), kbd: 'Ctrl+X', act: A.cut },
      { t: 'item', label: tl('a.pl.splitHere'), act: () => A.sliceAt(c.id, snapRound(p.beat, unit(false))) },
      { t: 'gain', clipId: c.id },
      { t: 'item', label: tl('a.pl.mixerChannel'), act: () => openRouteMenu(c.track, x, y) },
      { t: 'sep' },
      { t: 'item', label: tl('a.pl.removeUnused') + (unused ? ' (' + unused + ')' : ''), disabled: !unused, act: () => CA.removeUnused(comp) },
      { t: 'item', label: tl('a.pl.flattenComp'), title: tl('a.pl.flattenTip'), act: () => CA.flatten(c.id) },
      { t: 'sub', label: tl('a.pl.autoVocal'), items: [
        { t: 'item', label: tl('a.pl.autoVocalSing'), title: tl('a.pl.autoVocalCompTip'), act: () => void A.autoVocalComp(c.id, 'natural') },
        { t: 'item', label: tl('a.pl.autoVocalRap'), act: () => void A.autoVocalComp(c.id, 'hard') },
      ] },
      { t: 'sep' },
      { t: 'item', label: tl('a.pl.delete'), kbd: 'Del', danger: true, act: () => A.delIds(U.sel.includes(c.id) ? [...U.sel] : [c.id]) },
    ], 470);
  }

  function openGridMenu(x: number, y: number, p: { beat: number; track: number }) {
    const at = { beat: snapFloor(p.beat, unit(false)), track: clamp(p.track, 0, S.project.tracks.length - 1) };
    openMenu(x, y, [
      { t: 'title', label: 'Playlist · ' + fmtPos(at.beat) },
      { t: 'item', label: tl('a.pl.pasteHere'), kbd: 'Ctrl+V', act: () => A.paste(at), disabled: !A.hasClipboard() },
      { t: 'item', label: tl('a.pl.setStartHere'), act: () => A.seek(at.beat) },
      { t: 'item', label: tl('a.pl.selectAll'), kbd: 'Ctrl+A', act: A.selectAll },
      { t: 'item', label: tl('a.pl.selectTrack'), act: () => setU('sel', S.project.clips.filter(c => c.track === at.track).map(c => c.id)) },
    ], 180);
  }

  // ---------- ruler (ui/timeRuler: the same gestures in the Piano roll and the Pitch editor) ----------
  const ruler: RulerHost = {
    // the grid's live rect: the view may scroll under the drag (edge auto-scroll)
    beatAt: x => (x - gridLeft()) / U.bw,
    bw: () => U.bw,
    unit: alt => unit(alt),
    max: () => TOTAL,
    loop: () => S.project.loop ?? null,
    edit: label => { const g = new Gesture(label); return { set: r => g.update([{ op: 'setLoop', loop: { a: r.a, b: r.b } }]), end: () => g.end() }; },
    clear: () => { A.setLoop(null, tl('a.pl.deleteLoop')); say(tl('a.pl.loopDeleted')); },
    seek: b => A.seekSong(b),
    drawn: r => A.seekSong(r.a),
    track: (m, u, c) => trackEdge(m, u, c, { x: true }),
    key: 'ruler',
  };
  const rulerHover = (e: PointerEvent) => { (e.currentTarget as HTMLElement).style.cursor = rulerCursor(ruler, e); };
  const rulerDown = (e: PointerEvent) => { if (e.button === 0) closeMenu(); rulerPress(ruler, e); };

  // ---------- header splitter ----------
  const headSplit = (e: PointerEvent) => {
    if (e.button !== 0) return;
    e.preventDefault(); e.stopPropagation();
    const sx = e.clientX, w0 = U.headW;
    track(ev => setU('headW', clamp(w0 + ev.clientX - sx, 150, 360)), null, 'col-resize');
  };

  const mq = () => { const m = U.marquee; return m && { x: Math.min(m.x0, m.x1), y: Math.min(m.y0, m.y1), w: Math.abs(m.x1 - m.x0), h: Math.abs(m.y1 - m.y0) }; };

  // ---------- overview bar (FL-style: on top, a minimap of the song under the view's thumb) ----------
  /** Beats the overview spans: the song plus some room, at least what is visible. */
  // memos: this was a walk over every clip through the store on each scroll / zoom step
  const songEnd = createMemo(() => {
    arrVersion();
    let end = 0;
    for (const c of unwrap(S.project).clips) end = Math.max(end, c.start + c.len);
    return end;
  });
  const ovRange = createMemo(() => Math.min(TOTAL, Math.max(64, songEnd() * 1.15 + 16, (scroll().x + cw()) / U.bw)));
  // repainted when the frame loop says the arrangement changed (a drag changes it on every move)
  const [ovVer, setOvVer] = createSignal(0);
  const bumpOv = () => setOvVer(v => v + 1);
  const ovView = (): ZRange => ({ pos: scroll().x / U.bw, len: cw() / U.bw });
  const ovLimits = () => ({ total: ovRange(), minLen: cw() / A.BW_MAX, maxLen: cw() / A.BW_MIN });
  const ovChange = (r: ZRange) => {
    holdFollow();
    const bw = clamp(cw() / r.len, A.BW_MIN, A.BW_MAX);
    if (bw !== U.bw) setU('bw', bw);
    scrollEl.scrollLeft = Math.max(0, r.pos * bw);
    syncScroll();
  };
  function paintOverview(g: CanvasRenderingContext2D, W: number, H: number, span: number) {
    const P = unwrap(S.project), n = Math.max(1, P.tracks.length);
    const lh = Math.max(1, (H - 4) / n);
    for (const c of P.clips) {
      const t = P.tracks[c.track]; if (!t) continue;
      g.globalAlpha = c.muted ? 0.25 : 0.75; g.fillStyle = t.color;
      g.fillRect(c.start / span * W, 2 + c.track * lh, Math.max(1, c.len / span * W), Math.max(1, lh - (lh > 2 ? 0.5 : 0)));
    }
    g.globalAlpha = 1;
  }

  return (
    <div class="plwrap" onPointerDown={() => A.playlistFront()}>
      <div class="ovrow" style={{ 'padding-left': U.headW + 'px' }}>
        <ZoomScrollbar orient="h" class="ov" range={ovView} limits={ovLimits} onChange={ovChange} title={tl('a.pl.ovTip')}
          paint={paintOverview} paintKey={ovVer} />
      </div>
      <div ref={scrollEl} class="plscroll" onPointerDown={panDown} onMouseDown={e => { if (e.button === 1) e.preventDefault(); }}
        onPointerMove={e => { lastPtr = { x: e.clientX, y: e.clientY }; }} onPointerLeave={() => { lastPtr = null; }}>
        <div style={{ width: U.headW + gridW() + 'px', position: 'relative' }}>
          <div class="rulerRow">
            <div class="plhead" style={{ width: U.headW + 'px' }}>
              {/* what dragging a clip edge does (FL's stretch switch, Shift+M) */}
              <div class="seg rsz" role="radiogroup" aria-label={tl('a.pl.resizeMode')}>
                <button role="radio" aria-checked={!U.stretchMode} classList={{ on: !U.stretchMode }} title={tl('a.pl.modeTrimTip')} onClick={() => { if (U.stretchMode) A.toggleStretchMode(); }}>{tl('a.pl.modeTrim')}</button>
                <button role="radio" aria-checked={U.stretchMode} classList={{ on: U.stretchMode }} title={tl('a.pl.modeStretchTip')} onClick={() => { if (!U.stretchMode) A.toggleStretchMode(); }}>{tl('a.pl.modeStretch')}</button>
              </div>
              <span class="mono" style={{ 'font-size': '10px', color: 'var(--fg5)', 'min-width': 0, overflow: 'hidden', 'white-space': 'nowrap', 'margin-left': '6px' }}>{S.project.tracks.length} tr</span>
            </div>
            <div style={{ position: 'relative', width: gridW() + 'px', height: '26px', flex: 'none' }}>
              <div class="phtile"><div ref={phRuler} class="phead" style={{ height: '26px' }} /></div>
              <canvas ref={rulerCv} class="ruler" title={tl('a.pl.rulerTip')}
                onPointerDown={rulerDown} onPointerMove={rulerHover} />

            </div>
          </div>
          <div style={{ display: 'flex' }}>
            <div class="lanesCol" style={{ width: U.headW + 'px' }}>
              <For each={S.project.tracks}>{(t, i) => <><LaneHeader t={t} i={i()} /><TakeHeaders i={i()} /></>}</For>
              <button class="addTrack" onClick={A.addTrack}>{tl('a.pl.addTrack')}</button>
              <div class="hsplit" onPointerDown={headSplit} title={tl('a.pl.headSplitTip')} />
            </div>
            <div ref={gridEl} class="gridArea" style={{ width: gridW() + 'px', height: gridH() + 'px' }}>
              <div class="phtile"><div ref={phGrid} class="phead" style={{ height: gridH() + 'px' }} /></div>
              <canvas ref={gridCv} class="gridCanvas" style={{ cursor: toolCursor() }}
                onPointerDown={gridDown} onPointerMove={hover} onDblClick={gridDbl} onContextMenu={e => e.preventDefault()} />
              <canvas ref={fadeCv} class="gridCanvas gridOverlay" />
              <Show when={mq()}>{m => <div class="marquee" style={{ left: m().x + 'px', top: m().y + 'px', width: m().w + 'px', height: m().h + 'px' }} />}</Show>
              <Show when={zbox()}>{z => <div class="zoombox" style={{ left: Math.min(z().x0, z().x1) + 'px', top: Math.min(z().y0, z().y1) + 'px', width: Math.abs(z().x1 - z().x0) + 'px', height: Math.abs(z().y1 - z().y0) + 'px' }} />}</Show>
            </div>
          </div>
        </div>
      </div>
      <Show when={U.dragOver}>
        <div class="dropHint"><span>{tl('a.pl.dropHint')}</span></div>
      </Show>
    </div>
  );
}

// ---------- lane headers & routing ----------
export function openRouteMenu(i: number, x: number, y: number) {
  const t = S.project.tracks[i]; if (!t) return;
  openMenu(x, y, [{ t: 'title', label: tl('a.pl.toMixer', { name: t.name }) }, ...routeItems(i)], 340);
}
/** Where track `i` plays: the master (the default) or a mixer insert the user picks. */
function routeItems(i: number): MenuItem[] {
  const t = S.project.tracks[i]; if (!t) return [];
  return S.project.inserts.map(m => ({ t: 'item', label: insLabel(m.id), kbd: t.insert === m.id ? '●' : '', act: () => {
    A.setTrack(i, { insert: m.id }, 'Routing'); A.selectInsert(m.id); say(t.name + ' → ' + insLabel(m.id));
  } }) as MenuItem);
}
const insLabel = (id: Id) => { const i = S.project.inserts.findIndex(m => m.id === id); const m = S.project.inserts[i]; return !m ? '?' : id === 0 ? 'Master' : 'Ins ' + i + ' · ' + m.name; };

function openLaneMenu(x: number, y: number, i: number) {
  const t = S.project.tracks[i];
  openMenu(x, y, [
    { t: 'title', label: t.name },
    { t: 'item', label: tl('a.pl.rename'), act: () => openRename(x, y, t.name, v => A.setTrack(i, { name: v }, tl('a.pl.trackName'))) },
    { t: 'swatches', current: t.color, pick: col => A.setTrack(i, { color: col }, tl('a.pl.trackColor')) },
    { t: 'icons', current: t.icon, color: t.color, pick: id => A.setTrack(i, { icon: id }, tl('a.pl.trackIcon')) },
    { t: 'sep' },
    { t: 'item', label: t.muted ? tl('a.pl.unmute') : tl('a.pl.mute'), act: () => A.toggleTrackMute(i) },
    { t: 'item', label: t.solo ? tl('a.pl.soloOff') : 'Solo', act: () => A.toggleTrackSolo(i) },
    { t: 'item', label: t.arm ? tl('a.pl.disarm') : tl('a.pl.arm'), act: () => A.armTrack(i) },
    { t: 'sub', label: tl('a.pl.routeItem'), items: routeItems(i) },
    { t: 'item', label: tl('a.pl.selTrackClips'), act: () => setU('sel', S.project.clips.filter(c => c.track === i).map(c => c.id)) },
    ...(CA.activeComp(i) ? [
      { t: 'item', label: U.takesOpen[t.id] ? tl('a.pl.hideTakes') : tl('a.pl.showTakes'), kbd: CA.takesLabel(CA.activeComp(i)!.comp.takes.length), act: () => CA.toggleTakes(i) } as MenuItem,
      { t: 'item', label: tl('a.pl.flattenComp'), title: tl('a.pl.flattenTip'), act: () => CA.flatten(CA.activeComp(i)!.clip.id) } as MenuItem,
    ] : []),
    { t: 'item', label: tl('a.pl.heightDefault'), act: () => A.setTrack(i, { h: 56 }, tl('a.pl.height')) },
    { t: 'sep' },
    { t: 'item', label: tl('a.pl.insertAbove'), act: () => A.insertTrack(i) },
    { t: 'item', label: tl('a.pl.insertBelow'), act: () => A.insertTrack(i + 1) },
    { t: 'item', label: tl('a.pl.deleteTrack'), danger: true, disabled: S.project.tracks.length < 2, act: () => A.removeTrack(t.id) },
  ], 400);
}

function LaneHeader(props: { t: Track; i: number }) {
  const t = () => props.t;
  const audible = () => !t().muted && (!anyTrackSolo() || t().solo);
  const routeIdx = () => S.project.inserts.findIndex(m => m.id === t().insert);
  const down = (e: PointerEvent) => {
    if (e.button === 2) { e.preventDefault(); return openLaneMenu(e.clientX, e.clientY, props.i); }
    if (e.button === 0) A.selectInsert(t().insert);
  };
  const resize = (e: PointerEvent) => {
    if (e.button !== 0) return;
    e.preventDefault(); e.stopPropagation();
    const g = new Gesture(tl('a.pl.trackHeight'));
    const sy = e.clientY, h0 = t().h;
    track(ev => {
      const h = clamp(Math.round(h0 + ev.clientY - sy), 28, 220);
      g.update([{ op: 'putTrack', track: { ...plain(t()), h } }]);
      say(tl('a.pl.nameHeight', { name: t().name, h }));
    }, () => g.end(), 'row-resize');
  };
  const volTarget = () => ({ kind: 'trackVol' as const, id: t().id });
  const shownVol = () => autoDisplay(volTarget()) ?? t().vol;
  const vol = (e: PointerEvent) => {
    if (e.button === 2) { e.preventDefault(); e.stopPropagation(); paramMenu(e.clientX, e.clientY, volTarget(), () => A.setTrack(props.i, { vol: VOL_UNITY }, tl('a.pl.volume')), t().color); return; }
    if (e.button !== 0) return;
    e.preventDefault(); e.stopPropagation();
    if (dblCheck('lvol' + props.i)) { A.setTrack(props.i, { vol: VOL_UNITY }, tl('a.pl.volume')); say(t().name + ' · 0.0 dB'); return; }
    const w = (e.currentTarget as HTMLElement).getBoundingClientRect().width;
    const g = new Gesture(tl('a.pl.trackVolume'));
    let v = t().vol, lx = e.clientX;
    track(ev => {
      v = clamp(v + (ev.clientX - lx) / w * (ev.shiftKey ? 0.2 : 1), 0, 1); lx = ev.clientX;
      g.update([{ op: 'putTrack', track: { ...plain(t()), vol: v } }]);
      say(t().name + ' · ' + volText(v));
    }, () => g.end(), 'ew-resize');
  };
  const stop = (e: Event) => e.stopPropagation();
  return (
    <div class="lane" classList={{ armrec: t().arm && S.transport.rec, dimmed: !audible() }} style={{ height: t().h + 'px' }} onPointerDown={down}>
      <div class="stripe" style={{ background: t().color }} />
      <div class="body">
        <div class="row" style={{ gap: '6px', 'min-width': 0 }}>
          <Show when={trackIcon(t())} fallback={<span class="num">{props.i + 1}</span>}>
            <span class="laneicon" style={{ color: t().color }} title={tl('a.pl.trackTip', { n: props.i + 1 })}><InstIcon id={trackIcon(t())} size={13} /></span>
          </Show>
          <span class="nm" style={{ color: audible() ? 'var(--fg-hi)' : 'var(--fg4)' }} title={tl('a.pl.nameTip')}
            onDblClick={e => openRename(e.clientX, e.clientY, t().name, v => A.setTrack(props.i, { name: v }, tl('a.pl.trackName')))}>{t().name}</span>
          <div class="row" style={{ gap: '2px', flex: 'none' }}>
            <Show when={CA.activeComp(props.i)}>{r => (
              <button class="mini tk" classList={{ on: !!U.takesOpen[t().id] }} onPointerDown={stop} onClick={() => CA.toggleTakes(props.i)}
                title={tl('a.pl.takesTip', { n: CA.takesLabel(r().comp.takes.length) })}>
                <svg width="7" height="7" viewBox="0 0 8 8" style={{ transform: U.takesOpen[t().id] ? 'rotate(90deg)' : 'none' }}><path d="M2 1l4 3-4 3z" fill="currentColor" /></svg>{r().comp.takes.length}
              </button>
            )}</Show>
            {/* tracks play into the master unless the user sends one to a mixer channel (lane menu); only then is it shown */}
            <Show when={routeIdx() > 0}>
              <button class="mini route" title={tl('a.pl.routeTip', { out: 'Insert ' + routeIdx() })} onPointerDown={stop} onClick={e => openRouteMenu(props.i, e.clientX, e.clientY)}>{'Ins ' + routeIdx()}</button>
            </Show>
            <button class="mini m" classList={{ on: t().muted }} title={tl('a.pl.muteTip')} onPointerDown={stop} onClick={() => A.toggleTrackMute(props.i)}>M</button>
            <button class="mini s" classList={{ on: t().solo }} title={tl('a.pl.soloTip')} onPointerDown={stop} onClick={e => A.toggleTrackSolo(props.i, e.ctrlKey || e.metaKey)}>S</button>
            <button class="mini r" classList={{ on: t().arm }} title={tl('a.pl.arm')} onPointerDown={stop} onClick={() => A.armTrack(props.i)}>
              <svg width="7" height="7" viewBox="0 0 8 8"><circle cx="4" cy="4" r="3.5" fill="currentColor" /></svg>
            </button>
          </div>
        </div>
        <Show when={t().h >= 50}>
          <div class="row" style={{ gap: '6px', 'padding-left': '20px' }}>
            <div class="lvol" classList={{ auto: isAutomated(volTarget()) }} onPointerDown={vol} onContextMenu={e => e.preventDefault()} title={tl('a.pl.volTip')}>
              <div class="track">
                <div class="fill" style={{ width: shownVol() * 100 + '%', background: audible() ? t().color : 'var(--fg6)' }} />
                <div class="thumb" style={{ left: shownVol() * 100 + '%' }} />
              </div>
            </div>
            <span class="mono" style={{ width: '44px', flex: 'none', 'text-align': 'right', 'font-size': '9px', color: 'var(--fg4)' }}>{volText(shownVol())}</span>
          </div>
        </Show>
      </div>
      <div class="lresize" onPointerDown={resize} title={tl('a.pl.resizeTip')} />
    </div>
  );
}

// ---------- take lanes ----------
/** Crossfade choices at a comp boundary, seconds. */
const XF = [0.005, 0.01, 0.02, 0.05];

function openTakeMenu(x: number, y: number, row: TakeRow, take: Take) {
  const comp = row.comp, used = usedBeats(comp, take.id), aud = S.audition[comp.id] === take.id, unused = unusedTakes(comp).length;
  openMenu(x, y, [
    { t: 'title', label: tl(used > 0 ? 'a.pl.takeTitleUsed' : 'a.pl.takeTitleUnused', { name: take.name }) },
    { t: 'item', label: tl('a.pl.useWholeTake'), kbd: tl('a.pl.clickName'), act: () => CA.useWholeTake(row, take) },
    { t: 'item', label: aud ? tl('a.pl.endSoloPrev') : tl('a.pl.soloPrev'), kbd: '▶', act: () => CA.audition(comp.id, aud ? null : take.id) },
    { t: 'item', label: tl('a.pl.rename'), act: () => openRename(x, y, take.name, v => CA.renameTake(comp, take.id, v)) },
    { t: 'sep' },
    { t: 'item', label: tl('a.pl.deleteTake'), danger: true, act: () => askDeleteTake(x, y, comp, take) },
    { t: 'item', label: tl('a.pl.removeUnused') + (unused ? ' (' + unused + ')' : ''), disabled: !unused, act: () => CA.removeUnused(comp) },
    { t: 'sep' },
    { t: 'item', label: tl('a.pl.flattenComp'), title: tl('a.pl.flattenTip'), act: () => CA.flatten(row.clip.id) },
  ], 290);
}

/** Deleting a take the comp plays asks first (its regions go silent). */
function askDeleteTake(x: number, y: number, comp: Comp, take: Take) {
  const used = usedBeats(comp, take.id);
  if (used <= 0) return CA.deleteTake(comp, take.id);
  openMenu(x, y, [
    { t: 'title', label: tl('a.pl.takePlays', { name: take.name, n: Math.round(used * 100) / 100 }) },
    { t: 'item', label: tl('a.pl.deleteTakeParts'), danger: true, act: () => CA.deleteTake(comp, take.id) },
    { t: 'item', label: tl('a.pl.cancel'), act: () => {} },
  ], 110);
}

/** Lane headers of the takes under track `i` (when expanded). */
function TakeHeaders(props: { i: number }) {
  const row = () => takeRow(props.i);
  const t = () => S.project.tracks[props.i];
  const stop = (e: Event) => e.stopPropagation();
  return (
    <Show when={row()}>{r => (
      <div class="takes" style={{ '--tc': t()?.color ?? 'var(--fg5)' }}>
        <For each={r().comp.takes}>{take => {
          const used = () => usedBeats(r().comp, take.id);
          const pct = () => Math.round(used() / Math.max(1e-9, take.len) * 100);
          const aud = () => S.audition[r().comp.id] === take.id;
          return (
            <div class="takeLane" classList={{ used: used() > 0, aud: aud() }} style={{ height: LANE_H + 'px' }}
              title={tl('a.pl.takeLaneTip')}
              onPointerDown={e => { stop(e); if (e.button === 2) { e.preventDefault(); openTakeMenu(e.clientX, e.clientY, r(), take); } else if (e.button === 0) CA.useWholeTake(r(), take); }}
              onContextMenu={e => e.preventDefault()}>
              <span class="tkbar" />
              <span class="tknm">{take.name}</span>
              <span class="tkuse" title={tl('a.pl.takeUseTip')}>{used() > 0 ? pct() + '%' : '—'}</span>
              <button class="mini tka" classList={{ on: aud() }} title={aud() ? tl('a.pl.endSoloPrev') : tl('a.pl.takeSoloTip')}
                onPointerDown={stop} onClick={() => CA.audition(r().comp.id, aud() ? null : take.id)}>
                <svg width="7" height="7" viewBox="0 0 8 8"><path d="M1.5 0.5l6 3.5-6 3.5z" fill="currentColor" /></svg>
              </button>
              <button class="xbtn" title={tl('a.pl.deleteTake')} onPointerDown={stop} onClick={e => askDeleteTake(e.clientX, e.clientY, r().comp, take)}>×</button>
            </div>
          );
        }}</For>
      </div>
    )}</Show>
  );
}
