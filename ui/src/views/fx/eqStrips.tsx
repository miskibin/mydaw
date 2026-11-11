// EQ band bank (FL Parametric EQ 2 style), beside the graph: per band a shape button, a slim gain
// fader and two small knobs (FREQ, BW). No numbers on the controls: values show in the tooltip and
// the status bar while you move them, and on the graph. Any edit on a switched-off band switches it
// back on, like dragging its handle on the graph.
import { createEffect, Index } from 'solid-js';
import type { BandType, EqBand, Fx, Id } from '../../api/types';
import { BTYPE_LABEL, defaultFreq, defaultQ, f2t, hasGain, q2t, SHAPE_STEPS, shapePatch, shapeStep, t2f, t2q } from '../../lib/eq';
import { clamp, dbText, hzText } from '../../lib/format';
import { t as tr } from '../../lib/i18n';
import { autoDisplay, isAutomated, paramMenu } from '../../state/automation';
import { commit, Gesture, wheelEdit } from '../../state/edit';
import { say, setU, U } from '../../state/ui';
import { dblCheck, track } from '../../ui/drag';
import { MiniKnob } from '../../ui/Knob';
import { bandColor } from './common';
import { eqRange } from './eq';
import * as A from '../../state/actions';
import { findFx, plain } from '../../state/store';
import { bandOp, openBandMenu } from './eqEdit';

/** Shape icons, 14×10 viewBox. */
export const SHAPE_PATH: Record<BandType, string> = {
  peak: 'M1 8 L4 8 Q7 -1 10 8 L13 8',
  lowshelf: 'M1 3 L4 3 C7 3 7 8 10 8 L13 8',
  highshelf: 'M1 8 L4 8 C7 8 7 3 10 3 L13 3',
  lowcut: 'M2 10 C3 4 5 3 7 3 L13 3',
  highcut: 'M1 3 L7 3 C9 3 11 4 12 10',
  notch: 'M1 3 L5 3 L7 9 L9 3 L13 3',
  bandpass: 'M1 9 C5 9 5 2 7 2 C9 2 9 9 13 9',
  tilt: 'M1 8 L13 2',
};

function edit(fxId: Id, i: number, patch: Partial<EqBand>) { return bandOp(fxId, i, { ...patch, on: true }); }

