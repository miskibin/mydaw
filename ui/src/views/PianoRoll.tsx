// The Piano roll (F7), as in FL Studio: a window of its own that edits the selected channel's
// notes in the current pattern — every channel, drums included (built-in drums and samplers
// follow the key). The pattern's other channels show as ghost notes; double right-click one to
// switch to its channel. Drawing past the end makes the pattern longer (its length follows the
// notes). Every edit is an op and one undo step.
import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show, untrack } from 'solid-js';
import { unwrap } from 'solid-js/store';
import { t } from '../lib/i18n';
import { TOTAL_BEATS, type Note } from '../api/types';
import { clamp } from '../lib/format';
import { duplicateRight, keyOf, moveNotes, noteAt, notesText, resizeNotes, ROLL_SNAPS, SCALES, snapStep, stepsOf, transpose } from '../lib/pattern';
import { NOTE_NAMES } from '../lib/pitch';
import { commit, Gesture } from '../state/edit';
import { audition, chanKindLabel, channels, closeRoll, curPat, patPlayhead, putNotes, rollFront, selChan, selectChannel } from '../state/pattern';
import { arrVersion, S } from '../state/store';
import { say, U } from '../state/ui';
import { dblCheck, track, trackFrame } from '../ui/drag';
import { typingHeld } from '../state/typingKeys';
import { addFrameTask, fitCanvas, fonts, Layer } from '../ui/frameLoop';
import { Icon } from '../ui/icons';
import { drawLane, drawRoll, rollX, rollY, type RollGeo } from './patternDraw';
import { PatFrame, patternMenu, useDataVersion, useSize } from './patternShared';

const LANE_H = 76, KW = 48;
/** Steps past the pattern's end the roll lets you scroll to and draw in (the pattern grows). */
const ROOM = 64;
const MAX_STEPS = TOTAL_BEATS * 4;
const ROLL_ICON = 'M4 4h16v16H4z M4 9h16 M4 14h16 M9 4v5 M15 14v6';
type Tool = 'draw' | 'paint' | 'select' | 'erase';
const TOOLS: { id: Tool; key: string; label: Parameters<typeof t>[0]; icon: string; tip: Parameters<typeof t>[0] }[] = [
  { id: 'draw', key: 'p', label: 'b.pe.tool.draw', icon: 'M4 20l4.5-1L19 8.5 15.5 5 5 15.5z M13.5 7l3.5 3.5', tip: 'b.pe.tool.draw.tip' },
  { id: 'paint', key: 'b', label: 'b.pe.tool.paint', icon: 'M18 3l3 3-9 9-3-3z M9 12c-3 0-4 2-4 4s-1 3-2 4c4 0 8-1 8-5', tip: 'b.pe.tool.paint.tip' },
  { id: 'select', key: 'e', label: 'b.pe.tool.select', icon: 'M4 4h4 M12 4h4 M20 4v4 M20 12v4 M20 20h-4 M12 20H8 M4 20v-4 M4 12V8', tip: 'b.pe.tool.select.tip' },
  { id: 'erase', key: 'd', label: 'b.pe.tool.erase', icon: 'M8 20h12 M4.5 15.5L14 6l5 5-7.5 7.5H8z', tip: 'b.pe.tool.erase.tip' },
];
/** Piano-roll note clipboard (shared across patterns); `noteClipAt` = where the next paste lands. */
let noteClip: Note[] = [];
let noteClipAt: number | null = null;

/** The Piano roll window (shown while `U.rollWin` is set and there is a pattern and a channel). */
export function PianoRoll(props: { z: number }) {
  return <Show when={U.rollWin && curPat() && selChan()}><Roll z={props.z} /></Show>;
}

const [lastLen, setLastLen] = createSignal(2);
const [rollTool, setRollTool] = createSignal<Tool>('draw');
const [rollSnap, setRollSnap] = createSignal(1);
const [rollKey, setRollKey] = createSignal<number | null>(null);
const [rollScale, setRollScale] = createSignal(0);

