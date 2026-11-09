import { prefs } from '../state/prefs';
import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from 'solid-js';
import { unwrap } from 'solid-js/store';
import { InstIcon, trackIcon } from '../ui/instIcons';
import { VOL_UNITY, type Clip, type Comp, type Id, type Op, type Take, type Track } from '../api/types';
import { boundaries, boundaryXfade, moveBoundary, setBoundaryXfade, takeEnd, toComp, toTimeline, unusedTakes, usedBeats, writeRange } from '../lib/comp';
import * as CA from '../state/comp';
import { compById, LANE_H, takeRow, type TakeRow } from '../state/comp';
import { clampMove, moveClips, resizeClip, TOTAL } from '../lib/clipOps';
import { clamp, fmtPos, volText } from '../lib/format';
import { snapFloor, snapRes, snapRound } from '../lib/snap';
import * as A from '../state/actions';
import { commit, Gesture, wheelEdit } from '../state/edit';
import { peaksVersion } from '../state/peaks';
import { allocId, projVersion, S, srcLenBeats, srcOf } from '../state/store';
import { curBeat, recState, tele } from '../state/telemetry';
import { closeMenu, openMenu, openRename, say, setU, U, type MenuItem } from '../state/ui';
import { dblCheck, track } from '../ui/drag';
import { addFrameTask, fitCanvas, fonts } from '../ui/frameLoop';
import { drawFades, drawGrid, drawRuler, type View } from './playlistDraw';
import { autoDisplay, isAutomated, paramMenu } from '../state/automation';
import { sortPoints } from '../lib/automation';
import { autoById } from '../state/store';
import { openPattern } from '../state/pattern';
import { T, theme } from '../ui/theme';

const plain = <T,>(v: T): T => JSON.parse(JSON.stringify(v));

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

/** Place a pattern/source clip at a client point (browser drag). */
export function dropClipAt(clientX: number, clientY: number, kind: Clip['kind'], ref: Id, alt: boolean): boolean {
  const p = gridPoint(clientX, clientY);
  if (!p.inside || p.track < 0) return false;
  const len = srcLenBeats(kind, ref), start = clamp(snapFloor(p.beat, unit(alt)), 0, TOTAL - Math.min(len, TOTAL));
  const nc: Clip = { id: allocId(), track: p.track, start, len: Math.min(len, TOTAL - start), offset: 0, kind, ref, muted: false, gain: 0, fadeIn: 0, fadeOut: 0, pan: 0, norm: false, rev: false };
  commit([{ op: 'putClips', clips: [nc] }], 'Wstaw klip');
  setU('sel', [nc.id]);
  return true;
}

/** `cb`: a comp region boundary (`bnd` = its index in `boundaries(comp.regions)`). */
interface Hit { c: Clip; edge: 'l' | 'r' | 'fi' | 'fo' | 'cb' | null; bnd?: number }
/** A take lane under a track. `cb` = comp beat under the pointer. */
interface LaneHit extends TakeRow { ti: number; k: number; take: Take; cb: number }

