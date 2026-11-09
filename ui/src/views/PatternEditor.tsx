// The pattern editor: an FL-style Channel Rack (step sequencer) and a piano roll for melodic
// channels, with a velocity lane under both. It is a floating window in the effect-window
// layer (F5 / a playlist click hide it, "Okna" brings it back). Grids are canvases with cached
// background layers; every edit goes out as an op (one undo step per click or drag).
import { createEffect, createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import { VOL_UNITY, type Channel, type Note } from '../api/types';
import { clamp, panText, volText } from '../lib/format';
import {
  cellsBetween, clearChannel, duplicateRight, fillEvery, INST_NAME, INSTS, isPitched, isRollRow, keyOf, MAX_BARS, moveNotes,
  noteAt, resizeNotes, ROLL_SNAPS, rotate, SCALES, setStep, setVelAt, snapStep, stepsOf, transpose, visible,
} from '../lib/pattern';
import { NOTE_NAMES } from '../lib/pitch';
import { commit, Gesture } from '../state/edit';
import {
  addBuiltin, addSampler, addSamplerFiles, audition, chanById, chanKindLabel, chanOp, channels, cloneChannel, closePattern, curPat,
  deleteChannel, newPattern, openPattern, patFront, patOp, playStep, previewPattern, putNotes, selectChannel, setBars, setChannel,
  setView, togglePreview, toggleChanMute, toggleChanSolo, patPlayhead,
} from '../state/pattern';
import { projVersion, S } from '../state/store';
import { openMenu, openRename, say, setU, U, type MenuItem } from '../state/ui';
import { dblCheck, track } from '../ui/drag';
import { addFrameTask, fitCanvas, fonts, Layer } from '../ui/frameLoop';
import { Icon } from '../ui/icons';
import { channelIcon, InstIcon } from '../ui/instIcons';
import { MiniKnob, NumField, parseNum } from '../ui/Knob';
import { theme } from '../ui/theme';
import { moveWindow } from './FxWindow';
import { drawLane, drawRack, drawRackHead, drawRoll, rollX, rollY, type RackGeo, type RollGeo } from './patternDraw';

const RH = 28, LEFTW = 252, LANE_H = 76, KW = 48;
const STEP_ICON = 'M4 5h4v4H4z M10 5h4v4h-4z M16 5h4v4h-4z M4 15h4v4H4z M10 15h4v4h-4z M16 15h4v4h-4z';
type Tool = 'draw' | 'paint' | 'select' | 'erase';
const TOOLS: { id: Tool; key: string; label: string; icon: string; tip: string }[] = [
  { id: 'draw', key: 'p', label: 'Rysuj', icon: 'M4 20l4.5-1L19 8.5 15.5 5 5 15.5z M13.5 7l3.5 3.5', tip: 'Klik = nuta (długość ostatniej) · przeciągnij = przesuń · krawędź = długość · PPM = usuń' },
  { id: 'paint', key: 'b', label: 'Maluj', icon: 'M18 3l3 3-9 9-3-3z M9 12c-3 0-4 2-4 4s-1 3-2 4c4 0 8-1 8-5', tip: 'Przeciągnij = maluj nuty po siatce · PPM = usuń' },
  { id: 'select', key: 'e', label: 'Zaznacz', icon: 'M4 4h4 M12 4h4 M20 4v4 M20 12v4 M20 20h-4 M12 20H8 M4 20v-4 M4 12V8', tip: 'Prostokąt = zaznacz · Shift = dodaj · przeciągnij zaznaczone = przesuń' },
  { id: 'erase', key: 'd', label: 'Usuń', icon: 'M8 20h12 M4.5 15.5L14 6l5 5-7.5 7.5H8z', tip: 'Klik / przeciągnij po nutach = usuń' },
];
const W = () => U.patWin!;

/**
 * Keys typed while the editor has focus never edit the playlist behind it: selection, delete,
 * clipboard and nudge shortcuts stop here (Space, Ctrl+Z, F-keys and Esc stay global).
 */
let viewKey: ((e: KeyboardEvent) => void) | null = null;
/** Piano-roll note clipboard (shared across patterns); `noteClipAt` = where the next paste lands. */
let noteClip: Note[] = [];
let noteClipAt: number | null = null;
function guardKeys(e: KeyboardEvent) {
  // the window has the focus: the active view (piano roll) gets the keys first
  viewKey?.(e);
  if (e.cancelBubble) return;
  const m = e.ctrlKey || e.metaKey, k = e.key.toLowerCase();
  if (e.key === 'Delete' || e.key === 'Backspace' || e.key.startsWith('Arrow') || (m && 'abcdxvm'.includes(k) && k.length === 1)) { e.preventDefault(); e.stopPropagation(); }
}

/** Pattern editor window (shown while `U.patWin` is set). */
export function PatternEditor(props: { z: number }) {
  return <Show when={U.patWin && curPat()}><Editor z={props.z} /></Show>;
}

function Editor(props: { z: number }) {
  let root!: HTMLDivElement;
  const pt = () => curPat()!;
  const chans = () => channels();
  const selCh = () => chanById(W().ch) ?? chans()[0];
  const resize = (e: PointerEvent) => {
    if (e.button !== 0) return; e.preventDefault(); e.stopPropagation();
    const sx = e.clientX, sy = e.clientY, o = { ...W() };
    track(ev => setU('patWin', w => w && { ...w, w: clamp(o.w + ev.clientX - sx, 640, window.innerWidth - 20), h: clamp(o.h + ev.clientY - sy, 320, window.innerHeight - 20) }), null, 'nwse-resize');
  };
  // a channel or pattern that vanished (undo, CLI) — fall back instead of showing nothing
  createEffect(() => {
    if (U.patWin?.ch != null && !chanById(U.patWin.ch)) setU('patWin', { ch: chans()[0]?.id ?? null });
    if (U.patWin?.view === 'roll' && !(selCh() && isPitched(selCh()!))) setU('patWin', 'view', 'rack');
  });
  const swingG: { g: Gesture | null } = { g: null };
  const applySwing = (v: number, mode: 'begin' | 'move' | 'end' | 'set' | 'wheel') => {
    const op = patOp(pt(), { swing: Math.round(clamp(v, 0, 100)) / 100 });
    if (mode === 'begin') { swingG.g = new Gesture('Swing'); swingG.g.update([op]); }
    else if (mode === 'move') swingG.g?.update([op]);
    else if (mode === 'end') { swingG.g?.end(); swingG.g = null; }
    else commit([op], 'Swing');
    say(pt().name + ' · swing ' + Math.round(clamp(v, 0, 100)) + '%');
  };
  const playing = () => previewPattern() === pt().id;
  return (
    <div ref={root} tabIndex={0} class="win patwin" data-patwin style={{ left: W().x + 'px', top: W().y + 'px', width: W().w + 'px', height: W().h + 'px', 'z-index': props.z }}
      onPointerDown={e => { patFront(); const tg = (e.target as HTMLElement).tagName; if (tg !== 'SELECT' && tg !== 'INPUT') root.focus({ preventScroll: true }); }}
      onKeyDown={guardKeys}>
      <div class="winhd" onPointerDown={e => moveWindow(e, () => W(), (x, y) => setU('patWin', w => w && { ...w, x, y }))}>
        <span class="tile" style={{ width: '20px', height: '20px', background: pt().color + '2e', color: pt().color }}><Icon d={STEP_ICON} size={12} /></span>
        <select class="sel patsel" title="Pattern (F6 otwiera edytor wybranego)" value={String(pt().id)} onPointerDown={e => e.stopPropagation()}
          onChange={e => openPattern(+e.currentTarget.value)}>
          <For each={S.project.patterns}>{p => <option value={String(p.id)}>{p.name}</option>}</For>
        </select>
        <button class="iconbtn" title="Nowy pattern" onPointerDown={e => e.stopPropagation()} onClick={newPattern}><Icon d="M12 5v14 M5 12h14" size={13} /></button>
        <div class="vsep" />
        <select class="sel" title="Długość patternu w taktach — klipy pokazujące cały pattern idą za nią; nuty poza końcem zostają (ukryte)" value={String(pt().len / 4)}
          onPointerDown={e => e.stopPropagation()} onChange={e => setBars(pt(), +e.currentTarget.value)}>
          <For each={Array.from({ length: MAX_BARS }, (_, i) => i + 1)}>{b => <option value={String(b)}>{b + (b === 1 ? ' takt' : b < 5 ? ' takty' : ' taktów')}</option>}</For>
          <Show when={pt().len % 4 !== 0 || pt().len > MAX_BARS * 4}><option value={String(pt().len / 4)}>{(pt().len / 4).toFixed(2)} t.</option></Show>
        </select>
        <div onPointerDown={e => e.stopPropagation()}>
          <NumField label="SWING" width={58} value={() => Math.round((pt().swing ?? 0) * 100)} text={v => Math.round(v) + '%'} parse={parseNum}
            drag={(v, d) => clamp(Math.round(v + d * 0.5), 0, 100)} apply={applySwing}
            title="Swing: opóźnia co drugą szesnastkę (100% = o pół kroku) · przeciągnij · kółko · 2× klik = wpisz" />
        </div>
        <button class="pplay" classList={{ on: playing() }} title={playing() ? 'Zatrzymaj podgląd patternu' : 'Odtwarzaj sam pattern w pętli (zatrzymuje utwór; Spacja = utwór)'}
          onPointerDown={e => e.stopPropagation()} onClick={() => void togglePreview(pt().id)}>
          <Show when={playing()} fallback={<svg width="10" height="10" viewBox="0 0 10 10"><path d="M1.5 1v8l7-4z" fill="currentColor" /></svg>}>
            <svg width="10" height="10" viewBox="0 0 10 10"><rect x="1.5" y="1.5" width="7" height="7" fill="currentColor" /></svg>
          </Show>
          <span>{playing() ? 'Stop' : 'Pętla'}</span>
        </button>
        <div style={{ flex: 1 }} />
        <div class="seg ptabs" onPointerDown={e => e.stopPropagation()}>
          <button classList={{ on: W().view === 'rack' }} onClick={() => setView('rack')} title="Channel Rack — kroki">Kanały</button>
          <button classList={{ on: W().view === 'roll' }} disabled={!selCh() || !isPitched(selCh()!)}
            title={selCh() && isPitched(selCh()!) ? 'Piano roll: ' + selCh()!.name : 'Piano roll — wybierz kanał melodyczny (Bass, sampler)'}
            onClick={() => selCh() && isPitched(selCh()!) && setView('roll')}>Piano roll</button>
        </div>
        <button class="closebtn" title="Zamknij (Esc)" onClick={closePattern}>×</button>
      </div>
      <Show when={W().view === 'roll' && selCh() && isPitched(selCh()!)} fallback={<Rack root={() => root} />}>
        <Roll root={() => root} />
      </Show>
      <div class="resizer" onPointerDown={resize} title="Zmień rozmiar" />
    </div>
  );
}

// ---------------------------------------------------------------- shared bits

/** Size of an element, tracked. */
function useSize(el: () => HTMLElement | undefined) {
  const [size, setSize] = createSignal({ w: 0, h: 0 });
  onMount(() => {
    const ro = new ResizeObserver(() => { const e = el(); if (e) setSize({ w: e.clientWidth, h: e.clientHeight }); });
    const e = el(); if (e) ro.observe(e);
    onCleanup(() => ro.disconnect());
  });
  return size;
}

/** Bumped when anything the canvases show changes (pattern notes, channels, theme). */
function useDataVersion() {
  const [v, setV] = createSignal(0);
  // projVersion bumps on every project change: no deep JSON walk of a 2000-note pattern per edit
  createEffect(() => { projVersion(); curPat(); channels(); theme.epoch; setV(x => x + 1); });
  return v;
}

function insLabel(c: Channel): string {
  if (c.insert == null) return '—';
  const i = S.project.inserts.findIndex(m => m.id === c.insert);
  return i <= 0 ? 'M' : String(i);
}
function insTitle(c: Channel): string {
  if (c.insert == null) return 'Kanał miksera: przez ścieżkę klipu (jak dotąd) — kliknij, aby wybrać insert';
  const m = S.project.inserts.find(x => x.id === c.insert);
  return 'Kanał miksera: ' + (m ? (m.id === 0 ? 'Master' : m.name) : '?') + ' — kliknij, aby zmienić';
}
function insertMenu(c: Channel, x: number, y: number) {
  const items: MenuItem[] = [{ t: 'title', label: 'Kanał miksera · ' + c.name },
    { t: 'item', label: 'Ścieżka klipu (auto)', kbd: c.insert == null ? '✓' : '', title: 'Gra przez insert ścieżki playlisty, na której leży klip patternu', act: () => setChannel(c, { insert: null }, 'Kanał miksera') }, { t: 'sep' }];
  S.project.inserts.forEach((m, i) => items.push({ t: 'item', label: i === 0 ? 'Master' : i + ' · ' + m.name, kbd: c.insert === m.id ? '✓' : '', act: () => { setChannel(c, { insert: m.id }, 'Kanał miksera'); say(c.name + ' → ' + (i === 0 ? 'Master' : 'Insert ' + i)); } }));
  openMenu(x, y, items, Math.min(window.innerHeight - 20, 60 + items.length * 26));
}

function chanMenu(c: Channel, x: number, y: number) {
  const pt = curPat(); if (!pt) return;
  const steps = stepsOf(pt);
  const edit = (notes: Note[], label: string) => commit([putNotes(pt, notes)], label);
  openMenu(x, y, [
    { t: 'title', label: c.name + ' · ' + chanKindLabel(c) },
    { t: 'item', label: 'Piano roll', kbd: '2× klik', disabled: !isPitched(c), title: isPitched(c) ? '' : 'Kanały perkusyjne grają tylko kroki — piano roll ma Bass i samplery', act: () => setView('roll', c.id) },
    { t: 'sep' },
    { t: 'item', label: 'Wypełnij co 2 kroki', act: () => edit(fillEvery(pt.notes, c.id, steps, 2), 'Wypełnij') },
    { t: 'item', label: 'Wypełnij co 4 kroki', act: () => edit(fillEvery(pt.notes, c.id, steps, 4), 'Wypełnij') },
    { t: 'item', label: 'Wypełnij co 8 kroków', act: () => edit(fillEvery(pt.notes, c.id, steps, 8), 'Wypełnij') },
    { t: 'item', label: 'Przesuń w lewo', kbd: '◀', act: () => edit(rotate(pt.notes, c.id, steps, -1), 'Przesuń kroki') },
    { t: 'item', label: 'Przesuń w prawo', kbd: '▶', act: () => edit(rotate(pt.notes, c.id, steps, 1), 'Przesuń kroki') },
    { t: 'item', label: 'Wyczyść', disabled: !pt.notes.some(n => n.ch === c.id), act: () => edit(clearChannel(pt.notes, c.id), 'Wyczyść kanał') },
    { t: 'sep' },
    { t: 'item', label: 'Odsłuchaj', kbd: 'Ctrl+klik', act: () => audition(c.id, undefined, 0.8, true) },
    { t: 'item', label: 'Zmień nazwę…', act: () => openRename(x, y, c.name, v => v.trim() && setChannel(c, { name: v.trim() }, 'Nazwa kanału')) },
    { t: 'swatches', current: c.color, pick: col => setChannel(c, { color: col }, 'Kolor kanału') },
    { t: 'item', label: 'Klonuj kanał', act: () => cloneChannel(c) },
    { t: 'sep' },
    { t: 'item', label: 'Usuń kanał', danger: true, act: () => deleteChannel(c) },
  ], 470);
}

function addChannelMenu(x: number, y: number) {
  const items: MenuItem[] = [{ t: 'title', label: 'Wbudowane instrumenty' }];
  for (const i of INSTS) items.push({ t: 'item', label: INST_NAME[i], act: () => addBuiltin(i) });
  items.push({ t: 'sep' }, { t: 'title', label: 'Sampler (one-shot, wysokość z nuty)' });
  for (const s of S.project.sources.slice(0, 14)) items.push({ t: 'item', label: s.name, kbd: s.dur.toFixed(1) + ' s', act: () => addSampler(s.id) });
  items.push({ t: 'item', label: 'Plik z dysku…', act: () => void addSamplerFiles(null) });
  openMenu(x, y, items, Math.min(window.innerHeight - 20, 80 + items.length * 26));
}

// ---------------------------------------------------------------- Channel Rack

function Rack(props: { root: () => HTMLDivElement }) {
  let gridWrap!: HTMLDivElement, cv!: HTMLCanvasElement, head!: HTMLCanvasElement, laneCv!: HTMLCanvasElement, laneWrap!: HTMLDivElement;
  const pt = () => curPat()!;
  const chans = () => channels();
  const selCh = () => chanById(W().ch) ?? chans()[0];
  const size = useSize(() => gridWrap);
  const laneSize = useSize(() => laneWrap);
  const data = useDataVersion();
  const [scroll, setScroll] = createSignal({ x: 0, y: 0 });
  const layer = new Layer();
  const geo = (): RackGeo => {
    const steps = stepsOf(pt()), w = size().w;
    const cw = clamp(Math.floor(w / steps), 14, 40);
    return { cw, sx: scroll().x, sy: scroll().y, rh: RH, steps, rows: chans().length };
  };
  const maxScroll = () => { const G = geo(); return { x: Math.max(0, G.steps * G.cw - size().w), y: Math.max(0, G.rows * RH - size().h) }; };
  createEffect(() => { const m = maxScroll(), s = scroll(); if (s.x > m.x || s.y > m.y) setScroll({ x: Math.min(s.x, m.x), y: Math.min(s.y, m.y) }); });
  const rollRows = () => new Set(chans().filter(c => isRollRow(pt().notes.filter(n => visible(n, pt())), c)).map(c => c.id));

  // static + notes: redrawn when data, geometry or selection change
  createEffect(() => {
    data(); const G = geo(), { w, h } = size(); void fonts.epoch;
    if (!w || !h) return;
    fitCanvas(cv, w, h); fitCanvas(head, w, h);
    const g = cv.getContext('2d')!, dpr = window.devicePixelRatio || 1;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    drawRack(g, w, h, G, layer, pt(), chans(), selCh()?.id ?? null, rollRows(), chans().some(c => c.solo));
    lastHead = -2;
  });
  createEffect(() => {
    data(); const G = geo(), { w, h } = laneSize(), c = selCh();
    if (!w || !h) return;
    fitCanvas(laneCv, w, h);
    const g = laneCv.getContext('2d')!, dpr = window.devicePixelRatio || 1;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    const notes = c ? pt().notes.map((n, i) => ({ n, i })).filter(x => x.n.ch === c.id && visible(x.n, pt())) : [];
    drawLane(g, w, h, { x0: 0, cw: G.cw, sx: G.sx, steps: G.steps, notes, color: c?.color ?? '#4f8ef7', cells: !(c && rollRows().has(c.id)), muted: !!c?.mute });
  });
  // playhead column: only when the step changes
  let lastHead = -2;
  onMount(() => onCleanup(addFrameTask(() => {
    const p = curPat(); if (!p || !head) return;
    const s = playStep(p);
    if (s === lastHead) return;
    lastHead = s;
    const { w, h } = size(); if (!w) return;
    const g = head.getContext('2d')!, dpr = window.devicePixelRatio || 1;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    drawRackHead(g, w, h, geo(), s);
  })));

  const cellAt = (e: PointerEvent | MouseEvent) => {
    const r = cv.getBoundingClientRect(), G = geo();
    return { row: Math.floor((e.clientY - r.top + G.sy) / RH), step: Math.floor((e.clientX - r.left + G.sx) / G.cw) };
  };
  const gridDown = (e: PointerEvent) => {
    if (e.button !== 0 && e.button !== 2) return;
    e.preventDefault();
    const G = geo(), { row, step } = cellAt(e), c = chans()[row];
    if (!c || step < 0 || step >= G.steps) return;
    selectChannel(c.id);
    if (rollRows().has(c.id)) {
      // a melodic line (lengths / pitches): the steps would not show it — the piano roll does
      if (e.button === 0 && dblCheck('prow' + c.id)) { if (isPitched(c)) setView('roll', c.id); return; }
      say(c.name + ': nuty z długością / wysokością — 2× klik otwiera piano roll');
      return;
    }
    const on = e.button === 0 ? !pt().notes.some(n => n.ch === c.id && Math.abs(n.step - step) < 1e-3) : false;
    const g = new Gesture(on ? 'Kroki' : 'Usuń kroki');
    let notes = setStep(pt().notes, c.id, step, on);
    g.update([putNotes(pt(), notes)]);
    if (on) audition(c.id, undefined, 0.8);
    let last = { row, step };
    track(ev => {
      const q = cellAt(ev);
      const r = clamp(q.row, 0, chans().length - 1), s = clamp(q.step, 0, G.steps - 1);
      if (r === last.row && s === last.step) return;
      for (const [rr, ss] of cellsBetween(last.row, last.step, r, s)) {
        const cc = chans()[rr];
        if (cc && !rollRows().has(cc.id)) notes = setStep(notes, cc.id, ss, on);
      }
      last = { row: r, step: s };
      g.update([putNotes(pt(), notes)]);
    }, () => g.end(), on ? 'copy' : 'not-allowed');
  };
  const wheel = (e: WheelEvent) => {
    const m = maxScroll(); if (!m.x && !m.y) return;
    e.preventDefault();
    const horiz = e.shiftKey || Math.abs(e.deltaX) > Math.abs(e.deltaY) || !m.y;
    const d = (e.deltaY || e.deltaX);
    setScroll(s => horiz ? { ...s, x: clamp(s.x + d, 0, m.x) } : { ...s, y: clamp(s.y + d * 0.6, 0, m.y) });
  };
  // velocity lane
  const laneDown = (e: PointerEvent) => {
    const c = selCh(); if (!c || e.button !== 0) return;
    e.preventDefault();
    const G = geo(), r = laneCv.getBoundingClientRect(), roll = rollRows().has(c.id);
    const at = (ev: PointerEvent) => ({ s: Math.floor((ev.clientX - r.left + G.sx) / G.cw), v: clamp(1 - (ev.clientY - r.top - 6) / (r.height - 9), 0, 1) });
    const g = new Gesture('Velocity');
    let notes = pt().notes, last = at(e);
    const apply = (s: number, v: number) => { notes = roll ? notes.map(n => n.ch === c.id && Math.floor(n.step + 1e-3) === s ? { ...n, v: Math.round(v * 100) / 100 } : n) : setVelAt(notes, c.id, s, v); };
    apply(last.s, last.v);
    g.update([putNotes(pt(), notes)]);
    say(c.name + ' · velocity ' + Math.round(last.v * 100) + '%');
    track(ev => {
      const q = at(ev);
      const n = Math.abs(q.s - last.s);
      for (let k = 0; k <= n; k++) { const t = n ? k / n : 1; apply(Math.round(last.s + (q.s - last.s) * t), last.v + (q.v - last.v) * t); }
      last = q;
      g.update([putNotes(pt(), notes)]);
      say(c.name + ' · velocity ' + Math.round(q.v * 100) + '%');
    }, () => g.end(), 'ns-resize');
  };
  const hbar = () => { const m = maxScroll(), w = size().w, G = geo(); if (!m.x) return null; const total = G.steps * G.cw; return { left: scroll().x / total * w, width: Math.max(24, w / total * w) }; };
  const hbarDown = (e: PointerEvent) => {
    e.preventDefault(); e.stopPropagation();
    const sx = e.clientX, x0 = scroll().x, G = geo(), k = (G.steps * G.cw) / size().w;
    track(ev => setScroll(s => ({ ...s, x: clamp(x0 + (ev.clientX - sx) * k, 0, maxScroll().x) })), null, 'grabbing');
  };
  void props;
  return (
    <div class="rack">
      <div class="rackbody" onWheel={wheel}>
        <div class="rackleft" style={{ width: LEFTW + 'px' }}>
          <div style={{ transform: `translateY(${-scroll().y}px)` }}>
            <For each={chans()}>{c => <ChanRow c={c} />}</For>
          </div>
        </div>
        <div ref={gridWrap} class="rackgrid">
          <canvas ref={cv} style={{ position: 'absolute', inset: 0, width: '100%', height: '100%', 'touch-action': 'none', cursor: 'pointer' }}
            onPointerDown={gridDown} onContextMenu={e => e.preventDefault()} />
          <canvas ref={head} style={{ position: 'absolute', inset: 0, width: '100%', height: '100%', 'pointer-events': 'none' }} />
          <Show when={!chans().length}><div class="pstate">Brak kanałów — dodaj instrument lub sampler przyciskiem „+ Kanał”</div></Show>
          <Show when={hbar()}>{b => <div class="hbar"><div style={{ left: b().left + 'px', width: b().width + 'px' }} onPointerDown={hbarDown} /></div>}</Show>
        </div>
      </div>
      <div class="lanerow" style={{ height: LANE_H + 'px' }}>
        <div class="lanelab" style={{ width: LEFTW + 'px' }}>
          <button class="smallbtn addch" title="Dodaj kanał: wbudowany instrument albo sampler z audio projektu / pliku"
            onClick={e => { const r = (e.currentTarget as HTMLElement).getBoundingClientRect(); addChannelMenu(r.left, r.top - 8 - Math.min(window.innerHeight - 40, 80 + (5 + Math.min(14, S.project.sources.length)) * 26)); }}>
            <Icon d="M12 5v14 M5 12h14" size={11} />Kanał
          </button>
          <div class="lanetitle">
            <span class="tag9">VELOCITY</span>
            <span class="ell" style={{ color: selCh()?.color ?? 'var(--fg3)' }}>{selCh()?.name ?? '—'}</span>
          </div>
        </div>
        <div ref={laneWrap} class="lanecv">
          <canvas ref={laneCv} style={{ position: 'absolute', inset: 0, width: '100%', height: '100%', 'touch-action': 'none', cursor: 'ns-resize' }}
            onPointerDown={laneDown} onContextMenu={e => e.preventDefault()} />
        </div>
      </div>
      <div class="pstatus mono">
        <span>{chans().length} kanałów · {pt().notes.filter(n => visible(n, pt())).length} nut{pt().notes.some(n => !visible(n, pt())) ? ' · ' + pt().notes.filter(n => !visible(n, pt())).length + ' poza końcem (ukryte)' : ''}</span>
        <span>Klik = krok · PPM = usuń · przeciągnij = maluj · Ctrl+klik w nazwę = odsłuch · PPM na kanale = menu</span>
      </div>
    </div>
  );
}

function ChanRow(props: { c: Channel }) {
  const c = () => props.c;
  const sel = () => U.patWin?.ch === c().id;
  const anySolo = () => channels().some(x => x.solo);
  let vg: Gesture | null = null, pg: Gesture | null = null;
  const live = () => chanById(c().id) ?? c();
  const volOp = (t: number) => chanOp(live(), { vol: Math.round(clamp(t, 0, 1) * 10000) / 10000 });
  const panOp = (t: number) => chanOp(live(), { pan: Math.round(clamp(t * 2 - 1, -1, 1) * 100) / 100 });
  const nameDown = (e: PointerEvent) => {
    if (e.button === 2) { e.preventDefault(); e.stopPropagation(); selectChannel(c().id); chanMenu(c(), e.clientX, e.clientY); return; }
    if (e.button !== 0) return;
    selectChannel(c().id);
    if (e.ctrlKey || e.metaKey) { audition(c().id, undefined, 0.8, true); return; }
    if (dblCheck('cname' + c().id) && isPitched(c())) setView('roll', c().id);
  };
  return (
    <div class="crow" classList={{ sel: sel(), muted: c().mute || (anySolo() && !c().solo) }} style={{ height: RH + 'px' }}
      onPointerDown={e => { if (e.button === 0) selectChannel(c().id); }}
      onContextMenu={e => { e.preventDefault(); selectChannel(c().id); chanMenu(c(), e.clientX, e.clientY); }}>
      <span class="cstrip" style={{ background: c().color }} />
      <button class="led" classList={{ on: !c().mute && (!anySolo() || c().solo), solo: c().solo }}
        title={(c().mute ? 'Wyciszony' : c().solo ? 'Solo' : 'Aktywny') + ' — klik = mute · PPM = solo'}
        onPointerDown={e => { e.stopPropagation(); e.preventDefault(); if (e.button === 2) toggleChanSolo(c()); else if (e.button === 0) toggleChanMute(c()); }}
        onContextMenu={e => { e.preventDefault(); e.stopPropagation(); }} />
      <MiniKnob key={'cv' + c().id} t={() => c().vol} title={'Głośność ' + volText(c().vol) + ' · przeciągnij ↕ · 2× klik = 0 dB'}
        onStart={() => { vg = new Gesture('Głośność kanału'); }} onChange={t => { vg?.update([volOp(t)]); say(c().name + ' · ' + volText(t)); }}
        onEnd={() => { vg?.end(); vg = null; }} onReset={() => commit([volOp(VOL_UNITY)], 'Głośność kanału')}
        onWheel={t => commit([volOp(t)], 'Głośność kanału')} />
      <MiniKnob key={'cp' + c().id} t={() => (c().pan + 1) / 2} title={'Panorama ' + panText(c().pan) + ' · przeciągnij ↕ · 2× klik = środek'}
        onStart={() => { pg = new Gesture('Panorama kanału'); }} onChange={t => { pg?.update([panOp(t)]); say(c().name + ' · pan ' + panText(t * 2 - 1)); }}
        onEnd={() => { pg?.end(); pg = null; }} onReset={() => commit([panOp(0.5)], 'Panorama kanału')}
        onWheel={t => commit([panOp(t)], 'Panorama kanału')} />
      <button class="cins" classList={{ auto: c().insert == null }} title={insTitle(c())}
        onPointerDown={e => e.stopPropagation()} onClick={e => { const r = (e.currentTarget as HTMLElement).getBoundingClientRect(); insertMenu(c(), r.left, r.bottom + 2); }}>{insLabel(c())}</button>
      <button class="cname" classList={{ sel: sel() }} title={c().name + ' · ' + chanKindLabel(c()) + ' — klik = wybierz · Ctrl+klik = odsłuch' + (isPitched(c()) ? ' · 2× klik = piano roll' : '') + ' · PPM = menu'}
        onPointerDown={nameDown}>
        <span class="cicon" style={{ color: c().color }}><InstIcon id={channelIcon(c())} size={11} /></span>
        <span class="ell">{c().name}</span>
        <Show when={c().kind === 'sample'}><span class="ckind">SMP</span></Show>
      </button>
    </div>
  );
}

// ---------------------------------------------------------------- piano roll

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

function Roll(props: { root: () => HTMLDivElement }) {
  let wrap!: HTMLDivElement, cv!: HTMLCanvasElement, phead!: HTMLDivElement, laneCv!: HTMLCanvasElement, laneWrap!: HTMLDivElement;
  const pt = () => curPat()!;
  const ch = () => chanById(W().ch)!;
  const size = useSize(() => wrap);
  const laneSize = useSize(() => laneWrap);
  const data = useDataVersion();
  const layer = new Layer();
  const [sel, setSel] = createSignal<Set<number>>(new Set());
  const [hoverKey, setHoverKey] = createSignal<number | null>(null);
  const [marq, setMarq] = createSignal<{ x0: number; y0: number; x1: number; y1: number } | null>(null);
  const [view, setViewS] = createSignal({ sx: 0, sy: 0, zoom: 1, rowH: 14 });
  const root = () => rollKey() ?? projectKey().root;
  const scaleIdx = () => (rollKey() != null ? rollScale() : projectKey().scale);
  const mask = () => SCALES[scaleIdx()][1];
  const unit = (alt = false) => (alt ? 0 : rollSnap());
  const geo = (): RollGeo => {
    const steps = stepsOf(pt()), fit = (size().w - KW) / steps;
    return { pps: clamp(fit, 6, 48) * view().zoom, sx: view().sx, sy: view().sy, rowH: view().rowH, kw: KW, steps };
  };
  const chanNotes = () => pt().notes.map((n, i) => ({ n, i })).filter(x => x.n.ch === ch().id && visible(x.n, pt()));
  // keep the selection valid when the notes change underneath (undo, CLI)
  createEffect(() => { const ok = new Set(chanNotes().map(x => x.i)); const s = sel(); if ([...s].some(i => !ok.has(i))) setSel(new Set([...s].filter(i => ok.has(i)))); });
  // initial vertical position: centred on the channel's notes (or its root)
  let centred = -1;
  createEffect(() => {
    const h = size().h, id = ch()?.id; if (!h || centred === id) return;
    centred = id;
    const ks = chanNotes().map(x => keyOf(x.n, ch()));
    const mid = ks.length ? (Math.min(...ks) + Math.max(...ks)) / 2 : ch().root;
    setViewS(v => ({ ...v, sx: 0, sy: clamp((127 - mid) * v.rowH - h / 2, 0, 128 * v.rowH - h) }));
    setSel(new Set<number>());
  });
  createEffect(() => {
    data(); sel(); hoverKey(); marq(); root(); mask(); void fonts.epoch;
    const { w, h } = size(); if (!w || !h) return;
    fitCanvas(cv, w, h);
    const g = cv.getContext('2d')!, dpr = window.devicePixelRatio || 1;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    drawRoll(g, w, h, geo(), layer, { pt: pt(), ch: ch(), chans: new Map(channels().map(c => [c.id, c])), sel: sel(), root: root(), mask: mask(), hoverKey: hoverKey(), marquee: marq() });
  });
  createEffect(() => {
    data(); sel();
    const { w, h } = laneSize(); if (!w || !h) return;
    fitCanvas(laneCv, w, h);
    const g = laneCv.getContext('2d')!, dpr = window.devicePixelRatio || 1;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    const G = geo();
    drawLane(g, w, h, { x0: KW, cw: G.pps, sx: G.sx, steps: G.steps, notes: chanNotes(), color: ch().color, sel: sel(), cells: false, muted: ch().mute });
  });
  onMount(() => onCleanup(addFrameTask(() => {
    const p = curPat(); if (!p || !phead) return;
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
  const hitNote = (step: number, key: number) => noteAt(pt().notes, ch().id, ch(), step, key);
  const edgeHit = (i: number, x: number) => { const n = pt().notes[i], G = geo(); return x >= rollX(G, n.step + n.len) - Math.min(6, n.len * G.pps * 0.3); };
  const commitNotes = (notes: Note[], label: string) => commit([putNotes(pt(), notes)], label);

  function eraseDrag(e: PointerEvent) {
    const g = new Gesture('Usuń nuty');
    let notes = pt().notes, n0 = notes.length;
    const del = (ev: PointerEvent | MouseEvent) => {
      const q = pos(ev), i = noteAt(notes, ch().id, ch(), q.step, q.key);
      if (i < 0) return;
      notes = notes.filter((_, k) => k !== i);
      g.update([putNotes(pt(), notes)]);
    };
    setSel(new Set<number>());
    del(e);
    track(ev => del(ev), () => { g.end(); if (notes.length < n0) say('Usunięto ' + (n0 - notes.length) + ' nut'); }, 'not-allowed');
  }
  function moveDrag(e: PointerEvent, idx: number[], mode: 'move' | 'resize', g?: Gesture) {
    const orig = pt().notes.slice(), p0 = pos(e), lead = pt().notes[idx[idx.length - 1]] ?? pt().notes[idx[0]];
    const gest = g ?? new Gesture(mode === 'move' ? 'Przesuń nuty' : 'Długość nut');
    let lastKey = keyOf(lead, ch()), moved = false;
    track(ev => {
      const q = pos(ev);
      if (!moved && Math.hypot(q.x - p0.x, q.y - p0.y) < 3) return;
      moved = true;
      const notes = mode === 'move'
        ? moveNotes(orig, idx, q.step - p0.step, q.key - p0.key, unit(ev.altKey), ch())
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
    track(ev => {
      const q = pos(ev), G = geo();
      setMarq({ x0: p0.x, y0: p0.y, x1: q.x, y1: q.y });
      const [x0, x1] = [Math.min(p0.x, q.x), Math.max(p0.x, q.x)], [y0, y1] = [Math.min(p0.y, q.y), Math.max(p0.y, q.y)];
      const s = new Set(base);
      for (const { n, i } of chanNotes()) {
        const nx = rollX(G, n.step), nw = n.len * G.pps, ny = rollY(G, keyOf(n, ch()));
        if (nx < x1 && nx + nw > x0 && ny < y1 && ny + G.rowH > y0) s.add(i);
      }
      setSel(s);
    }, () => { setMarq(null); if (sel().size) say('Zaznaczono ' + sel().size + ' nut'); });
  }
  function paintDrag(e: PointerEvent, g: Gesture, startNotes: Note[]) {
    let notes = startNotes;
    let lastCell = Math.floor(pos(e).step / (unit(e.altKey) || 1));
    track(ev => {
      const q = pos(ev), u = unit(ev.altKey) || 1, cell = Math.floor(q.step / u);
      if (cell === lastCell || q.step < 0 || q.step >= stepsOf(pt())) return;
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
    if (e.button === 2 || tool === 'erase') return eraseDrag(e);
    const i = hitNote(p.step, p.key);
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
    if (s < 0 || s >= stepsOf(pt())) return;
    const n: Note = { ch: ch().id, step: s, len: lastLen(), p: p.key };
    const notes = [...pt().notes, n];
    const g = new Gesture(tool === 'paint' ? 'Maluj nuty' : 'Nowa nuta');
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
    cv.style.cursor = t === 'erase' ? 'not-allowed' : i >= 0 ? (t !== 'select' && edgeHit(i, p.x) ? 'ew-resize' : 'grab') : t === 'select' ? 'crosshair' : 'copy';
  };
  const wheel = (e: WheelEvent) => {
    e.preventDefault();
    const d = e.deltaY || e.deltaX, h = size().h;
    if (e.ctrlKey || e.metaKey) {
      const p = pos(e), G = geo(), z = clamp(view().zoom * Math.pow(1.0015, -d), 0.5, 8);
      const pps = G.pps / view().zoom * z;
      setViewS(v => ({ ...v, zoom: z, sx: Math.max(0, p.step * pps - (p.x - KW)) }));
    } else if (e.altKey) {
      setViewS(v => { const rowH = clamp(v.rowH * Math.pow(1.0015, -d), 8, 28); const k = 127 - (v.sy + h / 2) / v.rowH; return { ...v, rowH, sy: clamp((127 - k) * rowH - h / 2, 0, 128 * rowH - h) }; });
    } else if (e.shiftKey || Math.abs(e.deltaX) > Math.abs(e.deltaY)) {
      const G = geo();
      setViewS(v => ({ ...v, sx: clamp(v.sx + d, 0, Math.max(0, G.steps * G.pps - (size().w - KW) + 40)) }));
    } else setViewS(v => ({ ...v, sy: clamp(v.sy + d * 0.7, 0, 128 * v.rowH - h) }));
  };
  const laneDown = (e: PointerEvent) => {
    if (e.button !== 0) return;
    e.preventDefault();
    const r = laneCv.getBoundingClientRect();
    const at = (ev: PointerEvent) => { const G = geo(); return { step: (ev.clientX - r.left - KW + G.sx) / G.pps, v: clamp(1 - (ev.clientY - r.top - 6) / (r.height - 9), 0, 1) }; };
    const g = new Gesture('Velocity');
    let notes = pt().notes;
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
    if (tl && !m && !e.altKey) { stop(); setRollTool(tl.id); say('Piano roll · ' + tl.label); return; }
    if (m && k === 'a') { stop(); setSel(new Set(chanNotes().map(x => x.i))); say('Zaznaczono ' + chanNotes().length + ' nut'); return; }
    const s = [...sel()].filter(i => pt().notes[i]?.ch === ch().id);
    if (m && (k === 'c' || k === 'x') && !e.shiftKey) {
      stop(); if (!s.length) return;
      noteClip = s.map(i => pt().notes[i]); noteClipAt = null;
      if (k === 'x') { const del = new Set(s); commitNotes(pt().notes.filter((_, i) => !del.has(i)), 'Wytnij nuty'); setSel(new Set<number>()); }
      say((k === 'x' ? 'Wycięto ' : 'Skopiowano ') + s.length + ' nut');
      return;
    }
    if (m && k === 'v' && !e.shiftKey) {
      stop(); if (!noteClip.length) return;
      const a = Math.min(...noteClip.map(n => n.step)), b = Math.max(...noteClip.map(n => n.step + n.len));
      const at = noteClipAt ?? a + Math.max(4, Math.ceil((b - a) / 4 - 1e-9) * 4); // first paste lands after the copied block, further ones chain on
      const copies = noteClip.map(n => ({ ...n, ch: ch().id, step: n.step - a + at }));
      const base = pt().notes.length;
      commitNotes([...pt().notes, ...copies], 'Wklej nuty'); setSel(new Set(copies.map((_, j) => base + j)));
      noteClipAt = at + (b - a);
      say('Wklejono ' + copies.length + ' nut');
      return;
    }
    if (m && (k === 'b' || k === 'd')) {
      stop(); if (!s.length) return;
      const r = duplicateRight(pt().notes, s);
      commitNotes(r.notes, 'Duplikuj nuty'); setSel(new Set(r.idx));
      const past = r.idx.filter(i => !visible(r.notes[i], pt())).length;
      say('Zduplikowano ' + s.length + ' nut' + (past ? ' · ' + past + ' poza końcem patternu (ukryte) — wydłuż go, aby je usłyszeć' : ''));
      return;
    }
    if ((e.key === 'ArrowUp' || e.key === 'ArrowDown') && s.length && !e.shiftKey) {
      stop();
      const d = (e.key === 'ArrowUp' ? 1 : -1) * (m ? 12 : 1);
      const notes = transpose(pt().notes, s, d, ch());
      commitNotes(notes, m ? 'Oktawa' : 'Transpozycja');
      const lead = notes[s[s.length - 1]]; audition(ch().id, keyOf(lead, ch()), lead.v ?? 1);
      return;
    }
    if ((e.key === 'Delete' || e.key === 'Backspace') && s.length) {
      stop();
      const del = new Set(s);
      commitNotes(pt().notes.filter((_, i) => !del.has(i)), 'Usuń nuty'); setSel(new Set<number>());
      say('Usunięto ' + s.length + ' nut');
      return;
    }
    if (e.key === 'Escape' && sel().size) { stop(); setSel(new Set<number>()); }
  };
  void props;
  viewKey = onKey;
  onCleanup(() => { if (viewKey === onKey) viewKey = null; });
  const pitched = () => channels().filter(isPitched);
  return (
    <div class="roll">
      <div class="ptools">
        <div class="seg tools">
          <For each={TOOLS}>{tl => (
            <button classList={{ on: rollTool() === tl.id }} title={tl.label + ' (' + tl.key.toUpperCase() + ') — ' + tl.tip} onClick={() => setRollTool(tl.id)}>
              <Icon d={tl.icon} size={14} />
            </button>
          )}</For>
        </div>
        <span class="toolname">{TOOLS.find(t => t.id === rollTool())!.label}</span>
        <div class="vsep" />
        <label class="plab">KANAŁ
          <select class="sel" value={String(ch().id)} onChange={e => setView('roll', +e.currentTarget.value)}>
            <For each={pitched()}>{c => <option value={String(c.id)}>{c.name}</option>}</For>
          </select>
        </label>
        <label class="plab">SIATKA
          <select class="sel" title="Przyciąganie (Alt = bez siatki)" value={String(rollSnap())} onChange={e => setRollSnap(+e.currentTarget.value)}>
            <For each={ROLL_SNAPS}>{([l, v]) => <option value={String(v)}>{l}</option>}</For>
          </select>
        </label>
        <label class="plab">TONACJA
          <span class="row" style={{ gap: '4px' }}>
            <select class="sel" title={rollKey() == null ? 'Z Pitch Correct w projekcie (wykryta tonacja), inaczej C dur' : 'Podświetlenie skali'} value={String(root())} onChange={e => { setRollKey(+e.currentTarget.value); setRollScale(scaleIdx()); }}>
              <For each={NOTE_NAMES}>{(n, i) => <option value={String(i())}>{n}</option>}</For>
            </select>
            <select class="sel" value={String(scaleIdx())} onChange={e => { if (rollKey() == null) setRollKey(root()); setRollScale(+e.currentTarget.value); }}>
              <For each={SCALES}>{([l], i) => <option value={String(i())}>{l}</option>}</For>
            </select>
          </span>
        </label>
        <div style={{ flex: 1 }} />
        <span class="note mono" style={{ 'align-self': 'center', 'font-size': '10px', color: 'var(--fg5)' }}>{sel().size ? sel().size + ' zazn.' : chanNotes().length + ' nut'} · nowa nuta: {lastLenText()}</span>
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
        <span>{TOOLS.find(t => t.id === rollTool())!.tip}</span>
        <span>↑↓ = półton · Ctrl+↑↓ = oktawa · Ctrl+B = duplikuj · Ctrl+kółko = zoom · Alt+kółko = wysokość wierszy</span>
      </div>
    </div>
  );
}

function lastLenText() {
  const l = lastLen();
  return l >= 4 && l % 4 === 0 ? l / 4 + (l === 4 ? ' beat' : ' beaty') : l === 1 ? '1/16' : l === 2 ? '1/8' : l === 0.5 ? '1/32' : l + ' kroków';
}