/** Key and scale to highlight: chosen here, else the project's Pitch Correct (where a detected key is applied), else C major. */
function projectKey(): { root: number; scale: number } {
  for (const m of S.project.inserts) for (const f of m.fx) if (f.type === 'tune' && (f.params.scale ?? 0) > 0) return { root: Math.round(f.params.key ?? 0), scale: (f.params.scale ?? 1) === 2 ? 1 : 0 };
  return { root: 0, scale: 0 };
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
  const [view, setViewS] = createSignal({ sx: 0, sy: 0, zoom: 1, rowH: 14 });
  const pk = createMemo(projectKey, undefined, { equals: (a, b) => a.root === b.root && a.scale === b.scale });
  const root = () => rollKey() ?? pk().root;
  const scaleIdx = () => (rollKey() != null ? rollScale() : pk().scale);
  const chansMap = createMemo(() => { arrVersion(); return new Map(unwrap(channels()).map(c => [c.id, c])); });
  const mask = () => SCALES[scaleIdx()][1];
  const unit = (alt = false) => (alt ? 0 : rollSnap());
  const geo = (): RollGeo => {
    const steps = stepsOf(pt()), fit = (size().w - KW) / steps;
    return { pps: clamp(fit, 6, 48) * view().zoom, sx: view().sx, sy: view().sy, rowH: view().rowH, kw: KW, steps };
  };
  /** How far right the view scrolls: the pattern plus room to draw on (the pattern grows to fit). */
  const maxSx = () => { const G = geo(); return Math.max(0, Math.min(MAX_STEPS, G.steps + ROOM) * G.pps - (size().w - KW)); };
  const chanNotes = createMemo(() => {
    arrVersion();
    const p = unwrap(pt()), id = ch().id;
    return p.notes.map((n, i) => ({ n, i })).filter(x => x.n.ch === id);
  });
  // keep the selection valid when the notes change underneath (undo, CLI)
  createEffect(() => { const ok = new Set(chanNotes().map(x => x.i)); const s = sel(); if ([...s].some(i => !ok.has(i))) setSel(new Set([...s].filter(i => ok.has(i)))); });
  // initial vertical position: centred on the channel's notes (or its root)
  let centred = -1;
  createEffect(() => {
    const h = size().h, id = ch()?.id; if (!h || centred === id) return;
    centred = id;
    const ks = chanNotes().map(x => keyOf(x.n, ch()));
    const mid = ks.length ? (Math.min(...ks) + Math.max(...ks)) / 2 : ch().root;
    setViewS(v => ({ ...v, sy: clamp((127 - mid) * v.rowH - h / 2, 0, 128 * v.rowH - h) }));
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
    drawRoll(g, w, h, geo(), layer, { pt: unwrap(pt()), ch: unwrap(ch()), chans: chansMap(), sel: sel(), root: root(), mask: mask(), hoverKey: hoverKey(), lit, marquee: marq() });
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
    data(); sel(); hoverKey(); marq(); root(); mask(); void fonts.epoch; size(); view(); typingHeld(); ch(); pt().len;
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
    if (b < 0) { phead.style.opacity = '0'; return; }
    const x = rollX(geo(), b * 4);
    phead.style.opacity = x >= KW ? '0.85' : '0';
    phead.style.transform = `translateX(${Math.round(x)}px)`;
  })));

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
  function moveDrag(e: PointerEvent, idx: number[], mode: 'move' | 'resize', g?: Gesture) {
    const orig = notesRaw().slice(), p0 = pos(e), lead = orig[idx[idx.length - 1]] ?? orig[idx[0]];
    const gest = g ?? new Gesture(mode === 'move' ? t('b.pe.gestMove') : t('b.pe.gestLen'));
    let lastKey = keyOf(lead, ch()), moved = false;
    trackFrame(ev => {
      const q = pos(ev);
      if (!moved && Math.hypot(q.x - p0.x, q.y - p0.y) < 3) return;
      moved = true;
      const notes = mode === 'move'
        ? moveNotes(orig, idx, q.step - p0.step, q.key - p0.key, unit(ev.altKey), unwrap(ch()))
        : resizeNotes(orig, idx, q.step - p0.step, unit(ev.altKey));
      gest.update([putNotes(pt(), notes)]);
      const ln = notes[idx[idx.length - 1]];
      if (ln) setLastLen(ln.len);
      if (mode === 'move' && ln && keyOf(ln, ch()) !== lastKey) { lastKey = keyOf(ln, ch()); audition(ch().id, lastKey, ln.v ?? 1); }
    }, () => gest.end(), mode === 'move' ? 'grabbing' : 'ew-resize');
  }
  function marqueeDrag(e: PointerEvent, add: boolean) {
    const p0 = pos(e), base = add ? new Set(sel()) : new Set<number>();
    setMarq({ x0: p0.x, y0: p0.y, x1: p0.x, y1: p0.y });
    trackFrame(ev => {
      const q = pos(ev), G = geo();
      setMarq({ x0: p0.x, y0: p0.y, x1: q.x, y1: q.y });
      const [x0, x1] = [Math.min(p0.x, q.x), Math.max(p0.x, q.x)], [y0, y1] = [Math.min(p0.y, q.y), Math.max(p0.y, q.y)];
      const s = new Set(base);
      for (const { n, i } of chanNotes()) {
        const nx = rollX(G, n.step), nw = n.len * G.pps, ny = rollY(G, keyOf(n, ch()));
        if (nx < x1 && nx + nw > x0 && ny < y1 && ny + G.rowH > y0) s.add(i);
      }
      setSel(s);
    }, () => { setMarq(null); say(sel().size ? t('b.pe.selHint', { x: notesText(sel().size) }) : t('b.pe.noSel')); });
  }
  function paintDrag(e: PointerEvent, g: Gesture, startNotes: Note[]) {
    let notes = startNotes;
    let lastCell = Math.floor(pos(e).step / (unit(e.altKey) || 1));
    track(ev => {
      const q = pos(ev), u = unit(ev.altKey) || 1, cell = Math.floor(q.step / u);
      if (cell === lastCell || q.step < 0 || q.step + lastLen() > MAX_STEPS) return;
      lastCell = cell;
      const s = cell * u;
      if (notes.some(n => n.ch === ch().id && Math.abs(n.step - s) < 1e-3)) return;
      notes = [...notes, { ch: ch().id, step: s, len: lastLen(), p: q.key }];
      g.update([putNotes(pt(), notes)]);
    }, () => g.end(), 'copy');
  }
  const down = (e: PointerEvent) => {
    if (e.button !== 0 && e.button !== 2) return;
    e.preventDefault();
    const p = pos(e);
    if (p.x < KW) { if (e.button === 0) { audition(ch().id, p.key); setHoverKey(p.key); track(() => {}, () => setHoverKey(null)); } return; }
    const tool = rollTool();
    const i = hitNote(p.step, p.key);
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
    if (e.button === 2 || tool === 'erase') return eraseDrag(e);
    if (tool === 'select') {
      if (i < 0) return marqueeDrag(e, e.shiftKey);
      if (e.shiftKey) { const s = new Set(sel()); if (s.has(i)) s.delete(i); else s.add(i); setSel(s); return; }
      const idx = sel().has(i) ? [...sel()] : [i];
      if (!sel().has(i)) setSel(new Set([i]));
      setLastLen(pt().notes[i].len);
      return moveDrag(e, idx.filter(k => k !== i).concat(i), 'move');
    }
    if (i >= 0) {
      const n = pt().notes[i];
      setLastLen(n.len);
      const idx = sel().has(i) ? [...sel()].filter(k => k !== i).concat(i) : [i];
      if (!sel().has(i)) setSel(new Set([i]));
      if (edgeHit(i, p.x)) return moveDrag(e, idx, 'resize');
      audition(ch().id, keyOf(n, ch()), n.v ?? 1);
      return moveDrag(e, idx, 'move');
    }
    // a new note: grid start, last touched length, the row's key
    const s = snapStep(p.step, unit(e.altKey), true);
    if (s < 0 || s + lastLen() > MAX_STEPS) return;
    const n: Note = { ch: ch().id, step: s, len: lastLen(), p: p.key };
    const notes = [...notesRaw(), n];
    const g = new Gesture(tool === 'paint' ? t('b.pe.gestPaint') : t('b.pe.gestNew'));
    g.update([putNotes(pt(), notes)]);
    setSel(new Set([notes.length - 1]));
    audition(ch().id, p.key, 1);
    if (tool === 'paint') return paintDrag(e, g, notes);
    moveDrag(e, [notes.length - 1], 'move', g);
  };
  const move = (e: PointerEvent) => {
    if (e.buttons) return;
    const p = pos(e);
    if (p.x < KW) { cv.style.cursor = 'pointer'; return; }
    const i = hitNote(p.step, p.key), t = rollTool();
    cv.style.cursor = e.ctrlKey || e.metaKey ? 'crosshair' : t === 'erase' ? 'not-allowed' : i >= 0 ? (t !== 'select' && edgeHit(i, p.x) ? 'ew-resize' : 'grab') : t === 'select' ? 'crosshair' : 'copy';
  };
  const wheel = (e: WheelEvent) => {
    e.preventDefault();
    const d = e.deltaY || e.deltaX, h = size().h;
    if (e.ctrlKey || e.metaKey) {
      const p = pos(e), G = geo(), z = clamp(view().zoom * Math.pow(1.0015, -d), 0.5, 8);
      const pps = G.pps / view().zoom * z;
      setViewS(v => ({ ...v, zoom: z, sx: clamp(p.step * pps - (p.x - KW), 0, Math.max(0, Math.min(MAX_STEPS, G.steps + ROOM) * pps - (size().w - KW))) }));
    } else if (e.altKey) {
      setViewS(v => { const rowH = clamp(v.rowH * Math.pow(1.0015, -d), 8, 28); const k = 127 - (v.sy + h / 2) / v.rowH; return { ...v, rowH, sy: clamp((127 - k) * rowH - h / 2, 0, 128 * rowH - h) }; });
    } else if (e.shiftKey || Math.abs(e.deltaX) > Math.abs(e.deltaY)) {
      setViewS(v => ({ ...v, sx: clamp(v.sx + d, 0, maxSx()) }));
    } else setViewS(v => ({ ...v, sy: clamp(v.sy + d * 0.7, 0, 128 * v.rowH - h) }));
  };
  const laneDown = (e: PointerEvent) => {
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
  const onKey = (e: KeyboardEvent) => {
    const tg = (e.target as HTMLElement).tagName;
    if (tg === 'INPUT' || tg === 'SELECT') return;
    const k = e.key.toLowerCase(), m = e.ctrlKey || e.metaKey;
    const tl = TOOLS.find(x => x.key === k);
    const stop = () => { e.preventDefault(); e.stopPropagation(); };
    if (tl && !m && !e.altKey) { stop(); setRollTool(tl.id); say(t('b.pe.rollTool', { tool: t(tl.label) })); return; }
    if (m && k === 'a') { stop(); setSel(new Set(chanNotes().map(x => x.i))); say(t('b.pe.selected', { x: notesText(chanNotes().length) })); return; }
    const s = [...sel()].filter(i => notesRaw()[i]?.ch === ch().id);
    if (m && (k === 'c' || k === 'x') && !e.shiftKey) {
      stop(); if (!s.length) return;
      noteClip = s.map(i => notesRaw()[i]); noteClipAt = null;
      if (k === 'x') { const del = new Set(s); commitNotes(notesRaw().filter((_, i) => !del.has(i)), t('b.pe.cutNotes')); setSel(new Set<number>()); }
      say(t(k === 'x' ? 'b.pe.cutX' : 'b.pe.copiedX', { x: notesText(s.length) }));
      return;
    }
    if (m && k === 'v' && !e.shiftKey) {
      stop(); if (!noteClip.length) return;
      const a = Math.min(...noteClip.map(n => n.step)), b = Math.max(...noteClip.map(n => n.step + n.len));
      const at = noteClipAt ?? a + Math.max(4, Math.ceil((b - a) / 4 - 1e-9) * 4); // first paste lands after the copied block, further ones chain on
      const copies = noteClip.map(n => ({ ...n, ch: ch().id, step: n.step - a + at }));
      const base = notesRaw().length;
      commitNotes([...notesRaw(), ...copies], t('b.pe.pasteNotes')); setSel(new Set(copies.map((_, j) => base + j)));
      noteClipAt = at + (b - a);
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
  return (
    <PatFrame win={U.rollWin!} key="rollWin" z={props.z} class="rollwin" minW={560} minH={300} front={rollFront} onKey={onKey}
      head={<>
        <span class="tile" style={{ width: '20px', height: '20px', background: ch().color + '2e', color: ch().color }}><Icon d={ROLL_ICON} size={12} /></span>
        <span class="wintitle">Piano roll</span>
        <select class="sel chsel" title={t('b.pr.chTitle')} value={String(ch().id)} style={{ color: ch().color }} onChange={e => selectChannel(+e.currentTarget.value)}>
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
          <select class="sel" title={t('b.pe.snapTitle')} value={String(rollSnap())} onChange={e => setRollSnap(+e.currentTarget.value)}>
            <For each={ROLL_SNAPS}>{([l, v]) => <option value={String(v)}>{l}</option>}</For>
          </select>
        </label>
        <label class="plab">{t('b.pe.lblKey')}
          <span class="row" style={{ gap: '4px' }}>
            <select class="sel" title={rollKey() == null ? t('b.pe.keyAuto') : t('b.pe.keyScale')} value={String(root())} onChange={e => { setRollKey(+e.currentTarget.value); setRollScale(scaleIdx()); }}>
              <For each={NOTE_NAMES}>{(n, i) => <option value={String(i())}>{n}</option>}</For>
            </select>
            <select class="sel" value={String(scaleIdx())} onChange={e => { if (rollKey() == null) setRollKey(root()); setRollScale(+e.currentTarget.value); }}>
              <For each={SCALES}>{([l], i) => <option value={String(i())}>{t(l)}</option>}</For>
            </select>
          </span>
        </label>
        <div style={{ flex: 1 }} />
        <span class="note mono" style={{ 'align-self': 'center', 'font-size': '10px', color: 'var(--fg5)' }}>{sel().size ? t('b.pe.nSel', { n: sel().size }) : notesText(chanNotes().length)}{t('b.pe.newNote', { len: lastLenText() })}</span>
      </div>
      <div ref={wrap} class="parea">
        <canvas ref={cv} style={{ position: 'absolute', inset: 0, width: '100%', height: '100%', 'touch-action': 'none' }}
          onPointerDown={down} onPointerMove={move} onWheel={wheel} onContextMenu={e => e.preventDefault()} />
        <div ref={phead} class="pphead" />
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
        <span>{t('b.pe.rollHint')}</span>
      </div>
    </div>
    </PatFrame>
  );
}

function lastLenText() {
  const l = lastLen();
  return l >= 4 && l % 4 === 0 ? t(l === 4 ? 'b.pe.beat1' : 'b.pe.beatN', { n: l / 4 }) : l === 1 ? '1/16' : l === 2 ? '1/8' : l === 0.5 ? '1/32' : t('b.pe.stepsN', { n: l });
}
