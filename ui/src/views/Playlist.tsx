import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import { unwrap } from 'solid-js/store';
import { VOL_UNITY, type Clip, type Id, type Op, type Track } from '../api/types';
import { clampMove, moveClips, resizeClip, TOTAL } from '../lib/clipOps';
import { clamp, fmtPos, volText } from '../lib/format';
import { snapFloor, snapRes, snapRound } from '../lib/snap';
import * as A from '../state/actions';
import { commit, Gesture, wheelEdit } from '../state/edit';
import { peaksVersion } from '../state/peaks';
import { allocId, S, srcLenBeats, srcOf } from '../state/store';
import { curBeat, recState, tele } from '../state/telemetry';
import { closeMenu, openMenu, openRename, say, setU, U, type MenuItem } from '../state/ui';
import { dblCheck, track } from '../ui/drag';
import { addFrameTask, fitCanvas, fonts } from '../ui/frameLoop';
import { drawGrid, drawRuler, type View } from './playlistDraw';

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

interface Hit { c: Clip; edge: 'l' | 'r' | 'fi' | 'fo' | null }

export function Playlist() {
  let scrollEl!: HTMLDivElement, gridEl!: HTMLDivElement, gridCv!: HTMLCanvasElement, rulerCv!: HTMLCanvasElement;
  let phGrid!: HTMLDivElement, phRuler!: HTMLDivElement;
  const [vw, setVw] = createSignal(800), [vh, setVh] = createSignal(400);
  const [scroll, setScroll] = createSignal({ x: 0, y: 0 });

  const tops = createMemo(() => { const t: number[] = []; let y = 0; for (const tr of S.project.tracks) { t.push(y); y += tr.h; } t.push(y); return t; });
  createEffect(() => { geom.tops = tops(); });
  const gridW = () => TOTAL * U.bw;
  const gridH = () => tops()[tops().length - 1];
  const cw = () => Math.max(1, Math.min(vw() - U.headW, gridW()));
  const ch = () => Math.max(1, Math.min(vh() - 26, gridH()));
  const editable = () => U.tool === 'draw' || U.tool === 'paint' || U.tool === 'select';
  const toolCursor = () => ({ draw: 'crosshair', paint: 'copy', erase: 'not-allowed', mute: 'pointer', slice: 'col-resize', select: 'crosshair' })[U.tool];

  let dirtyGrid = true, dirtyRuler = true, lastRecV = -1, lastPhX = -1, fontEpoch = 0;
  // what the grid canvas depends on (not mixer state), split so a zoom does not re-walk every clip
  createEffect(() => { U.bw; U.snap; U.tool; S.transport.rec; S.project.bpm; S.project.loop?.a; S.project.loop?.b; peaksVersion(); scroll(); cw(); ch(); dirtyGrid = true; });
  createEffect(() => { for (const _ of U.sel) void _; dirtyGrid = true; });
  createEffect(() => { for (const t of S.project.tracks) { t.h; t.arm; t.color; } dirtyGrid = true; });
  createEffect(() => { for (const c of S.project.clips) { c.start; c.len; c.track; c.offset; c.muted; c.gain; c.fadeIn; c.fadeOut; c.ref; c.kind; c.norm; c.rev; } dirtyGrid = true; });
  createEffect(() => {
    for (const p of S.project.patterns) { p.name; p.color; p.len; for (const n of p.notes) { n.step; n.len; n.p; } }
    for (const s of S.project.sources) { s.name; s.color; s.dur; }
    dirtyGrid = true;
  });
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
    if (fontEpoch !== fonts.epoch) { fontEpoch = fonts.epoch; dirtyGrid = dirtyRuler = true; }
    if (dirtyGrid) {
      dirtyGrid = false;
      const W = cw(), H = ch(), o = offsets();
      fitCanvas(gridCv, W, H);
      const g = gridCv.getContext('2d')!;
      g.setTransform(dpr, 0, 0, dpr, 0, 0);
      g.fillStyle = '#111419'; g.fillRect(0, 0, W, H);
      const v: View = { offX: o.x, offY: o.y, W, H, bw: U.bw, snap: U.snap, spb: 60 / S.project.bpm, tops: tops(), sel: new Set(U.sel), editable: editable(), recArmedBg: S.transport.rec };
      drawGrid(g, v, unwrap(S.project), recActive ? recState : null);
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
    if (U.follow && tele.frame.playing && b >= 0) {
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

  // ---------- wheel ----------
  const onWheel = (e: WheelEvent) => {
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
      if (c.kind === 'audio' && editable() && cwid > 36 && y - cy >= 15 && y - cy <= 25) {
        const fi = cx + c.fadeIn / spb * bw, fo = cx + cwid - c.fadeOut / spb * bw;
        if (x >= fi - 1 && x <= fi + 8) edge = 'fi';
        else if (x >= fo - 8 && x <= fo + 1) edge = 'fo';
      }
      if (!edge) edge = x - cx < 6 ? 'l' : cx + cwid - x < 6 ? 'r' : null;
      return { c, edge };
    }
    return null;
  }

  const hover = (e: PointerEvent) => {
    if (e.buttons) return;
    const p = pt(e), h = hit(p.x, p.y);
    let cur = toolCursor();
    if (h) {
      if (h.edge === 'fi' || h.edge === 'fo') cur = 'col-resize';
      else if (h.edge) cur = editable() ? 'ew-resize' : toolCursor();
      else cur = editable() ? 'grab' : toolCursor();
    }
    if (gridCv.style.cursor !== cur) gridCv.style.cursor = cur;
  };

  // ---------- pointer interactions ----------
  const gridDown = (e: PointerEvent) => {
    const p = pt(e), h = hit(p.x, p.y), id = h?.c.id ?? null;
    const modK = e.ctrlKey || e.metaKey;
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
      setU('cur', { kind: clip.kind, ref: clip.ref });
      if (h.edge === 'fi' || h.edge === 'fo') return fadeDrag(e, clip, h.edge);
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
    const srcLen = o.kind === 'audio' ? srcLenBeats('audio', o.ref) : Infinity;
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
      if (h) openClipMenu(sx, sy, h.c, p); else openGridMenu(sx, sy, p);
    });
  }

  const gridDbl = (e: MouseEvent) => {
    const p = pt(e), h = hit(p.x, p.y); if (!h) return;
    setU('cur', { kind: h.c.kind, ref: h.c.ref });
    if (h.c.kind === 'audio') A.openEditor(h.c.id);
  };

  // ---------- menus ----------
  function openClipMenu(x: number, y: number, c: Clip, p: { beat: number }) {
    if (!U.sel.includes(c.id)) setU('sel', [c.id]);
    const src = srcOf(c), isPat = c.kind === 'pat';
    const items: MenuItem[] = [
      { t: 'title', label: (isPat ? 'Pattern · ' : 'Audio · ') + (src?.name ?? '') },
      { t: 'item', label: 'Zmień nazwę…', act: () => openRename(x, y, src?.name ?? '', v => A.setSrcProp(c, { name: v })) },
      { t: 'swatches', current: src?.color, pick: col => A.setSrcProp(c, { color: col }) },
      { t: 'sep' },
      { t: 'item', label: c.muted ? 'Odcisz' : 'Wycisz', kbd: 'Ctrl+M', act: A.toggleMuteSel },
      { t: 'item', label: 'Duplikuj', kbd: 'Ctrl+B', act: A.duplicate },
      { t: 'item', label: 'Kopiuj', kbd: 'Ctrl+C', act: A.copy },
      { t: 'item', label: 'Wytnij', kbd: 'Ctrl+X', act: A.cut },
      { t: 'item', label: 'Podziel tutaj', act: () => A.sliceAt(c.id, snapRound(p.beat, unit(false))) },
      { t: 'item', label: 'Podziel na głowicy', act: () => A.sliceAt(c.id, curBeat()) },
      isPat ? { t: 'item', label: 'Make unique', act: () => A.makeUnique(c) } : { t: 'gain', clipId: c.id },
      ...(isPat ? [] : [{ t: 'item', label: 'Edytuj sample…', kbd: '2× klik', act: () => A.openEditor(c.id) } as MenuItem]),
      { t: 'item', label: 'Kanał miksera…', act: () => openRouteMenu(c.track, x, y) },
      { t: 'item', label: 'Zaznacz wszystkie z tego źródła', act: () => setU('sel', S.project.clips.filter(v => v.kind === c.kind && v.ref === c.ref).map(v => v.id)) },
      { t: 'sep' },
      { t: 'item', label: 'Usuń', kbd: 'Del', danger: true, act: () => A.delIds(U.sel.includes(c.id) ? [...U.sel] : [c.id]) },
    ];
    openMenu(x, y, items, 420);
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
  const rulerDown = (e: PointerEvent) => {
    if (e.button !== 0) return;
    e.preventDefault(); closeMenu();
    if (dblCheck('ruler')) { if (S.project.loop) { A.setLoop(null, 'Usuń pętlę'); say('Pętla usunięta'); } return; }
    const r = gridEl.getBoundingClientRect();
    const b0 = clamp(snapRound((e.clientX - r.left) / U.bw, unit(e.altKey)), 0, TOTAL);
    let g: Gesture | null = null;
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

  return (
    <div class="plwrap">
      <div ref={scrollEl} class="plscroll">
        <div style={{ width: U.headW + gridW() + 'px', position: 'relative' }}>
          <div class="rulerRow">
            <div class="plhead" style={{ width: U.headW + 'px' }}>
              <span style={{ font: '600 10px var(--sans)', 'letter-spacing': '.12em', color: 'var(--fg4)' }}>PLAYLIST</span>
              <span class="mono" style={{ 'font-size': '10px', color: 'var(--fg5)' }}>{S.project.tracks.length} tr</span>
            </div>
            <div style={{ position: 'relative', width: gridW() + 'px', height: '26px', flex: 'none' }}>
              <canvas ref={rulerCv} class="ruler" title="Klik = start · przeciągnij = pętla · 2× klik = usuń pętlę"
                style={{ left: U.headW + 'px', width: cw() + 'px' }} onPointerDown={rulerDown} />
              <div ref={phRuler} class="phead" style={{ height: '26px', 'z-index': 2 }} />
            </div>
          </div>
          <div style={{ display: 'flex' }}>
            <div class="lanesCol" style={{ width: U.headW + 'px' }}>
              <For each={S.project.tracks}>{(t, i) => <LaneHeader t={t} i={i()} />}</For>
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
    { t: 'sep' },
    { t: 'item', label: t.muted ? 'Odcisz' : 'Wycisz', act: () => A.setTrack(i, { muted: !t.muted }, 'Mute') },
    { t: 'item', label: t.solo ? 'Wyłącz solo' : 'Solo', act: () => A.setTrack(i, { solo: !t.solo }, 'Solo') },
    { t: 'item', label: t.arm ? 'Rozbrój nagrywanie' : 'Uzbrój do nagrywania', act: () => A.armTrack(i) },
    { t: 'item', label: 'Zaznacz klipy ścieżki', act: () => setU('sel', S.project.clips.filter(c => c.track === i).map(c => c.id)) },
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
  const vol = (e: PointerEvent) => {
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
    <div class="lane" classList={{ armrec: t().arm && S.transport.rec }} style={{ height: t().h + 'px' }} onPointerDown={down}>
      <div class="stripe" style={{ background: t().color }} />
      <div class="body">
        <div class="row" style={{ gap: '6px', 'min-width': 0 }}>
          <span class="num">{props.i + 1}</span>
          <span class="nm" style={{ color: audible() ? '#f2f4f7' : '#7d8592' }} title="2× klik = zmień nazwę"
            onDblClick={e => openRename(e.clientX, e.clientY, t().name, v => A.setTrack(props.i, { name: v }, 'Nazwa ścieżki'))}>{t().name}</span>
          <div class="row" style={{ gap: '2px', flex: 'none' }}>
            <button class="mini route" title="Kanał miksera" onPointerDown={stop} onClick={e => openRouteMenu(props.i, e.clientX, e.clientY)}>{routeIdx() <= 0 ? '→M' : '→' + routeIdx()}</button>
            <button class="mini m" classList={{ on: t().muted }} title="Mute" onPointerDown={stop} onClick={() => A.setTrack(props.i, { muted: !t().muted }, 'Mute')}>M</button>
            <button class="mini s" classList={{ on: t().solo }} title="Solo" onPointerDown={stop} onClick={() => A.setTrack(props.i, { solo: !t().solo }, 'Solo')}>S</button>
            <button class="mini r" classList={{ on: t().arm }} title="Uzbrój do nagrywania" onPointerDown={stop} onClick={() => A.armTrack(props.i)}>
              <svg width="7" height="7" viewBox="0 0 8 8"><circle cx="4" cy="4" r="3.5" fill="currentColor" /></svg>
            </button>
          </div>
        </div>
        <Show when={t().h >= 50}>
          <div class="row" style={{ gap: '6px', 'padding-left': '20px' }}>
            <div class="lvol" onPointerDown={vol} title="Głośność — przeciągnij, 2× klik = 0 dB">
              <div class="track">
                <div class="fill" style={{ width: t().vol * 100 + '%', background: audible() ? t().color : '#4a5360' }} />
                <div class="thumb" style={{ left: t().vol * 100 + '%' }} />
              </div>
            </div>
            <span class="mono" style={{ width: '44px', flex: 'none', 'text-align': 'right', 'font-size': '9px', color: 'var(--fg4)' }}>{volText(t().vol)}</span>
          </div>
        </Show>
      </div>
      <div class="lresize" onPointerDown={resize} title="Przeciągnij = wysokość ścieżki" />
    </div>
  );
}
