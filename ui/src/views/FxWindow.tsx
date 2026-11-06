import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show, type JSX } from 'solid-js';
import { be } from '../api/backend';
import type { Fx, Id, ParamDef, Vst3Param } from '../api/types';
import { clamp } from '../lib/format';
import { NOTE_NAMES } from '../lib/pitch';
import { paramDefs, pFmt, pNorm, presetInfo, presetList, pVal } from '../lib/presets';
import * as A from '../state/actions';
import { autoDisplay, isAutomated, paramMenu } from '../state/automation';
import { commit, Gesture, wheelEdit } from '../state/edit';
import { findFx, insName, S } from '../state/store';
import { onFxFrame, tele } from '../state/telemetry';
import { openMenu, say, U, type PlugWin } from '../state/ui';
import { dblCheck, track } from '../ui/drag';
import { addFrameTask, fitCanvas, fonts } from '../ui/frameLoop';
import { FXMETA, FxTile } from '../ui/icons';
import { Knob, MiniKnob, Segmented } from '../ui/Knob';
import { theme } from '../ui/theme';
import type { Viz } from './fx/common';
import { deessDb, DeessViz, deessFx, deessGeom, deessXf, deessY } from './fx/deess';
import { DenoiseViz } from './fx/denoise';
import { DynViz } from './fx/dyn';
import { EqViz } from './fx/eq';
import { EqPanel, eqHover, eqLeave, eqPointerDown, eqWheel } from './fx/eqEdit';
import { fetModes, FetViz, type FetMode } from './fx/fet';
import { defaultView, ReverbViz, views3d } from './fx/reverb';
import { DelayViz, SatViz } from './fx/satDelay';
import { TuneViz } from './fx/tune';

const plain = <T,>(v: T): T => JSON.parse(JSON.stringify(v));

function makeViz(f: Fx): Viz | null {
  switch (f.type) {
    case 'eq': return new EqViz();
    case 'comp': case 'limit': return new DynViz();
    case 'sat': return new SatViz();
    case 'delay': return new DelayViz();
    case 'reverb': return new ReverbViz(f.id);
    case 'tune': return new TuneViz();
    case 'fet': return new FetViz(f.id);
    case 'deess': return new DeessViz();
    case 'denoise': return new DenoiseViz();
    default: return null;
  }
}
const VIZ_H: Partial<Record<Fx['type'], number>> = { eq: 300, reverb: 260, tune: 280, fet: 250, deess: 230, denoise: 240 };
/** Default window widths per type (actions.openPlug). */
export const WIN_W: Partial<Record<Fx['type'], number>> = { eq: 780, reverb: 640, vst3: 520, tune: 800, fet: 600, deess: 680, denoise: 640 };

const HINTS: Partial<Record<Fx['type'], string>> = {
  eq: 'kółko = Q · PPM na punkcie = kształt / kanał / dynamika · PPM na tle = nowy band danego typu',
  reverb: 'Przeciągnij = obrót 3D · 2× klik = reset',
  deess: 'Przeciągnij linię THR = próg · uchwyt częstotliwości = pasmo detekcji',
  fet: 'Klik na wskaźnik = GR / OUT',
  denoise: 'Profil szumu uczy się sam w przerwach · Hold = zamroź · Listen = posłuchaj, co jest usuwane',
  tune: 'Retune: 0 ms = efekt robota, dłużej = naturalnie',
};

/** Move a fixed window by its header. */
export function moveWindow(e: PointerEvent, get: () => { x: number; y: number; w: number }, set: (x: number, y: number) => void) {
  if (e.button !== 0 || (e.target as HTMLElement).closest('button,select,input,.miniknob')) return;
  e.preventDefault();
  const sx = e.clientX, sy = e.clientY, o = { ...get() };
  track(ev => set(clamp(o.x + ev.clientX - sx, 80 - o.w, window.innerWidth - 80), clamp(o.y + ev.clientY - sy, 0, window.innerHeight - 40)), null, 'grabbing');
}

