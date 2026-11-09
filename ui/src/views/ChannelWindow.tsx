// Channel settings window (FL "Channel settings"): click a Channel Rack name to see what makes
// the sound — the sample with its waveform, or the built-in instrument — plus level, pan, root
// key and mixer routing. The synth's own page will live here too.
import { createEffect, createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import { be } from '../api/backend';
import { VOL_UNITY, type Channel, type Id } from '../api/types';
import { baseName, clamp, panText, volText } from '../lib/format';
import { noteName } from '../lib/pitch';
import { commit, Gesture, wheelEdit } from '../state/edit';
import { audition, chanById, chanOp, chanSource } from '../state/pattern';
import { getPeaks, peakRange, peaksVersion } from '../state/peaks';
import { insName, S } from '../state/store';
import { say } from '../state/ui';
import { fitCanvas } from '../ui/frameLoop';
import { channelIcon, InstIcon } from '../ui/instIcons';
import { Knob } from '../ui/Knob';
import { rgba, T, themeVersion } from '../ui/theme';
import { moveWindow } from './FxWindow';

type Win = { id: Id; x: number; y: number; w: number };
const [win, setWin] = createSignal<Win | null>(null);
export { win as chanWin };

/** Open the channel's window, or close it when it is already showing that channel. */
export function toggleChannelWindow(id: Id) {
  const w = win();
  if (w && w.id === id) { setWin(null); return; }
  setWin(w ? { ...w, id } : { id, x: Math.max(20, window.innerWidth / 2 - 230), y: 110, w: 460 });
}
export const closeChannelWindow = () => setWin(null);

const INST_TEXT: Record<string, string> = {
  kick: 'Stopa z syntezy: opadająca sinusoida z krótkim klikiem.',
  clap: 'Klaśnięcie z syntezy: kilka szybkich impulsów szumu z pogłosem.',
  hat: 'Hi-hat z syntezy: filtrowany szum metaliczny, krótki.',
  bass: 'Prosty bas z syntezy: grany z piano rolla, wysokość = nuta.',
};

const instOf = (c: Channel): string => (c.kind === 'inst' ? c.inst : '');

export function ChannelWindow() {
  return (
    <Show when={win()}>
      {w => (
        <Show when={chanById(w().id)} fallback={<>{void closeChannelWindow()}</>}>
          {c => <Body w={w()} c={c()} />}
        </Show>
      )}
    </Show>
  );
}

function Body(props: { w: Win; c: Channel }) {
  const c = () => chanById(props.c.id) ?? props.c;
  const live = () => chanById(props.c.id)!;
  const src = () => chanSource(c());
  const setPos = (x: number, y: number) => setWin(v => v && { ...v, x, y });
  const knob = (label: string, t: () => number, text: () => string, toPatch: (t: number) => Partial<Channel>, def: number, zero?: number) => {
    let g: Gesture | null = null;
    return (
      <Knob key={'cw' + label + props.c.id} label={label} title="Przeciągnij ↕ · Shift = precyzyjnie · 2× klik = reset" t={t} text={text}
        zero={zero != null ? () => zero : undefined}
        onStart={() => { g = new Gesture(label + ' kanału'); }}
        onChange={v => { g?.update([chanOp(live(), toPatch(v))]); say(c().name + ' · ' + label.toLowerCase() + ' ' + text()); }}
        onEnd={() => { g?.end(); g = null; }}
        onReset={() => commit([chanOp(live(), toPatch(def))], label + ' kanału')}
        onWheel={v => wheelEdit('cw' + label, label + ' kanału', [chanOp(live(), toPatch(v))])} />
    );
  };
  const ROOT_LO = 24, ROOT_HI = 96;
  const replaceSample = async () => {
    try {
      const ps = await be().pickAudioFiles(); if (!ps?.length) return;
      const r = await be().importAudio({ paths: [ps[0]] });
      const id = r.sourceIds[0]; if (id == null) return;
      for (let k = 0; k < 50 && !S.project.sources.some(s => s.id === id); k++) await new Promise(res => setTimeout(res, 20));
      const s = S.project.sources.find(x => x.id === id);
      commit([chanOp(live(), { source: id, name: s ? s.name : c().name } as Partial<Channel>)], 'Zmień sample');
    } catch (e) { say('Nie zmieniono sampla: ' + String(e)); }
  };
  return (
    <div class="win chanwin" style={{ left: props.w.x + 'px', top: props.w.y + 'px', width: props.w.w + 'px', 'z-index': 47 }}>
      <div class="winhd" onPointerDown={e => moveWindow(e, () => props.w, setPos)}>
        <span class="chicon" style={{ color: c().color }}><InstIcon id={channelIcon(c())} size={14} /></span>
        <span class="title">{c().name}</span>
        <span class="sub">{c().kind === 'sample' ? 'Sampler' : 'Wbudowany instrument'}</span>
        <div style={{ flex: 1 }} />
        <button class="iconbtn" title="Odsłuchaj (C5 / wysokość bazowa)" onClick={() => audition(c().id, undefined, 0.9, true)}>▶</button>
        <button class="closebtn" title="Zamknij" onClick={closeChannelWindow}>×</button>
      </div>
      <Show when={c().kind === 'sample'} fallback={
        <div class="chinst">
          <span class="big" style={{ color: c().color }}><InstIcon id={channelIcon(c())} size={34} /></span>
          <div>
            <div class="nm">{instOf(c()).toUpperCase()}</div>
            <div class="note">{INST_TEXT[instOf(c())] ?? ''}</div>
          </div>
        </div>
      }>
        <Wave c={c()} />
        <div class="chsrc">
          <span class="ell" title={src()?.path ?? ''}>{src() ? baseName(src()!.path) : 'brak pliku'}</span>
          <Show when={src()}>{s => <span class="mono">{s().dur.toFixed(2)} s · {Math.round(s().sr / 100) / 10} kHz · {s().ch === 1 ? 'mono' : 'stereo'}</span>}</Show>
          <button class="smallbtn" onClick={replaceSample} title="Wybierz inny plik audio dla tego kanału">Zmień sample…</button>
        </div>
      </Show>
      <div class="chknobs">
        {knob('Głośność', () => c().vol, () => volText(c().vol), t => ({ vol: Math.round(clamp(t, 0, 1) * 10000) / 10000 }), VOL_UNITY)}
        {knob('Panorama', () => (c().pan + 1) / 2, () => panText(c().pan), t => ({ pan: Math.round(clamp(t * 2 - 1, -1, 1) * 100) / 100 }), 0.5, 0.5)}
        {knob('Nuta bazowa', () => (c().root - ROOT_LO) / (ROOT_HI - ROOT_LO), () => noteName(c().root), t => ({ root: Math.round(ROOT_LO + clamp(t, 0, 1) * (ROOT_HI - ROOT_LO)) }), (60 - ROOT_LO) / (ROOT_HI - ROOT_LO))}
        <div class="chroute">
          <label>Mikser</label>
          <select class="sel" value={c().insert == null ? '' : String(c().insert)}
            onChange={e => commit([chanOp(live(), { insert: e.currentTarget.value === '' ? null : +e.currentTarget.value })], 'Kanał miksera')}>
            <option value="">Jak ścieżka klipu</option>
            <For each={S.project.inserts.filter(m => m.id !== 0)}>{m => <option value={String(m.id)}>{insName(m)}</option>}</For>
          </select>
        </div>
      </div>
    </div>
  );
}

function Wave(props: { c: Channel }) {
  let cv!: HTMLCanvasElement;
  const draw = () => {
    if (!cv) return;
    const w = cv.clientWidth, h = cv.clientHeight; if (!w || !h) return;
    fitCanvas(cv, w, h);
    const g = cv.getContext('2d')!, dpr = window.devicePixelRatio || 1;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    g.clearRect(0, 0, w, h);
    g.fillStyle = T.viz; g.fillRect(0, 0, w, h);
    g.fillStyle = T.vizGrid; g.fillRect(0, Math.round(h / 2), w, 1);
    const s = props.c.kind === 'sample' ? props.c.source : null;
    const pk = s != null ? getPeaks(s) : null;
    if (!pk) { g.fillStyle = T.fg5; g.font = '11px sans-serif'; g.fillText('Wczytywanie…', 10, h / 2 - 6); return; }
    const n = pk.length >> 1, mid = h / 2, amp = h / 2 - 4;
    g.fillStyle = rgba(props.c.color, 0.85);
    for (let x = 0; x < w; x++) {
      const [mn, mx] = peakRange(pk, Math.floor(x / w * n), Math.floor((x + 1) / w * n));
      g.fillRect(x, mid - mx * amp, 1, Math.max(1, (mx - mn) * amp));
    }
  };
  onMount(() => {
    const ro = new ResizeObserver(draw); ro.observe(cv);
    onCleanup(() => ro.disconnect());
  });
  createEffect(() => { peaksVersion(); themeVersion(); void props.c.color; void (props.c.kind === 'sample' && props.c.source); draw(); });
  return <canvas ref={cv} class="chwave" />;
}
