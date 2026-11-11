// EQ editing: canvas gestures (drag handles, wheel = Q, double-click = new bell, drag from a
// spectral peak = new bell there, right-click = band menu), and the compact panel for the
// selected band (shape, freq, gain, Q, slope, placement, dynamic). The per-band strips live in eqStrips.tsx.
import { Show } from 'solid-js';
import { MAX_BANDS, type BandType, type EqBand, type Fx, type Id, type Op, type Placement } from '../../api/types';
import { BTYPE_LABEL, BTYPE_ORDER, canBeDynamic, fx2x, handleShown, shapePatch, handleDb, hasGain, isCut, newBand, placeOf, slopeOf, x2f } from '../../lib/eq';
import { clamp, dbText, hzText } from '../../lib/format';
import { t as tr } from '../../lib/i18n';
import { hzToMidi, noteName } from '../../lib/pitch';
import * as A from '../../state/actions';
import { commit, Gesture, wheelEdit } from '../../state/edit';
import { findFx, plain } from '../../state/store';
import { openMenu, say, setU, U, type MenuItem } from '../../state/ui';
import { dblCheck, track } from '../../ui/drag';
import { NumField, parseNum } from '../../ui/Knob';
import { bandColor } from './common';
import { dbY, eqRange, EqViz, PLACE_SHORT, yDb } from './eq';

export { BTYPE_LABEL, BTYPE_ORDER };
const BTYPE_SHORT: Record<BandType, string> = { peak: 'Bell', lowshelf: 'Lo shelf', highshelf: 'Hi shelf', lowcut: 'Lo cut', highcut: 'Hi cut', notch: 'Notch', bandpass: 'BPass', tilt: 'Tilt' };
const PLACES: Placement[] = ['stereo', 'left', 'right', 'mid', 'side'];
const SLOPES = [12, 24, 36, 48];

// ---- ops ----
function withBands(fxId: Id, f: (bands: EqBand[]) => void): Op | null {
  const ff = findFx(fxId); if (!ff || !ff.f.bands) return null;
  const fx = plain(ff.f) as Fx;
  f(fx.bands!);
  return A.fxOp(fx, ff.m.id);
}
export const bandOp = (fxId: Id, i: number, patch: Partial<EqBand>) => withBands(fxId, b => { if (b[i]) b[i] = { ...b[i], ...patch }; });
export function updBand(fxId: Id, i: number, patch: Partial<EqBand>, label = 'Band EQ') { const op = bandOp(fxId, i, patch); if (op) commit([op], label); }
export function addBand(fxId: Id, band: EqBand): number {
  const ff = findFx(fxId); if (!ff?.f.bands) return -1;
  if (ff.f.bands.length >= MAX_BANDS) { say(tr('c.eq.maxBands', { n: MAX_BANDS })); return -1; }
  const i = ff.f.bands.length;
  const op = withBands(fxId, b => { b.push(band); });
  if (op) { commit([op], tr('c.eq.newBand')); setU('eqSel', fxId, i); }
  return i;
}
export function deleteBand(fxId: Id, i: number) {
  const op = withBands(fxId, b => { b.splice(i, 1); });
  if (!op) return;
  commit([op], tr('c.eq.deleteBand'));
  const sel = U.eqSel[fxId];
  setU('eqSel', fxId, sel == null || sel < i ? sel : sel === i ? (undefined as unknown as number) : sel - 1);
  say(tr('c.eq.bandDeleted', { n: i + 1 }));
}
/** Changing the shape keeps the band coherent: gain only where it means something, sensible Q. */
export function setShape(fxId: Id, i: number, type: BandType) {
  const b = findFx(fxId)?.f.bands?.[i]; if (!b) return;
  updBand(fxId, i, shapePatch(b, type), tr('c.eq.bandShape'));
}

export function openBandMenu(x: number, y: number, fxId: Id, i: number) {
  const b = findFx(fxId)?.f.bands?.[i]; if (!b) return;
  const items: MenuItem[] = [
    { t: 'title', label: 'Band ' + (i + 1) + ' · ' + hzText(b.f) },
    { t: 'chips', label: tr('c.eq.shape'), options: BTYPE_ORDER.slice(0, 4).map(t => BTYPE_SHORT[t]), active: BTYPE_ORDER.indexOf(b.type), pick: k => setShape(fxId, i, BTYPE_ORDER[k]) },
    { t: 'chips', options: BTYPE_ORDER.slice(4).map(t => BTYPE_SHORT[t]), active: BTYPE_ORDER.indexOf(b.type) - 4, pick: k => setShape(fxId, i, BTYPE_ORDER[k + 4]) },
    { t: 'chips', label: tr('c.eq.slope'), options: SLOPES.map(s => s + ''), active: SLOPES.indexOf(slopeOf(b)), disabled: !isCut(b.type), pick: k => updBand(fxId, i, { slope: SLOPES[k] }, tr('c.eq.slope')) },
    { t: 'chips', label: tr('c.eq.channel'), options: PLACES.map(p => PLACE_SHORT[p]), active: PLACES.indexOf(placeOf(b)), pick: k => updBand(fxId, i, { place: PLACES[k] }, tr('c.eq.bandChannel')) },
    { t: 'sep' },
    { t: 'item', label: b.dynamic ? tr('c.eq.dynOff') : tr('c.eq.dynOn'), disabled: !canBeDynamic(b.type), title: canBeDynamic(b.type) ? '' : tr('c.eq.dynOnly'),
      act: () => updBand(fxId, i, { dynamic: b.dynamic ? null : { range: b.g > 0 ? -b.g : -6, thr: -30 } }, tr('c.eq.dynBand')) },
    { t: 'item', label: b.on ? tr('c.eq.disableBand') : tr('c.eq.enableBand'), kbd: tr('c.eq.dblClick'), act: () => updBand(fxId, i, { on: !b.on }) },
    { t: 'item', label: tr('c.eq.deleteBand'), kbd: '', danger: true, act: () => deleteBand(fxId, i) },
  ];
  openMenu(x, y, items, 330);
}