export function FxWindow(props: { pw: PlugWin; z: number }) {
  const ff = () => findFx(props.pw.fxId);
  const setPos = (x: number, y: number) => A.movePlug(props.pw.fxId, x, y);
  return (
    <Show when={ff()}>
      {got => {
        const fx = () => got().f, m = () => got().m;
        const isVst = () => fx().type === 'vst3';
        const info = () => presetInfo(S.catalog, fx(), S.project.bpm);
        return (
          <div class="win fxwin" style={{ left: props.pw.x + 'px', top: props.pw.y + 'px', width: props.pw.w + 'px', 'z-index': props.z }} data-fxwin={props.pw.fxId}
            onPointerDown={() => A.plugFront(props.pw.fxId)}>
            <div class="winhd" onPointerDown={e => moveWindow(e, () => props.pw, setPos)}>
              <button class="pwr" classList={{ off: fx().bypass }} title={fx().bypass ? 'Bypass — kliknij, by włączyć' : 'Aktywny — kliknij = bypass'}
                onPointerDown={e => { e.stopPropagation(); A.toggleBypass(fx().id); }} />
              <FxTile type={fx().type} size={20} icon={13} />
              <span class="title">{A.fxName(fx())}</span>
              <span style={{ width: '6px', height: '6px', flex: 'none', 'border-radius': '2px', background: m().color }} />
              <span class="sub">{insName(m())}</span>
              <Show when={fx().bypass}><span class="bypasstag">BYPASS</span></Show>
              <div style={{ flex: 1 }} />
              <MixControl fx={fx()} />
              <Show when={!isVst() && presetList(S.catalog, fx().type).length}>
                <div class="presetbar">
                  <button class="arrow" title="Poprzedni preset" onClick={() => A.stepPreset(fx().id, -1)}>‹</button>
                  <button class="name" title="Presety" classList={{ mod: info().mod }} data-preset-name
                    onClick={e => { const r = (e.currentTarget as HTMLElement).getBoundingClientRect(); openPresetMenu(fx().id, r.left, r.bottom + 4); }}>
                    <span class="ell" style={{ flex: 1, 'min-width': 0 }}>{info().name}{info().mod ? ' *' : ''}</span>
                    <svg width="9" height="9" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" style={{ flex: 'none' }}><path d="M6 9l6 6 6-6" /></svg>
                  </button>
                  <button class="arrow" title="Następny preset" onClick={() => A.stepPreset(fx().id, 1)}>›</button>
                </div>
              </Show>
              <button class="closebtn" title="Zamknij (Esc)" onClick={() => A.closePlug(fx().id)}>×</button>
            </div>
            <Show when={!isVst()} fallback={<Vst3Body fx={fx()} />}>
              <VizCanvas fxId={props.pw.fxId} pw={props.pw} height={VIZ_H[fx().type] ?? 190} />
              <Body fx={fx()} />
            </Show>
          </div>
        );
      }}
    </Show>
  );
}

/** Dry/wet of the effect (fx.mix), automatable. */
function MixControl(props: { fx: Fx }) {
  const target = () => ({ kind: 'mix' as const, fxId: props.fx.id });
  const val = () => autoDisplay(target()) ?? props.fx.mix ?? 1;
  let g: Gesture | null = null;
  const op = (v: number) => A.fxOp({ ...plain(findFx(props.fx.id)!.f), mix: Math.round(clamp(v, 0, 1) * 1000) / 1000 });
  return (
    <div class="mixctl" title="Mix (dry/wet) efektu">
      <span class="lab">MIX</span>
      <MiniKnob key={'mix' + props.fx.id} title="Mix (dry/wet) · przeciągnij ↕ · 2× klik = 100% · PPM = automatyzacja" t={val} automated={() => isAutomated(target())}
        onStart={() => { g = new Gesture('Mix'); }} onChange={t => { g?.update([op(t)]); say(A.fxName(props.fx) + ' · Mix ' + Math.round(t * 100) + '%'); }}
        onEnd={() => { g?.end(); g = null; }} onReset={() => commit([op(1)], 'Mix')}
        onWheel={t => wheelEdit('mix' + props.fx.id, 'Mix', [op(t)])}
        onMenu={(x, y) => paramMenu(x, y, target(), () => commit([op(1)], 'Mix'), FXMETA[props.fx.type].color)} />
      <span class="v">{Math.round(val() * 100)}%</span>
    </div>
  );
}