function BandStrip(props: { fx: Fx; i: number; band: () => EqBand }) {
  const fxId = () => props.fx.id, i = () => props.i, b = () => props.band();
  const col = () => bandColor(i());
  const sel = () => U.eqSel[fxId()] === i();
  const select = () => setU('eqSel', fxId(), i());
  const name = () => tr('c.eq.bandN', { n: i() + 1 });
  const shapeName = () => b().on ? BTYPE_LABEL[b().type] : tr('c.eq.off');
  const target = (k: 'f' | 'g' | 'q') => ({ kind: 'fx' as const, fxId: fxId(), param: 'band:' + i() + ':' + k });

  // ---- shape: click = band menu, drag ↕ = step through Off and the shapes (one undo step)
  const shapeDown = (e: PointerEvent) => {
    e.stopPropagation();
    select();
    if (e.button === 2) { e.preventDefault(); openBandMenu(e.clientX, e.clientY, fxId(), i()); return; }
    if (e.button !== 0) return;
    e.preventDefault();
    const el = e.currentTarget as HTMLElement, b0 = { ...b() }, s0 = shapeStep(b0), sy = e.clientY;
    let g: Gesture | null = null, cur = s0;
    track(ev => {
      const s = clamp(s0 + Math.round((sy - ev.clientY) / 14), 0, SHAPE_STEPS.length - 1);
      if (s === cur) return;
      cur = s;
      g ??= new Gesture(tr('c.eq.bandShape'));
      const st = SHAPE_STEPS[s];
      const op = bandOp(fxId(), i(), st === 'off' ? { ...b0, on: false } : { ...b0, ...shapePatch(b0, st) });
      if (op) g.update([op]);
      say(name() + ' · ' + (st === 'off' ? tr('c.eq.off') : BTYPE_LABEL[st]));
    }, () => {
      if (g) g.end();
      else { const r = el.getBoundingClientRect(); openBandMenu(r.left, r.bottom + 4, fxId(), i()); }
    }, 'ns-resize');
  };

  // ---- gain fader
  let slot!: HTMLDivElement;
  const range = () => eqRange(props.fx.bands ?? []);
  const gainOk = () => hasGain(b().type);
  const capY = () => 1 - (clamp(b().g, -range(), range()) + range()) / (2 * range());
  const resetGain = () => { const op = edit(fxId(), i(), { g: 0 }); if (op) commit([op], tr('c.eq.gain')); };
  const faderDown = (e: PointerEvent) => {
    e.stopPropagation();
    select();
    if (!gainOk()) return;
    if (e.button === 2) { e.preventDefault(); paramMenu(e.clientX, e.clientY, target('g'), resetGain, col()); return; }
    if (e.button !== 0) return;
    e.preventDefault();
    if (dblCheck('eqfader' + fxId() + i())) { resetGain(); return; }
    const h = Math.max(40, slot.getBoundingClientRect().height), r0 = range();
    let v = b().g, ly = e.clientY;
    const g = new Gesture(tr('c.eq.gain'));
    track(ev => {
      v = clamp(v + (ly - ev.clientY) / h * 2 * r0 * (ev.shiftKey ? 0.2 : 1), -30, 30); ly = ev.clientY;
      const op = edit(fxId(), i(), { g: Math.round(v * 10) / 10 }); if (op) g.update([op]);
      say(name() + ' · ' + dbText(v));
    }, () => g.end(), 'ns-resize');
  };
  const faderWheel = (e: WheelEvent) => {
    if (!gainOk()) return;
    const d = e.deltaY || e.deltaX; if (!d) return;
    e.preventDefault(); e.stopPropagation();
    const v = clamp(Math.round((b().g + (d < 0 ? 1 : -1) * (e.shiftKey ? 0.1 : 0.5)) * 10) / 10, -30, 30);
    const op = edit(fxId(), i(), { g: v }); if (op) wheelEdit('eqg' + fxId() + i(), tr('c.eq.gain'), [op]);
    say(name() + ' · ' + dbText(v));
  };

  // ---- FREQ / BW knobs
  const knob = (k: 'f' | 'q') => {
    let g: Gesture | null = null;
    const label = k === 'f' ? 'FREQ' : 'BW';
    const val = (t: number) => k === 'f' ? Math.round(t2f(t)) : Math.round(t2q(t) * 100) / 100;
    const txt = (v: number) => k === 'f' ? hzText(v) : 'Q ' + v.toFixed(v < 10 ? 2 : 1);
    const show = (v: number) => say(name() + ' · ' + txt(v));
    const reset = () => { const op = edit(fxId(), i(), { [k]: k === 'f' ? defaultFreq(i()) : defaultQ(b().type) }); if (op) commit([op], label); };
    const disabled = () => k === 'q' && b().type === 'tilt';
    return (
      <MiniKnob key={'eqs' + k + fxId() + i()} color={b().on && !disabled() ? col() : 'var(--fg6)'}
        t={() => disabled() ? 0 : k === 'f' ? f2t(b().f) : q2t(b().q)}
        title={name() + ' · ' + label + ' ' + (disabled() ? '—' : txt(k === 'f' ? b().f : b().q)) + ' — ' + (k === 'f' ? tr('c.eq.freqTip') : disabled() ? tr('c.eq.bwTilt') : tr('c.eq.bwTip'))}
        automated={() => isAutomated(target(k))}
        onStart={() => { select(); if (!disabled()) g = new Gesture(label); }}
        onChange={t => { if (disabled()) return; const v = val(t), op = edit(fxId(), i(), { [k]: v }); if (op) g?.update([op]); show(v); }}
        onEnd={() => { g?.end(); g = null; }}
        onReset={() => { if (!disabled()) reset(); }}
        onWheel={t => { if (disabled()) return; const v = val(t), op = edit(fxId(), i(), { [k]: v }); if (op) wheelEdit('eqs' + k + fxId() + i(), label, [op]); show(v); }}
        onMenu={(x, y) => { select(); if (!disabled()) paramMenu(x, y, target(k), reset, col()); }} />
    );
  };

  return (
    <div class="eqstrip" classList={{ picked: sel(), off: !b().on }} style={{ '--c': col() }} data-band={i()} onPointerDown={select}>
      <button class="eqshape" title={tr('c.eq.shapeTip', { n: i() + 1, type: shapeName() })} onPointerDown={shapeDown} onContextMenu={e => e.preventDefault()}>
        <svg width="14" height="10" viewBox="0 0 14 10" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">
          <path d={b().on ? SHAPE_PATH[b().type] : 'M3 5 L11 5'} />
        </svg>
      </button>
      <div class="eqfader" classList={{ dis: !gainOk(), auto: isAutomated(target('g')) }} onPointerDown={faderDown} onWheel={faderWheel} onContextMenu={e => e.preventDefault()}
        title={gainOk() ? name() + ' · ' + dbText(b().g) + ' — ' + tr('c.eq.gainTip') : tr('c.eq.noGain', { type: BTYPE_LABEL[b().type] })}>
        <div class="fslot" ref={slot}>
          {gainOk() && b().on && <div class="fill" style={{ top: Math.min(50, capY() * 100) + '%', bottom: Math.min(50, (1 - capY()) * 100) + '%' }} />}
        </div>
        <div class="fcap" style={{ top: `calc(${(gainOk() ? capY() : 0.5) * 100}% - 4px)` }} />
      </div>
      {knob('f')}
      {knob('q')}
    </div>
  );
}