function openAddMenu(x: number, y: number, fxId: Id, f: number, g: number) {
  openMenu(x, y, [{ t: 'title', label: tr('c.eq.newBandAt', { hz: hzText(f) }) },
    ...BTYPE_ORDER.map(t => ({ t: 'item' as const, label: BTYPE_LABEL[t], act: () => addBand(fxId, newBand(t, f, hasGain(t) ? g : 0)) }))], 280);
}

// ---- canvas gestures ----
const HIT = 11;
export function eqHit(bands: EqBand[], px: number, py: number, W: number, H: number, sel?: number | null): number {
  const range = eqRange(bands);
  let best = -1, bd = HIT;
  bands.forEach((b, i) => { if (!handleShown(b, i, sel)) return; const d = Math.hypot(px - fx2x(b.f, W), py - dbY(handleDb(b), H, range)); if (d <= bd) { bd = d; best = i; } });
  return best;
}

export function eqPointerDown(e: PointerEvent, cv: HTMLCanvasElement, fxId: Id, viz: EqViz) {
  const ff = findFx(fxId); if (!ff) return;
  const r = cv.getBoundingClientRect(), W = r.width, H = r.height, px = e.clientX - r.left, py = e.clientY - r.top;
  const bands = ff.f.bands ?? [], range = eqRange(bands);
  const hit = eqHit(bands, px, py, W, H, U.eqSel[fxId]);
  if (e.button === 2) {
    e.preventDefault();
    if (hit >= 0) { setU('eqSel', fxId, hit); openBandMenu(e.clientX, e.clientY, fxId, hit); }
    else openAddMenu(e.clientX, e.clientY, fxId, x2f(px, W), clamp(yDb(py, H, range), -30, 30));
    return;
  }
  if (e.button !== 0) return;
  e.preventDefault();
  if (hit >= 0) {
    setU('eqSel', fxId, hit);
    if (dblCheck('band' + fxId + hit)) { updBand(fxId, hit, { on: !bands[hit].on }); return; }
    const b0 = { ...bands[hit] }, sx = e.clientX, sy = e.clientY, x0 = fx2x(b0.f, W), y0 = dbY(handleDb(b0), H, range);
    const gain = hasGain(b0.type), mul = b0.type === 'tilt' ? 2 : 1;
    let g: Gesture | null = null;
    track(ev => {
      const k = ev.shiftKey ? 0.2 : 1;
      if (!g) { if (Math.hypot(ev.clientX - sx, ev.clientY - sy) < 2) return; g = new Gesture('Band EQ'); }
      const nf = clamp(x2f(x0 + (ev.clientX - sx) * k, W), 10, 22000);
      const ng = gain ? clamp(yDb(y0 + (ev.clientY - sy) * k, H, range) * mul, -30, 30) : b0.g;
      const op = bandOp(fxId, hit, { f: nf, g: Math.round(ng * 100) / 100, on: true }); if (op) g.update([op]);
      say('Band ' + (hit + 1) + ' · ' + hzText(nf) + (gain ? ' · ' + dbText(ng) : '') + ' · ' + tr('c.eq.dragHint'));
    }, () => g?.end(), 'move');
    return;
  }
  // empty area
  if (dblCheck('eqempty' + fxId)) {
    const f = clamp(x2f(px, W), 10, 22000), gdb = clamp(yDb(py, H, range), -30, 30);
    if (addBand(fxId, newBand('peak', f, Math.round(gdb * 10) / 10)) >= 0) say(tr('c.eq.newBandSay', { hz: hzText(f) }));
    return;
  }
  const pk = viz.peak;
  if (pk && bands.length < MAX_BANDS) {
    // grab the resonance under the cursor: a new bell at the peak, gain follows the drag
    const i = bands.length, sy = e.clientY, f = clamp(pk.f, 10, 22000);
    let g: Gesture | null = null;
    track(ev => {
      const d = ev.clientY - sy;
      if (!g) { if (Math.abs(d) < 3) return; g = new Gesture(tr('c.eq.newBand')); setU('eqSel', fxId, i); }
      const gdb = clamp(-d / ((H / 2) * 0.86) * range * (ev.shiftKey ? 0.2 : 1), -30, 30);
      const op = withBands(fxId, b => { if (b.length === i) b.push(newBand('peak', f, 0)); b[i] = { ...b[i], q: 4, g: Math.round(gdb * 10) / 10 }; });
      if (op) g.update([op]);
      say(tr('c.eq.onResonance', { n: i + 1, hz: hzText(f), note: noteName(hzToMidi(f)), db: dbText(gdb) }));
    }, () => { if (g) g.end(); else setU('eqSel', fxId, undefined as unknown as number); }, 'ns-resize');
    return;
  }
  setU('eqSel', fxId, undefined as unknown as number);
}

