// The built-in synth's page in the channel window, laid out like a modern soft synth: a preset
// bar, three tabs (OSC / MOD / FX) and cards that each draw what their knobs do — the oscillator
// waveform (with unison copies), the filter's response, the envelopes and LFO shapes. Edits are
// `putChannel` (engine fast path, held notes follow).
import { createEffect, createSignal, For, onCleanup, onMount, Show, type JSX } from 'solid-js';
import type { Channel, ParamDef, SynthCatalog } from '../api/types';
import { pFmt, pNorm, pVal } from '../lib/presets';
import { clampSynth, synthParam, synthValue, withSynthParam, withSynthPreset } from '../lib/synth';
import { commit, Gesture, wheelEdit } from '../state/edit';
import { chanById, chanOp } from '../state/pattern';
import { S } from '../state/store';
import { openMenu, say, type MenuItem } from '../state/ui';
import { Knob, Segmented } from '../ui/Knob';
import { rgba, T, themeVersion } from '../ui/theme';

type SynthCh = Channel & { kind: 'synth' };
type Ctx = { cat: SynthCatalog; ch: () => SynthCh; v: (id: string) => number; set: (id: string, x: number, label?: string) => void; def: (id: string) => ParamDef | undefined };

const C_OSC = '#a78bfa', C_FLT = '#f59e0b', C_ENV = '#34d399', C_LFO = '#60a5fa', C_FX = '#f472b6';
type Tab = 'osc' | 'mod' | 'fx';
const [lastTab, setLastTab] = createSignal<Tab>('osc');

export function SynthPanel(props: { c: Channel }) {
  const cat = () => S.catalog?.synth as SynthCatalog | undefined;
  return (
    <Show when={cat()} fallback={<div class="note" style={{ padding: '14px' }}>Katalog syntezatora niedostępny.</div>}>
      {c => <Panel cat={c()} c={props.c} />}
    </Show>
  );
}

function Panel(props: { cat: SynthCatalog; c: Channel }) {
  const ch = () => (chanById(props.c.id) ?? props.c) as SynthCh;
  const v = (id: string) => synthValue(props.cat, ch().synth, id);
  const def = (id: string) => synthParam(props.cat, id);
  const set = (id: string, x: number, label?: string) => {
    const d = def(id); if (!d) return;
    commit([chanOp(withSynthParam(props.cat, ch(), id, clampSynth(d, x)))], label ?? d.label);
  };
  const ctx: Ctx = { cat: props.cat, ch, v, set, def };
  const presetName = () => ch().synth.preset;
  const list = () => props.cat.presets;
  const load = (name: string) => { commit([chanOp(withSynthPreset(props.cat, ch(), name))], 'Preset syntezatora'); say('Preset: ' + name); };
  const step = (d: number) => {
    if (!list().length) return;
    const i = list().findIndex(p => p.name === presetName());
    load(list()[(i + d + list().length) % list().length].name);
  };
  const pick = (x: number, y: number) => {
    const items: MenuItem[] = [{ t: 'title', label: 'Presety syntezatora' }];
    for (const g of props.cat.categories) {
      const ps = props.cat.presets.filter(p => p.category === g);
      if (ps.length) items.push({ t: 'sub', label: g, items: ps.map(p => ({ t: 'item', label: p.name, kbd: p.name === presetName() ? '●' : '', act: () => load(p.name) }) as MenuItem) });
    }
    openMenu(x, y, items, 360);
  };
  return (
    <div class="syn">
      <div class="syn-top">
        <div class="syn-preset">
          <button class="arrow" title="Poprzedni preset" onClick={() => step(-1)}>‹</button>
          <button class="name" onClick={e => { const r = e.currentTarget.getBoundingClientRect(); pick(r.left, r.bottom + 4); }} title="Wybierz preset">
            <span class="ell">{presetName() ?? 'Własny dźwięk'}</span>
            <span class="cat">{list().find(p => p.name === presetName())?.category ?? 'edytowany'}</span>
          </button>
          <button class="arrow" title="Następny preset" onClick={() => step(1)}>›</button>
        </div>
        <div class="syn-tabs">
          <For each={[['osc', 'OSC'], ['mod', 'MOD'], ['fx', 'FX']] as [Tab, string][]}>{([id, l]) =>
            <button classList={{ on: lastTab() === id }} onClick={() => setLastTab(id)}>{l}</button>}</For>
        </div>
        <div class="syn-voice">
          <Choice ctx={ctx} id="v_mode" />
          <K ctx={ctx} id="v_glide" label="Glide" />
          <K ctx={ctx} id="fx_out" label="Wyjście" />
        </div>
      </div>
      <Show when={lastTab() === 'osc'}><OscTab ctx={ctx} /></Show>
      <Show when={lastTab() === 'mod'}><ModTab ctx={ctx} /></Show>
      <Show when={lastTab() === 'fx'}><FxTab ctx={ctx} /></Show>
    </div>
  );
}

