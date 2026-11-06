// Note-based pitch editor of an audio clip. The engine analyses the source (`pitch_analyze`)
// and renders `clip.tune` offline; here the user sees notes as blobs (thickness = level) at their
// edited pitch with the original (faint) and edited (bright) pitch curve through them, and edits
// them with tools: Main (pitch, snapped to the scale; Alt = free), Drift, Formant, Gain, Split,
// Pencil. Every drag is one gesture (= one undo step); the engine re-renders when it ends.
import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import { be } from '../api/backend';
import type { Clip, ClipTune, Op, PitchAnalysis } from '../api/types';
import { clamp, dbg, dbText, fmtPos } from '../lib/format';
import {
  addPencil, correctPitch, dragShift, editedCurve, editNotes, erasePencil, inScale, initTune, isBlack, mergeNotes, MIN_NOTE,
  noteCenter, NOTE_NAMES, noteName, scaleMask, splitNear, splitNote,
} from '../lib/pitch';
import * as A from '../state/actions';
import { commit, Gesture, inGesture } from '../state/edit';
import { clipPeaksPending, getClipPeaks, peakRange, peaksVersion } from '../state/peaks';
import { S } from '../state/store';
import { tele } from '../state/telemetry';
import { say, setU, U } from '../state/ui';
import { dblCheck, track } from '../ui/drag';
import { addFrameTask, fitCanvas, fonts, Layer } from '../ui/frameLoop';
import { FXMETA, Icon } from '../ui/icons';
import { NumField, parseNum, Segmented } from '../ui/Knob';
import { rgba, T, theme, themeVersion } from '../ui/theme';
import { moveWindow } from './FxWindow';

type Tool = 'main' | 'drift' | 'formant' | 'gain' | 'split' | 'pencil';
const TOOLS: { id: Tool; key: string; label: string; icon: string; tip: string }[] = [
  { id: 'main', key: '1', label: 'Wysokość', icon: 'M12 3v18 M7 8l5-5 5 5 M7 16l5 5 5-5', tip: 'Przeciągnij nutę ↕ — przyciąga do skali, Alt = swobodnie (centy) · 2× klik = podziel' },
  { id: 'drift', key: '2', label: 'Dryf', icon: 'M3 12c2-4 4-4 6 0s4 4 6 0 4-4 6 0', tip: 'Przeciągnij ↕ — spłaszcza wahania wysokości w nucie (0–100 %)' },
  { id: 'formant', key: '3', label: 'Formant', icon: 'M4 18c3-12 5-12 8 0s5-8 8-8', tip: 'Przeciągnij ↕ — przesuwa formanty (barwę) w centach' },
  { id: 'gain', key: '4', label: 'Głośność', icon: 'M4 9h4l5-4v14l-5-4H4z M17 9a4 4 0 0 1 0 6', tip: 'Przeciągnij ↕ — głośność nuty w dB' },
  { id: 'split', key: '5', label: 'Podział', icon: 'M12 3v18 M5 8h4 M15 16h4', tip: 'Klik na nucie = podziel w tym miejscu · klik na podziale = scal' },
  { id: 'pencil', key: '6', label: 'Ołówek', icon: 'M4 20l4.5-1L19 8.5 15.5 5 5 15.5z', tip: 'Rysuj przebieg wysokości · Alt + przeciągnij = zmaż rysunek' },
];
const SCALES = ['Chromatic', 'Major', 'Minor', 'Maj pent', 'Min pent'];
const KW = 46, RH = 22, BH = 48;

const anCache = new Map<string, PitchAnalysis>();
const plain = <T,>(v: T): T => JSON.parse(JSON.stringify(v));

export function PitchEditor() {
  const clip = () => U.pitchEd ? S.project.clips.find(c => c.id === U.pitchEd!.clipId && c.kind === 'audio') : undefined;
  return <Show when={clip()}>{c => <PitchBody clipId={c().id} />}</Show>;
}