export function eqHover(e: PointerEvent, cv: HTMLCanvasElement, fxId: Id, viz: EqViz) {
  const ff = findFx(fxId); if (!ff) return;
  const r = cv.getBoundingClientRect(), px = e.clientX - r.left, py = e.clientY - r.top;
  const hit = eqHit(ff.f.bands ?? [], px, py, r.width, r.height, U.eqSel[fxId]);
  viz.hover = { x: px, y: py, band: hit >= 0 ? hit : null, inside: true };
  viz.hoverVersion++;
  cv.style.cursor = hit >= 0 ? 'move' : viz.peak ? 'ns-resize' : 'crosshair';
}
export function eqLeave(viz: EqViz) { viz.hover = null; viz.hoverVersion++; }

export function eqWheel(e: WheelEvent, fxId: Id, viz: EqViz) {
  const ff = findFx(fxId); if (!ff) return;
  const i = viz.hover?.band ?? U.eqSel[fxId]; if (i == null || !ff.f.bands?.[i]) return;
  const d = e.deltaY || e.deltaX; if (!d) return;
  e.preventDefault(); e.stopPropagation();
  const b = ff.f.bands[i], q = clamp(b.q * Math.pow(2, (d < 0 ? 1 : -1) * 0.15 * (e.shiftKey ? 0.2 : 1)), 0.1, 18);
  const op = bandOp(fxId, i, { q }); if (op) wheelEdit('eqq' + fxId, 'Q', [op]);
  say('Band ' + (i + 1) + ' · Q ' + q.toFixed(2));
}

// ---- panel ----
function dynField(fxId: Id, i: () => number, key: 'range' | 'thr') {
  let g: Gesture | null = null;
  return (v: number, mode: 'begin' | 'move' | 'end' | 'set' | 'wheel') => {
    const b = findFx(fxId)?.f.bands?.[i()]; if (!b?.dynamic) return;
    const op = bandOp(fxId, i(), { dynamic: { ...b.dynamic, [key]: v } });
    if (mode === 'begin') g = new Gesture(tr('c.eq.dynBand'));
    else if (mode === 'move') { if (op) g?.update([op]); }
    else if (mode === 'end') { g?.end(); g = null; }
    else if (mode === 'set') { if (op) commit([op], tr('c.eq.dynBand')); }
    else if (op) wheelEdit('eqd' + fxId + key, tr('c.eq.dynBand'), [op]);
  };
}

/** Below the graph only when the selected band is dynamic: its range and threshold (everything else
 * lives on the band bank, the graph and the band's right-click menu). */
export function EqPanel(props: { fx: Fx }) {
  const fxId = () => props.fx.id;
  const sel = () => { const i = U.eqSel[fxId()]; return i != null && props.fx.bands?.[i]?.dynamic ? i : null; };
  const si = () => sel() ?? 0;
  return (
    <Show when={sel() != null && props.fx.bands![si()]}>
      {band => (
        <div class="eqpanel bedit">
          <span class="bnum" style={{ color: bandColor(si()) }}>{si() + 1}</span>
          <span class="dyntag">{tr('c.eq.dynBand')}</span>
          <NumField label={tr('c.eq.range')} width={64} value={() => band().dynamic!.range} text={dbText} parse={parseNum}
            drag={(v, d) => clamp(Math.round((v + d * 0.05) * 10) / 10, -30, 30)} apply={dynField(fxId(), si, 'range')} />
          <NumField label={tr('c.eq.thresh')} width={64} value={() => band().dynamic!.thr} text={v => Math.round(v) + ' dB'} parse={parseNum}
            drag={(v, d) => clamp(Math.round(v + d * 0.1), -80, 0)} apply={dynField(fxId(), si, 'thr')} />
          <div style={{ flex: 1 }} />
          <button class="smallbtn" onClick={() => updBand(fxId(), si(), { dynamic: null }, tr('c.eq.dynBand'))}>{tr('c.eq.dynOff')}</button>
        </div>
      )}
    </Show>
  );
}
export { yDb };