// ------------------------------------------------------------------ tabs

function OscTab(props: { ctx: Ctx }) {
  const c = props.ctx;
  return (
    <div class="syn-grid osc">
      <Osc ctx={c} n={1} />
      <Osc ctx={c} n={2} />
      <Card title="Sub / Szum" color={C_OSC} class="sub">
        <div class="syn-row"><K ctx={c} id="sub_level" label="Sub" color={C_OSC} /><Choice ctx={c} id="sub_oct" /></div>
        <div class="syn-row"><K ctx={c} id="noise_level" label="Szum" color={C_OSC} /><K ctx={c} id="noise_color" label="Barwa" color={C_OSC} /></div>
      </Card>
      <Card title="Filtr" color={C_FLT} class="flt" head={<Choice ctx={c} id="flt_type" />}>
        <Plot draw={(g, w, h) => drawFilter(g, w, h, c)} deps={() => [c.v('flt_type'), c.v('flt_cutoff'), c.v('flt_res'), c.v('flt_drive')]} />
        <div class="syn-row">
          <K ctx={c} id="flt_cutoff" label="Cutoff" color={C_FLT} size="md" />
          <K ctx={c} id="flt_res" label="Rezonans" color={C_FLT} size="md" />
          <K ctx={c} id="flt_drive" label="Drive" color={C_FLT} />
          <K ctx={c} id="flt_env" label="Obwiednia" color={C_FLT} />
          <K ctx={c} id="flt_key" label="Key" color={C_FLT} />
        </div>
      </Card>
      <Card title="Unison" color={C_OSC} class="uni">
        <Choice ctx={c} id="uni_voices" />
        <div class="syn-row"><K ctx={c} id="uni_detune" label="Detune" color={C_OSC} /><K ctx={c} id="uni_width" label="Szerokość" color={C_OSC} /></div>
      </Card>
      <Card title="Obwiednia głośności" color={C_ENV} class="amp">
        <Plot draw={(g, w, h) => drawAdsr(g, w, h, c, 'amp')} deps={() => [c.v('amp_a'), c.v('amp_d'), c.v('amp_s'), c.v('amp_r')]} />
        <div class="syn-row">
          <K ctx={c} id="amp_a" label="Atak" color={C_ENV} /><K ctx={c} id="amp_d" label="Zanik" color={C_ENV} />
          <K ctx={c} id="amp_s" label="Podtrzym." color={C_ENV} /><K ctx={c} id="amp_r" label="Wybrzmienie" color={C_ENV} />
        </div>
      </Card>
    </div>
  );
}

function ModTab(props: { ctx: Ctx }) {
  const c = props.ctx;
  return (
    <div class="syn-grid mod">
      <Card title="Obwiednia modulacji" color={C_ENV} class="menv">
        <Plot draw={(g, w, h) => drawAdsr(g, w, h, c, 'mod')} deps={() => [c.v('mod_a'), c.v('mod_d'), c.v('mod_s'), c.v('mod_r')]} />
        <div class="syn-row">
          <K ctx={c} id="mod_a" label="Atak" color={C_ENV} /><K ctx={c} id="mod_d" label="Zanik" color={C_ENV} />
          <K ctx={c} id="mod_s" label="Podtrzym." color={C_ENV} /><K ctx={c} id="mod_r" label="Wybrzmienie" color={C_ENV} />
        </div>
      </Card>
      <For each={[1, 2]}>{n => (
        <Card title={'LFO ' + n} color={C_LFO} class={'lfo' + n} head={<Choice ctx={c} id={`l${n}_wave`} />}>
          <Plot draw={(g, w, h) => drawLfo(g, w, h, c.v(`l${n}_wave`))} deps={() => [c.v(`l${n}_wave`)]} />
          <div class="syn-row"><K ctx={c} id={`l${n}_rate`} label="Tempo" color={C_LFO} /><Choice ctx={c} id={`l${n}_sync`} /></div>
        </Card>
      )}</For>
      <Card title="Modulacja (źródło → cel)" color={C_LFO} class="matrix">
        <For each={[1, 2, 3, 4]}>{n => (
          <div class="syn-mrow" classList={{ off: c.v(`m${n}_src`) === 0 }}>
            <span class="slot">{n}</span>
            <Choice ctx={c} id={`m${n}_src`} bare />
            <span class="to">→</span>
            <Choice ctx={c} id={`m${n}_dst`} bare />
            <AmtBar ctx={c} id={`m${n}_amt`} />
          </div>
        )}</For>
      </Card>
    </div>
  );
}