function openPresetMenu(fxId: Id, x: number, y: number) {
  const ff = findFx(fxId); if (!ff || !S.catalog) return;
  const info = presetInfo(S.catalog, ff.f, S.project.bpm), list = presetList(S.catalog, ff.f.type);
  openMenu(x, y, [{ t: 'title', label: 'Presety · ' + A.fxName(ff.f) },
    ...list.map((pr, i) => ({ t: 'item' as const, label: pr.name, kbd: i === info.idx ? (info.mod ? 'zmieniony' : '●') : '', act: () => A.loadPreset(fxId, i) }))], 40 + list.length * 26);
}

// ---- parameter controls ----
function setParam(fxId: Id, id: string, v: number) {
  const f = findFx(fxId)?.f; if (!f) return null;
  return A.fxOp({ ...plain(f), params: { ...plain(f.params), [id]: v } });
}
const defOf = (fx: Fx, id: string) => paramDefs(S.catalog, fx.type).find(p => p.id === id);

function ParamKnob(props: { fxId: Id; p: ParamDef; ends?: [string, string]; color?: string }) {
  const fx = () => findFx(props.fxId)?.f;
  const target = () => ({ kind: 'fx' as const, fxId: props.fxId, param: props.p.id });
  const val = () => autoDisplay(target()) ?? fx()?.params[props.p.id] ?? props.p.def;
  const p = props.p;
  let g: Gesture | null = null;
  const reset = () => { const op = setParam(props.fxId, p.id, p.def); if (op) commit([op], p.label); };
  return (
    <Knob key={props.fxId + p.id} label={p.label} t={() => clamp(pNorm(p, val()), 0, 1)} ends={props.ends} color={props.color}
      automated={() => isAutomated(target())}
      zero={() => (p.min < 0 && p.max > 0 && !p.log ? pNorm(p, 0) : 0)} text={() => pFmt(p, val())}
      onStart={() => { g = new Gesture(p.label); }}
      onChange={t => { const v = pVal(p, t); const op = setParam(props.fxId, p.id, v); if (op) g?.update([op]); say(A.fxName(fx()!) + ' · ' + p.label + ' ' + pFmt(p, v)); }}
      onEnd={() => { g?.end(); g = null; }}
      onReset={reset}
      onMenu={(x, y) => paramMenu(x, y, target(), reset, props.color)}
      onWheel={t => { const v = pVal(p, t); const op = setParam(props.fxId, p.id, v); if (op) wheelEdit('fx' + props.fxId + p.id, p.label, [op]); say(A.fxName(fx()!) + ' · ' + p.label + ' ' + pFmt(p, v)); }} />
  );
}

/** A choice parameter as segmented buttons (one click = one undo step). */
function ParamChoice(props: { fxId: Id; p: ParamDef; danger?: number; color?: string; labels?: string[]; small?: boolean; hideLabel?: boolean }) {
  const fx = () => findFx(props.fxId)?.f;
  const target = () => ({ kind: 'fx' as const, fxId: props.fxId, param: props.p.id });
  const val = () => autoDisplay(target()) ?? fx()?.params[props.p.id] ?? props.p.def;
  const pick = (i: number) => {
    if (Math.round(val()) === i) return;
    const op = setParam(props.fxId, props.p.id, i); if (!op) return;
    commit([op], props.p.label);
    say(A.fxName(fx()!) + ' · ' + props.p.label + ': ' + props.p.options![i]);
  };
  return <Segmented label={props.hideLabel ? undefined : props.p.label} options={props.labels ?? props.p.options!} value={val} pick={pick} danger={props.danger} color={props.color} small={props.small}
    onMenu={(x, y) => paramMenu(x, y, target(), () => pick(props.p.def), props.color)} />;
}

