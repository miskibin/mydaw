import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import { be } from '../api/backend';
import type { BandType, EqBand, Fx, Id, ParamDef, Vst3Param } from '../api/types';
import { fx2x, x2f } from '../lib/eq';
import { clamp, dbText, hzText } from '../lib/format';
import { defaultBands, paramDefs, pFmt, pNorm, presetInfo, presetList, pVal } from '../lib/presets';
import * as A from '../state/actions';
import { commit, Gesture, wheelEdit } from '../state/edit';
import { findFx, insName, S } from '../state/store';
import { tele } from '../state/telemetry';
import { openMenu, say, setU, U, type PlugWin } from '../state/ui';
import { dblCheck, track } from '../ui/drag';
import { addFrameTask, fitCanvas } from '../ui/frameLoop';
import { FxTile } from '../ui/icons';
import { Knob } from '../ui/Knob';
import { BAND_COLORS, type Viz } from './fx/common';
import { DynViz } from './fx/dyn';
import { bandY, EqViz } from './fx/eq';
import { defaultView, ReverbViz, views3d } from './fx/reverb';
import { DelayViz, SatViz } from './fx/satDelay';

const plain = <T,>(v: T): T => JSON.parse(JSON.stringify(v));
const BTYPE_ORDER: BandType[] = ['lowcut', 'lowshelf', 'peak', 'highshelf', 'highcut'];
const BTYPE_LABEL: Record<BandType, string> = { lowcut: 'Low cut', lowshelf: 'Low shelf', peak: 'Bell', highshelf: 'High shelf', highcut: 'High cut' };

function makeViz(f: Fx): Viz | null {
  switch (f.type) {
    case 'eq': return new EqViz();
    case 'comp': case 'limit': return new DynViz();
    case 'sat': return new SatViz();
    case 'delay': return new DelayViz();
    case 'reverb': return new ReverbViz(f.id);
    default: return null;
  }
}

/** Move a fixed window by its header. */
export function moveWindow(e: PointerEvent, get: () => { x: number; y: number; w: number }, set: (x: number, y: number) => void) {
  if (e.button !== 0 || (e.target as HTMLElement).closest('button,select,input')) return;
  e.preventDefault();
  const sx = e.clientX, sy = e.clientY, o = { ...get() };
  track(ev => set(clamp(o.x + ev.clientX - sx, 80 - o.w, window.innerWidth - 80), clamp(o.y + ev.clientY - sy, 0, window.innerHeight - 40)), null, 'grabbing');
}