function FxTab(props: { ctx: Ctx }) {
  const c = props.ctx;
  return (
    <div class="syn-grid fx">
      <Card title="Drive" color={C_FX}><div class="syn-row"><K ctx={c} id="fx_drive" label="Przester" color={C_FX} size="md" /></div></Card>
      <Card title="Chorus" color={C_FX}><div class="syn-row"><K ctx={c} id="fx_chorus" label="Ilość" color={C_FX} size="md" /></div></Card>
      <Card title="Delay" color={C_FX}>
        <div class="syn-row"><K ctx={c} id="fx_delay" label="Ilość" color={C_FX} size="md" /><K ctx={c} id="fx_dfb" label="Sprzężenie" color={C_FX} /></div>
        <Choice ctx={c} id="fx_dtime" />
      </Card>
      <Card title="Reverb" color={C_FX}><div class="syn-row"><K ctx={c} id="fx_reverb" label="Ilość" color={C_FX} size="md" /><K ctx={c} id="fx_rsize" label="Rozmiar" color={C_FX} /></div></Card>
    </div>
  );
}

function Osc(props: { ctx: Ctx; n: 1 | 2 }) {
  const c = props.ctx, p = 'o' + props.n + '_';
  return (
    <Card title={'Oscylator ' + props.n} color={C_OSC} class={'osc' + props.n} head={<Choice ctx={c} id={p + 'wave'} bare />}
      dim={c.v(p + 'level') <= 0}>
      <Plot draw={(g, w, h) => drawOsc(g, w, h, c, props.n)} deps={() => [c.v(p + 'wave'), c.v(p + 'shape'), c.v(p + 'level'), c.v('uni_voices'), c.v('uni_detune')]} />
      <div class="syn-row">
        <K ctx={c} id={p + 'level'} label="Głośność" color={C_OSC} size="md" />
        <K ctx={c} id={p + 'shape'} label="Kształt" color={C_OSC} />
        <K ctx={c} id={p + 'coarse'} label="Oktawa / st" color={C_OSC} />
        <Show when={props.n === 2}><K ctx={c} id="o2_fine" label="Fine" color={C_OSC} /></Show>
      </div>
    </Card>
  );
}

// ------------------------------------------------------------------ building blocks

function Card(props: { title: string; color: string; class?: string; head?: JSX.Element; dim?: boolean; children: JSX.Element }) {
  return (
    <div class={'syn-card ' + (props.class ?? '')} classList={{ dim: !!props.dim }} style={{ '--sc': props.color }}>
      <div class="syn-chd"><span class="t">{props.title}</span><div style={{ flex: 1 }} />{props.head}</div>
      {props.children}
    </div>
  );
}

function K(props: { ctx: Ctx; id: string; label: string; color?: string; size?: 'sm' | 'md' }) {
  const d = () => props.ctx.def(props.id);
  let g: Gesture | null = null;
  return (
    <Show when={d()}>{p => (
      <Knob key={'syn' + props.id} label={props.label} size={props.size ?? 'sm'} color={props.color}
        t={() => pNorm(p(), props.ctx.v(props.id))} text={() => pFmt(p(), props.ctx.v(props.id))}
        zero={() => (p().min < 0 && p().max > 0 && !p().log ? pNorm(p(), 0) : 0)}
        title={p().label + ' · przeciągnij ↕ · Shift = precyzyjnie · 2× klik = domyślna'}
        onStart={() => { g = new Gesture(props.label); }}
        onChange={t => { const ch = props.ctx.ch(); g?.update([chanOp(withSynthParam(props.ctx.cat, ch, props.id, pVal(p(), t)))]); say(props.label + ' ' + pFmt(p(), pVal(p(), t))); }}
        onEnd={() => { g?.end(); g = null; }}
        onReset={() => props.ctx.set(props.id, p().def, props.label)}
        onWheel={t => wheelEdit('syn' + props.id, props.label, [chanOp(withSynthParam(props.ctx.cat, props.ctx.ch(), props.id, pVal(p(), t)))])} />
    )}</Show>
  );
}

