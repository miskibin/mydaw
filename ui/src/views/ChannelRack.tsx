// The Channel Rack (F6), as in FL Studio: one row per channel of the project — mute LED, pan,
// volume, mixer insert, name — and the current pattern's steps for it. A channel whose notes are
// more than steps (lengths, chords, off-grid) shows a mini piano roll; a click opens the Piano
// roll. The graph editor (G) edits the selected channel's per-step velocity and key.
// It is a floating window in the effect-window layer; every edit is an op and one undo step.
import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from 'solid-js';
import { unwrap } from 'solid-js/store';
import { t } from '../lib/i18n';
import { TOTAL_BEATS, VOL_UNITY, type Channel } from '../api/types';
import { clamp, panText, volText } from '../lib/format';
import { barsText, cellsBetween, isRollRow, keyOf, notesText, rotate, setKeyAt, setStep, setVelAt, stepsOf } from '../lib/pattern';
import { noteName } from '../lib/pitch';
import { autoDisplay, isAutomated, paramMenu } from '../state/automation';
import { commit, Gesture } from '../state/edit';
import {
  addBar, STEP_MAX, STEP_MIN, audition, chanById, chanKindLabel, chanOp, channels, closeRack, curPat, openRoll, patOp, playStep, putNotes, RACK_LEFT, RACK_ROW,
  rackFront, removeBar, selChan, selectChannel, toggleChanMute, toggleChanSolo,
} from '../state/pattern';
import { arrVersion } from '../state/store';
import { say, setU, U } from '../state/ui';
import { dblCheck, track } from '../ui/drag';
import { addFrameTask, fitCanvas, fonts, Layer } from '../ui/frameLoop';
import { Icon } from '../ui/icons';
import { channelIcon, InstIcon } from '../ui/instIcons';
import { MiniKnob, NumField, parseNum } from '../ui/Knob';
import { closeChannelWindow, toggleChannelWindow } from './ChannelWindow';
import { drawKeyGraph, drawLane, drawRack, drawRackHead, type RackGeo } from './patternDraw';
import { addChannelMenu, chanMenu, insertMenu, insLabel, insTitle, PatFrame, patternMenu, useDataVersion, useSize } from './patternShared';

const GRAPH_H = 92;
const RACK_ICON = 'M4 5h4v4H4z M10 5h4v4h-4z M16 5h4v4h-4z M4 15h4v4H4z M10 15h4v4h-4z M16 15h4v4h-4z';
const GRAPH_ICON = 'M4 20V10 M9 20V4 M14 20v-7 M19 20V8';
/** Key graph range around the channel's root (FL's note pitch graph: ± one octave). */
const KEY_SPAN = 12;

/** The Channel Rack window (shown while `U.rackWin` is set and the project has a pattern). */
export function ChannelRack(props: { z: number }) {
  return <Show when={U.rackWin && curPat()}><Rack z={props.z} /></Show>;
}