export function Playlist() {
  let scrollEl!: HTMLDivElement, gridEl!: HTMLDivElement, gridCv!: HTMLCanvasElement, rulerCv!: HTMLCanvasElement;
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

  let dirtyGrid = true, dirtyFade = true, dirtyRuler = true, lastRecV = -1, lastPhX = -1, fontEpoch = 0, themeEpoch = -1;
  /** Everything but the fade overlays, cached: a fade drag only re-composites (cost independent of clip length). */
  const base = document.createElement('canvas');
  const [autoHover, setAutoHover] = createSignal<number | null>(null);
  // what the grid canvas depends on (not mixer state), split so a zoom does not re-walk every clip
  createEffect(() => { U.bw; U.snap; U.tool; S.transport.rec; S.project.bpm; S.project.loop?.a; S.project.loop?.b; peaksVersion(); scroll(); cw(); ch(); dirtyGrid = true; });
  createEffect(() => { for (const _ of U.sel) void _; dirtyGrid = true; });
  createEffect(() => { for (const t of S.project.tracks) { t.h; t.arm; t.color; } dirtyGrid = true; });
  // clips, comps, automations, patterns, sources: one project-version signal, not a
  // subscription to every field of every item (that cost ~7 ms per clip move on a big song)
  createEffect(() => { projVersion(); prefs().clipIcons; dirtyGrid = true; dirtyFade = true; });
  createEffect(() => {
    for (const k in S.audition) void S.audition[k];
    U.compSel?.from; U.compSel?.to; U.compSel?.clipId; rows(); bndHover(); dirtyGrid = true;
  });
  createEffect(() => { U.autoEdit; autoHover(); dirtyGrid = true; });
  createEffect(() => { U.bw; U.start; S.project.loop?.a; S.project.loop?.b; scroll(); cw(); dirtyRuler = true; });

  const offsets = () => {
    const s = scroll();
    return { x: clamp(s.x, 0, Math.max(0, gridW() - cw())), y: clamp(s.y, 0, Math.max(0, gridH() - ch())) };
  };

  const draw = () => {
    const recActive = recState.active && tele.frame.playing;
    if (recActive && recState.version !== lastRecV) { dirtyGrid = true; lastRecV = recState.version; }
    else if (!recActive && lastRecV !== -1) { dirtyGrid = true; lastRecV = -1; }
    const dpr = window.devicePixelRatio || 1;
    if (fontEpoch !== fonts.epoch || themeEpoch !== theme.epoch) { fontEpoch = fonts.epoch; themeEpoch = theme.epoch; dirtyGrid = dirtyRuler = true; }
    if (dirtyGrid || dirtyFade) {
      const W = cw(), H = ch(), o = offsets();
      const v: View = { offX: o.x, offY: o.y, W, H, bw: U.bw, snap: U.snap, spb: 60 / S.project.bpm, tops: tops(), sel: new Set(U.sel), editable: editable(), recArmedBg: S.transport.rec, autoEdit: U.autoEdit, autoHover: autoHover(),
        rows: rows().map(r => r && { clip: unwrap(r.clip), comp: unwrap(r.comp) }), laneH: LANE_H, audition: { ...S.audition }, compSel: U.compSel ? { ...U.compSel } : null, bndHover: bndHover() };
      if (dirtyGrid) {
        fitCanvas(base, W, H);
        const b = base.getContext('2d')!;
        b.setTransform(dpr, 0, 0, dpr, 0, 0);
        b.fillStyle = T.grid; b.fillRect(0, 0, W, H);
        drawGrid(b, v, unwrap(S.project), recActive ? recState : null);
      }
      dirtyGrid = dirtyFade = false;
      fitCanvas(gridCv, W, H);
      const g = gridCv.getContext('2d')!;
      g.setTransform(1, 0, 0, 1, 0, 0);
      g.drawImage(base, 0, 0);
      g.setTransform(dpr, 0, 0, dpr, 0, 0);
      drawFades(g, v, unwrap(S.project));
    }
    if (dirtyRuler) {
      dirtyRuler = false;
      const W = cw(), o = offsets();
      fitCanvas(rulerCv, W, 26);
      const g = rulerCv.getContext('2d')!;
      g.setTransform(dpr, 0, 0, dpr, 0, 0);
      drawRuler(g, o.x, W, U.bw, S.project.loop, U.start);
    }
    // playhead + follow
    const b = curBeat(), x = Math.max(0, b) * U.bw;
    if (x !== lastPhX) {
      lastPhX = x;
      phGrid.style.transform = phRuler.style.transform = `translateX(${x}px)`;
    }
    if (U.follow && !followHold && tele.frame.playing && b >= 0) {
      const view = scrollEl.clientWidth - U.headW;
      if (x > scrollEl.scrollLeft + view - 40) scrollEl.scrollLeft = x - 80;
      else if (x < scrollEl.scrollLeft) scrollEl.scrollLeft = Math.max(0, x - 80);
    }
  };

  onMount(() => {
    geom.grid = gridEl; geom.scroll = scrollEl;
    const ro = new ResizeObserver(() => { setVw(scrollEl.clientWidth); setVh(scrollEl.clientHeight); });
    ro.observe(scrollEl);
    const onScroll = () => setScroll({ x: scrollEl.scrollLeft, y: scrollEl.scrollTop });
    scrollEl.addEventListener('scroll', onScroll, { passive: true });
    scrollEl.addEventListener('wheel', onWheel, { passive: false });
    const stop = addFrameTask(draw);
    onCleanup(() => { ro.disconnect(); stop(); scrollEl.removeEventListener('scroll', onScroll); scrollEl.removeEventListener('wheel', onWheel); });
  });

  // the user moved the view while playing: stop following until the next Play (no tug of war)
  let followHold = false;
  const holdFollow = () => {
    if (!U.follow || !U.playing || followHold) return;
    followHold = true; say('Podążanie wstrzymane do następnego Play (F = włącz z powrotem)');
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
      const bw = clamp(U.bw * (e.deltaY < 0 ? 1.15 : 1 / 1.15), 4, 96);
      setU('bw', bw);
      scrollEl.scrollLeft = beat * bw - mx;
    } else if (e.altKey) {
      e.preventDefault();
      const d = e.deltaY < 0 ? 6 : -6;
      wheelEdit('heights', 'Wysokość ścieżek', S.project.tracks.map(t => ({ op: 'putTrack', track: { ...plain(t), h: clamp(t.h + d, 28, 220) } }) as Op));
    }
  };

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
      if (!edge) edge = x - cx < 6 ? 'l' : cx + cwid - x < 6 ? 'r' : null;
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
    if (U.autoEdit != null && h && h.c.id === U.autoEdit && p.y > tops()[h.c.track] + 16) {
      const i = envHit(h.c, p.x, p.y);
      if (i !== autoHover()) setAutoHover(i < 0 ? null : i);
      gridCv.style.cursor = i >= 0 ? 'move' : 'copy';
      return;
    }
    if (autoHover() != null) setAutoHover(null);
    let cur = toolCursor();
    if (h) {
      if (h.edge === 'fi' || h.edge === 'fo' || h.edge === 'cb') cur = 'col-resize';
      else if (h.edge) cur = editable() ? 'ew-resize' : toolCursor();
      else cur = editable() ? 'grab' : toolCursor();
    }
    if (gridCv.style.cursor !== cur) gridCv.style.cursor = cur;
  };

  // ---------- pointer interactions ----------
  const gridDown = (e: PointerEvent) => {
    holdFollow();
    const p = pt(e), h = hit(p.x, p.y), id = h?.c.id ?? null;
    const modK = e.ctrlKey || e.metaKey;
    if (U.autoEdit != null) {
      if (h && h.c.id === U.autoEdit && (e.button === 0 || e.button === 2) && envDown(e, h.c, p.x, p.y)) return;
      if (!h || h.c.id !== U.autoEdit) setU('autoEdit', null);
    }
    const L = laneHit(p.x, p.y);
    if (L) {
      e.preventDefault(); closeMenu();
      if (e.button === 2) { const sx = e.clientX, sy = e.clientY; track(() => {}, () => openTakeMenu(sx, sy, L, L.take)); return; }
      if (e.button === 0) return swipeDrag(e, L);
      return;
    }
    if (U.compSel && e.button === 0) setU('compSel', null);
    if (e.button === 2) return rightDown(e, h, p);
    if (e.button !== 0) return;
    e.preventDefault(); closeMenu();
    let tool = U.tool;
    if (modK && !id && (tool === 'draw' || tool === 'paint')) tool = 'select';
    if (tool === 'erase') return eraseDrag(id);
    if (tool === 'mute') return muteDrag(id);
    if (tool === 'slice') { if (id) A.sliceAt(id, snapRound(p.beat, unit(e.altKey))); return; }
    if (tool === 'select' && (!id || modK)) return marqueeDrag(p, modK);
    if (h) {
      const clip = h.c;
      if (modK) { setU('sel', U.sel.includes(clip.id) ? U.sel.filter(x => x !== clip.id) : [...U.sel, clip.id]); return; }
      if (clip.kind !== 'comp') setU('cur', { kind: clip.kind, ref: clip.ref });
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
    const g = new Gesture(tool === 'paint' ? 'Maluj klipy' : 'Wstaw klip');
    g.update([{ op: 'putClips', clips: [nc] }]);
    setU('sel', [nc.id]);
    if (tool === 'paint') return paintDrag(g, nc);
    moveDrag(e, nc, false, g);
  };

  function moveDrag(e: PointerEvent, clip: Clip, clone: boolean, fresh?: Gesture) {
    const g = fresh ?? new Gesture(clone ? 'Kopiuj klipy' : 'Przesuń klipy');
    const ids = U.sel.includes(clip.id) ? [...U.sel] : [clip.id];
    const idSet = new Set(ids);
    let orig = plain(S.project.clips.filter(c => idSet.has(c.id)));
    let lead = clip.id;
    setU('sel', ids);
    const sx = e.clientX, sy = e.clientY, n = S.project.tracks.length;
    const t0 = trackAtYClamped(sy - gridEl.getBoundingClientRect().top);
    let moved = false;
    track(ev => {
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
      const raw = (ev.clientX - sx) / U.bw;
      const d0 = snapRound(o.start + raw, unit(ev.altKey)) - o.start;
      const dt0 = trackAtYClamped(ev.clientY - gridEl.getBoundingClientRect().top) - t0;
      const [d, dt] = clampMove(orig, d0, dt0, n);
      g.update([{ op: 'putClips', clips: moveClips(orig, d, dt) }]);
      const src = srcOf(o);
      say((clone ? 'Kopia · ' : '') + (src ? src.name : '') + ' → ' + fmtPos(o.start + d) + ' · ' + (S.project.tracks[o.track + dt]?.name ?? ''));
    }, () => g.end(), 'grabbing');
  }

  function resizeDrag(e: PointerEvent, clip: Clip, edge: 'l' | 'r') {
    const g = new Gesture('Zmień długość klipu');
    const o = plain(clip), sx = e.clientX;
    const srcLen = o.kind === 'audio' || o.kind === 'comp' ? srcLenBeats(o.kind, o.ref) : Infinity;
    const minL = Math.max(1 / 16, snapRes(U.snap, U.bw) || 1 / 16);
    track(ev => {
      const raw = (ev.clientX - sx) / U.bw;
      const target = snapRound((edge === 'r' ? o.start + o.len : o.start) + raw, unit(ev.altKey));
      const nc = resizeClip(o, edge, target, minL, srcLen);
      g.update([{ op: 'putClips', clips: [nc] }]);
      say('Start ' + fmtPos(nc.start) + ' · długość ' + nc.len.toFixed(2) + ' beat');
    }, () => g.end(), 'ew-resize');
  }

  function fadeDrag(e: PointerEvent, clip: Clip, which: 'fi' | 'fo') {
    const g = new Gesture(which === 'fi' ? 'Fade in' : 'Fade out');
    const o = plain(clip), sx = e.clientX, spb = 60 / S.project.bpm, max = o.len * spb;
    track(ev => {
      const d = (ev.clientX - sx) / U.bw * spb, nc = { ...o };
      if (which === 'fi') nc.fadeIn = clamp(o.fadeIn + d, 0, max - o.fadeOut); else nc.fadeOut = clamp(o.fadeOut - d, 0, max - o.fadeIn);
      g.update([{ op: 'putClips', clips: [nc] }]);
      say((which === 'fi' ? 'Fade in ' : 'Fade out ') + Math.round((which === 'fi' ? nc.fadeIn : nc.fadeOut) * 1000) + ' ms');
    }, () => g.end(), 'ew-resize');
  }

  function paintDrag(g: Gesture, first: Clip) {
    let lastEnd = first.start + first.len;
    const len = first.len;
    track(ev => {
      const b = pt(ev).beat, add: Clip[] = [];
      while (b >= lastEnd && lastEnd + len <= TOTAL) { add.push({ ...first, id: allocId(), start: lastEnd }); lastEnd += len; }
      if (add.length) { g.update([{ op: 'putClips', clips: add }]); setU('sel', [...U.sel, ...add.map(c => c.id)]); }
    }, () => g.end(), 'copy');
  }

  function eraseDrag(id: Id | null) {
    const g = new Gesture('Usuń klipy');
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
    const g = new Gesture('Wycisz klipy');
    const val = !S.project.clips.find(c => c.id === id)!.muted;
    const set = (cid: Id | undefined) => {
      const c = S.project.clips.find(x => x.id === cid);
      if (c && c.muted !== val) g.update([{ op: 'putClips', clips: [{ ...plain(c), muted: val }] }]);
    };
    set(id);
    track(ev => { const p = pt(ev); set(hit(p.x, p.y)?.c.id); }, () => g.end());
  }

  function marqueeDrag(p: { x: number; y: number }, additive: boolean) {
    const base = additive ? [...U.sel] : [];
    const x0 = p.x, y0 = p.y;
    setU({ marquee: { x0, y0, x1: x0, y1: y0 }, sel: base });
    track(ev => {
      const q = pt(ev), bw = U.bw, t = tops();
      const ax = Math.min(x0, q.x), bx = Math.max(x0, q.x), ay = Math.min(y0, q.y), by = Math.max(y0, q.y);
      const hits = S.project.clips.filter(c => c.start * bw < bx && (c.start + c.len) * bw > ax && t[c.track] < by && t[c.track + 1] > ay).map(c => c.id);
      setU({ marquee: { x0, y0, x1: q.x, y1: q.y }, sel: [...new Set([...base, ...hits])] });
    }, () => { setU('marquee', null); say('Zaznaczono ' + U.sel.length); }, 'crosshair');
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
    track(ev => {
      if (!g && Math.hypot(ev.clientX - sx, ev.clientY - sy) > 5) {
        g = new Gesture('Usuń klipy'); document.body.style.cursor = 'not-allowed'; del(h?.c.id);
      }
      if (g) { const q = pt(ev); del(hit(q.x, q.y)?.c.id); }
    }, () => {
      if (g) { g.end(); return; }
      if (h?.edge === 'cb') openBoundaryMenu(sx, sy, h.c, h.bnd!);
      else if (h) openClipMenu(sx, sy, h.c, p); else openGridMenu(sx, sy, p);
    });
  }

  const gridDbl = (e: MouseEvent) => {
    const p = pt(e), h = hit(p.x, p.y); if (!h) return;
    if (h.c.kind === 'comp') { if (!U.sel.includes(h.c.id)) setU('sel', [h.c.id]); CA.toggleTakes(h.c.track); return; }
    if (h.c.kind === 'auto') { setU({ autoEdit: h.c.id, sel: [h.c.id] }); say('Edycja automatyzacji: klik = punkt · przeciągnij = przesuń · PPM = usuń · Esc = koniec'); return; }
    setU('cur', { kind: h.c.kind, ref: h.c.ref });
    if (h.c.kind === 'audio') A.openEditor(h.c.id);
    if (h.c.kind === 'pat') openPattern(h.c.ref);
  };

  // ---------- automation envelope editing (inline, after a double-click on an automation clip) ----------
  const envGeom = (c: Clip) => {
    const t = tops(), tr = S.project.tracks[c.track], cy = t[c.track] + 1, by = cy + 15, bh = tr.h - 2 - 15, pad = 3;
    return { by, bh, X: (tt: number) => (c.start + tt - c.offset) * U.bw, Y: (v: number) => by + pad + (1 - v) * (bh - 2 * pad), T: (x: number) => x / U.bw - c.start + c.offset, V: (y: number) => clamp(1 - (y - by - pad) / (bh - 2 * pad), 0, 1) };
  };
  const envHit = (c: Clip, x: number, y: number) => {
    const a = autoById(c.ref); if (!a) return -1;
    const G = envGeom(c);
    let best = -1, bd = 8;
    a.points.forEach((q, i) => { const d = Math.hypot(G.X(q.t) - x, G.Y(q.v) - y); if (d < bd) { bd = d; best = i; } });
    return best;
  };
  const envDown = (e: PointerEvent, c: Clip, x: number, y: number): boolean => {
    const a = autoById(c.ref); if (!a) return false;
    const G = envGeom(c);
    if (y < G.by) return false;
    e.preventDefault();
    let i = envHit(c, x, y);
    if (e.button === 2) {
      if (i >= 0 && a.points.length > 1) { commit([{ op: 'putAutomation', automation: { ...plain(a), points: a.points.filter((_, k) => k !== i).map(q => ({ ...q })) } }], 'Usuń punkt'); say('Usunięto punkt'); }
      else if (i >= 0) say('Automatyzacja musi mieć co najmniej jeden punkt');
      return true;
    }
    const g = new Gesture(i >= 0 ? 'Przesuń punkt' : 'Dodaj punkt');
    let pts = plain(a.points);
    if (i < 0) {
      const nt = Math.max(0, snapRound(G.T(x) + c.start - c.offset, unit(e.altKey)) - c.start + c.offset);
      pts = sortPoints([...pts, { t: nt, v: G.V(y) }]);
      i = pts.findIndex(q => q.t === nt && Math.abs(q.v - G.V(y)) < 1e-9);
      g.update([{ op: 'putAutomation', automation: { ...plain(a), points: pts } }]);
    }
    const idx = i, r = gridEl.getBoundingClientRect();
    track(ev => {
      const px = ev.clientX - r.left, py = ev.clientY - r.top;
      const lo = idx > 0 ? pts[idx - 1].t : -Infinity, hi = idx < pts.length - 1 ? pts[idx + 1].t : Infinity;
      const nt = clamp(snapRound(G.T(px) + c.start - c.offset, unit(ev.altKey)) - c.start + c.offset, Math.max(0, lo), hi);
      const next = pts.map((q, k) => k === idx ? { t: nt, v: G.V(py) } : q);
      g.update([{ op: 'putAutomation', automation: { ...plain(a), points: next } }]);
      say(a.name + ' · ' + Math.round(G.V(py) * 100) + '% · ' + fmtPos(c.start + nt - c.offset));
    }, () => g.end(), 'move');
    return true;
  };

  // ---------- menus ----------
  // ---------- swipe comping ----------
  /** Dragging across a take lane writes that range of the take into the comp. */
  function swipeDrag(e: PointerEvent, L: LaneHit) {
    const clip = plain(L.clip), comp = plain(L.comp), take = plain(L.take);
    const lo = Math.max(take.start, clip.offset), hi = Math.min(takeEnd(take), clip.offset + clip.len);
    if (hi <= lo) { say(take.name + ' leży poza oknem klipu — wydłuż klip, aby go usłyszeć'); return; }
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
      say(take.name + ' · ' + fmtPos(toTimeline(clip, from)) + ' – ' + fmtPos(toTimeline(clip, to)) + ' · Ctrl+↑/↓ = inny take');
    }, () => {
      if (g) { g.end(); return; }
      setU('compSel', null);
      say('Przeciągnij po take\'u, aby wpisać ten fragment do compu · klik w nazwę = cały take');
    }, 'text');
  }
  function boundaryDrag(e: PointerEvent, clip: Clip, i: number) {
    const cp = compById(clip.ref); if (!cp) return;
    const comp = plain(cp), c = plain(clip), b = boundaries(comp.regions)[i]; if (!b) return;
    const g = new Gesture('Granica compu'), sx = e.clientX;
    track(ev => {
      if (Math.abs(ev.clientX - sx) < 2) return;
      const cb = toComp(c, snapRound(pt(ev).beat, unit(ev.altKey)));
      const regions = moveBoundary(comp, b, cb, 1 / 32);
      g.update([{ op: 'putComp', comp: { ...comp, regions } }]);
      const r = b.left >= 0 ? regions[b.left].to : regions[b.right].from;
      say('Granica compu → ' + fmtPos(toTimeline(c, r)));
    }, () => g.end(), 'col-resize');
  }
  function openBoundaryMenu(x: number, y: number, clip: Clip, i: number) {
    const comp = compById(clip.ref); if (!comp) return;
    const b = boundaries(comp.regions)[i]; if (!b) return;
    const cur = boundaryXfade(comp, b), own = (b.right >= 0 ? comp.regions[b.right] : comp.regions[b.left])?.xfade;
    const set = (v: number | null, label: string) => { CA.putComp({ ...plain(comp), regions: setBoundaryXfade(comp, b, v) }, 'Crossfade granicy'); say(label); };
    openMenu(x, y, [
      { t: 'title', label: 'Granica ' + fmtPos(toTimeline(clip, b.at)) + ' · crossfade ' + Math.round(cur * 1000) + ' ms' },
      { t: 'chips', label: 'Crossfade', options: XF.map(v => v * 1000 + ' ms'), active: XF.findIndex(v => Math.abs(v - cur) < 1e-6), pick: k => set(XF[k], 'Crossfade granicy: ' + XF[k] * 1000 + ' ms') },
      { t: 'item', label: 'Domyślny compu (' + Math.round(comp.xfade * 1000) + ' ms)', disabled: own == null, act: () => set(null, 'Crossfade granicy: domyślny') },
    ], 130);
  }

  function openClipMenu(x: number, y: number, c: Clip, p: { beat: number }) {
    if (!U.sel.includes(c.id)) setU('sel', [c.id]);
    if (c.kind === 'comp') return openCompMenu(x, y, c, p);
    const src = srcOf(c), isPat = c.kind === 'pat';
    if (c.kind === 'auto') {
      const a = autoById(c.ref);
      openMenu(x, y, [
        { t: 'title', label: 'Automatyzacja · ' + (a?.name ?? '') },
        { t: 'item', label: 'Edytuj punkty', kbd: '2× klik', act: () => setU({ autoEdit: c.id }) },
        { t: 'item', label: 'Zmień nazwę…', act: () => openRename(x, y, a?.name ?? '', v => A.setSrcProp(c, { name: v })) },
        { t: 'swatches', current: a?.color, pick: col => A.setSrcProp(c, { color: col }) },
        { t: 'sep' },
        { t: 'item', label: c.muted ? 'Odcisz' : 'Wycisz', kbd: 'Ctrl+M', act: A.toggleMuteSel },
        { t: 'item', label: 'Duplikuj', kbd: 'Ctrl+B', act: A.duplicate },
        { t: 'item', label: 'Podziel tutaj', act: () => A.sliceAt(c.id, snapRound(p.beat, unit(false))) },
        { t: 'item', label: 'Zaznacz wszystkie z tej automatyzacji', act: () => setU('sel', S.project.clips.filter(v => v.kind === 'auto' && v.ref === c.ref).map(v => v.id)) },
        { t: 'sep' },
        { t: 'item', label: 'Usuń klip', kbd: 'Del', danger: true, act: () => A.delIds(U.sel.includes(c.id) ? [...U.sel] : [c.id]) },
        { t: 'item', label: 'Usuń automatyzację (wszystkie klipy)', danger: true, act: () => { commit([{ op: 'removeAutomation', id: c.ref }], 'Usuń automatyzację'); setU('autoEdit', null); } },
      ], 380);
      return;
    }
    const all = () => setU('sel', S.project.clips.filter(v => v.kind === c.kind && v.ref === c.ref).map(v => v.id));
    const items: MenuItem[] = [
      { t: 'title', label: (isPat ? 'Pattern · ' : 'Audio · ') + (src?.name ?? '') },
      { t: 'item', label: 'Zmień nazwę…', act: () => openRename(x, y, src?.name ?? '', v => A.setSrcProp(c, { name: v })) },
      { t: 'swatches', current: (src as { color?: string } | undefined)?.color, pick: col => A.setSrcProp(c, { color: col }) },
      { t: 'sec', label: 'Edycja' },
      { t: 'item', label: c.muted ? 'Odcisz' : 'Wycisz', kbd: 'Ctrl+M', act: A.toggleMuteSel },
      { t: 'item', label: 'Duplikuj', kbd: 'Ctrl+B', act: A.duplicate },
      { t: 'sub', label: 'Schowek', items: [
        { t: 'item', label: 'Kopiuj', kbd: 'Ctrl+C', act: A.copy },
        { t: 'item', label: 'Wytnij', kbd: 'Ctrl+X', act: A.cut },
      ] },
      { t: 'sub', label: 'Podziel', items: [
        { t: 'item', label: 'Podziel tutaj', act: () => A.sliceAt(c.id, snapRound(p.beat, unit(false))) },
        { t: 'item', label: 'Podziel na głowicy', act: () => A.sliceAt(c.id, curBeat()) },
      ] },
      ...(isPat ? [
        { t: 'sec', label: 'Pattern' } as MenuItem,
        { t: 'item', label: 'Edytuj pattern…', kbd: '2× klik', act: () => openPattern(c.ref) } as MenuItem,
        { t: 'item', label: 'Make unique', act: () => A.makeUnique(c) } as MenuItem,
      ] : [
        { t: 'sec', label: 'Audio' } as MenuItem,
        { t: 'gain', clipId: c.id } as MenuItem,
        ...(c.kind === 'audio' ? [
          { t: 'item', label: 'Edytuj sample…', kbd: '2× klik', act: () => A.openEditor(c.id) } as MenuItem,
          { t: 'item', label: 'Edytuj wysokość…', kbd: c.tune?.notes.length ? 'edytowany' : '', act: () => A.openPitchEditor(c.id) } as MenuItem,
          { t: 'item', label: 'Wykryj tempo i tonację…', act: () => A.openMusicAnalysis({ clipId: c.id }) } as MenuItem,
          { t: 'sub', label: 'Auto wokal', items: [
            { t: 'item', label: 'Auto wokal — śpiew', act: () => void A.autoVocal(c.id, 'natural'), title: 'Analizuje nagranie i ustawia na kanale miksera: wyrównanie, kompresję, de-esser, autotune w tonacji bitu, mały reverb i limiter' },
            { t: 'item', label: 'Auto wokal — rap', act: () => void A.autoVocal(c.id, 'hard'), title: 'Jak śpiew, ale szybsze wyrównanie i twardszy autotune' },
          ] } as MenuItem,
        ] : []),
      ]),
      { t: 'sec', label: 'Miks' },
      { t: 'item', label: 'Kanał miksera…', act: () => openRouteMenu(c.track, x, y) },
      { t: 'item', label: 'Zaznacz wszystkie z tego źródła', act: all },
      { t: 'sep' },
      { t: 'item', label: 'Usuń', kbd: 'Del', danger: true, act: () => A.delIds(U.sel.includes(c.id) ? [...U.sel] : [c.id]) },
    ];
    openMenu(x, y, items, 420);
  }
  function openCompMenu(x: number, y: number, c: Clip, p: { beat: number }) {
    const comp = compById(c.ref); if (!comp) return;
    const tr = S.project.tracks[c.track], open = !!(tr && U.takesOpen[tr.id]);
    const aud = S.audition[comp.id], unused = unusedTakes(comp).length;
    openMenu(x, y, [
      { t: 'title', label: comp.name + ' · ' + CA.takesLabel(comp.takes.length) },
      { t: 'item', label: open ? 'Ukryj take\'i' : 'Pokaż take\'i', kbd: '2× klik', act: () => CA.toggleTakes(c.track) },
      ...(aud != null ? [{ t: 'item', label: 'Zakończ odsłuch solo', act: () => CA.audition(comp.id, null) } as MenuItem] : []),
      { t: 'item', label: 'Zmień nazwę…', act: () => openRename(x, y, comp.name, v => CA.renameComp(comp, v)) },
      { t: 'chips', label: 'Crossfade', options: XF.map(v => v * 1000 + ' ms'), active: XF.findIndex(v => Math.abs(v - comp.xfade) < 1e-6), pick: k => CA.setXfade(comp, XF[k]) },
      { t: 'sep' },
      { t: 'item', label: c.muted ? 'Odcisz' : 'Wycisz', kbd: 'Ctrl+M', act: A.toggleMuteSel },
      { t: 'item', label: 'Duplikuj', kbd: 'Ctrl+B', act: A.duplicate, title: 'Kopia gra ten sam comp (edycja compu zmienia obie)' },
      { t: 'item', label: 'Kopiuj', kbd: 'Ctrl+C', act: A.copy },
      { t: 'item', label: 'Wytnij', kbd: 'Ctrl+X', act: A.cut },
      { t: 'item', label: 'Podziel tutaj', act: () => A.sliceAt(c.id, snapRound(p.beat, unit(false))) },
      { t: 'gain', clipId: c.id },
      { t: 'item', label: 'Kanał miksera…', act: () => openRouteMenu(c.track, x, y) },
      { t: 'sep' },
      { t: 'item', label: 'Usuń nieużyte take\'i' + (unused ? ' (' + unused + ')' : ''), disabled: !unused, act: () => CA.removeUnused(comp) },
      { t: 'item', label: 'Spłaszcz comp', title: 'Renderuje comp do jednego klipu audio i usuwa take\'i (Ctrl+Z cofa)', act: () => CA.flatten(c.id) },
      { t: 'sub', label: 'Auto wokal', items: [
        { t: 'item', label: 'Auto wokal — śpiew', title: 'Spłaszcza comp do jednego klipu i ustawia łańcuch wokalu na kanale miksera', act: () => void A.autoVocalComp(c.id, 'natural') },
        { t: 'item', label: 'Auto wokal — rap', act: () => void A.autoVocalComp(c.id, 'hard') },
      ] },
      { t: 'sep' },
      { t: 'item', label: 'Usuń', kbd: 'Del', danger: true, act: () => A.delIds(U.sel.includes(c.id) ? [...U.sel] : [c.id]) },
    ], 470);
  }

  function openGridMenu(x: number, y: number, p: { beat: number; track: number }) {
    const at = { beat: snapFloor(p.beat, unit(false)), track: clamp(p.track, 0, S.project.tracks.length - 1) };
    openMenu(x, y, [
      { t: 'title', label: 'Playlist · ' + fmtPos(at.beat) },
      { t: 'item', label: 'Wklej tutaj', kbd: 'Ctrl+V', act: () => A.paste(at), disabled: !A.hasClipboard() },
      { t: 'item', label: 'Ustaw start tutaj', act: () => A.seek(at.beat) },
      { t: 'item', label: 'Zaznacz wszystko', kbd: 'Ctrl+A', act: A.selectAll },
      { t: 'item', label: 'Zaznacz ścieżkę', act: () => setU('sel', S.project.clips.filter(c => c.track === at.track).map(c => c.id)) },
    ], 180);
  }

  // ---------- ruler ----------
  /** Ruler x near the loop's start / end (8 px) → that edge; inside with Shift → body. */
  const loopEdge = (clientX: number, shift = false): 'a' | 'b' | 'body' | null => {
    const lp = S.project.loop; if (!lp) return null;
    const x = clientX - gridEl.getBoundingClientRect().left, xa = lp.a * U.bw, xb = lp.b * U.bw;
    if (Math.abs(x - xa) <= 8) return 'a';
    if (Math.abs(x - xb) <= 8) return 'b';
    return shift && x > xa && x < xb ? 'body' : null;
  };
  const rulerHover = (e: PointerEvent) => {
    const ed = loopEdge(e.clientX, e.shiftKey);
    (e.currentTarget as HTMLElement).style.cursor = ed === 'a' || ed === 'b' ? 'ew-resize' : ed === 'body' ? 'grab' : '';
  };
  const rulerDown = (e: PointerEvent) => {
    if (e.button !== 0) return;
    e.preventDefault(); closeMenu();
    if (dblCheck('ruler')) { if (S.project.loop) { A.setLoop(null, 'Usuń pętlę'); say('Pętla usunięta'); } return; }
    const r = gridEl.getBoundingClientRect();
    const b0 = clamp(snapRound((e.clientX - r.left) / U.bw, unit(e.altKey)), 0, TOTAL);
    let g: Gesture | null = null;
    // an existing loop: grab its start or end edge (or drag its body) instead of drawing a new one
    const lp = S.project.loop, edge = loopEdge(e.clientX, e.shiftKey);
    if (lp && edge) {
      const len = lp.b - lp.a, raw0 = (e.clientX - r.left) / U.bw;
      track(ev => {
        const raw = (ev.clientX - r.left) / U.bw, u = unit(ev.altKey);
        if (!g && Math.abs(ev.clientX - e.clientX) > 2) g = new Gesture(edge === 'body' ? 'Przesuń pętlę' : 'Zakres pętli');
        if (!g) return;
        let a = lp.a, z = lp.b;
        if (edge === 'a') a = clamp(snapRound(raw, u), 0, lp.b - u);
        else if (edge === 'b') z = clamp(snapRound(raw, u), lp.a + u, TOTAL);
        else { a = clamp(snapRound(lp.a + raw - raw0, u), 0, TOTAL - len); z = a + len; }
        g.update([{ op: 'setLoop', loop: { a, b: z } }]);
      }, () => { if (g) g.end(); else A.seek(b0); }, edge === 'body' ? 'grabbing' : 'ew-resize');
      return;
    }
    track(ev => {
      const b1 = clamp(snapRound((ev.clientX - r.left) / U.bw, unit(ev.altKey)), 0, TOTAL);
      if (!g && Math.abs(ev.clientX - e.clientX) > 4) g = new Gesture('Pętla');
      if (g && b1 !== b0) g.update([{ op: 'setLoop', loop: { a: Math.min(b0, b1), b: Math.max(b0, b1) } }]);
    }, () => {
      if (!g) A.seek(b0);
      else { g.end(); if (S.project.loop) A.seek(S.project.loop.a); }
    }, 'ew-resize');
  };

  // ---------- header splitter ----------
  const headSplit = (e: PointerEvent) => {
    if (e.button !== 0) return;
    e.preventDefault(); e.stopPropagation();
    const sx = e.clientX, w0 = U.headW;
    track(ev => setU('headW', clamp(w0 + ev.clientX - sx, 150, 360)), null, 'col-resize');
  };

  const mq = () => { const m = U.marquee; return m && { x: Math.min(m.x0, m.x1), y: Math.min(m.y0, m.y1), w: Math.abs(m.x1 - m.x0), h: Math.abs(m.y1 - m.y0) }; };

  // ---------- overview scrollbar (FL-style: on top, wide thumb over a minimap of the song) ----------
  let ovCv!: HTMLCanvasElement;
  const [ovW, setOvW] = createSignal(600);
  /** Beats the overview spans: the song plus some room, at least what is visible. */
  const ovRange = () => {
    projVersion();
    let end = 0;
    for (const c of S.project.clips) end = Math.max(end, c.start + c.len);
    return Math.min(TOTAL, Math.max(64, end * 1.15 + 16, (scroll().x + cw()) / U.bw));
  };
  const thumb = () => {
    const R = ovRange(), W = ovW();
    const a = scroll().x / U.bw / R * W, w = Math.max(24, cw() / U.bw / R * W);
    return { x: Math.min(a, W - w), w };
  };
  onMount(() => {
    const ro = new ResizeObserver(() => setOvW(ovCv.clientWidth)); ro.observe(ovCv);
    onCleanup(() => ro.disconnect());
  });
  createEffect(() => {
    const W = ovW(), H = 18, R = ovRange(); projVersion(); theme.epoch;
    fitCanvas(ovCv, W, H);
    const g = ovCv.getContext('2d')!, P = unwrap(S.project), n = Math.max(1, P.tracks.length);
    g.clearRect(0, 0, W, H);
    const lh = Math.max(1, (H - 4) / n);
    for (const c of P.clips) {
      const t = P.tracks[c.track]; if (!t) continue;
      g.globalAlpha = c.muted ? 0.25 : 0.75; g.fillStyle = t.color;
      g.fillRect(c.start / R * W, 2 + c.track * lh, Math.max(1, c.len / R * W), Math.max(1, lh - (lh > 2 ? 0.5 : 0)));
    }
    g.globalAlpha = 1;
  });
  const ovDown = (e: PointerEvent) => {
    holdFollow();
    if (e.button !== 0) return;
    e.preventDefault(); e.stopPropagation();
    const r = ovCv.getBoundingClientRect(), x = e.clientX - r.left, th = thumb(), R = ovRange(), W = ovW();
    const toBeat = (px: number) => px / W * R;
    const edge = x < th.x + 6 && x > th.x - 4 ? 'l' : x > th.x + th.w - 6 && x < th.x + th.w + 4 ? 'r' : null;
    if (edge) {
      // drag an edge = zoom, the other edge stays put
      const b0 = scroll().x / U.bw, b1 = (scroll().x + cw()) / U.bw;
      track(ev => {
        const b = clamp(toBeat(ev.clientX - r.left), 0, R);
        const [lo, hi] = edge === 'l' ? [Math.min(b, b1 - 1), b1] : [b0, Math.max(b, b0 + 1)];
        const bw = clamp(cw() / (hi - lo), 4, 96);
        setU('bw', bw); scrollEl.scrollLeft = lo * bw;
      }, null, 'ew-resize');
      return;
    }
    // outside the thumb: centre the view there, then drag as if grabbed
    let grab = x - th.x;
    if (x < th.x || x > th.x + th.w) { grab = th.w / 2; scrollEl.scrollLeft = toBeat(x - grab) * U.bw; }
    track(ev => { scrollEl.scrollLeft = Math.max(0, toBeat(ev.clientX - r.left - grab) * U.bw); }, null, 'grabbing');
  };
  const ovHover = (e: PointerEvent) => {
    const x = e.clientX - ovCv.getBoundingClientRect().left, th = thumb();
    ovCv.style.cursor = Math.abs(x - th.x) < 6 || Math.abs(x - th.x - th.w) < 6 ? 'ew-resize' : x >= th.x && x <= th.x + th.w ? 'grab' : 'pointer';
  };

  return (
    <div class="plwrap" onPointerDown={() => A.playlistFront()}>
      <div class="ploverview" style={{ 'padding-left': U.headW + 'px' }}>
        <div class="ovtrack">
          <canvas ref={ovCv} onPointerDown={ovDown} onPointerMove={ovHover} title="Przeciągnij = przewiń · krawędzie = zoom · klik = skocz" />
          <div class="ovthumb" style={{ left: thumb().x + 'px', width: thumb().w + 'px' }} />
        </div>
      </div>
      <div ref={scrollEl} class="plscroll">
        <div style={{ width: U.headW + gridW() + 'px', position: 'relative' }}>
          <div class="rulerRow">
            <div class="plhead" style={{ width: U.headW + 'px' }}>
              <span style={{ font: '600 10px var(--sans)', 'letter-spacing': '.12em', color: 'var(--fg4)' }}>PLAYLIST</span>
              <span class="mono" style={{ 'font-size': '10px', color: 'var(--fg5)' }}>{S.project.tracks.length} tr</span>
            </div>
            <div style={{ position: 'relative', width: gridW() + 'px', height: '26px', flex: 'none' }}>
              <canvas ref={rulerCv} class="ruler" title="Klik = start · przeciągnij = pętla · 2× klik = usuń pętlę"
                style={{ left: U.headW + 'px', width: cw() + 'px' }} onPointerDown={rulerDown} onPointerMove={rulerHover} />
              <div ref={phRuler} class="phead" style={{ height: '26px', 'z-index': 2 }} />
            </div>
          </div>
          <div style={{ display: 'flex' }}>
            <div class="lanesCol" style={{ width: U.headW + 'px' }}>
              <For each={S.project.tracks}>{(t, i) => <><LaneHeader t={t} i={i()} /><TakeHeaders i={i()} /></>}</For>
              <button class="addTrack" onClick={A.addTrack}>+ Dodaj ścieżkę</button>
              <div class="hsplit" onPointerDown={headSplit} title="Przeciągnij = szerokość nagłówków" />
            </div>
            <div ref={gridEl} class="gridArea" style={{ width: gridW() + 'px', height: gridH() + 'px' }}>
              <canvas ref={gridCv} class="gridCanvas" style={{ top: '26px', left: U.headW + 'px', width: cw() + 'px', height: ch() + 'px', cursor: toolCursor() }}
                onPointerDown={gridDown} onPointerMove={hover} onDblClick={gridDbl} onContextMenu={e => e.preventDefault()} />
              <div ref={phGrid} class="phead" style={{ height: gridH() + 'px', 'z-index': 2 }} />
              <Show when={mq()}>{m => <div class="marquee" style={{ left: m().x + 'px', top: m().y + 'px', width: m().w + 'px', height: m().h + 'px' }} />}</Show>
            </div>
          </div>
        </div>
      </div>
      <Show when={U.dragOver}>
        <div class="dropHint"><span>Upuść, aby wstawić audio w miejscu kursora</span></div>
      </Show>
    </div>
  );
}