function Choice(props: { ctx: Ctx; id: string; bare?: boolean }) {
  const d = () => props.ctx.def(props.id);
  return (
    <Show when={d()?.options}>{opts => (
      <Show when={opts().length <= 5} fallback={
        <select class="sel syn-sel" title={d()!.label} value={String(Math.round(props.ctx.v(props.id)))} onChange={e => props.ctx.set(props.id, +e.currentTarget.value)}>
          <For each={opts()}>{(o, i) => <option value={String(i())}>{o}</option>}</For>
        </select>}>
        <Segmented small options={opts()} value={() => Math.round(props.ctx.v(props.id))} pick={i => props.ctx.set(props.id, i)} title={d()!.label} />
      </Show>
    )}</Show>
  );
}

/** Bipolar amount as a horizontal bar from the centre (drag ↔, 2× klik = 0). */
function AmtBar(props: { ctx: Ctx; id: string }) {
  const v = () => props.ctx.v(props.id);
  const down = (e: PointerEvent) => {
    if (e.button !== 0) return; e.preventDefault();
    const el = e.currentTarget as HTMLElement, r = el.getBoundingClientRect();
    if (e.detail === 2) { props.ctx.set(props.id, 0, 'Ilość modulacji'); return; }
    const g = new Gesture('Ilość modulacji');
    const at = (x: number) => { const t = Math.max(-1, Math.min(1, ((x - r.left) / r.width) * 2 - 1)); g.update([chanOp(withSynthParam(props.ctx.cat, props.ctx.ch(), props.id, Math.round(t * 100)))]); say('Ilość ' + Math.round(t * 100) + '%'); };
    at(e.clientX);
    const mv = (ev: PointerEvent) => at(ev.clientX);
    const up = () => { g.end(); window.removeEventListener('pointermove', mv); window.removeEventListener('pointerup', up); };
    window.addEventListener('pointermove', mv); window.addEventListener('pointerup', up);
  };
  return (
    <div class="syn-amt" onPointerDown={down} title="Ilość: przeciągnij w lewo / prawo · 2× klik = 0">
      <div class="fill" style={{ left: (v() < 0 ? 50 + v() / 2 : 50) + '%', width: Math.abs(v()) / 2 + '%' }} />
      <span>{(v() > 0 ? '+' : '') + Math.round(v())}%</span>
    </div>
  );
}

/** A canvas that redraws when its deps or the theme change. */
function Plot(props: { draw: (g: CanvasRenderingContext2D, w: number, h: number) => void; deps: () => unknown[] }) {
  let cv!: HTMLCanvasElement;
  const paint = () => {
    const w = cv.clientWidth, h = cv.clientHeight; if (!w || !h) return;
    const dpr = window.devicePixelRatio || 1;
    if (cv.width !== Math.round(w * dpr) || cv.height !== Math.round(h * dpr)) { cv.width = Math.round(w * dpr); cv.height = Math.round(h * dpr); }
    const g = cv.getContext('2d')!;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    g.clearRect(0, 0, w, h);
    props.draw(g, w, h);
  };
  onMount(() => { const ro = new ResizeObserver(paint); ro.observe(cv); onCleanup(() => ro.disconnect()); });
  createEffect(() => { props.deps(); themeVersion(); paint(); });
  return <canvas ref={cv} class="syn-plot" />;
}

// ------------------------------------------------------------------ drawings

function grid(g: CanvasRenderingContext2D, w: number, h: number, cols = 8, rows = 4) {
  g.fillStyle = T.viz; g.fillRect(0, 0, w, h);
  g.strokeStyle = T.vizGrid; g.lineWidth = 1; g.beginPath();
  for (let i = 1; i < cols; i++) { const x = Math.round(i * w / cols) + 0.5; g.moveTo(x, 0); g.lineTo(x, h); }
  for (let i = 1; i < rows; i++) { const y = Math.round(i * h / rows) + 0.5; g.moveTo(0, y); g.lineTo(w, y); }
  g.stroke();
}