/** The EQ's output level (`out`, ±18 dB): FL's main level slider, grey, left of the bands. */
const OUT_MAX = 18;
function OutStrip(props: { fx: Fx }) {
  let slot!: HTMLDivElement;
  const target = () => ({ kind: 'fx' as const, fxId: props.fx.id, param: 'out' });
  const val = () => autoDisplay(target()) ?? props.fx.params.out ?? 0;
  const capY = () => 1 - (clamp(val(), -OUT_MAX, OUT_MAX) + OUT_MAX) / (2 * OUT_MAX);
  const label = () => tr('c.eq.output');
  const op = (v: number) => { const f = findFx(props.fx.id)?.f; return f ? A.fxOp({ ...plain(f), params: { ...plain(f.params), out: v } }) : null; };
  const reset = () => { const o = op(0); if (o) commit([o], label()); };
  const down = (e: PointerEvent) => {
    e.stopPropagation();
    if (e.button === 2) { e.preventDefault(); paramMenu(e.clientX, e.clientY, target(), reset); return; }
    if (e.button !== 0) return;
    e.preventDefault();
    if (dblCheck('eqout' + props.fx.id)) { reset(); return; }
    const h = Math.max(40, slot.getBoundingClientRect().height);
    let v = val(), ly = e.clientY;
    const g = new Gesture(label());
    track(ev => {
      v = clamp(v + (ly - ev.clientY) / h * 2 * OUT_MAX * (ev.shiftKey ? 0.2 : 1), -OUT_MAX, OUT_MAX); ly = ev.clientY;
      const o = op(Math.round(v * 10) / 10); if (o) g.update([o]);
      say(label() + ' ' + dbText(v));
    }, () => g.end(), 'ns-resize');
  };
  const wheel = (e: WheelEvent) => {
    const d = e.deltaY || e.deltaX; if (!d) return;
    e.preventDefault(); e.stopPropagation();
    const v = clamp(Math.round((val() + (d < 0 ? 1 : -1) * (e.shiftKey ? 0.1 : 0.5)) * 10) / 10, -OUT_MAX, OUT_MAX);
    const o = op(v); if (o) wheelEdit('eqout' + props.fx.id, label(), [o]);
    say(label() + ' ' + dbText(v));
  };
  return (
    <div class="eqstrip eqout" style={{ '--c': 'var(--fg4)' }}>
      <span class="outlab">OUT</span>
      <div class="eqfader" classList={{ auto: isAutomated(target()) }} onPointerDown={down} onWheel={wheel} onContextMenu={e => e.preventDefault()}
        title={label() + ' ' + dbText(val()) + ' — ' + tr('c.eq.gainTip')}>
        <div class="fslot" ref={slot} />
        <div class="fcap" style={{ top: `calc(${capY() * 100}% - 4px)` }} />
      </div>
      <span class="outsp" />
    </div>
  );
}

/** The bank: output level, one strip per band, then FREQ / BW row labels like FL. */
export function EqStrips(props: { fx: Fx }) {
  let row!: HTMLDivElement;
  // keep the selected band's strip in view (only matters past ~7 bands)
  createEffect(() => {
    const i = U.eqSel[props.fx.id];
    if (i == null) return;
    (row?.querySelector(`[data-band="${i}"]`) as HTMLElement | null)?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  });
  return (
    <div class="eqbank">
      <OutStrip fx={props.fx} />
      <div class="eqstriprow" ref={row}>
        <Index each={props.fx.bands ?? []}>{(bd, i) => <BandStrip fx={props.fx} i={i} band={bd} />}</Index>
      </div>
      <div class="eqrowlab"><span>FREQ</span><span>BW</span></div>
    </div>
  );
}