function Ctl(props: { fx: Fx; id: string; ends?: [string, string]; danger?: number; labels?: string[]; small?: boolean; hideLabel?: boolean }) {
  const d = () => defOf(props.fx, props.id);
  const color = () => FXMETA[props.fx.type].color;
  return (
    <Show when={d()}>
      {p => p().options
        ? <ParamChoice fxId={props.fx.id} p={p()} danger={props.danger} color={color()} labels={props.labels} small={props.small} hideLabel={props.hideLabel} />
        : <ParamKnob fxId={props.fx.id} p={p()} ends={props.ends} />}
    </Show>
  );
}

function Hint(props: { fx: Fx }) { return <span class="hint">{HINTS[props.fx.type] ?? ''}</span>; }

function Body(props: { fx: Fx }): JSX.Element {
  const t = () => props.fx.type;
  return (
    <>
      <Show when={t() === 'eq'}><EqPanel fx={props.fx} /></Show>
      <Show when={t() === 'tune'}><TuneControls fx={props.fx} /></Show>
      <Show when={t() === 'fet'}><FetControls fx={props.fx} /></Show>
      <Show when={t() === 'deess'}>
        <div class="knobs">
          <For each={['thr', 'range', 'freq', 'look']}>{id => <Ctl fx={props.fx} id={id} />}</For>
          <div class="vline" />
          <Ctl fx={props.fx} id="mode" />
          <Ctl fx={props.fx} id="listen" labels={['Off', 'Tylko S']} />
          <div style={{ flex: 1 }} /><Hint fx={props.fx} />
        </div>
      </Show>
      <Show when={!['eq', 'tune', 'fet', 'deess'].includes(t())}>
        <div class="knobs">
          <For each={paramDefs(S.catalog, t())}>{p => <Ctl fx={props.fx} id={p.id} />}</For>
          <div style={{ flex: 1 }} /><Hint fx={props.fx} />
        </div>
      </Show>
      <Show when={t() === 'eq'}>
        <div class="knobs slim">
          <For each={paramDefs(S.catalog, 'eq')}>{p => <Ctl fx={props.fx} id={p.id} />}</For>
          <div style={{ flex: 1 }} /><Hint fx={props.fx} />
        </div>
      </Show>
    </>
  );
}

function TuneControls(props: { fx: Fx }) {
  const color = FXMETA.tune.color;
  return (
    <>
      <div class="ctlrow">
        <Show when={defOf(props.fx, 'key')}>{p => <ParamChoice fxId={props.fx.id} p={p()} color={color} labels={NOTE_NAMES} small />}</Show>
        <Show when={defOf(props.fx, 'scale')}>{p => <ParamChoice fxId={props.fx.id} p={p()} color={color} small />}</Show>
      </div>
      <div class="knobs">
        <Ctl fx={props.fx} id="retune" ends={['robot', 'natural']} />
        <For each={['humanize', 'flex', 'vibrato']}>{id => <Ctl fx={props.fx} id={id} />}</For>
        <div class="vline" />
        <For each={['transpose', 'formant']}>{id => <Ctl fx={props.fx} id={id} />}</For>
        <div style={{ flex: 1 }} /><Hint fx={props.fx} />
      </div>
    </>
  );
}