/** One cycle of wave `w` (0..7) at phase x ∈ [0,1), morphed by shape s ∈ [0,1] (display approximation). */
function waveAt(w: number, s: number, x: number): number {
  const TAU = Math.PI * 2;
  switch (w) {
    case 0: { const k = 0.5 - s * 0.45, p = x < k ? x / k * 0.5 : 0.5 + (x - k) / (1 - k) * 0.5; return Math.sin(TAU * p); }
    case 1: { const k = 0.5 + s * 0.45; return x < k ? -1 + 2 * x / k : 1 - 2 * (x - k) / (1 - k); }
    case 2: { const y = 1 - 2 * x; return Math.tanh(y * (1 + s * 4)) / Math.tanh(1 + s * 4); }
    case 3: { const pw = 0.5 - s * 0.45; return x < pw ? 1 : -1; }
    case 4: return (Math.sin(TAU * x) + 0.6 * Math.sin(TAU * 3 * x) * (0.4 + s) + 0.3 * Math.sin(TAU * 5 * x)) / 1.6;
    case 5: { let y = 0; for (let k = 1; k <= 9; k++) y += Math.sin(TAU * k * x + k * k) / (k * (1.2 - s * 0.6)); return y * 0.55; }
    case 6: { const steps = Math.round(16 - s * 12); return Math.round(Math.sin(TAU * x) * steps / 2) / (steps / 2); }
    case 7: { let y = 0; for (const [f, a] of [[1, 1], [2.76, 0.6], [5.4, 0.4 + s * 0.4], [8.93, 0.25]] as const) y += a * Math.sin(TAU * f * x); return y / 2.2; }
    default: return Math.sin(TAU * x);
  }
}

function drawOsc(g: CanvasRenderingContext2D, w: number, h: number, c: Ctx, n: 1 | 2) {
  grid(g, w, h);
  const p = 'o' + n + '_', wave = Math.round(c.v(p + 'wave')), s = c.v(p + 'shape') / 100, lvl = c.v(p + 'level') / 100;
  const voices = [1, 3, 5, 7][Math.round(c.v('uni_voices'))] ?? 1, det = c.v('uni_detune') / 100;
  const mid = h / 2, amp = (h / 2 - 6) * Math.max(0.15, lvl);
  const path = (phase: number) => {
    g.beginPath();
    for (let i = 0; i <= w; i++) { const x = ((i / w) * 2 + phase) % 1; const y = mid - waveAt(wave, s, x) * amp; i ? g.lineTo(i, y) : g.moveTo(i, y); }
  };
  // unison copies: faint, phase-spread by the detune
  for (let k = 1; k < voices; k++) { path(((k * 0.618) % 1) * det * 0.5); g.strokeStyle = rgba(C_OSC, 0.18); g.lineWidth = 1.5; g.stroke(); }
  path(0);
  g.lineTo(w, mid); g.lineTo(0, mid); g.closePath();
  g.fillStyle = rgba(C_OSC, 0.12); g.fill();
  path(0); g.strokeStyle = C_OSC; g.lineWidth = 2; g.shadowColor = rgba(C_OSC, 0.6); g.shadowBlur = 6; g.stroke(); g.shadowBlur = 0;
}

function drawFilter(g: CanvasRenderingContext2D, w: number, h: number, c: Ctx) {
  grid(g, w, h, 10, 4);
  const type = Math.round(c.v('flt_type')), fc = c.v('flt_cutoff'), res = c.v('flt_res') / 100;
  const Q = 0.707 * Math.pow(18, res * res);
  const top = 18, bot = -36, ydb = (db: number) => (top - Math.max(bot, Math.min(top, db))) / (top - bot) * h;
  g.font = '9px monospace'; g.fillStyle = T.vizLabel;
  for (const f of [100, 1000, 10000]) { const x = Math.log(f / 20) / Math.log(1000) * w; g.fillText(f >= 1000 ? f / 1000 + 'k' : String(f), x + 3, h - 4); }
  g.beginPath();
  for (let i = 0; i <= w; i++) {
    const f = 20 * Math.pow(1000, i / w), x = f / fc;
    const re = 1 - x * x, im = x / Q, den = re * re + im * im;
    let mag2: number;
    if (type === 0) mag2 = 1 / den;
    else if (type === 1) mag2 = 1 / (den * den);
    else if (type === 2) mag2 = im * im / den;
    else if (type === 3) mag2 = (x ** 4) / den;
    else mag2 = (re * re) / den;
    const db = 10 * Math.log10(Math.max(1e-9, mag2));
    i ? g.lineTo(i, ydb(db)) : g.moveTo(i, ydb(db));
  }
  g.strokeStyle = C_FLT; g.lineWidth = 2; g.shadowColor = rgba(C_FLT, 0.6); g.shadowBlur = 6; g.stroke(); g.shadowBlur = 0;
  g.lineTo(w, h); g.lineTo(0, h); g.closePath(); g.fillStyle = rgba(C_FLT, 0.12); g.fill();
  const xc = Math.log(fc / 20) / Math.log(1000) * w;
  g.fillStyle = C_FLT; g.beginPath(); g.arc(xc, ydb(0), 4, 0, Math.PI * 2); g.fill();
}