// ---------- lane headers & routing ----------
export function openRouteMenu(i: number, x: number, y: number) {
  const t = S.project.tracks[i]; if (!t) return;
  openMenu(x, y, [
    { t: 'title', label: t.name + ' → kanał miksera' },
    ...S.project.inserts.map(m => ({ t: 'item', label: insLabel(m.id), kbd: t.insert === m.id ? '●' : '', act: () => {
      A.setTrack(i, { insert: m.id }, 'Routing'); A.selectInsert(m.id); say(t.name + ' → ' + insLabel(m.id));
    } }) as MenuItem),
  ], 340);
}
const insLabel = (id: Id) => { const i = S.project.inserts.findIndex(m => m.id === id); const m = S.project.inserts[i]; return !m ? '?' : id === 0 ? 'Master' : 'Ins ' + i + ' · ' + m.name; };

function openLaneMenu(x: number, y: number, i: number) {
  const t = S.project.tracks[i];
  openMenu(x, y, [
    { t: 'title', label: t.name },
    { t: 'item', label: 'Zmień nazwę…', act: () => openRename(x, y, t.name, v => A.setTrack(i, { name: v }, 'Nazwa ścieżki')) },
    { t: 'swatches', current: t.color, pick: col => A.setTrack(i, { color: col }, 'Kolor ścieżki') },
    { t: 'icons', current: t.icon, color: t.color, pick: id => A.setTrack(i, { icon: id }, 'Ikona ścieżki') },
    { t: 'sep' },
    { t: 'item', label: t.muted ? 'Odcisz' : 'Wycisz', act: () => A.toggleTrackMute(i) },
    { t: 'item', label: t.solo ? 'Wyłącz solo' : 'Solo', act: () => A.toggleTrackSolo(i) },
    { t: 'item', label: t.arm ? 'Rozbrój nagrywanie' : 'Uzbrój do nagrywania', act: () => A.armTrack(i) },
    { t: 'item', label: 'Zaznacz klipy ścieżki', act: () => setU('sel', S.project.clips.filter(c => c.track === i).map(c => c.id)) },
    ...(CA.activeComp(i) ? [
      { t: 'item', label: U.takesOpen[t.id] ? 'Ukryj take\'i' : 'Pokaż take\'i', kbd: CA.takesLabel(CA.activeComp(i)!.comp.takes.length), act: () => CA.toggleTakes(i) } as MenuItem,
      { t: 'item', label: 'Spłaszcz comp', title: 'Renderuje comp do jednego klipu audio i usuwa take\'i (Ctrl+Z cofa)', act: () => CA.flatten(CA.activeComp(i)!.clip.id) } as MenuItem,
    ] : []),
    { t: 'item', label: 'Wysokość: domyślna', act: () => A.setTrack(i, { h: 56 }, 'Wysokość') },
    { t: 'sep' },
    { t: 'item', label: 'Wstaw ścieżkę powyżej', act: () => A.insertTrack(i) },
    { t: 'item', label: 'Wstaw ścieżkę poniżej', act: () => A.insertTrack(i + 1) },
    { t: 'item', label: 'Usuń ścieżkę', danger: true, disabled: S.project.tracks.length < 2, act: () => A.removeTrack(t.id) },
  ], 400);
}

