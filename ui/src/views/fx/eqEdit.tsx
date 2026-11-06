// EQ editing: canvas gestures (drag handles, wheel = Q, double-click = new bell, drag from a
// spectral peak = new bell there, right-click = band menu), and the compact panel for the
// selected band (shape, freq, gain, Q, slope, placement, dynamic).
import { For, Show } from 'solid-js';
import { MAX_BANDS, type BandType, type EqBand, type Fx, type Id, type Op, type Placement } from '../../api/types';
import { canBeDynamic, fx2x, handleDb, hasGain, isCut, newBand, placeOf, slopeOf, x2f } from '../../lib/eq';
import { clamp, dbText, hzText } from '../../lib/format';
import { hzToMidi, noteName } from '../../lib/pitch';
import * as A from '../../state/actions';
import { paramMenu } from '../../state/automation';
import { commit, Gesture, wheelEdit } from '../../state/edit';
import { findFx } from '../../state/store';
import { openMenu, say, setU, U, type MenuItem } from '../../state/ui';
import { dblCheck, track } from '../../ui/drag';
import { NumField, parseNum, Segmented } from '../../ui/Knob';
import { bandColor } from './common';
import { dbY, eqRange, EqViz, PLACE_SHORT, yDb } from './eq';

const plain = <T,>(v: T): T => JSON.parse(JSON.stringify(v));
export const BTYPE_ORDER: BandType[] = ['peak', 'lowshelf', 'highshelf', 'lowcut', 'highcut', 'notch', 'bandpass', 'tilt'];
export const BTYPE_LABEL: Record<BandType, string> = { peak: 'Bell', lowshelf: 'Low shelf', highshelf: 'High shelf', lowcut: 'Low cut', highcut: 'High cut', notch: 'Notch', bandpass: 'Band pass', tilt: 'Tilt' };
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
  if (ff.f.bands.length >= MAX_BANDS) { say('Maksymalnie ' + MAX_BANDS + ' bandów'); return -1; }
  const i = ff.f.bands.length;
  const op = withBands(fxId, b => { b.push(band); });
  if (op) { commit([op], 'Nowy band'); setU('eqSel', fxId, i); }
  return i;
}
export function deleteBand(fxId: Id, i: number) {
  const op = withBands(fxId, b => { b.splice(i, 1); });
  if (!op) return;
  commit([op], 'Usuń band');
  const sel = U.eqSel[fxId];
  setU('eqSel', fxId, sel == null || sel < i ? sel : sel === i ? (undefined as unknown as number) : sel - 1);
  say('Usunięto band ' + (i + 1));
}
/** Changing the shape keeps the band coherent: gain only where it means something, sensible Q. */
export function setShape(fxId: Id, i: number, type: BandType) {
  const b = findFx(fxId)?.f.bands?.[i]; if (!b) return;
  const q = type === 'notch' ? Math.max(b.q, 4) : isCut(type) && !isCut(b.type) ? 0.71 : b.q;
  updBand(fxId, i, { type, on: true, q, g: hasGain(type) ? b.g : 0, dynamic: canBeDynamic(type) ? b.dynamic ?? null : null }, 'Kształt bandu');
}