function drawAdsr(g: CanvasRenderingContext2D, w: number, h: number, c: Ctx, k: 'amp' | 'mod') {
  grid(g, w, h, 8, 4);
  const a = c.v(k + '_a'), d = c.v(k + '_d'), s = c.v(k + '_s') / 100, r = c.v(k + '_r');
  // each stage gets width ∝ log of its time, the sustain a fixed hold
  const lw = (t: number) => Math.log10(1 + t);
  const hold = 1.4, tot = lw(a) + lw(d) + hold + lw(r);
  const sx = (w - 8) / tot, x0 = 4, top = 8, bot = h - 6, y = (lv: number) => bot - lv * (bot - top);
  const xa = x0 + lw(a) * sx, xd = xa + lw(d) * sx, xs = xd + hold * sx, xr = xs + lw(r) * sx;
  g.beginPath(); g.moveTo(x0, y(0));
  for (let i = 1; i <= 24; i++) { const t = i / 24; g.lineTo(x0 + (xa - x0) * t, y(1 - Math.pow(1 - t, 2.2))); }
  for (let i = 1; i <= 24; i++) { const t = i / 24; g.lineTo(xa + (xd - xa) * t, y(s + (1 - s) * Math.pow(1 - t, 3))); }
  g.lineTo(xs, y(s));
  for (let i = 1; i <= 24; i++) { const t = i / 24; g.lineTo(xs + (xr - xs) * t, y(s * Math.pow(1 - t, 3))); }
  g.strokeStyle = C_ENV; g.lineWidth = 2; g.shadowColor = rgba(C_ENV, 0.6); g.shadowBlur = 6; g.stroke(); g.shadowBlur = 0;
  g.lineTo(xr, bot); g.lineTo(x0, bot); g.closePath(); g.fillStyle = rgba(C_ENV, 0.12); g.fill();
  g.fillStyle = C_ENV;
  for (const [px, py] of [[xa, y(1)], [xd, y(s)], [xs, y(s)], [xr, y(0)]]) { g.beginPath(); g.arc(px, py, 3.5, 0, Math.PI * 2); g.fill(); }
  g.font = '9px monospace'; g.fillStyle = T.vizLabel;
  g.fillText('A', (x0 + xa) / 2 - 3, h - 2); g.fillText('D', (xa + xd) / 2 - 3, h - 2); g.fillText('S', (xd + xs) / 2 - 3, h - 2); g.fillText('R', (xs + xr) / 2 - 3, h - 2);
}

function drawLfo(g: CanvasRenderingContext2D, w: number, h: number, wave: number) {
  grid(g, w, h, 8, 2);
  const mid = h / 2, amp = h / 2 - 8, wv = Math.round(wave);
  const hold = [0.6, -0.3, 0.9, -0.7, 0.2, -0.9, 0.5, 0.1];
  g.beginPath();
  for (let i = 0; i <= w; i++) {
    const x = (i / w) * 2 % 1, cyc = Math.floor((i / w) * 2);
    const y = wv === 0 ? Math.sin(Math.PI * 2 * x) : wv === 1 ? 1 - 4 * Math.abs(x - 0.5) : wv === 2 ? 1 - 2 * x : wv === 3 ? (x < 0.5 ? 1 : -1) : hold[(cyc * 4 + Math.floor(x * 4)) % hold.length];
    const py = mid - y * amp; i ? g.lineTo(i, py) : g.moveTo(i, py);
  }
  g.strokeStyle = C_LFO; g.lineWidth = 2; g.shadowColor = rgba(C_LFO, 0.6); g.shadowBlur = 6; g.stroke(); g.shadowBlur = 0;
}