function LaneHeader(props: { t: Track; i: number }) {
  const t = () => props.t;
  const anySolo = () => S.project.tracks.some(x => x.solo);
  const audible = () => !t().muted && (!anySolo() || t().solo);
  const routeIdx = () => S.project.inserts.findIndex(m => m.id === t().insert);
  const down = (e: PointerEvent) => {
    if (e.button === 2) { e.preventDefault(); return openLaneMenu(e.clientX, e.clientY, props.i); }
    if (e.button === 0) A.selectInsert(t().insert);
  };
  const resize = (e: PointerEvent) => {
    if (e.button !== 0) return;
    e.preventDefault(); e.stopPropagation();
    const g = new Gesture('Wysokość ścieżki');
    const sy = e.clientY, h0 = t().h;
    track(ev => {
      const h = clamp(Math.round(h0 + ev.clientY - sy), 28, 220);
      g.update([{ op: 'putTrack', track: { ...plain(t()), h } }]);
      say(t().name + ' · wysokość ' + h + ' px');
    }, () => g.end(), 'row-resize');
  };
  const volTarget = () => ({ kind: 'trackVol' as const, id: t().id });
  const shownVol = () => autoDisplay(volTarget()) ?? t().vol;
  const vol = (e: PointerEvent) => {
    if (e.button === 2) { e.preventDefault(); e.stopPropagation(); paramMenu(e.clientX, e.clientY, volTarget(), () => A.setTrack(props.i, { vol: VOL_UNITY }, 'Głośność'), t().color); return; }
    if (e.button !== 0) return;
    e.preventDefault(); e.stopPropagation();
    if (dblCheck('lvol' + props.i)) { A.setTrack(props.i, { vol: VOL_UNITY }, 'Głośność'); say(t().name + ' · 0.0 dB'); return; }
    const w = (e.currentTarget as HTMLElement).getBoundingClientRect().width;
    const g = new Gesture('Głośność ścieżki');
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
            <span class="laneicon" style={{ color: t().color }} title={'Ścieżka ' + (props.i + 1) + ' · PPM = zmień ikonę'}><InstIcon id={trackIcon(t())} size={13} /></span>
          </Show>
          <span class="nm" style={{ color: audible() ? 'var(--fg-hi)' : 'var(--fg4)' }} title="2× klik = zmień nazwę"
            onDblClick={e => openRename(e.clientX, e.clientY, t().name, v => A.setTrack(props.i, { name: v }, 'Nazwa ścieżki'))}>{t().name}</span>
          <div class="row" style={{ gap: '2px', flex: 'none' }}>
            <Show when={CA.activeComp(props.i)}>{r => (
              <button class="mini tk" classList={{ on: !!U.takesOpen[t().id] }} onPointerDown={stop} onClick={() => CA.toggleTakes(props.i)}
                title={'Take\'i (' + CA.takesLabel(r().comp.takes.length) + ') — pokaż/ukryj linie take\'ów'}>
                <svg width="7" height="7" viewBox="0 0 8 8" style={{ transform: U.takesOpen[t().id] ? 'rotate(90deg)' : 'none' }}><path d="M2 1l4 3-4 3z" fill="currentColor" /></svg>{r().comp.takes.length}
              </button>
            )}</Show>
            <button class="mini route" title={'Wyjście ścieżki: ' + (routeIdx() <= 0 ? 'Master' : 'Insert ' + routeIdx()) + ' — kliknij, by zmienić kanał miksera'} onPointerDown={stop} onClick={e => openRouteMenu(props.i, e.clientX, e.clientY)}>{routeIdx() <= 0 ? 'M' : 'Ins ' + routeIdx()}</button>
            <button class="mini m" classList={{ on: t().muted }} title="Mute (wyłącza solo)" onPointerDown={stop} onClick={() => A.toggleTrackMute(props.i)}>M</button>
            <button class="mini s" classList={{ on: t().solo }} title="Solo — tylko ta ścieżka · Ctrl+klik = dodaj do solo" onPointerDown={stop} onClick={e => A.toggleTrackSolo(props.i, e.ctrlKey || e.metaKey)}>S</button>
            <button class="mini r" classList={{ on: t().arm }} title="Uzbrój do nagrywania" onPointerDown={stop} onClick={() => A.armTrack(props.i)}>
              <svg width="7" height="7" viewBox="0 0 8 8"><circle cx="4" cy="4" r="3.5" fill="currentColor" /></svg>
            </button>
          </div>
        </div>
        <Show when={t().h >= 50}>
          <div class="row" style={{ gap: '6px', 'padding-left': '20px' }}>
            <div class="lvol" classList={{ auto: isAutomated(volTarget()) }} onPointerDown={vol} onContextMenu={e => e.preventDefault()} title="Głośność — przeciągnij, 2× klik = 0 dB, PPM = automatyzacja">
              <div class="track">
                <div class="fill" style={{ width: shownVol() * 100 + '%', background: audible() ? t().color : 'var(--fg6)' }} />
                <div class="thumb" style={{ left: shownVol() * 100 + '%' }} />
              </div>
            </div>
            <span class="mono" style={{ width: '44px', flex: 'none', 'text-align': 'right', 'font-size': '9px', color: 'var(--fg4)' }}>{volText(shownVol())}</span>
          </div>
        </Show>
      </div>
      <div class="lresize" onPointerDown={resize} title="Przeciągnij = wysokość ścieżki" />
    </div>
  );
}