export function FxWindow(props: { pw: PlugWin; z: number }) {
  const ff = () => findFx(props.pw.fxId);
  const setPos = (x: number, y: number) => setU('plugs', p => p.fxId === props.pw.fxId, { x, y });
  return (
    <Show when={ff()}>
      {got => {
        const fx = () => got().f, m = () => got().m;
        const isEq = () => fx().type === 'eq', isVerb = () => fx().type === 'reverb', isVst = () => fx().type === 'vst3';
        const info = () => presetInfo(S.catalog, fx(), S.project.bpm);
        return (
          <div class="win" style={{ left: props.pw.x + 'px', top: props.pw.y + 'px', width: props.pw.w + 'px', 'z-index': props.z }} data-fxwin={props.pw.fxId}
            onPointerDown={() => A.plugFront(props.pw.fxId)}>
            <div class="winhd" onPointerDown={e => moveWindow(e, () => props.pw, setPos)}>
              <button title="Włącz / bypass" style={{ width: '12px', height: '12px', flex: 'none', padding: 0, border: 0, 'border-radius': '50%', background: fx().bypass ? '#2c333d' : '#4ec08a', cursor: 'pointer' }}
                onPointerDown={e => { e.stopPropagation(); A.toggleBypass(fx().id); }} />
              <FxTile type={fx().type} size={20} icon={13} />
              <span class="title">{A.fxName(fx())}</span>
              <span style={{ width: '6px', height: '6px', flex: 'none', 'border-radius': '2px', background: m().color }} />
              <span class="sub">{insName(m())}</span>
              <span class="mono" style={{ font: '600 9px var(--mono)', color: 'var(--amber)', 'letter-spacing': '.08em' }}>{fx().bypass ? 'BYPASS' : ''}</span>
              <div style={{ flex: 1 }} />
              <Show when={!isVst() && presetList(S.catalog, fx().type).length}>
                <div class="presetbar">
                  <button class="arrow" title="Poprzedni preset" onClick={() => A.stepPreset(fx().id, -1)}>‹</button>
                  <button class="name" title="Presety" style={{ color: info().mod ? '#e0a44a' : '#d5dae2' }} data-preset-name
                    onClick={e => { const r = (e.currentTarget as HTMLElement).getBoundingClientRect(); openPresetMenu(fx().id, r.left, r.bottom + 4); }}>
                    <span class="ell" style={{ flex: 1, 'min-width': 0 }}>{info().name}{info().mod ? ' *' : ''}</span>
                    <svg width="9" height="9" viewBox="0 0 24 24" fill="none" stroke="#7d8592" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" style={{ flex: 'none' }}><path d="M6 9l6 6 6-6" /></svg>
                  </button>
                  <button class="arrow" title="Następny preset" onClick={() => A.stepPreset(fx().id, 1)}>›</button>
                </div>
              </Show>
              <button class="closebtn" title="Zamknij (Esc)" onClick={() => A.closePlug(fx().id)}>×</button>
            </div>
            <Show when={!isVst()} fallback={<Vst3Body fx={fx()} />}>
              <VizCanvas fxId={props.pw.fxId} pw={props.pw} height={isEq() ? 250 : isVerb() ? 260 : 190} />
              <Show when={isEq()}><EqBands fx={fx()} insId={m().id} /></Show>
              <div class="knobs">
                <For each={paramDefs(S.catalog, fx().type)}>{p => <ParamKnob fxId={fx().id} p={p} />}</For>
                <div style={{ flex: 1 }} />
                <span class="hint">{isEq() ? 'Przeciągnij punkt · kółko = Q · 2× klik = wł./wył. · PPM = typ' : isVerb() ? 'Przeciągnij = obrót 3D · 2× klik = reset' : ''}</span>
              </div>
            </Show>
          </div>
        );
      }}
    </Show>
  );
}

function openPresetMenu(fxId: Id, x: number, y: number) {
  const ff = findFx(fxId); if (!ff || !S.catalog) return;
  const info = presetInfo(S.catalog, ff.f, S.project.bpm), list = presetList(S.catalog, ff.f.type);
  openMenu(x, y, [{ t: 'title', label: 'Presety · ' + A.fxName(ff.f) },
    ...list.map((pr, i) => ({ t: 'item' as const, label: pr.name, kbd: i === info.idx ? (info.mod ? 'zmieniony' : '●') : '', act: () => A.loadPreset(fxId, i) }))], 40 + list.length * 26);
}

function ParamKnob(props: { fxId: Id; p: ParamDef }) {
  const fx = () => findFx(props.fxId)?.f;
  const val = () => fx()?.params[props.p.id] ?? props.p.def;
  const p = props.p;
  let g: Gesture | null = null;
  const setV = (v: number) => {
    const f = fx(); if (!f) return null;
    return A.fxOp({ ...plain(f), params: { ...plain(f.params), [p.id]: v } });
  };
  return (
    <Knob key={props.fxId + p.id} label={p.label} t={() => clamp(pNorm(p, val()), 0, 1)}
      zero={() => (p.min < 0 && p.max > 0 && !p.log ? pNorm(p, 0) : 0)} text={() => pFmt(p, val())}
      onStart={() => { g = new Gesture(p.label); }}
      onChange={t => { const v = pVal(p, t); const op = setV(v); if (op) g?.update([op]); say(A.fxName(fx()!) + ' · ' + p.label + ' ' + pFmt(p, v)); }}
      onEnd={() => { g?.end(); g = null; }}
      onReset={() => { const op = setV(p.def); if (op) commit([op], p.label); }}
      onWheel={t => { const v = pVal(p, t); const op = setV(v); if (op) wheelEdit('fx' + props.fxId + p.id, p.label, [op]); say(A.fxName(fx()!) + ' · ' + p.label + ' ' + pFmt(p, v)); }} />
  );
}