function PitchBody(props: { clipId: number }) {
  let cv!: HTMLCanvasElement, area!: HTMLDivElement, phead!: HTMLDivElement, status!: HTMLSpanElement, root!: HTMLDivElement;
  const E = () => U.pitchEd!;
  const c = () => S.project.clips.find(x => x.id === props.clipId)!;
  const src = () => S.project.sources.find(s => s.id === c()?.ref);
  const spb = () => 60 / S.project.bpm;
  const t0 = () => c().offset * spb(), t1 = () => (c().offset + c().len) * spb();
  const anKey = () => c().ref + '|' + (c().norm ? 1 : 0) + (c().rev ? 1 : 0);
  const [an, setAn] = createSignal<PitchAnalysis | null>(anCache.get(anKey()) ?? null);
  const [err, setErr] = createSignal<string | null>(null);
  const load = () => {
    const k = anKey();
    const hit = anCache.get(k); if (hit) { setAn(hit); return; }
    setAn(null); setErr(null);
    be().pitchAnalyze(props.clipId).then(a => { anCache.set(k, a); if (anKey() === k) { setAn(a); fitView(a); } }).catch(e => setErr(String(e?.message ?? e)));
  };
  createEffect(() => { anKey(); load(); });

  // ---- editor state
  const tuneFx = () => { const tr = S.project.tracks[c().track]; return S.project.inserts.find(m => m.id === tr?.insert)?.fx.find(f => f.type === 'tune'); };
  const [tool, setTool] = createSignal<Tool>('main');
  const [sel, setSel] = createSignal<Set<number>>(new Set());
  const [key, setKey] = createSignal(Math.round(tuneFx()?.params.key ?? 0));
  const [scale, setScale] = createSignal(Math.round(tuneFx()?.params.scale ?? 0));
  const [centerPct, setCenterPct] = createSignal(100);
  const [driftPct, setDriftPct] = createSignal(40);
  const [hover, setHover] = createSignal<{ note: number; split: number } | null>(null);
  const [marq, setMarq] = createSignal<{ x0: number; y0: number; x1: number; y1: number } | null>(null);
  const [pencilLive, setPencilLive] = createSignal<[number, number][] | null>(null);
  const view = { t: 0, pps: 100, lo: 48, hi: 72 };
  const [viewV, setViewV] = createSignal(0);
  const bumpView = () => setViewV(v => v + 1);
  let size = { W: 0, H: 0 };

  const fitView = (a: PitchAnalysis | null = an()) => {
    const W = Math.max(200, size.W || E().w - 2);
    view.t = t0(); view.pps = (W - KW) / Math.max(0.2, t1() - t0());
    // fit the sung range by time: 2nd–98th percentile of note time, so a stray blob can't zoom out to octaves
    const vis = a ? a.notes.filter(n => n.end > t0() && n.start < t1()).sort((x, y) => x.pitch - y.pitch) : [];
    let lo = 55, hi = 67;
    if (vis.length) {
      const tot = vis.reduce((s, n) => s + (n.end - n.start), 0);
      const at = (q: number) => { let acc = 0; for (const n of vis) { acc += n.end - n.start; if (acc >= q * tot) return n.pitch; } return vis[vis.length - 1].pitch; };
      lo = at(0.02); hi = at(0.98);
    }
    const mid = (lo + hi) / 2, span = Math.max(14, hi - lo + 7);
    view.lo = mid - span / 2; view.hi = mid + span / 2;
    bumpView();
  };

  const tune = (): ClipTune | null => { const t = c().tune; return t && t.notes.length ? t : null; };
  const tuneOrInit = (): ClipTune => tune() ? plain(tune()!) : initTune(an()!);
  const notes = () => tuneOrInit().notes;
  const centers = createMemo(() => { const a = an(); if (!a) return []; return notes().map(n => noteCenter(a, n.start, n.end)); });
  const edited = createMemo(() => { const a = an(); return a ? editedCurve(a, tune(), tune() ? centers() : undefined) : new Float32Array(0); });
  const mask = () => scaleMask(scale());
  const putTune = (t: ClipTune | null): Op => ({ op: 'putClips', clips: [{ ...plain(c()), tune: t } as Clip] });

  // ---- geometry
  const X = (t: number) => KW + (t - view.t) * view.pps;
  const tAt = (x: number) => view.t + (x - KW) / view.pps;
  const noteH = () => size.H - RH - BH;
  const rowH = () => noteH() / (view.hi - view.lo);
  const Y = (m: number) => RH + (view.hi - m) * rowH();
  const mAt = (y: number) => view.hi - (y - RH) / rowH();
  const pitchOf = (i: number) => { const n = notes()[i], cc = centers()[i]; return cc == null ? null : cc + n.shift / 100; };

  // ---- hit testing
  const halfThick = (lv: number) => Math.max(2.5, rowH() * (0.16 + 0.42 * clamp(lv, 0, 1)));
  const hitNote = (x: number, y: number): number => {
    const ns = notes(), t = tAt(x);
    for (let i = 0; i < ns.length; i++) {
      const n = ns[i]; if (t < n.start || t >= n.end) continue;
      const p = pitchOf(i); if (p == null) continue;
      const a = an()!, lv = a.levels[Math.min(a.levels.length - 1, Math.floor(t * a.rate))] * dbg(n.gain);
      if (Math.abs(y - Y(p)) <= halfThick(lv) + 4) return i;
    }
    return -1;
  };
  const hitSplit = (x: number, y: number): number => {
    const ns = notes(), i = splitNear(ns, tAt(x), 5 / view.pps);
    if (i < 0) return -1;
    const p1 = pitchOf(i), p2 = pitchOf(i + 1); if (p1 == null || p2 == null) return -1;
    const ya = Y(Math.max(p1, p2)) - rowH(), yb = Y(Math.min(p1, p2)) + rowH();
    return y >= ya && y <= yb ? i : -1;
  };

  // ---- drawing
  const layer = new Layer();
  let dirty = true;
  createEffect(() => {
    viewV(); sel(); hover(); tool(); key(); scale(); marq(); pencilLive(); an(); peaksVersion(); edited(); themeVersion();
    JSON.stringify(c().tune ?? null); c().offset; c().len; c().gain; S.project.bpm; src()?.color;
    dirty = true;
  });
  onMount(() => {
    root.focus();
    const stop = addFrameTask(() => {
      const W = area.clientWidth, H = area.clientHeight;
      if (!W || !H) return;
      if (W !== size.W || H !== size.H) { const first = !size.W; size = { W, H }; if (first) fitView(); dirty = true; }
      if (dirty) { dirty = false; draw(W, H); }
      // playhead (telemetry), as a DOM line — no canvas redraw per frame
      const f = tele.frame;
      let ts = -1;
      if (f.playing && f.beat >= c().start && f.beat < c().start + c().len) ts = c().offset * spb() + (f.beat - c().start) * spb();
      else if (f.previewing && f.previewPos >= 0) ts = f.previewPos;
      if (ts >= 0 && X(ts) >= KW && X(ts) <= W) { phead.style.opacity = '1'; phead.style.transform = `translateX(${X(ts)}px)`; }
      else if (phead.style.opacity !== '0') phead.style.opacity = '0';
      const busy = !inGesture() && clipPeaksPending(c());
      const txt = !an() ? '' : busy ? 'renderuję…' : '';
      if (status.textContent !== txt) status.textContent = txt;
    });
    onCleanup(stop);
  });

  const draw = (W: number, H: number) => {
    fitCanvas(cv, W, H);
    const g = cv.getContext('2d')!, dpr = window.devicePixelRatio || 1;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    const a = an(), accent = FXMETA.tune.color, col = src()?.color ?? accent;
    const rh = rowH();
    // static-ish background (rows + keyboard) cached per view/scale
    const bgKey = [view.lo.toFixed(3), view.hi.toFixed(3), key(), scale(), W, H].join('|');
    g.drawImage(layer.get(W, H, bgKey, (b, w) => {
      b.fillStyle = T.viz; b.fillRect(0, 0, w, H);
      b.font = '500 9px "Geist Mono Variable", monospace'; b.textBaseline = 'middle';
      for (let m = Math.floor(view.lo); m <= Math.ceil(view.hi); m++) {
        const y = Y(m + 0.5), inS = inScale(m, key(), mask()), root = ((m - key()) % 12 + 12) % 12 === 0;
        b.fillStyle = root ? rgba(accent, 0.08 * T.tint) : inS ? T.vizPanel : T.viz; b.fillRect(KW, y, w - KW, rh);
        b.fillStyle = T.vizGrid; b.fillRect(KW, Math.round(y + rh) - 0.5, w - KW, 1);
        const blk = isBlack(m);
        b.fillStyle = blk ? T.keyB : T.keyW; b.fillRect(0, y + 0.5, KW - 2, Math.max(1, rh - 1));
        if (!inS) { b.fillStyle = rgba(T.viz, 0.5); b.fillRect(0, y + 0.5, KW - 2, rh - 1); }
        if (root) { b.fillStyle = accent; b.fillRect(KW - 5, y + 1, 3, rh - 2); }
        if (rh >= 10 && (root || m % 12 === 0 || rh >= 15)) { b.fillStyle = blk ? T.keyBText : T.keyWText; b.fillText(NOTE_NAMES[((m % 12) + 12) % 12] + (Math.floor(m / 12) - 1), 4, y + rh / 2 + 0.5); }
      }
      b.textBaseline = 'alphabetic';
    }) as CanvasImageSource, 0, 0, W, H);
    g.save(); g.beginPath(); g.rect(KW, 0, W - KW, H); g.clip();
    // beat grid + ruler
    const sp = spb(), b0 = c().start + (tAt(KW) - t0()) / sp, b1 = c().start + (tAt(W) - t0()) / sp;
    const beatX = (bt: number) => X(t0() + (bt - c().start) * sp);
    const step = view.pps * sp < 14 ? 4 : 1;
    g.fillStyle = T.rulerBg; g.fillRect(KW, 0, W - KW, RH);
    g.font = '500 9px "Geist Mono Variable", monospace';
    for (let bt = Math.ceil(b0 / step) * step; bt <= b1; bt += step) {
      const x = Math.round(beatX(bt)) + 0.5, bar = bt % 4 === 0;
      g.strokeStyle = bar ? T.vizGrid2 : T.vizGrid; g.beginPath(); g.moveTo(x, RH); g.lineTo(x, H - BH); g.stroke();
      if (bar || view.pps * sp > 40) { g.fillStyle = bar ? T.fg3 : T.fg5; g.fillText(fmtPos(bt), x + 3, 14); }
    }
    g.fillStyle = T.vizAxis; g.fillRect(KW, RH - 1, W - KW, 1);
    // outside the clip region
    g.fillStyle = T.shade;
    if (X(t0()) > KW) g.fillRect(KW, RH, X(t0()) - KW, H - RH);
    if (X(t1()) < W) g.fillRect(X(t1()), RH, W - X(t1()), H - RH);
    if (!a) { g.restore(); return; }
    const vt0 = Math.max(0, tAt(KW) - 0.05), vt1 = tAt(W) + 0.05;
    const f0 = Math.max(0, Math.floor(vt0 * a.rate)), f1 = Math.min(a.curve.length, Math.ceil(vt1 * a.rate));
    // unvoiced but audible (sibilants, breaths): hatched — left untouched by the editor
    const hatch = (xa: number, xb: number, ya: number, yb: number, alpha: number) => {
      g.save(); g.beginPath(); g.rect(xa, ya, xb - xa, yb - ya); g.clip();
      g.strokeStyle = rgba(T.fg4, alpha); g.lineWidth = 1; g.beginPath();
      for (let x = xa - (yb - ya); x < xb; x += 6) { g.moveTo(x, yb); g.lineTo(x + (yb - ya), ya); }
      g.stroke(); g.restore();
    };
    const runs: [number, number][] = [];
    let rs = -1;
    for (let f = f0; f < f1; f++) {
      const un = a.curve[f] == null && a.levels[f] > 0.03;
      if (un && rs < 0) rs = f; else if (!un && rs >= 0) { runs.push([rs, f]); rs = -1; }
    }
    if (rs >= 0) runs.push([rs, f1]);
    for (const [fa, fb] of runs) hatch(X(fa / a.rate), X(fb / a.rate), RH, H - BH, 0.12);
    // note blobs
    const ns = notes(), cs = centers(), s = sel(), hv = hover();
    for (let i = 0; i < ns.length; i++) {
      const n = ns[i]; if (n.end < vt0 || n.start > vt1) continue;
      const cc = cs[i]; if (cc == null) continue;
      const pm = cc + n.shift / 100, yc = Y(pm), gn = dbg(n.gain);
      const fa = Math.floor(n.start * a.rate), fb = Math.min(a.levels.length, Math.ceil(n.end * a.rate));
      const stepF = Math.max(1, Math.floor((fb - fa) / Math.max(4, (X(n.end) - X(n.start)) / 3)));
      const top: number[] = [];
      for (let f = fa; f < fb; f += stepF) top.push(X(f / a.rate), halfThick(a.levels[f] * gn));
      top.push(X(n.end), halfThick(a.levels[Math.max(fa, fb - 1)] * gn));
      g.beginPath();
      g.moveTo(top[0], yc);
      for (let k = 0; k < top.length; k += 2) g.lineTo(top[k], yc - top[k + 1]);
      for (let k = top.length - 2; k >= 0; k -= 2) g.lineTo(top[k], yc + top[k + 1]);
      g.closePath();
      const isSel = s.has(i), isHov = hv?.note === i;
      g.fillStyle = rgba(col, isSel ? 0.62 : 0.34); g.fill();
      g.lineJoin = 'round'; g.strokeStyle = isSel ? T.ink : isHov ? rgba(col, 1) : rgba(col, 0.7); g.lineWidth = isSel ? 1.5 : 1; g.stroke();
      // label: note + cents off, and edits
      const w = X(n.end) - X(n.start);
      if (w > 34 && rh > 8) {
        const cents = Math.round((pm - Math.round(pm)) * 100);
        let lab = noteName(pm) + (Math.abs(cents) >= 3 ? (cents > 0 ? ' +' : ' −') + Math.abs(cents) : '');
        if (n.drift) lab += ' · D' + Math.round(n.drift * 100) + '%';
        if (n.formant) lab += ' · F' + (n.formant > 0 ? '+' : '') + Math.round(n.formant);
        if (n.gain) lab += ' · ' + dbText(n.gain);
        g.font = '500 9px "Geist Mono Variable", monospace'; g.fillStyle = isSel ? T.fgHi : T.fg3;
        g.fillText(lab, X(n.start) + 3, yc - Math.max(halfThick(n.gain ? 0.6 : 0.5), 6) - 4);
      }
    }
    // split markers (touching notes)
    for (let i = 0; i + 1 < ns.length; i++) {
      if (Math.abs(ns[i].end - ns[i + 1].start) > 1e-6 || ns[i].end < vt0 || ns[i].end > vt1) continue;
      const p1 = pitchOf(i), p2 = pitchOf(i + 1); if (p1 == null || p2 == null) continue;
      const x = Math.round(X(ns[i].end)) + 0.5;
      g.strokeStyle = hv?.split === i ? T.ink : rgba(T.fg3, 0.7); g.lineWidth = hv?.split === i ? 2 : 1;
      g.beginPath(); g.moveTo(x, Y(Math.max(p1, p2)) - rh * 0.6); g.lineTo(x, Y(Math.min(p1, p2)) + rh * 0.6); g.stroke();
    }
    // pitch curves: original faint, edited bright
    const curve = (val: (f: number) => number | null, stroke: string, lw: number) => {
      g.beginPath(); let pen = false;
      for (let f = f0; f < f1; f++) {
        const v = val(f);
        if (v == null || Number.isNaN(v)) { pen = false; continue; }
        const x = X(f / a.rate), y = Y(v);
        if (!pen) { g.moveTo(x, y); pen = true; } else g.lineTo(x, y);
      }
      g.strokeStyle = stroke; g.lineWidth = lw; g.stroke();
    };
    const ed = edited();
    curve(f => a.curve[f], rgba(T.pre, tune() ? 0.45 : 0.8), 1);
    if (tune()) curve(f => ed[f], accent, 1.6);
    const pl = pencilLive();
    if (pl && pl.length > 1) { g.beginPath(); pl.forEach(([t, m], k) => (k ? g.lineTo(X(t), Y(m)) : g.moveTo(X(t), Y(m)))); g.strokeStyle = T.ink; g.lineWidth = 2; g.stroke(); }
    // bottom strip: what the clip plays (rendered) + unvoiced stretches
    const by = H - BH;
    g.fillStyle = T.vizPanel; g.fillRect(KW, by, W - KW, BH);
    g.fillStyle = T.vizAxis; g.fillRect(KW, by, W - KW, 1);
    const pk = getClipPeaks(c());
    if (pk) {
      const mid = by + BH / 2 + 2, gn = dbg(c().gain);
      g.fillStyle = rgba(col, 0.75); g.beginPath();
      for (let x = KW; x < W; x++) {
        const i0 = Math.floor(tAt(x) * 100), i1 = Math.floor(tAt(x + 1) * 100);
        if (i0 < 0 || i0 * 2 >= pk.length) continue;
        const [mn, mx] = peakRange(pk, i0, i1);
        const ya = mid - clamp(mx * gn, 0, 1) * (BH / 2 - 4), yb = mid - clamp(mn * gn, -1, 0) * (BH / 2 - 4);
        g.rect(x, ya, 1, Math.max(0.6, yb - ya));
      }
      g.fill();
    }
    for (const [fa, fb] of runs) { g.fillStyle = rgba(T.amber, 0.12); g.fillRect(X(fa / a.rate), by + 1, X(fb / a.rate) - X(fa / a.rate), BH - 1); hatch(X(fa / a.rate), X(fb / a.rate), by + 1, H, 0.35); }
    g.font = '600 8px "Geist Variable", sans-serif'; g.fillStyle = T.fg5; g.fillText('BEZDŹWIĘCZNE (S, ODDECH) — NIETKNIĘTE', KW + 6, by + 11);
    // marquee
    const mq = marq();
    if (mq) { g.fillStyle = rgba(T.blue, 0.1); g.strokeStyle = T.blue; g.lineWidth = 1; const x = Math.min(mq.x0, mq.x1), y = Math.min(mq.y0, mq.y1); g.fillRect(x, y, Math.abs(mq.x1 - mq.x0), Math.abs(mq.y1 - mq.y0)); g.strokeRect(x + 0.5, y + 0.5, Math.abs(mq.x1 - mq.x0), Math.abs(mq.y1 - mq.y0)); }
    g.restore();
  };

  // ---- pointer
  const pos = (e: PointerEvent | WheelEvent | MouseEvent) => { const r = cv.getBoundingClientRect(); return { x: e.clientX - r.left, y: e.clientY - r.top }; };
  const targets = (i: number) => sel().has(i) ? [...sel()] : [i];
  const editDrag = (e: PointerEvent, i: number, label: string, patch: (n: ClipTune['notes'][number], i: number, dy: number, ev: PointerEvent) => Partial<ClipTune['notes'][number]>) => {
    const idx = targets(i); if (!sel().has(i)) setSel(new Set([i]));
    const base = tuneOrInit(), sy = e.clientY;
    let g: Gesture | null = null;
    track(ev => {
      const dy = (sy - ev.clientY) * (ev.shiftKey ? 0.25 : 1);
      if (!g) { if (Math.abs(dy) < 2) return; g = new Gesture(label); }
      const t = editNotes(base, idx, (n, k) => patch(base.notes[k], k, dy, ev));
      g.update([putTune(t)]);
      const n0 = t.notes[i];
      say(label + ' · ' + (tool() === 'main' ? noteName((centers()[i] ?? 0) + n0.shift / 100) + ' (' + (n0.shift >= 0 ? '+' : '') + Math.round(n0.shift) + ' ct)' : tool() === 'drift' ? Math.round(n0.drift * 100) + '%' : tool() === 'formant' ? (n0.formant >= 0 ? '+' : '') + Math.round(n0.formant) + ' ct' : dbText(n0.gain)));
    }, () => { g?.end(); }, 'ns-resize');
  };

  const down = (e: PointerEvent) => {
    root.focus();
    if (!an()) return;
    const { x, y } = pos(e);
    if (e.button === 1 || (e.button === 0 && e.altKey && tool() !== 'pencil' && tool() !== 'main')) return pan(e);
    if (e.button !== 0) return;
    e.preventDefault();
    if (y < RH) { A.seek(c().start + (tAt(x) - t0()) / spb()); return; }
    if (x < KW) return;
    const i = hitNote(x, y), sp = hitSplit(x, y), t = tool();
    if (t === 'pencil') return pencil(e);
    if (t === 'split' || (t === 'main' && dblCheck('pnote' + props.clipId))) {
      if (sp >= 0) { const nt = mergeNotes(tuneOrInit(), sp); if (nt) { commit([putTune(nt)], 'Scal nuty'); setSel(new Set<number>()); say('Scalono nuty'); } return; }
      if (i >= 0) { const nt = splitNote(tuneOrInit(), i, tAt(x)); if (nt) { commit([putTune(nt)], 'Podziel nutę'); setSel(new Set([i + 1])); say('Podzielono nutę w ' + tAt(x).toFixed(3) + ' s'); } else say('Za blisko krawędzi nuty (min. ' + MIN_NOTE * 1000 + ' ms)'); }
      return;
    }
    if (i < 0) return marquee(e, x, y);
    if (e.ctrlKey || e.metaKey || e.shiftKey) { const s2 = new Set(sel()); s2.has(i) ? s2.delete(i) : s2.add(i); setSel(s2); return; }
    const k0 = key(), m0 = mask();
    if (t === 'main') editDrag(e, i, 'Wysokość nuty', (n, k, dy, ev) => ({ shift: dragShift(centers()[k] ?? 0, n.shift, dy / rowH() * 100, k0, m0, ev.altKey) }));
    else if (t === 'drift') editDrag(e, i, 'Dryf', (n, _k, dy) => ({ drift: clamp(n.drift + dy / 120, 0, 1) }));
    else if (t === 'formant') editDrag(e, i, 'Formant', (n, _k, dy) => ({ formant: Math.round(clamp(n.formant + dy * 3, -1200, 1200)) }));
    else if (t === 'gain') editDrag(e, i, 'Głośność nuty', (n, _k, dy) => ({ gain: Math.round(clamp(n.gain + dy * 0.1, -24, 12) * 10) / 10 }));
  };
  const marquee = (e: PointerEvent, x: number, y: number) => {
    const add = e.ctrlKey || e.metaKey || e.shiftKey, base = add ? new Set(sel()) : new Set<number>();
    if (!add) setSel(new Set<number>());
    const r = cv.getBoundingClientRect();
    track(ev => {
      const x1 = ev.clientX - r.left, y1 = ev.clientY - r.top;
      setMarq({ x0: x, y0: y, x1, y1 });
      const ta = tAt(Math.min(x, x1)), tb = tAt(Math.max(x, x1)), ma = mAt(Math.max(y, y1)), mb = mAt(Math.min(y, y1));
      const s2 = new Set(base);
      notes().forEach((n, i) => { const p = pitchOf(i); if (p != null && n.end > ta && n.start < tb && p + 0.5 > ma && p - 0.5 < mb) s2.add(i); });
      setSel(s2);
    }, () => { setMarq(null); if (sel().size) say('Zaznaczono ' + sel().size + ' nut'); }, 'crosshair');
  };
  const pan = (e: PointerEvent) => {
    e.preventDefault();
    let lx = e.clientX, ly = e.clientY;
    track(ev => { view.t -= (ev.clientX - lx) / view.pps; const dm = (ev.clientY - ly) / rowH(); view.lo += dm; view.hi += dm; lx = ev.clientX; ly = ev.clientY; bumpView(); }, null, 'grabbing');
  };
  const pencil = (e: PointerEvent) => {
    const erase = e.altKey, r = cv.getBoundingClientRect();
    const pts: [number, number][] = [];
    const addPt = (ev: PointerEvent) => {
      const t = clamp(tAt(ev.clientX - r.left), t0(), t1()), m = mAt(ev.clientY - r.top);
      const last = pts[pts.length - 1];
      if (last && Math.abs(t - last[0]) < 0.004) return;
      if (last) { // keep the run continuous (< 30 ms between points)
        const steps = Math.ceil(Math.abs(t - last[0]) / 0.02);
        for (let k = 1; k < steps; k++) pts.push([last[0] + (t - last[0]) * k / steps, last[1] + (m - last[1]) * k / steps]);
      }
      pts.push([t, m]);
      setPencilLive(erase ? null : pts.slice().sort((p, q) => p[0] - q[0]));
    };
    addPt(e);
    track(addPt, () => {
      setPencilLive(null);
      if (pts.length < 2 && !erase) return;
      const base = tuneOrInit(), ta = Math.min(...pts.map(p => p[0])), tb = Math.max(...pts.map(p => p[0]));
      const nt: ClipTune = { ...base, pencil: erase ? erasePencil(base.pencil, ta, tb) : addPencil(base.pencil, pts) };
      commit([putTune(nt)], erase ? 'Zmaż ołówek' : 'Ołówek');
      say(erase ? 'Zmazano rysunek wysokości' : 'Narysowano przebieg wysokości (' + (tb - ta).toFixed(2) + ' s)');
    }, 'crosshair');
  };
  const move = (e: PointerEvent) => {
    if (e.buttons || !an()) return;
    const { x, y } = pos(e);
    const i = x > KW && y > RH && y < size.H - BH ? hitNote(x, y) : -1, sp = x > KW ? hitSplit(x, y) : -1;
    const h = hover();
    if (!h || h.note !== i || h.split !== sp) setHover({ note: i, split: sp });
    const t = tool();
    cv.style.cursor = y < RH ? 'pointer' : t === 'pencil' ? 'crosshair' : t === 'split' ? (sp >= 0 ? 'col-resize' : i >= 0 ? 'text' : 'default') : i >= 0 ? 'ns-resize' : 'default';
  };
  const wheel = (e: WheelEvent) => {
    e.preventDefault(); e.stopPropagation();
    const { x, y } = pos(e), d = e.deltaY || e.deltaX;
    if (e.ctrlKey || e.metaKey) {
      const t = tAt(x), k = Math.pow(1.0015, -d);
      view.pps = clamp(view.pps * k, 10, 4000); view.t = t - (x - KW) / view.pps;
    } else if (e.shiftKey || Math.abs(e.deltaX) > Math.abs(e.deltaY)) view.t += d / view.pps * 0.8;
    else if (e.altKey) {
      const m = mAt(y), k = Math.pow(1.0015, d), span = clamp((view.hi - view.lo) * k, 6, 60);
      view.lo = m - (m - view.lo) / (view.hi - view.lo) * span; view.hi = view.lo + span;
    } else { const dm = -d / rowH() * 0.25; view.lo += dm; view.hi += dm; }
    bumpView();
  };
  const onKey = (e: KeyboardEvent) => {
    if ((e.target as HTMLElement).tagName === 'INPUT') return;
    const k = e.key.toLowerCase(), m = e.ctrlKey || e.metaKey;
    const tl = TOOLS.find(x => x.key === e.key);
    if (tl && !m) { e.stopPropagation(); setTool(tl.id); say('Narzędzie: ' + tl.label); return; }
    if (m && k === 'a') { e.preventDefault(); e.stopPropagation(); setSel(new Set(notes().map((_, i) => i))); return; }
    if (e.key === 'Escape' && sel().size) { e.stopPropagation(); setSel(new Set<number>()); return; }
    if ((e.key === 'Delete' || e.key === 'Backspace') && sel().size) { e.preventDefault(); e.stopPropagation(); resetNotes(); return; }
    if (e.key === 'f' && !m) { e.stopPropagation(); fitView(); return; }
    // everything else (Space, Esc to close, Ctrl+Z, F8/F9…) stays global
  };

  // ---- macros
  const scope = () => sel().size ? [...sel()] : notes().map((_, i) => i);
  const correct = () => {
    const a = an(); if (!a) return;
    const nt = correctPitch(tuneOrInit(), a, scope(), centerPct(), driftPct(), key(), mask());
    commit([putTune(nt)], 'Popraw wysokość');
    say('Poprawiono ' + scope().length + ' nut · centrum ' + centerPct() + '% · dryf ' + driftPct() + '% · ' + NOTE_NAMES[key()] + ' ' + SCALES[scale()]);
  };
  const resetNotes = () => {
    if (!tune()) return;
    if (!sel().size) { commit([putTune(null)], 'Reset wysokości'); say('Usunięto wszystkie zmiany wysokości'); return; }
    const nt = editNotes(tuneOrInit(), sel(), () => ({ shift: 0, drift: 0, formant: 0, gain: 0 }));
    commit([putTune(nt)], 'Reset nut'); say('Zresetowano ' + sel().size + ' nut');
  };
  const previewing = () => U.previewing;
  const resize = (e: PointerEvent) => {
    if (e.button !== 0) return; e.preventDefault(); e.stopPropagation();
    const sx = e.clientX, sy = e.clientY, o = { ...E() };
    track(ev => setU('pitchEd', ed => ed && { ...ed, w: clamp(o.w + ev.clientX - sx, 720, window.innerWidth - 20), h: clamp(o.h + ev.clientY - sy, 380, window.innerHeight - 20) }), null, 'nwse-resize');
  };
  void theme; void fonts;
  return (
    <div ref={root} tabIndex={0} class="win pitchwin" style={{ left: E().x + 'px', top: E().y + 'px', width: E().w + 'px', height: E().h + 'px', 'z-index': 44 }}
      onKeyDown={onKey} data-pitched>
      <div class="winhd" onPointerDown={e => moveWindow(e, () => E(), (x, y) => setU('pitchEd', ed => ed && { ...ed, x, y }))}>
        <span class="tile" style={{ width: '20px', height: '20px', background: FXMETA.tune.color + '26', color: FXMETA.tune.color }}><Icon d={FXMETA.tune.icon} size={13} /></span>
        <span class="title">Edytor wysokości</span>
        <span style={{ width: '6px', height: '6px', 'border-radius': '2px', background: src()?.color ?? 'var(--fg5)' }} />
        <span class="sub">{src()?.name ?? ''} · {S.project.tracks[c().track]?.name ?? ''}</span>
        <span ref={status} class="rendering" />
        <div style={{ flex: 1 }} />
        <button class="closebtn" title="Zamknij (Esc)" onClick={A.closePitchEditor}>×</button>
      </div>
      <div class="ptools">
        <div class="seg tools">
          <For each={TOOLS}>{tl => (
            <button classList={{ on: tool() === tl.id }} title={tl.label + ' (' + tl.key + ') — ' + tl.tip} onClick={() => setTool(tl.id)}>
              <Icon d={tl.icon} size={14} />
            </button>
          )}</For>
        </div>
        <span class="toolname">{TOOLS.find(t => t.id === tool())!.label}</span>
        <div class="vsep" />
        <Segmented small label="Tonacja" options={NOTE_NAMES} value={key} pick={setKey} color={FXMETA.tune.color} title="Do tej skali przyciąga narzędzie Wysokość i makro" />
        <Segmented small options={SCALES} value={scale} pick={setScale} color={FXMETA.tune.color} />
        <div class="vsep" />
        <div class="macro" title="Makro: przesuwa środek nut do najbliższej nuty skali i spłaszcza dryf">
          <NumField label="CENTRUM" width={62} value={centerPct} text={v => Math.round(v) + '%'} parse={parseNum} drag={(v, d) => clamp(Math.round(v + d * 0.5), 0, 100)} apply={v => setCenterPct(Math.round(clamp(v, 0, 100)))} />
          <NumField label="DRYF" width={52} value={driftPct} text={v => Math.round(v) + '%'} parse={parseNum} drag={(v, d) => clamp(Math.round(v + d * 0.5), 0, 100)} apply={v => setDriftPct(Math.round(clamp(v, 0, 100)))} />
          <button class="smallbtn primary" disabled={!an()} onClick={correct}>Popraw wysokość{sel().size ? ' (' + sel().size + ')' : ''}</button>
        </div>
        <button class="smallbtn" disabled={!tune()} title={sel().size ? 'Zeruj zmiany zaznaczonych nut (Del)' : 'Usuń wszystkie zmiany wysokości tego klipu'} onClick={resetNotes}>{sel().size ? 'Reset nut' : 'Reset'}</button>
        <div style={{ flex: 1 }} />
        <button class="smallbtn" classList={{ playing: previewing() }} onClick={() => previewing() ? be().previewStop() : be().previewClip(c().id).catch(e => say(String(e)))}>{previewing() ? 'Stop' : 'Podgląd'}</button>
        <button class="iconbtn" title="Dopasuj widok (F)" onClick={() => fitView()}><Icon d="M4 9V4h5 M20 9V4h-5 M4 15v5h5 M20 15v5h-5" size={13} /></button>
      </div>
      <div ref={area} class="parea">
        <canvas ref={cv} style={{ position: 'absolute', inset: 0, width: '100%', height: '100%', 'touch-action': 'none' }}
          onPointerDown={down} onPointerMove={move} onPointerLeave={() => setHover(null)} onWheel={wheel} onContextMenu={e => e.preventDefault()} />
        <div ref={phead} class="pphead" />
        <Show when={!an() && !err()}><div class="pstate"><span class="spin" />Analizuję wysokość…</div></Show>
        <Show when={err()}><div class="pstate err">Nie udało się przeanalizować: {err()} <button class="smallbtn" onClick={load}>Spróbuj ponownie</button></div></Show>
      </div>
      <div class="pstatus mono">
        <span>{an() ? notes().length + ' nut' : ''}{sel().size ? ' · zaznaczone ' + sel().size : ''}</span>
        <span>{TOOLS.find(t => t.id === tool())!.tip}</span>
        <span>Ctrl+kółko = zoom · kółko = góra/dół · Shift+kółko = przewiń · F = dopasuj</span>
      </div>
      <div class="resizer" onPointerDown={resize} title="Zmień rozmiar" />
    </div>
  );
}
