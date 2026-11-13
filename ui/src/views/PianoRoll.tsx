// The Piano roll (F7), as in FL Studio: a window of its own that edits the selected channel's
// notes in the current pattern — every channel, drums included (built-in drums and samplers
// follow the key). The pattern's other channels show as ghost notes; double right-click one to
// switch to its channel. Drawing past the end makes the pattern longer (its length follows the
// notes). Every edit is an op and one undo step.
import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show, untrack } from 'solid-js';
import { unwrap } from 'solid-js/store';
import { be } from '../api/backend';
import { t, type TranslationKey } from '../lib/i18n';
import { cloneNotes } from '../lib/noteClone';
import { zoomAround, type ZRange } from '../lib/zoomBar';
import { ZoomScrollbar } from '../ui/ZoomScrollbar';
import { TOTAL_BEATS, type Note } from '../api/types';
import { clamp } from '../lib/format';
import { duplicateRight, keyOf, moveNotes, noteAt, notesText, resizeNotes, ROLL_SNAPS, SCALES, scaleMask, snapStep, stepsOf, transpose, velOf } from '../lib/pattern';
import { NOTE_NAMES, noteName } from '../lib/pitch';
import {
  applyProps, fitRange, fmtLen, fmtStart, followPage, invertSelection, legato, notesOfKey, nudgeVelocity,
  parseKey, parseLen, parseStart, parseVel, quantizeStarts, readProps, wheelNotches,
} from '../lib/rollEdit';
import { commit, Gesture, wheelEdit } from '../state/edit';
import type { TransformSelect, TransformVerb } from '../api/types';

/** The project key/scale as a setKey op (the roll header's key selectors commit it). */
const setKeyOp = (key: number, scale: number) => ({ op: 'setKey' as const, key, scale });
import { audition, chanKindLabel, channels, closeRoll, curPat, moveNotesToNewPattern, patPlayhead, putNotes, rollFront, selChan, selectChannel, toggleRollMax } from '../state/pattern';
import { arrVersion, S } from '../state/store';
import { openMenu, say, U } from '../state/ui';
import { selectionSpan, vocalsUnder } from '../lib/melody';
import { fmtPos } from '../lib/format';
import { openPitchEditor } from '../state/actions';
import { dblCheck, edgeScroller, panDrag, track, trackFrame } from '../ui/drag';
import { typingHeld } from '../state/typingKeys';
import { noteClip, setNoteClip } from '../state/noteClipboard';
import { addFrameTask, fitCanvas, fonts, Layer } from '../ui/frameLoop';
import { Icon } from '../ui/icons';
import { drawLane, drawRoll, rollX, rollY, type RollGeo } from './patternDraw';
import { PatFrame, patternMenu, useDataVersion, useSize } from './patternShared';

const LANE_H = 76, KW = 48;
/** Steps past the pattern's end the roll lets you scroll to and draw in (the pattern grows). */
const ROOM = 64;
const MAX_STEPS = TOTAL_BEATS * 4;
/** Key height (px): default (about FL's density) and the vertical zoom's limits. */
const ROW_H = 18, ROW_MIN = 6, ROW_MAX = 40;
/** Time zoom limits, relative to the fit-the-pattern width. */
const Z_MIN = 0.5, Z_MAX = 8;
const ROLL_ICON = 'M4 4h16v16H4z M4 9h16 M4 14h16 M9 4v5 M15 14v6';
type Tool = 'draw' | 'paint' | 'select' | 'erase';
const TOOLS: { id: Tool; key: string; label: Parameters<typeof t>[0]; icon: string; tip: Parameters<typeof t>[0] }[] = [
  { id: 'draw', key: 'p', label: 'b.pe.tool.draw', icon: 'M4 20l4.5-1L19 8.5 15.5 5 5 15.5z M13.5 7l3.5 3.5', tip: 'b.pe.tool.draw.tip' },
  { id: 'paint', key: 'b', label: 'b.pe.tool.paint', icon: 'M18 3l3 3-9 9-3-3z M9 12c-3 0-4 2-4 4s-1 3-2 4c4 0 8-1 8-5', tip: 'b.pe.tool.paint.tip' },
  { id: 'select', key: 'e', label: 'b.pe.tool.select', icon: 'M4 4h4 M12 4h4 M20 4v4 M20 12v4 M20 20h-4 M12 20H8 M4 20v-4 M4 12V8', tip: 'b.pe.tool.select.tip' },
  { id: 'erase', key: 'd', label: 'b.pe.tool.erase', icon: 'M8 20h12 M4.5 15.5L14 6l5 5-7.5 7.5H8z', tip: 'b.pe.tool.erase.tip' },
];

/** The Piano roll window (shown while `U.rollWin` is set and there is a pattern and a channel). */
export function PianoRoll(props: { z: number }) {
  return <Show when={U.rollWin && curPat() && selChan()}><Roll z={props.z} /></Show>;
}

const [lastLen, setLastLen] = createSignal(2);
/** Velocity of the last note touched: new notes take it, like the length (FL). 1 = full, stored as none. */
const [lastVel, setLastVel] = createSignal(1);
const velProp = () => (lastVel() < 0.995 ? { v: lastVel() } : {});
const [rollTool, setRollTool] = createSignal<Tool>('draw');
const [rollSnap, setRollSnap] = createSignal(1);
const [rollKey, setRollKey] = createSignal<number | null>(null);
const [rollScale, setRollScale] = createSignal(0);

/** The note tools the roll header offers: [A, B] are the two params' defaults; the
 * meaning of A/B is per tool (timing/velocity, grid/amount, hits/slots, width/ramp,
 * hits/slots, density/motif-bars). */
const TOOL_META: Record<'humanize' | 'quantize' | 'euclid' | 'strum' | 'drums' | 'melody', { label: TranslationKey; labelA: TranslationKey; labelB: TranslationKey; defA: number; defB: number }> = {
  humanize: { label: 'b.pr.tool.humanize', labelA: 'b.pr.tool.humanizeA', labelB: 'b.pr.tool.humanizeB', defA: 0.3, defB: 0.2 },
  quantize: { label: 'b.pr.tool.quantize', labelA: 'b.pr.tool.quantizeA', labelB: 'b.pr.tool.quantizeB', defA: 1, defB: 1 },
  euclid: { label: 'b.pr.tool.euclid', labelA: 'b.pr.tool.euclidA', labelB: 'b.pr.tool.euclidB', defA: 4, defB: 16 },
  strum: { label: 'b.pr.tool.strum', labelA: 'b.pr.tool.strumA', labelB: 'b.pr.tool.strumB', defA: 0.5, defB: 0.05 },
  drums: { label: 'b.pr.tool.drums', labelA: 'b.pr.tool.drumsA', labelB: 'b.pr.tool.drumsB', defA: 4, defB: 16 },
  melody: { label: 'b.pr.tool.melody', labelA: 'b.pr.tool.melodyA', labelB: 'b.pr.tool.melodyB', defA: 0.5, defB: 2 },
};
type ToolId = keyof typeof TOOL_META;

/** Key and scale to highlight: chosen here (the project key — also what scale-aware
 * transforms use), else the project's Pitch Correct (a detected key), else C major.
 * `scale` is the engine's scale id (pitch::scale_mask), not the list position. */
function projectKey(): { root: number; scale: number } {
  if (S.project.key != null) return { root: S.project.key, scale: S.project.scale ?? 1 };
  for (const m of S.project.inserts) for (const f of m.fx) if (f.type === 'tune' && (f.params.scale ?? 0) > 0) return { root: Math.round(f.params.key ?? 0), scale: f.params.scale ?? 1 };
  return { root: 0, scale: 1 };
}