function FetControls(props: { fx: Fx }) {
  const mode = () => fetModes.get(props.fx.id);
  const pick = (m: FetMode) => fetModes.set(props.fx.id, m);
  return (
    <div class="knobs">
      <Ctl fx={props.fx} id="input" />
      <div class="vline" />
      <div class="stack">
        <Ctl fx={props.fx} id="ratio" danger={4} />
        <Segmented label="Wskaźnik" small options={['GR', 'OUT']} value={() => (mode() === 'gr' ? 0 : 1)} pick={i => pick(i ? 'out' : 'gr')} title="Co pokazuje wskaźnik: redukcję (GR) albo poziom wyjścia (VU)" />
      </div>
      <div class="vline" />
      <For each={['attack', 'release', 'output', 'mix']}>{id => <Ctl fx={props.fx} id={id} />}</For>
      <div style={{ flex: 1 }} /><Hint fx={props.fx} />
    </div>
  );
}

/** The window's canvas: one Viz drawn from the shared frame loop, only when something changed. */
function VizCanvas(props: { fxId: Id; pw: PlugWin; height: number }) {
  let cv!: HTMLCanvasElement;
  const ff = () => findFx(props.fxId);
  const viz = makeViz(ff()!.f);
  const height = createMemo(() => props.height);
  const pw = createMemo(() => props.pw);
  let version = 0, drawnVersion = -1, lastW = 0, fontEpoch = -1, themeEpoch = -1;
  createEffect(() => { // anything param-ish changes → redraw
    const f = ff()?.f; if (!f) return;
    f.bypass; f.mix; for (const k in f.params) void f.params[k];
    f.bands?.forEach(b => { b.on; b.type; b.f; b.g; b.q; b.slope; b.place; b.dynamic?.range; b.dynamic?.thr; });
    f.bands?.length;
    U.eqSel[props.fxId]; U.fetMode[props.fxId]; S.project.bpm;
    version++;
  });
  onMount(() => {
    const off = viz?.ingest ? onFxFrame(props.fxId, (a, at) => viz.ingest!(a, at)) : null;
    onCleanup(() => off?.());
    const stop = addFrameTask(now => {
      const f = ff()?.f; if (!f || !viz) return;
      const w = pw();
      if (w.x > window.innerWidth || w.y > window.innerHeight || w.x + w.w < 0 || document.hidden) return;
      const raw = tele.fx.get(props.fxId) ?? null;
      const a = raw && tele.frame.seq - raw.seq < 30 ? raw : null;
      const changed = viz.update(f, a, now);
      const W = cv.clientWidth, H = height();
      if (!W) return;
      if (!changed && version === drawnVersion && W === lastW && fontEpoch === fonts.epoch && themeEpoch === theme.epoch) return;
      drawnVersion = version; lastW = W; fontEpoch = fonts.epoch; themeEpoch = theme.epoch;
      fitCanvas(cv, W, H);
      const g = cv.getContext('2d')!;
      const dpr = window.devicePixelRatio || 1;
      g.setTransform(dpr, 0, 0, dpr, 0, 0);
      viz.draw(g, W, H, f, a, { selBand: U.eqSel[props.fxId] ?? null, fxId: props.fxId, now });
    });
    onCleanup(stop);
  });

  const rel = (e: PointerEvent | WheelEvent) => { const r = cv.getBoundingClientRect(); return { x: e.clientX - r.left, y: e.clientY - r.top, W: r.width, H: r.height }; };
  const down = (e: PointerEvent) => {
    const got = ff(); if (!got || !viz) return;
    if (got.f.type === 'eq') eqPointerDown(e, cv, props.fxId, viz as EqViz);
    else if (got.f.type === 'reverb') verbDown(e);
    else if (got.f.type === 'deess') deessDown(e, got.f);
    else if (got.f.type === 'fet' && e.button === 0) {
      const m = fetModes.get(props.fxId) === 'out' ? 'gr' : 'out';
      fetModes.set(props.fxId, m); version++; say('Wskaźnik: ' + (m === 'gr' ? 'redukcja (GR)' : 'wyjście (VU)'));
    }
  };
  const move = (e: PointerEvent) => {
    const got = ff(); if (!got || !viz || e.buttons) return;
    if (got.f.type === 'eq') eqHover(e, cv, props.fxId, viz as EqViz);
    else if (got.f.type === 'deess') {
      const d = viz as DeessViz, { x, y, W, H } = rel(e), G = deessGeom(W, H);
      const onThr = x < G.hx + G.hw && Math.abs(y - deessY(got.f.params.thr ?? -32, H)) < 6;
      const onF = x >= G.sx && Math.abs(x - deessFx(got.f.params.freq ?? 6000, W, H)) < 8;
      if (onThr !== d.hoverThr || onF !== d.hoverFreq) { d.hoverThr = onThr; d.hoverFreq = onF; version++; }
      cv.style.cursor = onThr ? 'ns-resize' : onF ? 'ew-resize' : 'default';
    }
  };
  const leave = () => {
    const got = ff(); if (!got || !viz) return;
    if (got.f.type === 'eq') eqLeave(viz as EqViz);
    if (got.f.type === 'deess') { const d = viz as DeessViz; d.hoverThr = d.hoverFreq = false; version++; }
  };
  const deessDown = (e: PointerEvent, f: Fx) => {
    if (e.button !== 0) return;
    const { x, y, W, H } = rel(e), G = deessGeom(W, H);
    const thrP = defOf(f, 'thr'), frP = defOf(f, 'freq'); if (!thrP || !frP) return;
    const onThr = x < G.hx + G.hw && Math.abs(y - deessY(f.params.thr ?? -32, H)) < 6;
    const onF = x >= G.sx - 4;
    if (!onThr && !onF) return;
    e.preventDefault();
    if (onThr && dblCheck('thr' + props.fxId)) { const op = setParam(props.fxId, 'thr', thrP.def); if (op) commit([op], 'Próg'); return; }
    const g = new Gesture(onThr ? 'Próg' : 'Częstotliwość');
    const r = cv.getBoundingClientRect();
    track(ev => {
      const op = onThr
        ? setParam(props.fxId, 'thr', Math.round(clamp(deessDb(ev.clientY - r.top, H), thrP.min, thrP.max)))
        : setParam(props.fxId, 'freq', Math.round(clamp(deessXf(ev.clientX - r.left, W, H), frP.min, frP.max)));
      if (op) g.update([op]);
      const nf = findFx(props.fxId)?.f;
      say(onThr ? 'Próg ' + Math.round(nf?.params.thr ?? 0) + ' dB' : 'Pasmo detekcji od ' + Math.round(nf?.params.freq ?? 0) + ' Hz');
    }, () => g.end(), onThr ? 'ns-resize' : 'ew-resize');
    if (onF && !onThr) { const op = setParam(props.fxId, 'freq', Math.round(clamp(deessXf(x, W, H), frP.min, frP.max))); if (op) g.update([op]); }
  };
  const verbDown = (e: PointerEvent) => {
    if (e.button !== 0) return; e.preventDefault();
    let v = views3d.get(props.fxId); if (!v) { v = defaultView(); views3d.set(props.fxId, v); }
    const bump = () => { (viz as ReverbViz).viewVersion++; };
    if (dblCheck('verb' + props.fxId)) { Object.assign(v, defaultView()); bump(); say('Widok 3D zresetowany'); return; }
    let lx = e.clientX, ly = e.clientY;
    track(ev => { v!.yaw = clamp(v!.yaw + (ev.clientX - lx) * 0.008, -1.2, 1.2); v!.pitch = clamp(v!.pitch + (ev.clientY - ly) * 0.006, 0.1, 1.3); lx = ev.clientX; ly = ev.clientY; bump(); }, null, 'grabbing');
  };
  const wheel = (e: WheelEvent) => {
    const got = ff(); if (!got || !viz) return;
    if (got.f.type === 'eq') eqWheel(e, props.fxId, viz as EqViz);
  };
  return <canvas ref={cv} class="vizcv" data-viz={props.fxId} style={{ height: props.height + 'px', cursor: viz?.cursor ?? 'default' }}
    onPointerDown={down} onPointerMove={move} onPointerLeave={leave} onWheel={wheel} onContextMenu={e => e.preventDefault()} />;
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