/** The window's canvas: one Viz drawn from the shared frame loop, only when something changed. */
function VizCanvas(props: { fxId: Id; pw: PlugWin; height: number }) {
  let cv!: HTMLCanvasElement;
  const ff = () => findFx(props.fxId);
  const viz = makeViz(ff()!.f);
  const height = createMemo(() => props.height);
  const pw = createMemo(() => props.pw);
  let version = 0, drawnVersion = -1, lastW = 0;
  createEffect(() => { // anything param-ish changes → redraw
    const f = ff()?.f; if (!f) return;
    f.bypass; for (const k in f.params) void f.params[k];
    f.bands?.forEach(b => { b.on; b.type; b.f; b.g; b.q; });
    U.eqSel[props.fxId]; S.project.bpm;
    version++;
  });
  onMount(() => {
    const stop = addFrameTask(now => {
      const f = ff()?.f; if (!f || !viz) return;
      // skip windows that are off-screen
      const w = pw();
      if (w.x > window.innerWidth || w.y > window.innerHeight || w.x + w.w < 0 || document.hidden) return;
      const raw = tele.fx.get(props.fxId) ?? null;
      const a = raw && tele.frame.seq - raw.seq < 30 ? raw : null;
      const changed = viz.update(f, a, now);
      const W = cv.clientWidth, H = height();
      if (!W) return;
      if (!changed && version === drawnVersion && W === lastW) return;
      drawnVersion = version; lastW = W;
      fitCanvas(cv, W, H);
      const g = cv.getContext('2d')!;
      const dpr = window.devicePixelRatio || 1;
      g.setTransform(dpr, 0, 0, dpr, 0, 0);
      viz.draw(g, W, H, f, a, { selBand: U.eqSel[props.fxId] ?? null, fxId: props.fxId });
    });
    onCleanup(stop);
  });

  const down = (e: PointerEvent) => {
    const got = ff(); if (!got) return;
    if (got.f.type === 'eq') eqDown(e, got.m.id, got.f);
    else if (got.f.type === 'reverb') verbDown(e);
  };
  const verbDown = (e: PointerEvent) => {
    if (e.button !== 0) return; e.preventDefault();
    let v = views3d.get(props.fxId); if (!v) { v = defaultView(); views3d.set(props.fxId, v); }
    const bump = () => { (viz as ReverbViz).viewVersion++; };
    if (dblCheck('verb' + props.fxId)) { Object.assign(v, defaultView()); bump(); say('Widok 3D zresetowany'); return; }
    let lx = e.clientX, ly = e.clientY;
    track(ev => { v!.yaw = clamp(v!.yaw + (ev.clientX - lx) * 0.008, -1.2, 1.2); v!.pitch = clamp(v!.pitch + (ev.clientY - ly) * 0.006, 0.1, 1.3); lx = ev.clientX; ly = ev.clientY; bump(); }, null, 'grabbing');
  };
  const eqDown = (e: PointerEvent, insId: Id, f: Fx) => {
    const r = cv.getBoundingClientRect(), W = r.width, H = r.height, px = e.clientX - r.left, py = e.clientY - r.top;
    const bands = f.bands ?? [];
    let best = -1, bd = 16;
    bands.forEach((b, i) => { const d = Math.hypot(px - fx2x(b.f, W), py - bandY(b, H)); if (d < bd) { bd = d; best = i; } });
    if (e.button === 2) { e.preventDefault(); if (best >= 0) openBandMenu(e.clientX, e.clientY, props.fxId, best); return; }
    if (e.button !== 0 || best < 0) return;
    e.preventDefault();
    setU('eqSel', props.fxId, best);
    if (dblCheck('band' + props.fxId + best)) { updBand(props.fxId, best, { on: !bands[best].on }); return; }
    const g = new Gesture('Band EQ');
    const b0 = { ...bands[best] }, sx = e.clientX, sy = e.clientY, x0 = fx2x(b0.f, W), y0 = bandY(b0, H), cut = b0.type.endsWith('cut');
    track(ev => {
      const k = ev.shiftKey ? 0.2 : 1;
      const nf = clamp(x2f(x0 + (ev.clientX - sx) * k, W), 20, 20000);
      const ng = cut ? b0.g : clamp(-((y0 + (ev.clientY - sy) * k) - H / 2) / ((H / 2) * 0.9) * 18, -18, 18);
      const op = bandOp(props.fxId, best, { f: nf, g: ng, on: true }); if (op) g.update([op]);
      say('Band ' + (best + 1) + ' · ' + hzText(nf) + (cut ? '' : ' · ' + dbText(ng)) + ' · kółko = Q');
    }, () => g.end(), 'move');
    void insId;
  };
  const wheel = (e: WheelEvent) => {
    const got = ff(); if (!got || got.f.type !== 'eq') return;
    const i = U.eqSel[props.fxId]; if (i == null) return;
    const d = e.deltaY || e.deltaX; if (!d) return;
    e.preventDefault(); e.stopPropagation();
    const b = got.f.bands![i], q = clamp(b.q * Math.pow(2, (d < 0 ? 1 : -1) * 0.15 * (e.shiftKey ? 0.2 : 1)), 0.1, 18);
    const op = bandOp(props.fxId, i, { q }); if (op) wheelEdit('eqq' + props.fxId, 'Q', [op]);
    say('Band ' + (i + 1) + ' · Q ' + q.toFixed(2));
  };
  return <canvas ref={cv} class="vizcv" data-viz={props.fxId} style={{ height: props.height + 'px', cursor: viz?.cursor ?? 'default' }}
    onPointerDown={down} onWheel={wheel} onContextMenu={e => e.preventDefault()} />;
}