export function openBandMenu(x: number, y: number, fxId: Id, i: number) {
  const b = findFx(fxId)?.f.bands?.[i]; if (!b) return;
  const items: MenuItem[] = [
    { t: 'title', label: 'Band ' + (i + 1) + ' · ' + hzText(b.f) },
    { t: 'chips', label: 'Kształt', options: BTYPE_ORDER.slice(0, 4).map(t => BTYPE_SHORT[t]), active: BTYPE_ORDER.indexOf(b.type), pick: k => setShape(fxId, i, BTYPE_ORDER[k]) },
    { t: 'chips', options: BTYPE_ORDER.slice(4).map(t => BTYPE_SHORT[t]), active: BTYPE_ORDER.indexOf(b.type) - 4, pick: k => setShape(fxId, i, BTYPE_ORDER[k + 4]) },
    { t: 'chips', label: 'Nachylenie', options: SLOPES.map(s => s + ''), active: SLOPES.indexOf(slopeOf(b)), disabled: !isCut(b.type), pick: k => updBand(fxId, i, { slope: SLOPES[k] }, 'Nachylenie') },
    { t: 'chips', label: 'Kanał', options: PLACES.map(p => PLACE_SHORT[p]), active: PLACES.indexOf(placeOf(b)), pick: k => updBand(fxId, i, { place: PLACES[k] }, 'Kanał bandu') },
    { t: 'sep' },
    { t: 'item', label: b.dynamic ? 'Dynamiczny: wyłącz' : 'Dynamiczny: włącz', disabled: !canBeDynamic(b.type), title: canBeDynamic(b.type) ? '' : 'Tylko bell, shelf i tilt mogą być dynamiczne',
      act: () => updBand(fxId, i, { dynamic: b.dynamic ? null : { range: b.g > 0 ? -b.g : -6, thr: -30 } }, 'Dynamiczny band') },
    { t: 'item', label: b.on ? 'Wyłącz band' : 'Włącz band', kbd: '2× klik', act: () => updBand(fxId, i, { on: !b.on }) },
    { t: 'item', label: 'Usuń band', kbd: '', danger: true, act: () => deleteBand(fxId, i) },
  ];
  openMenu(x, y, items, 330);
}

function openAddMenu(x: number, y: number, fxId: Id, f: number, g: number) {
  openMenu(x, y, [{ t: 'title', label: 'Nowy band · ' + hzText(f) },
    ...BTYPE_ORDER.map(t => ({ t: 'item' as const, label: BTYPE_LABEL[t], act: () => addBand(fxId, newBand(t, f, hasGain(t) ? g : 0)) }))], 280);
}

// ---- canvas gestures ----
const HIT = 11;
export function eqHit(bands: EqBand[], px: number, py: number, W: number, H: number): number {
  const range = eqRange(bands);
  let best = -1, bd = HIT;
  bands.forEach((b, i) => { const d = Math.hypot(px - fx2x(b.f, W), py - dbY(handleDb(b), H, range)); if (d <= bd) { bd = d; best = i; } });
  return best;
}

export function eqPointerDown(e: PointerEvent, cv: HTMLCanvasElement, fxId: Id, viz: EqViz) {
  const ff = findFx(fxId); if (!ff) return;
  const r = cv.getBoundingClientRect(), W = r.width, H = r.height, px = e.clientX - r.left, py = e.clientY - r.top;
  const bands = ff.f.bands ?? [], range = eqRange(bands);
  const hit = eqHit(bands, px, py, W, H);
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
      say('Band ' + (hit + 1) + ' · ' + hzText(nf) + (gain ? ' · ' + dbText(ng) : '') + ' · kółko = Q · Shift = precyzyjnie');
    }, () => g?.end(), 'move');
    return;
  }
  // empty area
  if (dblCheck('eqempty' + fxId)) {
    const f = clamp(x2f(px, W), 10, 22000), gdb = clamp(yDb(py, H, range), -30, 30);
    if (addBand(fxId, newBand('peak', f, Math.round(gdb * 10) / 10)) >= 0) say('Nowy band · ' + hzText(f) + ' — przeciągnij, kółko = Q, PPM = kształt');
    return;
  }
  const pk = viz.peak;
  if (pk && bands.length < MAX_BANDS) {
    // grab the resonance under the cursor: a new bell at the peak, gain follows the drag
    const i = bands.length, sy = e.clientY, f = clamp(pk.f, 10, 22000);
    let g: Gesture | null = null;
    track(ev => {
      const d = ev.clientY - sy;
      if (!g) { if (Math.abs(d) < 3) return; g = new Gesture('Nowy band'); setU('eqSel', fxId, i); }
      const gdb = clamp(-d / ((H / 2) * 0.86) * range * (ev.shiftKey ? 0.2 : 1), -30, 30);
      const op = withBands(fxId, b => { if (b.length === i) b.push(newBand('peak', f, 0)); b[i] = { ...b[i], q: 4, g: Math.round(gdb * 10) / 10 }; });
      if (op) g.update([op]);
      say('Band ' + (i + 1) + ' na rezonansie ' + hzText(f) + ' (' + noteName(hzToMidi(f)) + ') · ' + dbText(gdb));
    }, () => { if (g) g.end(); else setU('eqSel', fxId, undefined as unknown as number); }, 'ns-resize');
    return;
  }
  setU('eqSel', fxId, undefined as unknown as number);
}