function Roll(props: { z: number }) {
  let wrap!: HTMLDivElement, cv!: HTMLCanvasElement, phead!: HTMLDivElement, laneCv!: HTMLCanvasElement, laneWrap!: HTMLDivElement;
  // memos: every read was a find() through the store (thousands per redraw of a big pattern)
  const ptM = createMemo(() => curPat()!), chM = createMemo(() => selChan()!);
  const pt = () => ptM(), ch = () => chM();
  /** The notes as plain data (canvas drawing and hit tests; redraws follow `data`/`arrVersion`). */
  const notesRaw = () => unwrap(pt()).notes;
  const size = useSize(() => wrap);
  const laneSize = useSize(() => laneWrap);
  const data = useDataVersion();
  const layer = new Layer();
  const [sel, setSel] = createSignal<Set<number>>(new Set());
  const [hoverKey, setHoverKey] = createSignal<number | null>(null);
  const [marq, setMarq] = createSignal<{ x0: number; y0: number; x1: number; y1: number } | null>(null);
  const [zbox, setZbox] = createSignal<{ x0: number; y0: number; x1: number; y1: number } | null>(null);
  /** The note properties popover (double-click a note): the notes it edits, the clicked one last. */
  const [propsFor, setPropsFor] = createSignal<number[] | null>(null);
  const [view, setViewS] = createSignal({ sx: 0, sy: 0, zoom: 1, rowH: ROW_H });
  const pk = createMemo(projectKey, undefined, { equals: (a, b) => a.root === b.root && a.scale === b.scale });
  // the note tools strip: verb + two params (meaning per verb) + an optional seed
  const [toolId, setToolId] = createSignal<'humanize' | 'quantize' | 'euclid' | 'strum' | 'drums' | 'melody'>('humanize');
  const [toolA, setToolA] = createSignal(0.3);
  const [toolB, setToolB] = createSignal(0.2);
  const [toolSeed, setToolSeed] = createSignal('');
  const applyTool = () => {
    const p = pt();
    const chan = ch().id;
    const seed = toolSeed().trim() === '' ? undefined : Math.max(0, Math.floor(+toolSeed() || 0));
    const id = toolId(), a = toolA(), b = toolB();
    const verb: TransformVerb = id === 'humanize' ? { kind: 'humanize', timing: a, velocity: b }
      : id === 'quantize' ? { kind: 'quantize', grid: a || 1, amount: Math.min(1, Math.max(0, b)), scaleAware: false }
      : id === 'euclid' ? { kind: 'euclidFill', ch: chan, k: Math.max(1, Math.round(a)), n: Math.max(1, Math.round(b)), rotate: 0 }
      : id === 'strum' ? { kind: 'strum', width: a, ramp: b }
      : id === 'drums' ? { kind: 'euclidDrums', voices: [{ ch: chan, k: Math.max(1, Math.round(a)), n: Math.max(1, Math.round(b)) }], bars: Math.max(1, Math.round(p.len / 4)) }
      : { kind: 'melody', ch: chan, bars: Math.max(1, Math.round(p.len / 4)), density: Math.min(1, Math.max(0.05, a)), repeat: Math.max(1, Math.round(b)) };
    const op: { op: 'transformNotes'; pattern: number; verb: TransformVerb; seed?: number; select?: TransformSelect } = {
      op: 'transformNotes', pattern: p.id, verb,
      ...(seed !== undefined ? { seed } : {}),
      // scope: the selection, else the channel the roll is showing — never the whole
      // pattern (a pattern is multi-channel; humanize with Hat shown must not jitter
      // the kick). Generators target their channel anyway, so they stay unscoped.
      ...(sel().size > 0
        ? { select: { kind: 'indices' as const, indices: [...sel()].sort((x, y) => x - y) } }
        : id === 'humanize' || id === 'quantize' || id === 'strum'
          ? { select: { kind: 'range' as const, from: 0, to: 1e9, ch: chan } }
          : {}),
    };
    commit([op], t(TOOL_META[id].label));
    setSel(new Set<number>());
    say(t('b.pr.toolApplied', { verb: t(TOOL_META[id].label) }));
  };
  const root = () => rollKey() ?? pk().root;
  const scaleIdx = () => (rollKey() != null ? rollScale() : pk().scale);
  const chansMap = createMemo(() => { arrVersion(); return new Map(unwrap(channels()).map(c => [c.id, c])); });
  const mask = () => scaleMask(scaleIdx());
  const unit = (alt = false) => (alt ? 0 : rollSnap());
  /** After a pick in a toolbar list the keyboard goes back to the roll (arrows transpose, Delete deletes) instead of staying in the list. */
  const backToRoll = (el: HTMLElement) => el.closest<HTMLElement>('.win')?.focus({ preventScroll: true });
  /** Width of the note grid (px). */
  const gridW = () => Math.max(1, size().w - KW);
  // The length the view was fitted to: taken when a pattern is shown, NOT while it grows. A
  // note dragged past the end makes the pattern longer; a scale that followed the length
  // would shrink under the pointer and the note would jump back and forth.
  const [fitSteps, setFitSteps] = createSignal(stepsOf(pt()));
  // (a memo of the id: every note edit replaces the pattern object, only another pattern changes the id)
  const patId = createMemo(() => pt().id);
  createEffect(on(patId, () => setFitSteps(stepsOf(untrack(pt)))));
  /** Px per step at zoom 1: the pattern as it was opened fits the window (within sensible limits). */
  const basePps = () => clamp(gridW() / fitSteps(), 6, 48);
  /** Smallest key height: never so small that the 128 keys leave the grid half empty. */
  const rowMin = () => Math.max(ROW_MIN, size().h / 128);
  const geo = (): RollGeo => {
    const steps = stepsOf(pt());
    return { pps: basePps() * view().zoom, sx: view().sx, sy: view().sy, rowH: Math.max(view().rowH, rowMin()), kw: KW, steps };
  };
  /** Steps the view scrolls over: the pattern plus room to draw on (the pattern grows to fit). */
  const hTotal = () => Math.min(MAX_STEPS, stepsOf(pt()) + ROOM);
  const maxSxAt = (pps: number) => Math.max(0, hTotal() * pps - gridW());
  const maxSx = () => maxSxAt(geo().pps);
  /** Zoom-out limit: half the fitted width, or further when that is needed to see all the steps the view scrolls over (pattern + room). */
  const zMin = () => Math.min(Z_MIN, gridW() / hTotal() / basePps());
  const maxSyAt = (rowH: number) => Math.max(0, 128 * rowH - size().h);
  // the zoom scrollbars, in steps (time) and keys from the top (key height)
  const hRange = (): ZRange => { const G = geo(); return { pos: G.sx / G.pps, len: gridW() / G.pps }; };
  const hLimits = () => ({ total: hTotal(), minLen: gridW() / (basePps() * Z_MAX), maxLen: gridW() / (basePps() * zMin()) });
  const applyH = (r: ZRange) => {
    const zoom = clamp(gridW() / r.len / basePps(), zMin(), Z_MAX), pps = basePps() * zoom;
    setViewS(v => ({ ...v, zoom, sx: clamp(r.pos * pps, 0, maxSxAt(pps)) }));
  };
  // Follow the playhead (the app-wide Follow toggle): the frame task flips the view a page when
  // the playhead leaves it. Moving the view by hand while it plays pauses that until the
  // pattern comes round again (no tug of war); nothing follows during a drag.
  let playingNow = false, followHold = false, dragging = false, lastBeat = -1;
  const userScrolled = () => {
    if (!playingNow || !U.follow || followHold) return;
    followHold = true; say(t('b.pr.followPaused'));
  };
  /** The time scrollbar (scrolls or zooms by hand). */
  const setH = (r: ZRange) => { userScrolled(); applyH(r); };
  const vRange = (): ZRange => { const G = geo(), h = Math.max(1, size().h); return { pos: G.sy / G.rowH, len: h / G.rowH }; };
  const vLimits = () => { const h = Math.max(1, size().h); return { total: 128, minLen: h / ROW_MAX, maxLen: h / rowMin() }; };
  const setV = (r: ZRange) => {
    const rowH = clamp(Math.max(1, size().h) / r.len, rowMin(), ROW_MAX);
    setViewS(v => ({ ...v, rowH, sy: clamp(r.pos * rowH, 0, maxSyAt(rowH)) }));
  };
  const chanNotes = createMemo(() => {
    arrVersion();
    const p = unwrap(pt()), id = ch().id;
    return p.notes.map((n, i) => ({ n, i })).filter(x => x.n.ch === id);
  });
  // keep the selection valid when the notes change underneath (undo, CLI)
  createEffect(() => { const ok = new Set(chanNotes().map(x => x.i)); const s = sel(); if ([...s].some(i => !ok.has(i))) setSel(new Set([...s].filter(i => ok.has(i)))); });
  // the popover's notes gone (undo, another channel): it closes rather than edit other notes
  createEffect(() => { const p = propsFor(); if (!p) return; const ok = new Set(chanNotes().map(x => x.i)); if (p.some(i => !ok.has(i))) setPropsFor(null); });
  // another pattern: the selection is note indices of the old one, they would point at random notes
  createEffect(on(patId, () => { setSel(new Set<number>()); setPropsFor(null); }, { defer: true }));
  // initial vertical position: centred on the channel's notes (or its root)
  let centred = -1;
  createEffect(() => {
    const h = size().h, id = ch()?.id; if (!h || centred === id) return;
    centred = id;
    const ks = chanNotes().map(x => keyOf(x.n, ch()));
    const mid = ks.length ? (Math.min(...ks) + Math.max(...ks)) / 2 : ch().root;
    setViewS(v => { const rh = Math.max(v.rowH, rowMin()); return { ...v, sy: clamp((127 - mid) * rh - h / 2, 0, maxSyAt(rh)) }; });
    setSel(new Set<number>());
  });
  // Drawing runs at most once per frame: a touchpad sends several wheel / pointer events per
  // frame and each one redrew the whole roll synchronously.
  const drawMain = () => {
    const { w, h } = size(); if (!w || !h) return;
    fitCanvas(cv, w, h);
    const g = cv.getContext('2d')!, dpr = window.devicePixelRatio || 1;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    const lit = typingHeld();
    drawRoll(g, w, h, geo(), layer, { pt: unwrap(pt()), ch: unwrap(ch()), chans: chansMap(), sel: sel(), root: root(), mask: mask(), hoverKey: hoverKey(), lit, marquee: marq(), zoomBox: zbox() });
  };
  const drawVel = () => {
    const { w, h } = laneSize(); if (!w || !h) return;
    fitCanvas(laneCv, w, h);
    const g = laneCv.getContext('2d')!, dpr = window.devicePixelRatio || 1;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    const G = geo();
    drawLane(g, w, h, { x0: KW, cw: G.pps, sx: G.sx, steps: G.steps, notes: chanNotes(), color: ch().color, sel: sel(), cells: false, muted: ch().mute });
  };
  let rafMain = 0, rafVel = 0;
  createEffect(() => {
    data(); sel(); hoverKey(); marq(); zbox(); root(); mask(); void fonts.epoch; size(); view(); typingHeld(); ch(); pt().len;
    if (!rafMain) rafMain = requestAnimationFrame(() => { rafMain = 0; untrack(drawMain); });
  });
  createEffect(() => {
    data(); sel(); laneSize(); view(); size(); ch(); pt().len;
    if (!rafVel) rafVel = requestAnimationFrame(() => { rafVel = 0; untrack(drawVel); });
  });
  onCleanup(() => { cancelAnimationFrame(rafMain); cancelAnimationFrame(rafVel); });
  onMount(() => onCleanup(addFrameTask(() => {
    const p = curPat(); if (!p || !phead || !U.rollWin) return;
    const b = patPlayhead(p);
    if (b < 0) { phead.style.opacity = '0'; playingNow = false; followHold = false; lastBeat = -1; return; }
    playingNow = true;
    if (b + 1e-6 < lastBeat) followHold = false; // the pattern came round: a new pass
    lastBeat = b;
    // follow: a store write only when the view flips a page
    if (U.follow && !followHold && !dragging) {
      const to = followPage(b * 4, hRange(), 2);
      if (to != null) { const sx = clamp(to * geo().pps, 0, maxSx()); if (Math.abs(sx - view().sx) > 0.5) setViewS(v => ({ ...v, sx })); }
    }
    const x = rollX(geo(), b * 4);
    phead.style.opacity = x >= KW ? '0.85' : '0';
    phead.style.transform = `translateX(${Math.round(x)}px)`;
  })));
  onMount(() => {
    const up = () => { dragging = false; };
    window.addEventListener('pointerup', up); window.addEventListener('pointercancel', up);
    onCleanup(() => { window.removeEventListener('pointerup', up); window.removeEventListener('pointercancel', up); });
  });
  /** Where the pointer last was over the grid (canvas px), for zooming around it; null = outside. */
  let ptr: { x: number; y: number } | null = null;
  /** The grid's box on screen (the keyboard excluded): auto-scroll zones of the drags. */
  const gridRect = () => { const r = cv.getBoundingClientRect(); return { left: r.left + KW, top: r.top, right: r.right, bottom: r.bottom }; };
  /** Scroll by (dx, dy) px; false when the view cannot move any further that way. */
  const scrollBy = (dx: number, dy: number) => {
    const v = view(), rh = geo().rowH;
    const sx = clamp(v.sx + dx, 0, maxSx()), sy = clamp(v.sy + dy, 0, maxSyAt(rh));
    if (Math.abs(sx - v.sx) < 0.01 && Math.abs(sy - v.sy) < 0.01) return false;
    if (sx !== v.sx) userScrolled();
    setViewS({ ...v, sx, sy });
    return true;
  };
  /** Edge auto-scroll for a drag: `replay` re-runs the drag with the last pointer event after each scroll. */
  const autoScroll = (replay: (e: PointerEvent) => void, axes?: { x?: boolean; y?: boolean }) => edgeScroller(gridRect, scrollBy, replay, axes);
  /** Middle-button pan (both axes on the grid). */
  const pan = (e: PointerEvent, yToo = true) => panDrag(e, () => ({ x: view().sx, y: view().sy }), (x, y) => {
    const sx = clamp(x, 0, maxSx()), sy = yToo ? clamp(y, 0, maxSyAt(geo().rowH)) : view().sy;
    if (sx !== view().sx) userScrolled();
    setViewS(v => ({ ...v, sx, sy }));
  });

  const pos = (e: PointerEvent | MouseEvent | WheelEvent) => {
    const r = cv.getBoundingClientRect(), G = geo();
    const x = e.clientX - r.left, y = e.clientY - r.top;
    return { x, y, step: (x - KW + G.sx) / G.pps, key: clamp(127 - Math.floor((y + G.sy) / G.rowH), 0, 127) };
  };
  const hitNote = (step: number, key: number) => noteAt(notesRaw(), ch().id, unwrap(ch()), step, key);
  /** Index of another channel's note (a ghost) under (step, key), or −1. */
  const ghostAt = (step: number, key: number) => {
    const ns = notesRaw(), m = chansMap();
    for (let i = ns.length - 1; i >= 0; i--) {
      const n = ns[i]; if (n.ch === ch().id) continue;
      if (keyOf(n, m.get(n.ch)) === key && step >= n.step - 1e-3 && step < n.step + n.len) return i;
    }
    return -1;
  };
  const edgeHit = (i: number, x: number) => { const n = notesRaw()[i], G = geo(); return x >= rollX(G, n.step + n.len) - Math.min(6, n.len * G.pps * 0.3); };
  const commitNotes = (notes: Note[], label: string) => commit([putNotes(pt(), notes)], label);

  function eraseDrag(e: PointerEvent) {
    const g = new Gesture(t('b.pe.gestDelNotes'));
    let notes = notesRaw(), n0 = notes.length;
    const del = (ev: PointerEvent | MouseEvent) => {
      const q = pos(ev), i = noteAt(notes, ch().id, unwrap(ch()), q.step, q.key);
      if (i < 0) return;
      notes = notes.filter((_, k) => k !== i);
      g.update([putNotes(pt(), notes)]);
    };
    setSel(new Set<number>());
    del(e);
    track(ev => del(ev), () => { g.end(); if (notes.length < n0) say(t('b.pe.deleted', { x: notesText(n0 - notes.length) })); }, 'not-allowed');
  }
  /** Drags notes `idx` (the last one leads). `clone`: Shift+drag — once the pointer really
   * moves, copies of the notes are made and moved instead (originals stay), one undo step;
   * released without moving, `clone` (the click) runs instead. */
  function moveDrag(e: PointerEvent, idx: number[], mode: 'move' | 'resize', g?: Gesture, clone?: () => void) {
    let orig = notesRaw().slice();
    const p0 = pos(e), lead = orig[idx[idx.length - 1]] ?? orig[idx[0]];
    let gest = clone ? null : g ?? new Gesture(mode === 'move' ? t('b.pe.gestMove') : t('b.pe.gestLen'));
    let lastKey = keyOf(lead, ch()), moved = false;
    // Axis lock (FL): Shift pressed after the drag started = time only, Ctrl = pitch only. Held
    // at pointer-down they keep their meaning (clone / select) and lock nothing.
    const shift0 = e.shiftKey, ctrl0 = e.ctrlKey || e.metaKey;
    let mods = { shift: shift0, ctrl: ctrl0 }, lastEv: PointerEvent | null = null, lockSaid = '';
    const run = (ev: PointerEvent) => {
      lastEv = ev;
      const q = pos(ev);
      if (!moved && Math.hypot(q.x - p0.x, q.y - p0.y) < 3) return;
      if (!moved && clone) {
        const r = cloneNotes(orig, idx);
        orig = r.notes; idx = r.idx;
        gest = new Gesture(t('b.pe.gestClone'));
        gest.update([putNotes(pt(), orig)]);
        setSel(new Set(idx)); // after the copies exist: the selection drops indices that do not
        say(t('b.pe.cloning', { x: notesText(idx.length) }));
      }
      moved = true;
      if (!gest) return;
      const lockPitch = mode === 'move' && mods.shift && !shift0, lockTime = mode === 'move' && mods.ctrl && !ctrl0;
      const lock = lockTime ? 'time' : lockPitch ? 'pitch' : '';
      if (lock && lock !== lockSaid) say(t(lockTime ? 'b.pr.lockTime' : 'b.pr.lockPitch'));
      lockSaid = lock;
      const dKey = lockPitch ? 0 : q.key - p0.key;
      const notes = mode === 'move'
        ? (lockTime ? transpose(orig, idx, dKey, unwrap(ch())) : moveNotes(orig, idx, q.step - p0.step, dKey, unit(ev.altKey), unwrap(ch())))
        : resizeNotes(orig, idx, q.step - p0.step, unit(ev.altKey));
      gest.update([putNotes(pt(), notes)]);
      const ln = notes[idx[idx.length - 1]];
      if (ln) setLastLen(ln.len);
      if (mode === 'move' && ln && keyOf(ln, ch()) !== lastKey) { lastKey = keyOf(ln, ch()); audition(ch().id, lastKey, ln.v ?? 1); }
    };
    const es = autoScroll(run, mode === 'resize' ? { x: true } : undefined);
    // a modifier pressed or released without moving the mouse applies at once
    const onMod = (ke: KeyboardEvent) => {
      const m = { shift: ke.shiftKey, ctrl: ke.ctrlKey || ke.metaKey };
      if (m.shift === mods.shift && m.ctrl === mods.ctrl) return;
      mods = m;
      if (lastEv) run(lastEv);
    };
    window.addEventListener('keydown', onMod, true); window.addEventListener('keyup', onMod, true);
    trackFrame(ev => { mods = { shift: ev.shiftKey, ctrl: ev.ctrlKey || ev.metaKey }; run(ev); if (moved) es.move(ev); }, () => {
      es.stop();
      window.removeEventListener('keydown', onMod, true); window.removeEventListener('keyup', onMod, true);
      gest?.end(); if (!moved) clone?.();
    }, mode === 'move' ? 'grabbing' : 'ew-resize');
  }
  /** Pressing the keyboard plays that key on the channel for as long as it is held (always,
   * whatever the "hear notes while editing" preference); dragging glides to other keys.
   * Further right on a key (towards its tip) = harder. */
  function keyboardDrag(e: PointerEvent) {
    const id = ch().id;
    const velAt = (x: number) => Math.round((0.35 + 0.65 * clamp(x / KW, 0, 1)) * 100) / 100;
    const on = (key: number, vel: number) => be().typingNote({ ch: id, key, vel, on: true }).catch(err => say(t('b.pk.fail', { err: String(err) })));
    const off = (key: number) => be().typingNote({ ch: id, key, on: false }).catch(() => {});
    const p = pos(e);
    let cur = p.key;
    void on(cur, velAt(p.x)); setHoverKey(cur);
    track(ev => {
      const q = pos(ev);
      if (q.key === cur) return;
      void off(cur); cur = q.key; void on(cur, velAt(q.x)); setHoverKey(cur);
    }, () => { void off(cur); setHoverKey(null); }, 'pointer');
  }
  function marqueeDrag(e: PointerEvent, add: boolean) {
    const p0 = pos(e), base = add ? new Set(sel()) : new Set<number>();
    // the box's anchor in content px: it stays on its notes while the view auto-scrolls
    const G0 = geo(), ax = p0.x + G0.sx, ay = p0.y + G0.sy;
    let moved = false;
    setMarq({ x0: p0.x, y0: p0.y, x1: p0.x, y1: p0.y });
    const run = (ev: PointerEvent) => {
      const q = pos(ev), G = geo(), sx = ax - G.sx, sy = ay - G.sy;
      if (Math.hypot(q.x - p0.x, q.y - p0.y) >= 3) moved = true;
      setMarq({ x0: sx, y0: sy, x1: q.x, y1: q.y });
      const [x0, x1] = [Math.min(sx, q.x), Math.max(sx, q.x)], [y0, y1] = [Math.min(sy, q.y), Math.max(sy, q.y)];
      const s = new Set(base);
      for (const { n, i } of chanNotes()) {
        const nx = rollX(G, n.step), nw = n.len * G.pps, ny = rollY(G, keyOf(n, ch()));
        if (nx < x1 && nx + nw > x0 && ny < y1 && ny + G.rowH > y0) s.add(i);
      }
      setSel(s);
    };
    const es = autoScroll(run);
    trackFrame(ev => { run(ev); if (moved) es.move(ev); }, () => { es.stop(); setMarq(null); say(sel().size ? t('b.pe.selHint', { x: notesText(sel().size) }) : t('b.pe.noSel')); });
  }
  function paintDrag(e: PointerEvent, g: Gesture, startNotes: Note[]) {
    let notes = startNotes;
    const p0 = pos(e);
    let lastCell = Math.floor(p0.step / (unit(e.altKey) || 1)), moved = false;
    const run = (ev: PointerEvent) => {
      const q = pos(ev), u = unit(ev.altKey) || 1, cell = Math.floor(q.step / u);
      if (Math.hypot(q.x - p0.x, q.y - p0.y) >= 3) moved = true;
      if (cell === lastCell || q.step < 0 || q.step + lastLen() > MAX_STEPS) return;
      lastCell = cell;
      const s = cell * u;
      if (notes.some(n => n.ch === ch().id && Math.abs(n.step - s) < 1e-3)) return;
      notes = [...notes, { ch: ch().id, step: s, len: lastLen(), p: q.key, ...velProp() }];
      g.update([putNotes(pt(), notes)]);
    };
    const es = autoScroll(run);
    track(ev => { run(ev); if (moved) es.move(ev); }, () => { es.stop(); g.end(); }, 'copy');
  }
  /** Ctrl+right-drag: a box to zoom the view to (both axes); a Ctrl+right-click without a drag goes back to the previous view. */
  function zoomBoxDrag(e: PointerEvent) {
    const p0 = pos(e), G0 = geo(), ax = p0.x + G0.sx, ay = p0.y + G0.sy;
    let q = p0;
    track(ev => {
      q = pos(ev);
      const G = geo();
      setZbox({ x0: ax - G.sx, y0: ay - G.sy, x1: Math.max(KW, q.x), y1: q.y });
    }, () => {
      setZbox(null);
      if (Math.hypot(q.x - p0.x, q.y - p0.y) < 4) {
        const v = viewBack.pop();
        if (!v) return say(t('b.pr.zoomNoBack'));
        setViewS(v); say(t('b.pr.zoomBack'));
        return;
      }
      const G = geo();
      // content units: steps across, rows (keys from the top) down
      const s0 = (ax - KW) / G.pps, s1 = (Math.max(KW, q.x) + G.sx - KW) / G.pps;
      const r0 = ay / G.rowH, r1 = (q.y + G.sy) / G.rowH;
      pushView();
      applyH(fitRange(Math.max(0, s0), Math.max(0, s1), hLimits()));
      setV(fitRange(clamp(r0, 0, 128), clamp(r1, 0, 128), vLimits()));
      say(t('b.pr.zoomBox'));
    }, 'zoom-in');
  }
  /** Views to go back to (Ctrl+right-click), newest last. */
  const viewBack: ReturnType<typeof view>[] = [];
  const pushView = () => { viewBack.push({ ...view() }); if (viewBack.length > 32) viewBack.shift(); };
  /** Time zoom to `len` visible steps around the pointer (or the view's centre when the pointer is not over the grid). */
  const zoomTimeTo = (len: number) => {
    const r = hRange(), G = geo();
    const at = ptr && ptr.x >= KW ? (ptr.x - KW + G.sx) / G.pps : r.pos + r.len / 2;
    applyH(zoomAround(r, hLimits(), at, len / r.len));
    const bars = hRange().len / 16;
    say(t('b.pr.zoomBars', { n: bars >= 10 ? Math.round(bars) : +bars.toFixed(bars < 1 ? 2 : 1) }));
  };
  const down = (e: PointerEvent) => {
    if (propsFor()) setPropsFor(null); // a click elsewhere cancels the properties popover
    if (pan(e)) { dragging = true; return; }
    if (e.button !== 0 && e.button !== 2) return;
    e.preventDefault();
    dragging = true;
    const p = pos(e);
    const m = e.ctrlKey || e.metaKey;
    if (e.button === 2 && m && p.x >= KW) return zoomBoxDrag(e);
    // a right-click anywhere in the roll (keyboard, ghost notes, empty grid) drops the selection
    if (e.button === 2 && sel().size) setSel(new Set<number>());
    if (p.x < KW) {
      if (e.button !== 0) return;
      // Ctrl+click on a key: every note of that pitch (Shift too = add to the selection)
      if (m) {
        const ks = notesOfKey(notesRaw(), ch().id, unwrap(ch()), p.key);
        const s = e.shiftKey ? new Set([...sel(), ...ks]) : new Set(ks);
        setSel(s);
        say(ks.length ? t('b.pr.keySel', { key: noteName(p.key), x: notesText(s.size) }) : t('b.pr.keyNone', { key: noteName(p.key) }));
        return;
      }
      keyboardDrag(e); return;
    }
    const tool = rollTool();
    const i = hitNote(p.step, p.key);
    // double-click a note: its properties (the selection's when it is selected). The first
    // click's move-drag has not moved anything (it starts after 3 px), so nothing is nudged.
    if (e.button === 0 && i >= 0 && !m && !e.shiftKey && tool !== 'erase' && dblCheck('rn' + i)) {
      const idx = sel().has(i) ? [...sel()].filter(k => k !== i && notesRaw()[k]?.ch === ch().id).concat(i) : [i];
      if (!sel().has(i)) setSel(new Set([i]));
      setPropsFor(idx);
      return;
    }
    // FL: Ctrl+drag selects with any tool; Ctrl+click toggles one note
    if (e.button === 0 && (e.ctrlKey || e.metaKey)) {
      if (i < 0) return marqueeDrag(e, e.shiftKey);
      const s = new Set(sel()); if (s.has(i)) s.delete(i); else s.add(i); setSel(s);
      say(s.size ? t('b.pe.selCopy', { x: notesText(s.size) }) : t('b.pe.noSel'));
      return;
    }
    if (e.button === 2 && i < 0) {
      // a ghost note of another channel under the pointer: double right-click edits that channel
      const gi = ghostAt(p.step, p.key);
      if (gi >= 0) {
        if (dblCheck('ghost' + notesRaw()[gi].ch)) { const id = notesRaw()[gi].ch; selectChannel(id); say(t('b.pr.switched', { name: channels().find(c => c.id === id)?.name ?? '' })); }
        return;
      }
    }
    // Shift with any tool: drag on empty space adds a box to the selection; on a note, a click
    // toggles it in the selection and a drag clones (FL): copies of the selection (or of that
    // note alone when it is not selected) move, the originals stay
    if (e.button === 0 && e.shiftKey) {
      if (i < 0) return marqueeDrag(e, true);
      const toggle = () => {
        const s = new Set(sel()); if (s.has(i)) s.delete(i); else s.add(i); setSel(s);
        say(s.size ? t('b.pe.selCopy', { x: notesText(s.size) }) : t('b.pe.noSel'));
      };
      if (tool === 'erase') return toggle();
      const idx = sel().has(i) ? [...sel()].filter(k => k !== i && notesRaw()[k]?.ch === ch().id).concat(i) : [i];
      setLastLen(notesRaw()[i].len); setLastVel(notesRaw()[i].v ?? 1);
      return moveDrag(e, idx, 'move', undefined, toggle);
    }
    if (e.button === 2 || tool === 'erase') return eraseDrag(e);
    if (tool === 'select') {
      if (i < 0) return marqueeDrag(e, false);
      const idx = sel().has(i) ? [...sel()] : [i];
      if (!sel().has(i)) setSel(new Set([i]));
      setLastLen(pt().notes[i].len); setLastVel(pt().notes[i].v ?? 1);
      return moveDrag(e, idx.filter(k => k !== i).concat(i), 'move');
    }
    if (i >= 0) {
      const n = pt().notes[i];
      setLastLen(n.len); setLastVel(n.v ?? 1);
      const idx = sel().has(i) ? [...sel()].filter(k => k !== i).concat(i) : [i];
      if (!sel().has(i)) setSel(new Set([i]));
      if (edgeHit(i, p.x)) return moveDrag(e, idx, 'resize');
      audition(ch().id, keyOf(n, ch()), n.v ?? 1);
      return moveDrag(e, idx, 'move');
    }
    // a new note: grid start, last touched length, the row's key
    const s = snapStep(p.step, unit(e.altKey), true);
    if (s < 0 || s + lastLen() > MAX_STEPS) return;
    const n: Note = { ch: ch().id, step: s, len: lastLen(), p: p.key, ...velProp() };
    const notes = [...notesRaw(), n];
    const g = new Gesture(tool === 'paint' ? t('b.pe.gestPaint') : t('b.pe.gestNew'));
    g.update([putNotes(pt(), notes)]);
    setSel(new Set([notes.length - 1]));
    audition(ch().id, p.key, 1);
    if (tool === 'paint') return paintDrag(e, g, notes);
    moveDrag(e, [notes.length - 1], 'move', g);
  };
  const move = (e: PointerEvent) => {
    const p = pos(e);
    ptr = { x: p.x, y: p.y };
    if (e.buttons) return;
    if (p.x < KW) { cv.style.cursor = 'pointer'; return; }
    const i = hitNote(p.step, p.key), t = rollTool();
    cv.style.cursor = e.ctrlKey || e.metaKey ? 'crosshair' : t === 'erase' ? 'not-allowed' : i >= 0 ? (t !== 'select' && edgeHit(i, p.x) ? 'ew-resize' : 'grab') : t === 'select' ? 'crosshair' : 'copy';
  };
  let velAcc = 0;
  /** Alt+wheel on note `i`: its velocity (the selection's when it is selected), 5 % a notch, 1 % with Ctrl; one undo step per wheel burst. */
  const velWheel = (e: WheelEvent, i: number) => {
    const r = wheelNotches(velAcc, e.deltaY || e.deltaX, e.deltaMode === 1); velAcc = r.acc;
    if (!r.n) return;
    const idx = sel().has(i) ? [...sel()].filter(k => notesRaw()[k]?.ch === ch().id) : [i];
    const notes = nudgeVelocity(notesRaw(), idx, r.n * (e.ctrlKey || e.metaKey ? 0.01 : 0.05));
    wheelEdit('rollVel', t('b.pr.gestVel'), [putNotes(pt(), notes)]);
    const vs = idx.map(k => Math.round(velOf(notes[k]) * 100)), lo = Math.min(...vs), hi = Math.max(...vs);
    say(t('b.pr.velX', { x: notesText(idx.length), v: (lo === hi ? lo : lo + '–' + hi) + ' %' }));
  };
  const wheel = (e: WheelEvent) => {
    e.preventDefault();
    const d = e.deltaY || e.deltaX, h = size().h;
    if (e.altKey) {
      // over a note: velocity (Ctrl+Alt = fine); elsewhere Alt+wheel stays the key height
      const p = pos(e), i = p.x >= KW ? hitNote(p.step, p.key) : -1;
      if (i >= 0) return velWheel(e, i);
    }
    if (e.ctrlKey || e.metaKey) {
      const p = pos(e), z = clamp(view().zoom * Math.pow(1.0015, -d), zMin(), Z_MAX);
      const pps = basePps() * z;
      setViewS(v => ({ ...v, zoom: z, sx: clamp(p.step * pps - (p.x - KW), 0, maxSxAt(pps)) }));
    } else if (e.altKey) {
      const r0 = geo().rowH, rowH = clamp(r0 * Math.pow(1.0015, -d), rowMin(), ROW_MAX);
      setViewS(v => { const k = 127 - (v.sy + h / 2) / r0; return { ...v, rowH, sy: clamp((127 - k) * rowH - h / 2, 0, maxSyAt(rowH)) }; });
    } else if (e.shiftKey || Math.abs(e.deltaX) > Math.abs(e.deltaY)) {
      userScrolled();
      setViewS(v => ({ ...v, sx: clamp(v.sx + d, 0, maxSx()) }));
    } else { const rh = geo().rowH; setViewS(v => ({ ...v, sy: clamp(v.sy + d * 0.7, 0, maxSyAt(rh)) })); }
  };
  const laneDown = (e: PointerEvent) => {
    if (pan(e, false)) { dragging = true; return; } // middle-drag: scroll time, as on the grid
    if (e.button === 2 && sel().size) setSel(new Set<number>());
    if (e.button !== 0) return;
    e.preventDefault();
    const r = laneCv.getBoundingClientRect();
    const at = (ev: PointerEvent) => { const G = geo(); return { step: (ev.clientX - r.left - KW + G.sx) / G.pps, v: clamp(1 - (ev.clientY - r.top - 6) / (r.height - 9), 0, 1) }; };
    const g = new Gesture('Velocity');
    let notes = notesRaw();
    const apply = (ev: PointerEvent) => {
      const q = at(ev), G = geo(), tol = Math.max(0.5, 5 / G.pps);
      // notes starting under the pointer (± a few px); with a selection, only selected ones
      const s = sel();
      notes = notes.map((n, i) => n.ch === ch().id && Math.abs(n.step - q.step) <= tol && (!s.size || s.has(i)) ? { ...n, v: Math.round(q.v * 100) / 100 } : n);
      g.update([putNotes(pt(), notes)]);
      say(ch().name + ' · velocity ' + Math.round(q.v * 100) + '%');
    };
    apply(e);
    track(apply, () => g.end(), 'ns-resize');
  };
  // ---- "Vocal reference": the melody (the selection, else the channel's notes) becomes the target of a vocal sung over it
  const vocals = createMemo(() => { void arrVersion(); return vocalsUnder(S.project, pt().id); });
  const vocalName = (id: number) => { const c = S.project.clips.find(x => x.id === id)!; return (S.project.sources.find(x => x.id === c.ref)?.name ?? '?') + ' · ' + (S.project.tracks[c.track]?.name ?? '') + ' · ' + fmtPos(c.start); };
  const toVocal = (clipId: number) => {
    const c = S.project.clips.find(x => x.id === clipId); if (!c) return;
    const idx = [...sel()].filter(i => notesRaw()[i]?.ch === ch().id);
    const span = idx.length ? selectionSpan(S.project, c, ch().id, pt().id, idx) : null;
    if (idx.length && !span) { say(t('b.pr.vocalNoSel')); return; }
    openPitchEditor(clipId, { channel: ch().id, span });
    say(t('b.pr.vocalOpened', { x: idx.length ? notesText(idx.length) : ch().name, name: vocalName(clipId) }));
  };
  const vocalRef = (e: MouseEvent) => {
    const vs = vocals();
    if (vs.length === 1) return toVocal(vs[0].id);
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    openMenu(r.left, r.bottom + 4, vs.map(v => ({ t: 'item' as const, label: vocalName(v.id), act: () => toVocal(v.id) })));
  };
  const moveSelToPattern = () => { const idx = [...sel()]; setSel(new Set<number>()); moveNotesToNewPattern(pt(), idx); };
  const onKey = (e: KeyboardEvent) => {
    const tg = (e.target as HTMLElement).tagName;
    if (tg === 'INPUT' || tg === 'SELECT') return;
    const k = e.key.toLowerCase(), m = e.ctrlKey || e.metaKey;
    const tl = TOOLS.find(x => x.key === k);
    const stop = () => { e.preventDefault(); e.stopPropagation(); };
    if (tl && !m && !e.altKey) { stop(); setRollTool(tl.id); say(t('b.pe.rollTool', { tool: t(tl.label) })); return; }
    if (m && k === 'a') { stop(); setSel(new Set(chanNotes().map(x => x.i))); say(t('b.pe.selected', { x: notesText(chanNotes().length) })); return; }
    // Shift+I: invert the selection within the channel
    if (e.shiftKey && !m && !e.altKey && e.code === 'KeyI') {
      stop();
      const s = invertSelection(chanNotes().map(x => x.i), sel());
      setSel(s); say(s.size ? t('b.pr.inverted', { x: notesText(s.size) }) : t('b.pe.noSel'));
      return;
    }
    // zoom: Page Up / Down in / out around the pointer; Shift+1/2/3 = 1/4/16 bars, Shift+4 = the whole pattern
    if (!m && !e.altKey && !e.shiftKey && (e.key === 'PageUp' || e.key === 'PageDown')) { stop(); zoomTimeTo(hRange().len * (e.key === 'PageUp' ? 0.8 : 1.25)); return; }
    if (e.shiftKey && !m && !e.altKey && /^Digit[1-4]$/.test(e.code)) {
      stop();
      const n = +e.code.slice(5);
      if (n < 4) return zoomTimeTo([16, 64, 256][n - 1]);
      applyH(fitRange(0, stepsOf(pt()), hLimits())); say(t('b.pr.zoomFit'));
      return;
    }
    const s = [...sel()].filter(i => notesRaw()[i]?.ch === ch().id);
    // Ctrl+Q quantize / Ctrl+L legato: the selection, or every note of the channel
    if (m && !e.shiftKey && !e.altKey && (k === 'q' || k === 'l')) {
      stop();
      const idx = s.length ? s : chanNotes().map(x => x.i);
      if (!idx.length) return;
      if (k === 'q') {
        const grid = ROLL_SNAPS.find(x => x[1] === rollSnap())?.[0] ?? '1/16';
        const r = quantizeStarts(notesRaw(), idx, rollSnap());
        if (!r.moved) return say(t('b.pr.onGrid', { grid }));
        commitNotes(r.notes, t('b.pr.quantize'));
        say(t('b.pr.quantized', { x: notesText(r.moved), grid }));
      } else {
        const r = legato(notesRaw(), idx);
        if (!r.changed) return say(t('b.pr.legatoNone'));
        commitNotes(r.notes, t('b.pr.legato'));
        say(t('b.pr.legatoDone', { x: notesText(r.changed) }));
      }
      return;
    }
    // Ctrl+Shift+X: cut the selection out to a new pattern, in place (the playlist keeps it)
    if (m && e.shiftKey && k === 'x') { stop(); if (s.length) moveSelToPattern(); else say(t('b.pe.noSel')); return; }
    if (m && (k === 'c' || k === 'x') && !e.shiftKey) {
      stop(); if (!s.length) return;
      setNoteClip(s.map(i => notesRaw()[i]));
      if (k === 'x') { const del = new Set(s); commitNotes(notesRaw().filter((_, i) => !del.has(i)), t('b.pe.cutNotes')); setSel(new Set<number>()); }
      say(t(k === 'x' ? 'b.pe.cutX' : 'b.pe.copiedX', { x: notesText(s.length) }));
      return;
    }
    if (m && k === 'v' && !e.shiftKey) {
      stop(); const clip = noteClip.notes; if (!clip.length) return;
      const a = noteClip.from ?? Math.min(...clip.map(n => n.step)), b = Math.max(...clip.map(n => n.step + n.len));
      const at = noteClip.at ?? a + Math.max(4, Math.ceil((b - a) / 4 - 1e-9) * 4); // first paste lands after the copied block, further ones chain on
      const copies = clip.map(n => ({ ...n, ch: ch().id, step: n.step - a + at }));
      const base = notesRaw().length;
      commitNotes([...notesRaw(), ...copies], t('b.pe.pasteNotes')); setSel(new Set(copies.map((_, j) => base + j)));
      // the next paste chains on: right after the block, or a whole bar on when the block keeps its place in the bar
      noteClip.at = at + (noteClip.from != null ? Math.max(1, Math.ceil((b - a) / 16 - 1e-9)) * 16 : b - a);
      say(t('b.pe.pastedX', { x: notesText(copies.length) }));
      return;
    }
    if (m && (k === 'b' || k === 'd')) {
      stop(); if (!s.length) return;
      const r = duplicateRight(notesRaw(), s);
      commitNotes(r.notes, t('b.pe.dupNotes')); setSel(new Set(r.idx));
      say(t('b.pe.dupX', { x: notesText(s.length) }));
      return;
    }
    if ((e.key === 'ArrowUp' || e.key === 'ArrowDown') && s.length && !e.shiftKey) {
      stop();
      const d = (e.key === 'ArrowUp' ? 1 : -1) * (m ? 12 : 1);
      const notes = transpose(notesRaw(), s, d, ch());
      commitNotes(notes, t(m ? 'b.pe.octave' : 'b.pe.transpose'));
      const lead = notes[s[s.length - 1]]; audition(ch().id, keyOf(lead, ch()), lead.v ?? 1);
      return;
    }
    if ((e.key === 'Delete' || e.key === 'Backspace') && s.length) {
      stop();
      const del = new Set(s);
      commitNotes(notesRaw().filter((_, i) => !del.has(i)), t('b.pe.gestDelNotes')); setSel(new Set<number>());
      say(t('b.pe.deleted', { x: notesText(s.length) }));
      return;
    }
    if (e.key === 'Escape' && sel().size) { stop(); setSel(new Set<number>()); }
  };
  /** The note properties popover, under the clicked note (above it near the bottom). */
  function PropsPop(pp: { idx: number[] }) {
    let box!: HTMLDivElement, first!: HTMLInputElement;
    const init = readProps(notesRaw(), pp.idx, unwrap(ch()));
    const shown = { vel: init.vel == null ? '' : String(init.vel), start: fmtStart(init.start), len: init.len == null ? '' : fmtLen(init.len), key: noteName(init.key) };
    const [err, setErr] = createSignal('');
    const W = 236, H = pp.idx.length > 1 ? 196 : 176;
    const place = () => {
      const n = notesRaw()[pp.idx[pp.idx.length - 1]], G = geo(), { w, h } = size();
      if (!n) return { left: 0, top: 0 };
      const x = rollX(G, n.step), y = rollY(G, keyOf(n, ch()));
      const below = y + G.rowH + 4;
      return { left: clamp(x, KW + 2, Math.max(KW + 2, w - W - 4)), top: below + H <= h ? below : clamp(y - H - 4, 2, Math.max(2, h - H - 2)) };
    };
    // the window takes the keyboard back (found before the popover leaves the DOM)
    const close = () => { const w = box.closest<HTMLElement>('.win'); setPropsFor(null); w?.focus({ preventScroll: true }); };
    const apply = () => {
      const val = (name: string) => (box.querySelector(`[name=${name}]`) as HTMLInputElement).value.trim();
      const ch0: { vel?: number; len?: number; start?: number; key?: number } = {};
      const bad = (msg: string, name: string) => { setErr(msg); say(msg); (box.querySelector(`[name=${name}]`) as HTMLInputElement).select(); };
      // a field left as shown (or empty when the notes differ) changes nothing
      const v = val('vel'), s = val('start'), l = val('len'), k = val('key');
      if (v && v !== shown.vel) { const x = parseVel(v); if (x == null) return bad(t('b.pr.pBadVel'), 'vel'); ch0.vel = x; }
      if (s && s !== shown.start) { const x = parseStart(s); if (x == null) return bad(t('b.pr.pBadStart'), 'start'); ch0.start = x; }
      if (l && l !== shown.len) { const x = parseLen(l); if (x == null) return bad(t('b.pr.pBadLen'), 'len'); ch0.len = x; }
      if (k && k !== shown.key) { const x = parseKey(k); if (x == null) return bad(t('b.pr.pBadKey'), 'key'); ch0.key = x; }
      if (!Object.keys(ch0).length) return close();
      const notes = applyProps(notesRaw(), pp.idx, unwrap(ch()), ch0);
      if (!notes) { setErr(t('b.pr.pRange')); say(t('b.pr.pRange')); return; }
      commitNotes(notes, t('b.pr.props'));
      if (ch0.len != null) setLastLen(ch0.len);
      say(t('b.pr.pDone', { x: notesText(pp.idx.length) }));
      close();
    };
    onMount(() => setTimeout(() => { first.focus(); first.select(); }, 0));
    const field = (name: 'vel' | 'start' | 'len' | 'key', label: string, tip: string | undefined, ref?: (el: HTMLInputElement) => void) => (
      <label class="rpf" title={tip}>
        <span>{label}</span>
        <input ref={ref} name={name} value={shown[name]} placeholder={shown[name] ? '' : t('b.pr.pMixed')} spellcheck={false} autocomplete="off" onInput={() => setErr('')} />
      </label>
    );
    return (
      <div ref={box} class="menu rollprops" style={{ position: 'absolute', left: place().left + 'px', top: place().top + 'px', width: W + 'px' }}
        onPointerDown={e => e.stopPropagation()} onWheel={e => e.stopPropagation()}
        onKeyDown={e => {
          e.stopPropagation(); // typed text never reaches the roll's shortcuts or the playlist
          if (e.key === 'Enter') { e.preventDefault(); apply(); }
          if (e.key === 'Escape') { e.preventDefault(); close(); }
        }}>
        <div class="mt">{pp.idx.length > 1 ? t('b.pr.props') + ' · ' + notesText(pp.idx.length) : t('b.pr.props')}</div>
        <Show when={pp.idx.length > 1}><div class="rpnote">{t('b.pr.propsN', { x: notesText(pp.idx.length) })}</div></Show>
        {field('vel', t('b.pr.pVel'), undefined, el => (first = el))}
        {field('start', t('b.pr.pStart'), t('b.pr.pStartTip'))}
        {field('len', t('b.pr.pLen'), t('b.pr.pLenTip'))}
        {field('key', t('b.pr.pKey'), t('b.pr.pKeyTip'))}
        <div class="rpfoot" classList={{ err: !!err() }}>{err() || t('b.pr.pFoot')}</div>
      </div>
    );
  }
  return (
    <PatFrame win={U.rollWin!} key="rollWin" z={props.z} class="rollwin" minW={560} minH={300} front={rollFront} onKey={onKey} onHeadDbl={toggleRollMax}
      head={<>
        <span class="tile" style={{ width: '20px', height: '20px', background: ch().color + '2e', color: ch().color }}><Icon d={ROLL_ICON} size={12} /></span>
        <span class="wintitle">Piano roll</span>
        <select class="sel chsel" title={t('b.pr.chTitle')} value={String(ch().id)} style={{ color: ch().color }} onChange={e => { selectChannel(+e.currentTarget.value); backToRoll(e.currentTarget); }}>
          <For each={channels()}>{c => <option value={String(c.id)}>{c.name}</option>}</For>
        </select>
        <button class="patname" title={t('b.rk.patTitle')} onClick={e => { const r = (e.currentTarget as HTMLElement).getBoundingClientRect(); patternMenu(r.left, r.bottom + 4); }}>
          <span class="sw" style={{ background: pt().color }} /><span class="ell">{pt().name}</span>
          <svg width="8" height="8" viewBox="0 0 8 8"><path d="M1 2.5l3 3 3-3" fill="none" stroke="currentColor" stroke-width="1.3" /></svg>
        </button>
        <div style={{ flex: 1 }} />
        <button class="closebtn" title={t('b.pe.close')} onClick={closeRoll}>×</button>
      </>}>
    <div class="roll">
      <div class="ptools">
        <div class="seg tools">
          <For each={TOOLS}>{tl => (
            <button classList={{ on: rollTool() === tl.id }} title={t(tl.label) + ' (' + tl.key.toUpperCase() + ') — ' + t(tl.tip)} onClick={() => setRollTool(tl.id)}>
              <Icon d={tl.icon} size={14} />
            </button>
          )}</For>
        </div>
        <span class="toolname">{t(TOOLS.find(x => x.id === rollTool())!.label)}</span>
        <div class="vsep" />
        <label class="plab">{t('b.pe.lblGrid')}
          <select class="sel" title={t('b.pe.snapTitle')} value={String(rollSnap())} onChange={e => { setRollSnap(+e.currentTarget.value); backToRoll(e.currentTarget); }}>
            <For each={ROLL_SNAPS}>{([l, v]) => <option value={String(v)}>{l}</option>}</For>
          </select>
        </label>
        <label class="plab">{t('b.pe.lblKey')}
          <span class="row" style={{ gap: '4px' }}>
            <select class="sel" title={rollKey() == null ? t('b.pe.keyAuto') : t('b.pe.keyScale')} value={String(root())} onChange={e => { const sc = scaleIdx(); setRollKey(+e.currentTarget.value); setRollScale(sc); commit([setKeyOp(+e.currentTarget.value, sc)], t('b.pr.keySet')); backToRoll(e.currentTarget); }}>
              <For each={NOTE_NAMES}>{(n, i) => <option value={String(i())}>{n}</option>}</For>
            </select>
            <select class="sel" value={String(scaleIdx())} onChange={e => { if (rollKey() == null) setRollKey(root()); setRollScale(+e.currentTarget.value); commit([setKeyOp(rollKey() ?? root(), +e.currentTarget.value)], t('b.pr.keySet')); backToRoll(e.currentTarget); }}>
              <For each={SCALES}>{([l, id]) => <option value={String(id)}>{t(l)}</option>}</For>
            </select>
          </span>
        </label>
        <div style={{ flex: 1 }} />
        <Show when={vocals().length > 0 && chanNotes().length > 0}>
          <button class="smallbtn" style={{ 'align-self': 'center' }} title={t('b.pr.vocalTip')} onClick={vocalRef}>♪ {t('b.pr.vocal')}{vocals().length > 1 ? ' …' : ''}</button>
        </Show>
        <Show when={sel().size > 0}>
          <button class="smallbtn" style={{ 'align-self': 'center' }} title={t('b.pe.toNewPatTip')} onClick={e => { moveSelToPattern(); backToRoll(e.currentTarget); }}>{t('b.pe.toNewPat')}</button>
        </Show>
        <label class="plab" title={t('b.pr.toolsTip')}>
          {t('b.pr.tools')}
          <span class="row" style={{ gap: '4px', 'align-items': 'center' }}>
            <select class="sel" value={toolId()} onChange={e => { setToolId(e.currentTarget.value as ToolId); const m = TOOL_META[e.currentTarget.value as ToolId]; setToolA(m.defA); setToolB(m.defB); backToRoll(e.currentTarget); }}>
              <For each={Object.keys(TOOL_META) as ToolId[]}>{v => <option value={v}>{t(TOOL_META[v].label)}</option>}</For>
            </select>
            <input class="textin" style={{ width: '52px' }} title={t(TOOL_META[toolId()].labelA)} type="number" step="0.1" value={toolA()} onInput={e => setToolA(+e.currentTarget.value)} onKeyDown={e => e.stopPropagation()} />
            <input class="textin" style={{ width: '52px' }} title={t(TOOL_META[toolId()].labelB)} type="number" step="0.1" value={toolB()} onInput={e => setToolB(+e.currentTarget.value)} onKeyDown={e => e.stopPropagation()} />
            <input class="textin" style={{ width: '64px' }} title={t('b.pr.seedTip')} placeholder={t('b.pr.seed')} type="number" step="1" value={toolSeed()} onInput={e => setToolSeed(e.currentTarget.value)} onKeyDown={e => e.stopPropagation()} />
            <button class="smallbtn" style={{ 'align-self': 'center' }} title={t('b.pr.applyTip')} onClick={e => { applyTool(); backToRoll(e.currentTarget); }}>{t('b.pr.apply')}</button>
          </span>
        </label>
        <span class="note mono" style={{ 'align-self': 'center', 'font-size': '10px', color: 'var(--fg5)' }}>{sel().size ? t('b.pe.nSel', { n: sel().size }) : notesText(chanNotes().length)}{t('b.pe.newNote', { len: lastLenText() })}</span>
      </div>
      <div class="rollgrid">
        <div ref={wrap} class="parea">
          <canvas ref={cv} style={{ position: 'absolute', inset: 0, width: '100%', height: '100%', 'touch-action': 'none' }}
            onPointerDown={down} onPointerMove={move} onPointerLeave={() => { ptr = null; }} onWheel={wheel} onContextMenu={e => e.preventDefault()} />
          <div ref={phead} class="pphead" />
          <Show when={propsFor()} keyed>{idx => <PropsPop idx={idx} />}</Show>
        </div>
        <ZoomScrollbar orient="v" range={vRange} limits={vLimits} onChange={setV} title={t('b.pr.vbarTip')} />
        <div style={{ 'padding-left': KW + 'px', display: 'grid', 'min-width': '0' }}>
          <ZoomScrollbar orient="h" range={hRange} limits={hLimits} onChange={setH} title={t('b.pr.hbarTip')} />
        </div>
      </div>
      <div class="lanerow" style={{ height: LANE_H + 'px' }}>
        <div ref={laneWrap} class="lanecv">
          <canvas ref={laneCv} style={{ position: 'absolute', inset: 0, width: '100%', height: '100%', 'touch-action': 'none', cursor: 'ns-resize' }}
            onPointerDown={laneDown} onContextMenu={e => e.preventDefault()} />
          <span class="lanetag tag9">VEL</span>
        </div>
      </div>
      <div class="pstatus mono">
        <span>{ch().name} · {chanKindLabel(ch())}</span>
        <span>{t(TOOLS.find(x => x.id === rollTool())!.tip)}</span>
        <span class="rollhint" title={t('b.pr.keysTip')}>{t('b.pe.rollHint')}</span>
      </div>
    </div>
    </PatFrame>
  );
}

function lastLenText() {
  const l = lastLen();
  return l >= 4 && l % 4 === 0 ? t(l === 4 ? 'b.pe.beat1' : 'b.pe.beatN', { n: l / 4 }) : l === 1 ? '1/16' : l === 2 ? '1/8' : l === 0.5 ? '1/32' : t('b.pe.stepsN', { n: l });
}