function bandOp(fxId: Id, i: number, patch: Partial<EqBand>) {
  const ff = findFx(fxId); if (!ff || !ff.f.bands) return null;
  const f = plain(ff.f);
  f.bands![i] = { ...f.bands![i], ...patch };
  return A.fxOp(f, ff.m.id);
}
function updBand(fxId: Id, i: number, patch: Partial<EqBand>) { const op = bandOp(fxId, i, patch); if (op) commit([op], 'Band EQ'); }

function openBandMenu(x: number, y: number, fxId: Id, i: number) {
  const b = findFx(fxId)?.f.bands?.[i]; if (!b) return;
  openMenu(x, y, [{ t: 'title', label: 'Band ' + (i + 1) },
    ...BTYPE_ORDER.map(ty => ({ t: 'item' as const, label: BTYPE_LABEL[ty], kbd: b.type === ty ? '●' : '', act: () => updBand(fxId, i, { type: ty, on: true }) })),
    { t: 'sep' },
    { t: 'item', label: b.on ? 'Wyłącz band' : 'Włącz band', act: () => updBand(fxId, i, { on: !b.on }) },
    { t: 'item', label: 'Reset bandu', act: () => updBand(fxId, i, { ...defaultBands()[i] }) }], 280);
}

function EqBands(props: { fx: Fx; insId: Id }) {
  const field = (i: number, k: 'on' | 'type' | 'f' | 'g' | 'q') => (e: PointerEvent) => {
    if (e.button !== 0) return; e.preventDefault(); e.stopPropagation();
    setU('eqSel', props.fx.id, i);
    const b0 = props.fx.bands![i];
    if (k === 'type') return updBand(props.fx.id, i, { type: BTYPE_ORDER[(BTYPE_ORDER.indexOf(b0.type) + 1) % BTYPE_ORDER.length], on: true });
    if (k === 'on') return updBand(props.fx.id, i, { on: !b0.on });
    const g = new Gesture('Band EQ');
    let v = b0[k], lx = e.clientX, ly = e.clientY;
    track(ev => {
      const d = ((ev.clientX - lx) + (ly - ev.clientY)) * (ev.shiftKey ? 0.2 : 1); lx = ev.clientX; ly = ev.clientY;
      v = k === 'f' ? clamp(v * Math.pow(2, d / 60), 20, 20000) : k === 'g' ? clamp(v + d * 0.1, -18, 18) : clamp(v * Math.pow(2, d / 80), 0.1, 18);
      const op = bandOp(props.fx.id, i, { [k]: v, on: true }); if (op) g.update([op]);
    }, () => g.end(), 'ns-resize');
  };
  return (
    <div class="bands">
      <For each={props.fx.bands ?? []}>
        {(b, i) => (
          <div class="band" classList={{ on: U.eqSel[props.fx.id] === i() }}>
            <div class="row" style={{ gap: '6px' }}>
              <button title="Włącz / wyłącz band" onPointerDown={field(i(), 'on')} style={{ width: '10px', height: '10px', padding: 0, border: 0, 'border-radius': '50%', background: b.on ? BAND_COLORS[i()] : '#2c333d', cursor: 'pointer' }} />
              <span class="mono" style={{ font: '600 10px var(--mono)', color: BAND_COLORS[i()] }}>{i() + 1}</span>
            </div>
            <button class="ty" title="Klik = zmień typ" onPointerDown={field(i(), 'type')}>{BTYPE_LABEL[b.type]}</button>
            <span class="v" title="Przeciągnij" style={{ color: b.on ? '#f2f4f7' : '#5c6573' }} onPointerDown={field(i(), 'f')}>{hzText(b.f)}</span>
            <span class="v" title="Przeciągnij" style={{ color: b.on ? '#f2f4f7' : '#5c6573' }} onPointerDown={field(i(), 'g')}>{b.type.endsWith('cut') ? '—' : dbText(b.g)}</span>
            <span class="v" title="Przeciągnij" style={{ color: '#7d8592' }} onPointerDown={field(i(), 'q')}>{'Q ' + b.q.toFixed(2)}</span>
          </div>
        )}
      </For>
    </div>
  );
}