// ---------- take lanes ----------
/** Crossfade choices at a comp boundary, seconds. */
const XF = [0.005, 0.01, 0.02, 0.05];

function openTakeMenu(x: number, y: number, row: TakeRow, take: Take) {
  const comp = row.comp, used = usedBeats(comp, take.id), aud = S.audition[comp.id] === take.id, unused = unusedTakes(comp).length;
  openMenu(x, y, [
    { t: 'title', label: 'Take · ' + take.name + (used > 0 ? ' · w compie' : ' · nieużyty') },
    { t: 'item', label: 'Użyj całego take\'a', kbd: 'klik w nazwę', act: () => CA.useWholeTake(row, take) },
    { t: 'item', label: aud ? 'Zakończ odsłuch solo' : 'Odsłuchaj solo', kbd: '▶', act: () => CA.audition(comp.id, aud ? null : take.id) },
    { t: 'item', label: 'Zmień nazwę…', act: () => openRename(x, y, take.name, v => CA.renameTake(comp, take.id, v)) },
    { t: 'sep' },
    { t: 'item', label: 'Usuń take', danger: true, act: () => askDeleteTake(x, y, comp, take) },
    { t: 'item', label: 'Usuń nieużyte take\'i' + (unused ? ' (' + unused + ')' : ''), disabled: !unused, act: () => CA.removeUnused(comp) },
    { t: 'sep' },
    { t: 'item', label: 'Spłaszcz comp', title: 'Renderuje comp do jednego klipu audio i usuwa take\'i (Ctrl+Z cofa)', act: () => CA.flatten(row.clip.id) },
  ], 290);
}

