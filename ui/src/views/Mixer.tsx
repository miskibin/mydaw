import { createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import { VOL_UNITY, type Fx, type Id, type Insert } from '../api/types';
import { clamp, panText, volText } from '../lib/format';
import { t as tr, tName } from '../lib/i18n';
import { presetInfo } from '../lib/presets';
import * as A from '../state/actions';
import { commit, Gesture, wheelEdit } from '../state/edit';
import { autoDisplay, isAutomated, paramMenu } from '../state/automation';
import { MiniKnob } from '../ui/Knob';
import { anyInsertSolo, insName, S } from '../state/store';
import { tele } from '../state/telemetry';
import { openMenu, openRename, say, setU, U, type MenuItem } from '../state/ui';
import { dblCheck, track } from '../ui/drag';
import { addFrameTask } from '../ui/frameLoop';
import { FXMETA, FxTile, Icon, ICONS } from '../ui/icons';
import { guessIcon, InstIcon, trackIcon } from '../ui/instIcons';
import { openFxPicker } from './FxPicker';

const L30 = 51.84, C30 = 69.12;

// meter masks per insert, updated from the frame loop
const meterEls = new Map<Id, [HTMLDivElement, HTMLDivElement]>();
const hold = new Map<Id, [number, number]>();
function meterTask() {
  const ins = tele.frame.ins;
  for (const [id, els] of meterEls) {
    const e = ins.get(id), h = hold.get(id) ?? [0, 0];
    const l = Math.max(e?.l ?? 0, h[0] * 0.9), r = Math.max(e?.r ?? 0, h[1] * 0.9);
    if (l === h[0] && r === h[1] && hold.has(id)) continue;
    hold.set(id, [l, r]);
    const f = (v: number) => { const db = v > 1e-5 ? 20 * Math.log10(v) : -90; return 1 - clamp((db + 60) / 66, 0, 1); };
    els[0].style.transform = `scaleY(${f(l)})`;
    els[1].style.transform = `scaleY(${f(r)})`;
  }
}

export function openFxMenu(x: number, y: number, insId: Id) {
  openFxPicker(insId, x, y);
}

function openStripMenu(x: number, y: number, id: Id) {
  const m = S.project.inserts.find(v => v.id === id); if (!m) return;
  openMenu(x, y, [
    { t: 'title', label: insName(m) },
    { t: 'item', label: tr('c.mix.rename'), act: () => openRename(x, y, m.name, v => A.setInsert(id, { name: v }, tr('c.mix.undoName'))) },
    { t: 'swatches', current: m.color, pick: col => A.setInsert(id, { color: col }, tr('c.mix.undoColor')) },
    { t: 'sep' },
    { t: 'item', label: tr('c.mix.addFx'), act: () => openFxMenu(x, y, id) },
    { t: 'item', label: tr('c.mix.resetVolPan'), act: () => A.setInsert(id, { vol: VOL_UNITY, pan: 0 }, tr('c.mix.undoReset')) },
    ...(id === 0 ? [] : [{ t: 'sep' }, { t: 'item', label: tr('c.mix.removeInsert'), danger: true, act: () => A.removeInsert(id) }] as MenuItem[]),
  ], 260);
}

function Strip(props: { m: Insert; idx: number }) {
  const m = () => props.m;
  let fl!: HTMLDivElement, fr!: HTMLDivElement;
  onMount(() => { meterEls.set(props.m.id, [fl, fr]); hold.delete(props.m.id); });
  onCleanup(() => meterEls.delete(props.m.id));
  const audible = () => !m().muted && (m().id === 0 || !anyInsertSolo() || m().solo);
  const route = () => { if (m().id === 0) return 'OUT'; const here = S.project.tracks.filter(t => t.insert === m().id).map(t => t.name); return here.length ? here.join(', ') : '—'; };
  const panT = () => ({ kind: 'insertPan' as const, id: m().id }), volT = () => ({ kind: 'insertVol' as const, id: m().id });
  const shownPan = () => autoDisplay(panT()) ?? m().pan, shownVol = () => autoDisplay(volT()) ?? m().vol;
  const pan = () => { const t = (shownPan() + 1) / 2; return { dash: Math.abs(t - 0.5) * L30 + 'px ' + C30 + 'px', off: -Math.min(t, 0.5) * L30 + 'px', angle: -135 + t * 270 }; };
  const put = (patch: Partial<Insert>) => A.insertOp(m().id, patch)!;
  // stereo width: 0 = mono, 100 % = as is, 200 % = extra wide (arc grows from the 100 % centre)
  const wid = () => m().width ?? 1;
  const widArc = () => { const t = wid() / 2; return { dash: Math.abs(t - 0.5) * L30 + 'px ' + C30 + 'px', off: -Math.min(t, 0.5) * L30 + 'px', angle: -135 + t * 270 }; };
  const widText = (w: number) => (w <= 0.005 ? 'mono' : Math.round(w * 100) + '%');
  const widDown = (e: PointerEvent) => {
    if (e.button !== 0) return; e.preventDefault(); e.stopPropagation(); A.selectInsert(m().id);
    if (dblCheck('wid' + m().id)) return A.setInsert(m().id, { width: 1 }, tr('c.mix.width'));
    const g = new Gesture(tr('c.mix.width')); let v = wid(), ly = e.clientY;
    track(ev => { v = clamp(v + (ly - ev.clientY) / 70 * (ev.shiftKey ? 0.2 : 1) * 2, 0, 2); if (Math.abs(v - 1) < 0.03) v = 1; ly = ev.clientY; g.update([put({ width: Math.round(v * 100) / 100 })]); say(tr('c.mix.sayWidth', { v: widText(v) })); }, () => g.end(), 'ns-resize');
  };
  const widWheel = (e: WheelEvent) => {
    const d = e.deltaY || e.deltaX; if (!d) return; e.preventDefault(); e.stopPropagation();
    const v = clamp(Math.round((wid() + (d < 0 ? 1 : -1) * 0.05 * (e.shiftKey ? 0.2 : 1)) * 100) / 100, 0, 2);
    wheelEdit('wid' + m().id, tr('c.mix.width'), [put({ width: v })]); say(tName(m().name) + ' · ' + tr('c.mix.widthLc') + ' ' + widText(v));
  };
  const select = (e: PointerEvent) => {
    if (e.button === 2) { e.preventDefault(); return openStripMenu(e.clientX, e.clientY, m().id); }
    A.selectInsert(m().id);
  };
  const panDown = (e: PointerEvent) => {
    if (e.button === 2) { e.preventDefault(); e.stopPropagation(); return paramMenu(e.clientX, e.clientY, panT(), () => A.setInsert(m().id, { pan: 0 }, tr('c.mix.pan')), m().color); }
    if (e.button !== 0) return; e.preventDefault(); e.stopPropagation(); A.selectInsert(m().id);
    if (dblCheck('pan' + m().id)) return A.setInsert(m().id, { pan: 0 }, tr('c.mix.pan'));
    const g = new Gesture(tr('c.mix.pan')); let v = m().pan, ly = e.clientY;
    track(ev => { v = clamp(v + (ly - ev.clientY) / 70 * (ev.shiftKey ? 0.2 : 1), -1, 1); ly = ev.clientY; g.update([put({ pan: v })]); say('Pan ' + panText(v)); }, () => g.end(), 'ns-resize');
  };
  const faderDown = (e: PointerEvent) => {
    if (e.button === 2) { e.preventDefault(); e.stopPropagation(); return paramMenu(e.clientX, e.clientY, volT(), () => A.setInsert(m().id, { vol: VOL_UNITY }, tr('c.mix.volume')), m().color); }
    if (e.button !== 0) return; e.preventDefault(); e.stopPropagation(); A.selectInsert(m().id);
    if (dblCheck('vol' + m().id)) return A.setInsert(m().id, { vol: VOL_UNITY }, tr('c.mix.volume'));
    const h = Math.max(40, (e.currentTarget as HTMLElement).getBoundingClientRect().height - 14);
    const g = new Gesture(tr('c.mix.volume')); let v = m().vol, ly = e.clientY;
    track(ev => { v = clamp(v + (ly - ev.clientY) / h * (ev.shiftKey ? 0.2 : 1), 0, 1); ly = ev.clientY; g.update([put({ vol: v })]); say(tr('c.mix.volume') + ' ' + volText(v)); }, () => g.end(), 'ns-resize');
  };
  const wheel = (k: 'pan' | 'vol') => (e: WheelEvent) => {
    const d = e.deltaY || e.deltaX; if (!d) return;
    e.preventDefault(); e.stopPropagation();
    const n = d < 0 ? 1 : -1, f = e.shiftKey ? 0.2 : 1;
    if (k === 'pan') { const v = clamp(m().pan + n * 0.04 * f, -1, 1); wheelEdit('pan' + m().id, tr('c.mix.pan'), [put({ pan: v })]); say(tName(m().name) + ' · pan ' + panText(v)); }
    else { const v = clamp(m().vol + n * 0.015 * f, 0, 1); wheelEdit('vol' + m().id, tr('c.mix.volume'), [put({ vol: v })]); say(tName(m().name) + ' · ' + volText(v)); }
  };
  const stop = (e: Event) => e.stopPropagation();
  const icon = () => { if (m().id === 0) return null; const t = S.project.tracks.find(x => x.insert === m().id); return guessIcon(m().name) ?? (t ? trackIcon(t) : null); };
  return (
    <div class="strip" data-ins={m().id} classList={{ on: U.mixSel === m().id, dimmed: !audible() }} onPointerDown={select}>
      <div class="bar" style={{ background: m().color }} />
      <div class="row" style={{ 'justify-content': 'space-between', gap: '4px' }}>
        <span class="lbl">{m().id === 0 ? 'MASTER' : 'INS ' + props.idx}</span>
        <Show when={m().fx.length}>
          <button class="fxn" classList={{ off: m().fx.every(f => f.bypass) }} onPointerDown={stop} onClick={() => A.toggleInsertFx(m().id)}
            title={tr('c.mix.fxToggleTip')}>{m().fx.length + ' FX'}</button>
        </Show>
      </div>
      <span class="nm" style={{ color: audible() ? 'var(--fg-hi)' : 'var(--fg4)' }} title={tr('c.mix.nameTip')}
        onDblClick={e => openRename(e.clientX, e.clientY, m().name, v => A.setInsert(m().id, { name: v }, tr('c.mix.undoName')))}>
        <Show when={icon()}>{id => <span class="micon" style={{ color: m().color }}><InstIcon id={id()} size={11} /></span>}</Show>{tName(m().name)}</span>
      <div class="knobrow">
      <div style={{ display: 'flex', 'flex-direction': 'column', 'align-items': 'center', gap: '2px' }}>
        <div class="pandial" classList={{ auto: isAutomated(panT()) }} onPointerDown={panDown} onWheel={wheel('pan')} onContextMenu={e => e.preventDefault()} title={tr('c.mix.panTip')}>
          <svg width="30" height="30" viewBox="0 0 30 30" style={{ position: 'absolute', inset: 0 }}>
            <circle cx="15" cy="15" r="11" fill="none" transform="rotate(135 15 15)" style={{ stroke: 'var(--knob-track)', 'stroke-width': '2.5px', 'stroke-linecap': 'round', 'stroke-dasharray': '51.84px 69.12px' }} />
            <circle cx="15" cy="15" r="11" fill="none" transform="rotate(135 15 15)" style={{ stroke: 'var(--blue)', 'stroke-width': '2.5px', 'stroke-linecap': 'round', 'stroke-dasharray': pan().dash, 'stroke-dashoffset': pan().off }} />
          </svg>
          <div class="pcap" />
          <div class="pptr" style={{ transform: `rotate(${pan().angle}deg)` }} />
        </div>
        <span class="pantxt">{panText(shownPan())}</span>
      </div>
      <div style={{ display: 'flex', 'flex-direction': 'column', 'align-items': 'center', gap: '2px' }}>
        <div class="pandial wdial" onPointerDown={widDown} onWheel={widWheel} onContextMenu={e => e.preventDefault()} title={tr('c.mix.widthTip')}>
          <svg width="30" height="30" viewBox="0 0 30 30" style={{ position: 'absolute', inset: 0 }}>
            <circle cx="15" cy="15" r="11" fill="none" transform="rotate(135 15 15)" style={{ stroke: 'var(--knob-track)', 'stroke-width': '2.5px', 'stroke-linecap': 'round', 'stroke-dasharray': '51.84px 69.12px' }} />
            <circle cx="15" cy="15" r="11" fill="none" transform="rotate(135 15 15)" style={{ stroke: 'var(--green)', 'stroke-width': '2.5px', 'stroke-linecap': 'round', 'stroke-dasharray': widArc().dash, 'stroke-dashoffset': widArc().off }} />
          </svg>
          <div class="pcap" />
          <div class="pptr" style={{ transform: `rotate(${widArc().angle}deg)` }} />
        </div>
        <span class="pantxt">{widText(wid())}</span>
      </div>
      </div>
      <div class="row" style={{ gap: '2px', 'justify-content': 'center' }}>
        <button class="mini m mute" classList={{ on: m().muted }} title={tr('c.mix.muteTip')} onPointerDown={stop} onClick={() => A.toggleInsertMute(m().id)}>M</button>
        <Show when={m().id !== 0}>
          <button class="mini s mute" classList={{ on: m().solo }} title={tr('c.mix.soloTip')} onPointerDown={stop} onClick={e => A.toggleInsertSolo(m().id, e.ctrlKey || e.metaKey)}>S</button>
        </Show>
      </div>
      <div style={{ flex: 1, 'min-height': '50px', display: 'flex', 'justify-content': 'center', gap: '3px' }}>
        <div class="meter"><div ref={fl} /></div>
        <div class="meter"><div ref={fr} /></div>
        <div class="fader" classList={{ auto: isAutomated(volT()) }} onPointerDown={faderDown} onWheel={wheel('vol')} onContextMenu={e => e.preventDefault()} title={tr('c.mix.faderTip')}>
          <div class="fslot" />
          <div class="unity" style={{ top: 'calc(0.2063 * (100% - 14px) + 7px)' }} />
          <div class="cap" style={{ top: `calc(${1 - shownVol()} * (100% - 14px))` }}><div /></div>
        </div>
      </div>
      <span class="db">{volText(shownVol())}</span>
      <span class="route" title={route()}>{route()}</span>
    </div>
  );
}

function FxSlot(props: { f: Fx; n: number; insId: Id }) {
  const f = () => props.f;
  const info = () => presetInfo(S.catalog, f(), S.project.bpm);
  const mixT = () => ({ kind: 'mix' as const, fxId: f().id });
  const mixV = () => autoDisplay(mixT()) ?? f().mix ?? 1;
  let mg: Gesture | null = null;
  const mixOp = (v: number) => A.fxOp({ ...JSON.parse(JSON.stringify(f())), mix: Math.round(clamp(v, 0, 1) * 1000) / 1000 }, props.insId);
  const down = (e: PointerEvent) => {
    if (e.button !== 0 || (e.target as HTMLElement).closest('button,.miniknob')) return;
    e.preventDefault();
    const sy = e.clientY, rowH = 34, ins0 = S.project.inserts.find(m => m.id === props.insId)!;
    const i0 = ins0.fx.findIndex(x => x.id === f().id), fxId = f().id;
    let g: Gesture | null = null;
    setU('fxSel', fxId);
    track(ev => {
      const dy = ev.clientY - sy;
      if (!g) { if (Math.abs(dy) < 4) return; g = new Gesture(tr('c.mix.fxOrder')); setU('fxDrag', fxId); document.body.style.cursor = 'grabbing'; }
      const ins = S.project.inserts.find(m => m.id === props.insId); if (!ins) return;
      const cur = ins.fx.findIndex(x => x.id === fxId), to = clamp(i0 + Math.round(dy / rowH), 0, ins.fx.length - 1);
      if (cur >= 0 && to !== cur) g.update([{ op: 'moveFx', fxId, to }]);
    }, () => {
      if (g) { g.end(); setU('fxDrag', null); say(tr('c.mix.fxOrderDone')); }
      else A.openPlug(fxId);
    });
  };
  return (
    <div class="slot" classList={{ on: U.fxSel === f().id, drag: U.fxDrag === f().id }} onPointerDown={down} title={tr('c.mix.slotTip')}>
      <span class="n">{props.n}</span>
      <FxTile type={f().type} size={22} dim={f().bypass} />
      <div style={{ flex: 1, 'min-width': 0, display: 'flex', 'flex-direction': 'column', gap: '1px' }}>
        <span class="ell" style={{ font: '500 12px var(--sans)', color: f().bypass ? 'var(--fg4)' : 'var(--fg-hi)' }}>{A.fxName(f())}</span>
        <span class="ell mono" style={{ 'font-size': '9px', color: 'var(--fg4)' }}>{f().type === 'vst3' ? (f().plugin?.vendor || 'VST3') : tName(info().name) + (info().mod ? ' *' : '')}</span>
      </div>
      <Show when={f().type !== 'vst3'}>
        <div class="slotmix" title={'Mix ' + Math.round(mixV() * 100) + '%'}>
          <MiniKnob key={'smix' + f().id} title={tr('c.mix.slotMixTip', { v: Math.round(mixV() * 100) })} t={mixV} automated={() => isAutomated(mixT())}
            onStart={() => { mg = new Gesture('Mix'); }} onChange={t => { mg?.update([mixOp(t)]); say(A.fxName(f()) + ' · Mix ' + Math.round(t * 100) + '%'); }}
            onEnd={() => { mg?.end(); mg = null; }} onReset={() => commit([mixOp(1)], 'Mix')} onWheel={t => wheelEdit('smix' + f().id, 'Mix', [mixOp(t)])}
            onMenu={(x, y) => paramMenu(x, y, mixT(), () => commit([mixOp(1)], 'Mix'), FXMETA[f().type].color)} />
          <span class="mono">{Math.round(mixV() * 100)}</span>
        </div>
      </Show>
      <button class="ledbtn" title={f().bypass ? tr('c.fx.bypassOn') : tr('c.fx.bypassOff')} onPointerDown={e => { e.stopPropagation(); A.toggleBypass(f().id); }}>
        <span class="led" style={{ background: f().bypass ? 'var(--line2)' : 'var(--green)' }} />
      </button>
      <button class="ledbtn xdel" title={tr('c.mix.removeFx')} onPointerDown={e => { e.stopPropagation(); A.removeFx(f().id); }}>×</button>
    </div>
  );
}

/** Strip width (`.strip`, border-box) and how many strips past each edge stay built. */
const STRIP_W = 80, STRIP_MARGIN = 4;

/**
 * The strip row. Only strips in (or near) the visible part are built; the others are empty
 * placeholders of the same size. Out of view they look the same, and the browser no longer
 * lays every strip of a 60-insert mixer into the compositor on each frame of playback.
 */
function Strips() {
  let el!: HTMLDivElement;
  const [view, setView] = createSignal({ x: 0, w: window.innerWidth });
  const sync = () => { if (el.scrollLeft !== view().x || el.clientWidth !== view().w) setView({ x: el.scrollLeft, w: el.clientWidth }); };
  onMount(() => {
    sync();
    el.addEventListener('scroll', sync, { passive: true });
    const ro = new ResizeObserver(sync); ro.observe(el);
    onCleanup(() => { el.removeEventListener('scroll', sync); ro.disconnect(); });
  });
  const near = (i: number) => { const v = view(), m = STRIP_MARGIN * STRIP_W; return (i + 1) * STRIP_W > v.x - m && i * STRIP_W < v.x + v.w + m; };
  return (
    <div ref={el} class="strips">
      <For each={S.project.inserts}>{(m, i) => <Show when={near(i())} fallback={<div class="strip" />}><Strip m={m} idx={i()} /></Show>}</For>
      <button class="addIns" onClick={A.addInsert} title={tr('c.mix.addInsert')}>+</button>
    </div>
  );
}

export function Mixer() {
  onMount(() => { const stop = addFrameTask(meterTask); onCleanup(stop); });
  const selIns = () => S.project.inserts.find(m => m.id === U.mixSel) ?? S.project.inserts[0];
  // One clamp for both the drag and the display: the old pair (drag up to innerHeight-200,
  // show at most 40 %) let the stored height run past what was drawn, so the splitter
  // stalled, then jumped, and the playlist above it jerked.
  const [winH, setWinH] = createSignal(window.innerHeight);
  onMount(() => { const r = () => setWinH(window.innerHeight); window.addEventListener('resize', r); onCleanup(() => window.removeEventListener('resize', r)); });
  const fitH = (h: number) => clamp(h, 170, Math.max(170, winH() - 200));
  const mixH = () => Math.round(fitH(U.mixH));
  const split = (e: PointerEvent) => {
    if (e.button !== 0) return; e.preventDefault();
    const sy = e.clientY, h0 = mixH();
    track(ev => setU('mixH', Math.round(fitH(h0 - (ev.clientY - sy)))), null, 'row-resize');
  };
  const hideWins = (e: PointerEvent) => { if (e.button === 2) A.playlistFront(); };
  return (
    <>
      <Show when={!U.mixMin}><div class="mixsplit" onPointerDown={split} title={tr('c.mix.heightTip')} /></Show>
      {/* FL: a right-click on the mixer sends the floating windows behind (like a click on the playlist) */}
      <div class="mixbar" on:pointerdown={{ capture: true, handleEvent: hideWins }}>
        <span class="t">MIXER</span>
        <span class="mono" style={{ 'font-size': '10px', color: 'var(--fg5)' }}>{tr('c.mix.insertCount', { n: S.project.inserts.length - 1 })}</span>
        <div style={{ flex: 1 }} />
        <button onClick={() => setU('mixMin', !U.mixMin)} title={U.mixMin ? tr('c.mix.expand') : tr('c.mix.collapse')}>
          <Icon d={U.mixMin ? 'M6 15l6-6 6 6' : 'M6 9l6 6 6-6'} size={12} sw={2.2} />
        </button>
        <button onClick={() => setU('mixOpen', false)} title={tr('c.mix.close')}>×</button>
      </div>
      <Show when={!U.mixMin}>
        <div class="mixer" style={{ height: mixH() + 'px' }} on:pointerdown={{ capture: true, handleEvent: hideWins }}>
          <Strips />
          <Show when={selIns()}>
            {ins => (
              <div class="fxchain" data-ins={ins().id}>
                <div class="hd">
                  <span style={{ width: '8px', height: '8px', 'border-radius': '2px', background: ins().color }} />
                  <span class="ell" style={{ flex: 1, font: '600 11px var(--sans)', color: 'var(--fg-hi)' }}>{insName(ins())}</span>
                  <span class="mono" style={{ 'font-size': '10px', color: 'var(--fg5)' }}>{ins().id === 0 ? tr('c.mix.toOutput') : '→ Master'}</span>
                </div>
                <div style={{ flex: 1, 'min-height': 0, overflow: 'auto' }}>
                  <For each={ins().fx}>{(f, i) => <FxSlot f={f} n={i() + 1} insId={ins().id} />}</For>
                  <div class="slot" title={tr('c.mix.addFxTip')} onPointerDown={e => { if (e.button === 0) openFxMenu(e.clientX, e.clientY, ins().id); }}>
                    <span class="n">{ins().fx.length + 1}</span>
                    <span class="tile addtile"><Icon d={ICONS.plus} /></span>
                    <span style={{ flex: 1, font: '500 12px var(--sans)', color: 'var(--blue)' }}>{tr('c.mix.addFxLabel')}</span>
                  </div>
                </div>
              </div>
            )}
          </Show>
        </div>
      </Show>
    </>
  );
}