function Rack(props: { z: number }) {
  let gridWrap!: HTMLDivElement, cv!: HTMLCanvasElement, head!: HTMLCanvasElement, graphCv!: HTMLCanvasElement, graphWrap!: HTMLDivElement;
  const ptM = createMemo(() => curPat()!);
  const pt = () => ptM();
  const chans = () => channels();
  const selCh = () => selChan();
  const size = useSize(() => gridWrap);
  const graphSize = useSize(() => graphWrap);
  const data = useDataVersion();
  const [scroll, setScroll] = createSignal({ x: 0, y: 0 });
  const layer = new Layer();
  // the pattern plus one greyed bar (FL): a step clicked there makes the pattern a bar longer
  const geo = (): RackGeo => {
    const end = stepsOf(pt()), steps = Math.min(TOTAL_BEATS * 4, end + 16), w = size().w;
    const cw = clamp(Math.floor(w / steps), STEP_MIN, STEP_MAX);
    return { cw, sx: scroll().x, sy: scroll().y, rh: RACK_ROW, steps, end, rows: chans().length };
  };
  // the grid's own height: the add-channel row below the channels scrolls with them
  const contentH = () => (chans().length + 1) * RACK_ROW;
  const maxScroll = () => { const G = geo(); return { x: Math.max(0, G.steps * G.cw - size().w), y: Math.max(0, contentH() - size().h) }; };
  createEffect(() => { const m = maxScroll(), s = scroll(); if (s.x > m.x || s.y > m.y) setScroll({ x: Math.min(s.x, m.x), y: Math.min(s.y, m.y) }); });
  const rollRowsM = createMemo(() => { arrVersion(); const notes = unwrap(pt()).notes; return new Set(unwrap(chans()).filter(c => isRollRow(notes, c)).map(c => c.id)); });
  const rollRows = () => rollRowsM();
  const graph = () => U.rackGraph;

  createEffect(() => {
    data(); const G = geo(), { w, h } = size(); void fonts.epoch;
    if (!w || !h) return;
    fitCanvas(cv, w, h); fitCanvas(head, w, h);
    const g = cv.getContext('2d')!, dpr = window.devicePixelRatio || 1;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    drawRack(g, w, h, G, layer, unwrap(pt()), unwrap(chans()), selCh()?.id ?? null, rollRows(), chans().some(c => c.solo));
    lastHead = -2;
  });
  createEffect(() => {
    const mode = graph(); if (!mode) return;
    data(); const G = geo(), { w, h } = graphSize(), c = selCh();
    if (!w || !h) return;
    fitCanvas(graphCv, w, h);
    const g = graphCv.getContext('2d')!, dpr = window.devicePixelRatio || 1;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    const p = unwrap(pt()), notes = c ? p.notes.map((n, i) => ({ n, i })).filter(x => x.n.ch === c.id) : [];
    const roll = !!c && rollRows().has(c.id);
    if (mode === 'vel') drawLane(g, w, h, { x0: 0, cw: G.cw, sx: G.sx, steps: G.steps, notes, color: c?.color ?? '#4f8ef7', cells: !roll, muted: !!c?.mute });
    else drawKeyGraph(g, w, h, { cw: G.cw, sx: G.sx, steps: G.steps, notes, color: c?.color ?? '#4f8ef7', root: c?.root ?? 60, span: KEY_SPAN, off: roll });
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
    return { row: Math.floor((e.clientY - r.top + G.sy) / RACK_ROW), step: Math.floor((e.clientX - r.left + G.sx) / G.cw) };
  };
  const gridDown = (e: PointerEvent) => {
    if (e.button !== 0 && e.button !== 2) return;
    e.preventDefault();
    const G = geo(), { row, step } = cellAt(e), c = chans()[row];
    if (!c || step < 0 || step >= G.steps) return;
    selectChannel(c.id);
    if (rollRows().has(c.id) && step < G.end) {
      // a melodic line (lengths / chords): the steps cannot show it — the piano roll does (FL: click the mini roll)
      if (e.button === 0) openRoll(c.id);
      return;
    }
    const on = e.button === 0 ? !unwrap(pt()).notes.some(n => n.ch === c.id && Math.abs(n.step - step) < 1e-3) : false;
    const g = new Gesture(on ? t('b.pe.gestSteps') : t('b.pe.gestDelSteps'));
    let notes = setStep(unwrap(pt()).notes, c.id, step, on);
    g.update([putNotes(pt(), notes)]);
    if (on) audition(c.id, undefined, 0.8);
    let last = { row, step };
    track(ev => {
      const q = cellAt(ev);
      const r = clamp(q.row, 0, chans().length - 1), s = clamp(q.step, 0, G.steps - 1);
      if (r === last.row && s === last.step) return;
      for (const [rr, ss] of cellsBetween(last.row, last.step, r, s)) {
        const cc = chans()[rr];
        if (cc && (!rollRows().has(cc.id) || ss >= G.end)) notes = setStep(notes, cc.id, ss, on);
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
  // graph editor: drag across the steps to draw values
  const graphDown = (e: PointerEvent) => {
    const c = selCh(), mode = graph(); if (!c || !mode || e.button !== 0) return;
    e.preventDefault();
    const roll = rollRows().has(c.id);
    if (mode === 'key' && roll) { say(t('b.rk.keyRoll', { name: c.name })); return; }
    const G = geo(), r = graphCv.getBoundingClientRect();
    const at = (ev: PointerEvent) => ({ s: Math.floor((ev.clientX - r.left + G.sx) / G.cw), f: clamp(1 - (ev.clientY - r.top - 6) / (r.height - 9), 0, 1) });
    const g = new Gesture(mode === 'vel' ? 'Velocity' : t('b.rk.keyGest'));
    let notes = unwrap(pt()).notes, last = at(e);
    const keyFor = (f: number) => clamp(Math.round(c.root - KEY_SPAN + f * 2 * KEY_SPAN), 0, 127);
    const apply = (s: number, f: number) => {
      if (mode === 'key') { notes = setKeyAt(notes, c, s, keyFor(f)); return; }
      notes = roll ? notes.map(n => n.ch === c.id && Math.floor(n.step + 1e-3) === s ? { ...n, v: Math.round(f * 100) / 100 } : n) : setVelAt(notes, c.id, s, f);
    };
    const tell = (f: number) => say(c.name + ' · ' + (mode === 'vel' ? 'velocity ' + Math.round(f * 100) + '%' : noteName(keyFor(f))));
    apply(last.s, last.f);
    g.update([putNotes(pt(), notes)]);
    tell(last.f);
    if (mode === 'key') { const n = notes.find(x => x.ch === c.id && Math.round(x.step) === last.s); if (n) audition(c.id, keyOf(n, c), 0.8); }
    let lastKey = keyFor(last.f);
    track(ev => {
      const q = at(ev);
      const n = Math.abs(q.s - last.s);
      for (let k = 0; k <= n; k++) { const tt = n ? k / n : 1; apply(Math.round(last.s + (q.s - last.s) * tt), last.f + (q.f - last.f) * tt); }
      last = q;
      g.update([putNotes(pt(), notes)]);
      tell(q.f);
      if (mode === 'key' && keyFor(q.f) !== lastKey) { lastKey = keyFor(q.f); if (notes.some(x => x.ch === c.id && Math.round(x.step) === q.s)) audition(c.id, lastKey, 0.8); }
    }, () => g.end(), 'ns-resize');
  };
  const hbar = () => { const m = maxScroll(), w = size().w, G = geo(); if (!m.x) return null; const total = G.steps * G.cw; return { left: scroll().x / total * w, width: Math.max(24, w / total * w) }; };
  const hbarDown = (e: PointerEvent) => {
    e.preventDefault(); e.stopPropagation();
    const sx = e.clientX, x0 = scroll().x, G = geo(), k = (G.steps * G.cw) / size().w;
    track(ev => setScroll(s => ({ ...s, x: clamp(x0 + (ev.clientX - sx) * k, 0, maxScroll().x) })), null, 'grabbing');
  };
  const swingG: { g: Gesture | null } = { g: null };
  const applySwing = (v: number, mode: 'begin' | 'move' | 'end' | 'set' | 'wheel') => {
    const op = patOp(pt(), { swing: Math.round(clamp(v, 0, 100)) / 100 });
    if (mode === 'begin') { swingG.g = new Gesture('Swing'); swingG.g.update([op]); }
    else if (mode === 'move') swingG.g?.update([op]);
    else if (mode === 'end') { swingG.g?.end(); swingG.g = null; }
    else commit([op], 'Swing');
    say(pt().name + ' · swing ' + Math.round(clamp(v, 0, 100)) + '%');
  };
  /** The graph editor adds its own height to the window instead of squeezing the channel rows. */
  const toggleGraph = () => {
    const on = !graph(), w = U.rackWin;
    setU('rackGraph', on ? 'vel' : null);
    if (w) setU('rackWin', 'h', clamp(w.h + (on ? GRAPH_H : -GRAPH_H), 200, window.innerHeight - w.y - 10));
  };
  // a pattern that grows (notes past the end, + bar) widens the window up to the screen, never narrows it
  createEffect(on(() => stepsOf(pt()), (steps, prev) => {
    const w = U.rackWin; if (!w || prev == null || steps <= prev) return;
    const want = RACK_LEFT + (steps + 16) * STEP_MAX + 8;
    if (want > w.w) setU('rackWin', 'w', Math.min(want, window.innerWidth - w.x - 10));
  }));
  const onKey = (e: KeyboardEvent) => {
    const tg = (e.target as HTMLElement).tagName;
    if (tg === 'INPUT' || tg === 'SELECT' || e.ctrlKey || e.metaKey) return;
    // FL: Shift+Alt+← / → shifts the selected channel's steps by one, wrapping around the pattern
    if (e.altKey && e.shiftKey && (e.key === 'ArrowLeft' || e.key === 'ArrowRight')) {
      e.preventDefault(); e.stopPropagation();
      const c = selCh(); if (!c) return;
      const d = e.key === 'ArrowLeft' ? -1 : 1;
      commit([putNotes(pt(), rotate(unwrap(pt()).notes, c.id, stepsOf(pt()), d))], t('b.pe.shiftSteps'));
      say(c.name + ' · ' + t(d < 0 ? 'b.pe.shiftL' : 'b.pe.shiftR'));
      return;
    }
    if (e.altKey) return;
    if (e.key.toLowerCase() === 'g') { e.preventDefault(); e.stopPropagation(); toggleGraph(); }
  };
  const notesCount = createMemo(() => { arrVersion(); return unwrap(pt()).notes.length; });
  return (
    <PatFrame win={U.rackWin!} key="rackWin" z={props.z} class="rackwin" minW={480} minH={200} front={rackFront} onKey={onKey}
      head={<>
        <span class="tile" style={{ width: '20px', height: '20px', background: pt().color + '2e', color: pt().color }}><Icon d={RACK_ICON} size={12} /></span>
        <span class="wintitle">Channel rack</span>
        <button class="patname" title={t('b.rk.patTitle')} onClick={e => { const r = (e.currentTarget as HTMLElement).getBoundingClientRect(); patternMenu(r.left, r.bottom + 4); }}>
          <span class="sw" style={{ background: pt().color }} /><span class="ell">{pt().name}</span>
          <svg width="8" height="8" viewBox="0 0 8 8"><path d="M1 2.5l3 3 3-3" fill="none" stroke="currentColor" stroke-width="1.3" /></svg>
        </button>
        <div class="bars" title={t('b.rk.barsTitle')}>
          <button disabled={pt().len <= 4} onClick={() => removeBar(pt())} title={t('b.pm.removeBarTitle')}>−</button>
          <span class="mono">{barsText(pt().len / 4)}</span>
          <button onClick={() => addBar(pt())} title={t('b.pm.addBarTitle')}>+</button>
        </div>
        <div>
          <NumField label="SWING" width={58} value={() => Math.round((pt().swing ?? 0) * 100)} text={v => Math.round(v) + '%'} parse={parseNum}
            drag={(v, d) => clamp(Math.round(v + d * 0.5), 0, 100)} apply={applySwing} title={t('b.pe.swingTitle')} />
        </div>
        <button class="iconbtn" classList={{ on: !!graph() }} title={t('b.rk.graphTitle')} onClick={toggleGraph}><Icon d={GRAPH_ICON} size={13} /></button>
        <div style={{ flex: 1 }} />
        <button class="closebtn" title={t('b.pe.close')} onClick={closeRack}>×</button>
      </>}>
      <div class="rack">
        <div class="rackbody" onWheel={wheel}>
          <div class="rackleft" style={{ width: RACK_LEFT + 'px' }}>
            <div style={{ transform: `translateY(${-scroll().y}px)` }}>
              <For each={chans()}>{c => <ChanRow c={c} />}</For>
              <div class="crow addrow" style={{ height: RACK_ROW + 'px' }}>
                <button class="smallbtn addch" title={t('b.pe.addChTitle')}
                  onClick={e => { const r = (e.currentTarget as HTMLElement).getBoundingClientRect(); addChannelMenu(r.left, r.bottom + 4); }}>
                  <Icon d="M12 5v14 M5 12h14" size={11} />{t('b.pe.addCh')}
                </button>
              </div>
            </div>
          </div>
          <div ref={gridWrap} class="rackgrid">
            <canvas ref={cv} style={{ position: 'absolute', inset: 0, width: '100%', height: '100%', 'touch-action': 'none', cursor: 'pointer' }}
              onPointerDown={gridDown} onContextMenu={e => e.preventDefault()} />
            <canvas ref={head} style={{ position: 'absolute', inset: 0, width: '100%', height: '100%', 'pointer-events': 'none' }} />
            <Show when={!chans().length}><div class="pstate">{t('b.pe.noChannels')}</div></Show>
            <Show when={hbar()}>{b => <div class="hbar"><div style={{ left: b().left + 'px', width: b().width + 'px' }} onPointerDown={hbarDown} /></div>}</Show>
          </div>
        </div>
        {/* always mounted (hidden when off) so its size is observed from the start */}
        <div class="lanerow" style={{ height: GRAPH_H + 'px', display: graph() ? 'flex' : 'none' }}>
            <div class="lanelab" style={{ width: RACK_LEFT + 'px' }}>
              <div class="seg graphtabs">
                <button classList={{ on: graph() === 'vel' }} onClick={() => setU('rackGraph', 'vel')}>{t('b.rk.vel')}</button>
                <button classList={{ on: graph() === 'key' }} onClick={() => setU('rackGraph', 'key')}>{t('b.rk.key')}</button>
              </div>
              <div class="lanetitle"><span class="ell" style={{ color: selCh()?.color ?? 'var(--fg3)' }}>{selCh()?.name ?? '—'}</span></div>
            </div>
            <div ref={graphWrap} class="lanecv">
              <canvas ref={graphCv} style={{ position: 'absolute', inset: 0, width: '100%', height: '100%', 'touch-action': 'none', cursor: 'ns-resize' }}
                onPointerDown={graphDown} onContextMenu={e => e.preventDefault()} />
            </div>
          </div>
        <div class="pstatus mono">
          <span>{t('b.rk.status', { ch: chans().length, notes: notesText(notesCount()), bars: barsText(pt().len / 4) })}</span>
          <span>{t('b.rk.hint')}</span>
        </div>
      </div>
    </PatFrame>
  );
}

function ChanRow(props: { c: Channel }) {
  const c = () => props.c;
  const sel = () => selChan()?.id === c().id;
  const anySolo = () => channels().some(x => x.solo);
  let vg: Gesture | null = null, pg: Gesture | null = null;
  const live = () => chanById(c().id) ?? c();
  const volOp = (v: number) => chanOp(live(), { vol: Math.round(clamp(v, 0, 1) * 10000) / 10000 });
  const volT = () => ({ kind: 'chanVol' as const, id: c().id }), panT = () => ({ kind: 'chanPan' as const, id: c().id });
  const panOp = (v: number) => chanOp(live(), { pan: Math.round(clamp(v * 2 - 1, -1, 1) * 100) / 100 });
  const nameDown = (e: PointerEvent) => {
    if (e.button === 2) { e.preventDefault(); e.stopPropagation(); selectChannel(c().id); chanMenu(c(), e.clientX, e.clientY); return; }
    if (e.button !== 0) return;
    selectChannel(c().id);
    if (e.ctrlKey || e.metaKey) { audition(c().id, undefined, 0.8, true); return; }
    if (dblCheck('cname' + c().id)) { closeChannelWindow(); openRoll(c().id); return; }
    toggleChannelWindow(c().id);
  };
  return (
    <div class="crow" data-ch={c().id} classList={{ sel: sel(), muted: c().mute || (anySolo() && !c().solo) }} style={{ height: RACK_ROW + 'px' }}
      onPointerDown={e => { if (e.button === 0) selectChannel(c().id); }}
      onContextMenu={e => { e.preventDefault(); selectChannel(c().id); chanMenu(c(), e.clientX, e.clientY); }}>
      <span class="cstrip" style={{ background: c().color }} />
      <button class="led" classList={{ on: !c().mute && (!anySolo() || c().solo), solo: c().solo }}
        title={t('b.pe.ledTitle', { state: t(c().mute ? 'b.pe.ledMuted' : c().solo ? 'b.pe.ledSolo' : 'b.pe.ledActive') })}
        onPointerDown={e => { e.stopPropagation(); e.preventDefault(); if (e.button === 2) toggleChanSolo(c()); else if (e.button === 0) toggleChanMute(c()); }}
        onContextMenu={e => { e.preventDefault(); e.stopPropagation(); }} />
      <MiniKnob key={'cp' + c().id} t={() => ((autoDisplay(panT()) ?? c().pan) + 1) / 2} title={t('b.pe.panTitle', { v: panText(autoDisplay(panT()) ?? c().pan) })}
        automated={() => isAutomated(panT())} onMenu={(x, y) => paramMenu(x, y, panT(), () => commit([panOp(0.5)], t('b.cw.panCh')), c().color)}
        onStart={() => { pg = new Gesture(t('b.cw.panCh')); }} onChange={v => { pg?.update([panOp(v)]); say(c().name + ' · pan ' + panText(v * 2 - 1)); }}
        onEnd={() => { pg?.end(); pg = null; }} onReset={() => commit([panOp(0.5)], t('b.cw.panCh'))}
        onWheel={v => commit([panOp(v)], t('b.cw.panCh'))} />
      <MiniKnob key={'cv' + c().id} t={() => autoDisplay(volT()) ?? c().vol} title={t('b.pe.volTitle', { v: volText(autoDisplay(volT()) ?? c().vol) })}
        automated={() => isAutomated(volT())} onMenu={(x, y) => paramMenu(x, y, volT(), () => commit([volOp(VOL_UNITY)], t('b.cw.volCh')), c().color)}
        onStart={() => { vg = new Gesture(t('b.cw.volCh')); }} onChange={v => { vg?.update([volOp(v)]); say(c().name + ' · ' + volText(v)); }}
        onEnd={() => { vg?.end(); vg = null; }} onReset={() => commit([volOp(VOL_UNITY)], t('b.cw.volCh'))}
        onWheel={v => commit([volOp(v)], t('b.cw.volCh'))} />
      <button class="cins" classList={{ auto: c().insert == null }} title={insTitle(c())}
        onPointerDown={e => e.stopPropagation()} onClick={e => { const r = (e.currentTarget as HTMLElement).getBoundingClientRect(); insertMenu(c(), r.left, r.bottom + 2); }}>{insLabel(c())}</button>
      <button class="cname" classList={{ sel: sel() }} title={t('b.pe.cnameTitle', { name: c().name, kind: chanKindLabel(c()) })}
        onPointerDown={nameDown}>
        <span class="cicon" style={{ color: c().color }}><InstIcon id={channelIcon(c())} size={11} /></span>
        <span class="ell">{c().name}</span>
        <Show when={c().kind === 'keys'}><span class="ckind">KEYS</span></Show>
      </button>
    </div>
  );
}