/** Deleting a take the comp plays asks first (its regions go silent). */
function askDeleteTake(x: number, y: number, comp: Comp, take: Take) {
  const used = usedBeats(comp, take.id);
  if (used <= 0) return CA.deleteTake(comp, take.id);
  openMenu(x, y, [
    { t: 'title', label: take.name + ' gra w compie (' + (Math.round(used * 100) / 100) + ' beat)' },
    { t: 'item', label: 'Usuń take i jego fragmenty compu', danger: true, act: () => CA.deleteTake(comp, take.id) },
    { t: 'item', label: 'Anuluj', act: () => {} },
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
              title="Klik = cały take do compu · przeciągnij po take'u w siatce = fragment · PPM = menu"
              onPointerDown={e => { stop(e); if (e.button === 2) { e.preventDefault(); openTakeMenu(e.clientX, e.clientY, r(), take); } else if (e.button === 0) CA.useWholeTake(r(), take); }}
              onContextMenu={e => e.preventDefault()}>
              <span class="tkbar" />
              <span class="tknm">{take.name}</span>
              <span class="tkuse" title="Ile tego take'a gra w compie">{used() > 0 ? pct() + '%' : '—'}</span>
              <button class="mini tka" classList={{ on: aud() }} title={aud() ? 'Zakończ odsłuch solo' : 'Odsłuch solo: ten take zamiast compu'}
                onPointerDown={stop} onClick={() => CA.audition(r().comp.id, aud() ? null : take.id)}>
                <svg width="7" height="7" viewBox="0 0 8 8"><path d="M1.5 0.5l6 3.5-6 3.5z" fill="currentColor" /></svg>
              </button>
              <button class="xbtn" title="Usuń take" onPointerDown={stop} onClick={e => askDeleteTake(e.clientX, e.clientY, r().comp, take)}>×</button>
            </div>
          );
        }}</For>
      </div>
    )}</Show>
  );
}