function Vst3Body(props: { fx: Fx }) {
  const [params, setParams] = createSignal<Vst3Param[]>([]);
  const [err, setErr] = createSignal<string | null>(null);
  const [guiOpen, setGuiOpen] = createSignal(false);
  const [q, setQ] = createSignal('');
  let refresh = 0;
  const load = () => be().vst3Params(props.fx.id).then(p => { setParams(p); setErr(null); }).catch(e => setErr(String(e)));
  onMount(load);
  onCleanup(() => { if (guiOpen()) void be().vst3Editor(props.fx.id, false).catch(() => {}); });
  const set = (id: number, v: number) => {
    setParams(ps => ps.map(p => p.id === id ? { ...p, value: v } : p));
    void be().vst3SetParam(props.fx.id, id, v).catch(e => say('VST3: ' + String(e)));
    clearTimeout(refresh); refresh = window.setTimeout(load, 200);
  };
  const shown = () => { const s = q().toLowerCase(); return params().filter(p => !s || p.name.toLowerCase().includes(s)); };
  return (
    <div style={{ padding: '12px 14px', display: 'flex', 'flex-direction': 'column', gap: '10px' }}>
      <div class="row" style={{ gap: '8px' }}>
        <button class="smallbtn primary" style={{ height: '26px', padding: '0 12px' }} onClick={() => {
          const open = !guiOpen();
          be().vst3Editor(props.fx.id, open).then(() => setGuiOpen(open)).catch(e => say('Nie otwarto GUI: ' + String(e)));
        }}>{guiOpen() ? 'Zamknij GUI pluginu' : 'Otwórz GUI pluginu'}</button>
        <span class="note ell" style={{ flex: 1 }}>{props.fx.plugin?.vendor} · {props.fx.plugin?.path}</span>
      </div>
      <input class="textin" placeholder="Szukaj parametru…" value={q()} onInput={e => setQ(e.currentTarget.value)} onKeyDown={e => e.stopPropagation()} />
      <Show when={err()}><span class="note err">{err()}</span></Show>
      <div style={{ 'max-height': '320px', overflow: 'auto', 'padding-right': '4px' }}>
        <For each={shown()} fallback={<span class="note">Brak parametrów</span>}>
          {p => (
            <div class="vparam">
              <span class="ell" style={{ font: '12px var(--sans)', color: 'var(--fg)' }} title={p.name}>{p.name}</span>
              <input type="range" min="0" max="1" step="0.001" value={p.value} onInput={e => set(p.id, +e.currentTarget.value)}
                onDblClick={() => set(p.id, p.def)} />
              <span class="mono ell" style={{ 'font-size': '10px', color: 'var(--fg2)', 'text-align': 'right' }}>{p.display}</span>
            </div>
          )}
        </For>
      </div>
    </div>
  );
}