export function eqHover(e: PointerEvent, cv: HTMLCanvasElement, fxId: Id, viz: EqViz) {
  const ff = findFx(fxId); if (!ff) return;
  const r = cv.getBoundingClientRect(), px = e.clientX - r.left, py = e.clientY - r.top;
  const hit = eqHit(ff.f.bands ?? [], px, py, r.width, r.height);
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
function field(fxId: Id, i: () => number, key: 'f' | 'g' | 'q') {
  let g: Gesture | null = null;
  return (v: number, mode: 'begin' | 'move' | 'end' | 'set' | 'wheel') => {
    const op = bandOp(fxId, i(), { [key]: v });
    if (mode === 'begin') g = new Gesture('Band EQ');
    else if (mode === 'move') { if (op) g?.update([op]); }
    else if (mode === 'end') { g?.end(); g = null; }
    else if (mode === 'set') { if (op) commit([op], 'Band EQ'); }
    else if (op) wheelEdit('eqf' + fxId + key, 'Band EQ', [op]);
  };
}
function dynField(fxId: Id, i: () => number, key: 'range' | 'thr') {
  let g: Gesture | null = null;
  return (v: number, mode: 'begin' | 'move' | 'end' | 'set' | 'wheel') => {
    const b = findFx(fxId)?.f.bands?.[i()]; if (!b?.dynamic) return;
    const op = bandOp(fxId, i(), { dynamic: { ...b.dynamic, [key]: v } });
    if (mode === 'begin') g = new Gesture('Dynamiczny band');
    else if (mode === 'move') { if (op) g?.update([op]); }
    else if (mode === 'end') { g?.end(); g = null; }
    else if (mode === 'set') { if (op) commit([op], 'Dynamiczny band'); }
    else if (op) wheelEdit('eqd' + fxId + key, 'Dynamiczny band', [op]);
  };
}

export function EqPanel(props: { fx: Fx }) {
  const fxId = () => props.fx.id;
  const sel = () => { const i = U.eqSel[fxId()]; return i != null && props.fx.bands?.[i] ? i : null; };
  const b = () => { const i = sel(); return i == null ? null : props.fx.bands![i]; };
  const si = () => sel() ?? 0;
  const menu = (k: 'f' | 'g' | 'q') => (x: number, y: number) => paramMenu(x, y, { kind: 'fx', fxId: fxId(), param: 'band:' + si() + ':' + k },
    k === 'g' ? () => updBand(fxId(), si(), { g: 0 }) : k === 'q' ? () => updBand(fxId(), si(), { q: b()?.type === 'peak' ? 1 : 0.71 }) : null, bandColor(si()));
  const addFree = () => {
    // a bell where the response has the most room: the widest gap between existing bands (log f)
    const fs = (props.fx.bands ?? []).map(x => Math.log(x.f)).sort((a, z) => a - z);
    const pts = [Math.log(30), ...fs, Math.log(16000)];
    let best = 0, bg = -1;
    for (let k = 0; k + 1 < pts.length; k++) if (pts[k + 1] - pts[k] > bg) { bg = pts[k + 1] - pts[k]; best = k; }
    addBand(fxId(), newBand('peak', Math.exp((pts[best] + pts[best + 1]) / 2), 0));
  };
  return (
    <div class="eqpanel">
      <div class="bchips">
        <For each={props.fx.bands ?? []}>{(bd, i) => (
          <button class="bchip" classList={{ sel: sel() === i(), off: !bd.on }} style={{ '--c': bandColor(i()) }}
            title={'Band ' + (i() + 1) + ' · ' + BTYPE_LABEL[bd.type] + ' · ' + hzText(bd.f) + ' — klik = wybierz · 2× klik = wł./wył. · PPM = menu'}
            onPointerDown={e => {
              e.stopPropagation();
              if (e.button === 2) { setU('eqSel', fxId(), i()); openBandMenu(e.clientX, e.clientY, fxId(), i()); return; }
              setU('eqSel', fxId(), i());
              if (dblCheck('chip' + fxId() + i())) updBand(fxId(), i(), { on: !bd.on });
            }}>{i() + 1}</button>
        )}</For>
        <button class="bchip add" title={'Dodaj band (lub 2× klik na wykresie) · ' + (props.fx.bands?.length ?? 0) + '/' + MAX_BANDS}
          disabled={(props.fx.bands?.length ?? 0) >= MAX_BANDS} onClick={addFree}>+</button>
        <span class="bcount">{(props.fx.bands?.length ?? 0) + '/' + MAX_BANDS}</span>
      </div>
      <Show when={b()} fallback={<div class="bedit empty">Wybierz band na wykresie albo 2× kliknij, by dodać. Przeciągnij z podświetlonego szczytu widma, by od razu wyciąć rezonans.</div>}>
        {band => (
          <div class="bedit">
            <button class="led" title="Włącz / wyłącz band" style={{ background: band().on ? bandColor(si()) : 'var(--line2)' }} onClick={() => updBand(fxId(), si(), { on: !band().on })} />
            <span class="bnum" style={{ color: bandColor(si()) }}>{si() + 1}</span>
            <button class="shape" title="Kształt" onClick={e => { const r = (e.currentTarget as HTMLElement).getBoundingClientRect(); openBandMenu(r.left, r.bottom + 4, fxId(), si()); }}>
              {BTYPE_LABEL[band().type]}<svg width="8" height="8" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3"><path d="M6 9l6 6 6-6" /></svg>
            </button>
            <NumField label="FREQ" width={74} value={() => band().f} text={hzText} parse={parseNum} onMenu={menu('f')}
              drag={(v, d) => clamp(v * Math.pow(2, d / 90), 10, 22000)} apply={field(fxId(), si, 'f')} />
            <NumField label="GAIN" width={64} value={() => band().g} text={dbText} parse={parseNum} disabled={() => !hasGain(band().type)} onMenu={menu('g')}
              drag={(v, d) => clamp(Math.round((v + d * 0.05) * 10) / 10, -30, 30)} apply={field(fxId(), si, 'g')} />
            <NumField label="Q" width={50} value={() => band().q} text={v => v.toFixed(2)} parse={parseNum} disabled={() => band().type === 'tilt'} onMenu={menu('q')}
              drag={(v, d) => clamp(v * Math.pow(2, d / 110), 0.1, 18)} apply={field(fxId(), si, 'q')} />
            <Show when={isCut(band().type)}>
              <Segmented small label="dB/okt" options={SLOPES.map(String)} value={() => SLOPES.indexOf(slopeOf(band()))} pick={k => updBand(fxId(), si(), { slope: SLOPES[k] }, 'Nachylenie')} />
            </Show>
            <Segmented small label="kanał" options={PLACES.map(p => PLACE_SHORT[p])} value={() => PLACES.indexOf(placeOf(band()))} pick={k => updBand(fxId(), si(), { place: PLACES[k] }, 'Kanał bandu')}
              title="Stereo / Left / Right / Mid / Side" />
            <Show when={canBeDynamic(band().type)}>
              <button class="tog" classList={{ on: !!band().dynamic }} title="Dynamiczny: gain zmienia się, gdy poziom pasma przekroczy próg"
                onClick={() => updBand(fxId(), si(), { dynamic: band().dynamic ? null : { range: band().g > 0 ? -band().g : -6, thr: -30 } }, 'Dynamiczny band')}>DYN</button>
              <Show when={band().dynamic}>
                <NumField label="ZAKRES" width={64} value={() => band().dynamic!.range} text={dbText} parse={parseNum}
                  drag={(v, d) => clamp(Math.round((v + d * 0.05) * 10) / 10, -30, 30)} apply={dynField(fxId(), si, 'range')} />
                <NumField label="PRÓG" width={64} value={() => band().dynamic!.thr} text={v => Math.round(v) + ' dB'} parse={parseNum}
                  drag={(v, d) => clamp(Math.round(v + d * 0.1), -80, 0)} apply={dynField(fxId(), si, 'thr')} />
              </Show>
            </Show>
            <div style={{ flex: 1 }} />
            <button class="iconbtn danger" title="Usuń band" onClick={() => deleteBand(fxId(), si())}>
              <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M4 7h16 M10 11v6 M14 11v6 M6 7l1 13h10l1-13 M9 7V4h6v3" /></svg>
            </button>
          </div>
        )}
      </Show>
    </div>
  );
}
export { yDb };
