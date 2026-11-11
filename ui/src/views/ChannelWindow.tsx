// Channel settings window (FL "Channel settings"): click a Channel Rack name to see what makes
// the sound — the sample with its waveform, or the built-in instrument — plus level, pan, root
// key and mixer routing. The synth and WAVE Keys have their own pages here too.
import { createEffect, createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import { be } from '../api/backend';
import { VOL_UNITY, type AutoTarget, type Channel, type Id } from '../api/types';
import { baseName, clamp, panText, volText } from '../lib/format';
import { lang, t, type TranslationKey } from '../lib/i18n';
import { noteName } from '../lib/pitch';
import { autoDisplay, controlMenu, isAutomated, paramMenu } from '../state/automation';
import { commit, Gesture, wheelEdit } from '../state/edit';
import { audition, chanById, chanOp, chanSource } from '../state/pattern';
import { getPeaks, peakRange, peaksVersion } from '../state/peaks';
import { insName, S } from '../state/store';
import { say } from '../state/ui';
import { fitCanvas } from '../ui/frameLoop';
import { channelIcon, InstIcon } from '../ui/instIcons';
import { Knob } from '../ui/Knob';
import { PianoKeys } from '../ui/PianoKeys';
import { rgba, T, themeVersion } from '../ui/theme';
import { moveWindow } from './FxWindow';
import { KeysPanel } from './KeysPanel';
import { SynthPanel } from './SynthPanel';

type Win = { id: Id; x: number; y: number; w: number };
const [win, setWin] = createSignal<Win | null>(null);
export { win as chanWin };

/** Open the channel's window, or close it when it is already showing that channel. */
export function toggleChannelWindow(id: Id) {
  const w = win();
  if (w && w.id === id) { setWin(null); return; }
  const kind = chanById(id)?.kind, synth = kind === 'synth', wd = synth ? 900 : kind === 'keys' ? 700 : 520;
  setWin(w ? { ...w, id, w: wd } : { id, x: Math.max(20, window.innerWidth / 2 - wd / 2), y: synth ? 70 : 110, w: wd });
}
export const closeChannelWindow = () => setWin(null);

const INST_TEXT: Record<string, TranslationKey> = {
  kick: 'b.cw.inst.kick',
  clap: 'b.cw.inst.clap',
  hat: 'b.cw.inst.hat',
  bass: 'b.cw.inst.bass',
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
  const knob = (lab: TranslationKey, undoLab: TranslationKey, tv: () => number, text: () => string, toPatch: (v: number) => Partial<Channel>, def: number, zero?: number, target?: () => AutoTarget) => {
    let g: Gesture | null = null;
    const reset = () => commit([chanOp(live(), toPatch(def))], t(undoLab));
    return (
      <Knob key={'cw' + lab + props.c.id} label={t(lab)} title={t('b.cw.knobTitle')} t={tv} text={text}
        automated={() => !!target && isAutomated(target())}
        onMenu={(x, y) => target ? paramMenu(x, y, target(), reset, c().color) : controlMenu(x, y, c().name + ' · ' + t(lab), reset, t('a.au.noRoot'))}
        zero={zero != null ? () => zero : undefined}
        onStart={() => { g = new Gesture(t(undoLab)); }}
        onChange={v => { g?.update([chanOp(live(), toPatch(v))]); say(c().name + ' · ' + t(lab).toLowerCase() + ' ' + text()); }}
        onEnd={() => { g?.end(); g = null; }}
        onReset={reset}
        onWheel={v => wheelEdit('cw' + lab, t(undoLab), [chanOp(live(), toPatch(v))])} />
    );
  };
  const ROOT_LO = 24, ROOT_HI = 96;
  // volume and pan follow their automation while it plays
  const volT = () => ({ kind: 'chanVol' as const, id: c().id }), panT = () => ({ kind: 'chanPan' as const, id: c().id });
  const vol = () => autoDisplay(volT()) ?? c().vol, pan = () => autoDisplay(panT()) ?? c().pan;
  const replaceSample = async () => {
    try {
      const ps = await be().pickAudioFiles(); if (!ps?.length) return;
      const r = await be().importAudio({ paths: [ps[0]] });
      const id = r.sourceIds[0]; if (id == null) return;
      for (let k = 0; k < 50 && !S.project.sources.some(s => s.id === id); k++) await new Promise(res => setTimeout(res, 20));
      const s = S.project.sources.find(x => x.id === id);
      commit([chanOp(live(), { source: id, name: s ? s.name : c().name } as Partial<Channel>)], t('b.cw.changeSample'));
    } catch (e) { say(t('b.cw.changeSampleErr', { err: String(e) })); }
  };
  return (
    <div class="win chanwin" style={{ left: props.w.x + 'px', top: props.w.y + 'px', width: props.w.w + 'px', 'z-index': 47 }}>
      <div class="winhd" onPointerDown={e => moveWindow(e, () => props.w, setPos)}>
        <span class="chicon" style={{ color: c().color }}><InstIcon id={channelIcon(c())} size={14} /></span>
        <span class="title">{c().name}</span>
        <span class="sub">{t(c().kind === 'sample' ? 'b.cw.kSampler' : c().kind === 'synth' ? 'b.cw.kSynth' : c().kind === 'keys' ? 'b.cw.kKeys' : 'b.cw.kInst')}</span>
        <div style={{ flex: 1 }} />
        <button class="iconbtn" title={t('b.cw.previewTitle')} onClick={() => audition(c().id, undefined, 0.9, true)}>▶</button>
        <button class="closebtn" title={t('b.cw.close')} onClick={closeChannelWindow}>×</button>
      </div>
      <Show when={c().kind === 'synth'}><SynthPanel c={c()} /></Show>
      <Show when={c().kind === 'keys' && c()}>{k => <KeysPanel c={k() as Extract<Channel, { kind: 'keys' }>} />}</Show>
      <Show when={c().kind === 'sample'} fallback={c().kind === 'synth' || c().kind === 'keys' ? null :
        <div class="chinst">
          <span class="big" style={{ color: c().color }}><InstIcon id={channelIcon(c())} size={34} /></span>
          <div>
            <div class="nm">{instOf(c()).toUpperCase()}</div>
            <div class="note">{INST_TEXT[instOf(c())] ? t(INST_TEXT[instOf(c())]) : ''}</div>
          </div>
        </div>
      }>
        <Wave c={c()} />
        <div class="chsrc">
          <span class="ell" title={src()?.path ?? ''}>{src() ? baseName(src()!.path) : t('b.cw.noFile')}</span>
          <Show when={src()}>{s => <span class="mono">{s().dur.toFixed(2)} s · {Math.round(s().sr / 100) / 10} kHz · {s().ch === 1 ? 'mono' : 'stereo'}</span>}</Show>
          <button class="smallbtn" onClick={replaceSample} title={t('b.cw.changeSampleTitle')}>{t('b.cw.changeSampleBtn')}</button>
        </div>
      </Show>
      <div class="chknobs">
        {knob('b.cw.vol', 'b.cw.volCh', () => vol(), () => volText(vol()), t => ({ vol: Math.round(clamp(t, 0, 1) * 10000) / 10000 }), VOL_UNITY, undefined, volT)}
        {knob('b.cw.pan', 'b.cw.panCh', () => (pan() + 1) / 2, () => panText(pan()), t => ({ pan: Math.round(clamp(t * 2 - 1, -1, 1) * 100) / 100 }), 0.5, 0.5, panT)}
        {knob('b.cw.root', 'b.cw.rootCh', () => (c().root - ROOT_LO) / (ROOT_HI - ROOT_LO), () => noteName(c().root), t => ({ root: Math.round(ROOT_LO + clamp(t, 0, 1) * (ROOT_HI - ROOT_LO)) }), (60 - ROOT_LO) / (ROOT_HI - ROOT_LO))}
        <div class="chroute">
          <label>{t('b.cw.mixer')}</label>
          <select class="sel" value={c().insert == null ? '' : String(c().insert)}
            onChange={e => commit([chanOp(live(), { insert: e.currentTarget.value === '' ? null : +e.currentTarget.value })], t('b.pe.mixCh'))}>
            <option value="">{t('b.cw.likeClipTrack')}</option>
            <For each={S.project.inserts.filter(m => m.id !== 0)}>{m => <option value={String(m.id)}>{insName(m)}</option>}</For>
          </select>
        </div>
      </div>
      <Show when={c().kind === 'keys'}><PianoKeys ch={c().id} lo={21} hi={108} /></Show>
      <Show when={c().kind === 'sample'}><PianoKeys ch={c().id} lo={36} hi={96} mark={c().root} /></Show>
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
    if (!pk) { g.fillStyle = T.fg5; g.font = '11px sans-serif'; g.fillText(t('b.cw.loading'), 10, h / 2 - 6); return; }
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
  createEffect(() => { peaksVersion(); themeVersion(); lang(); void props.c.color; void (props.c.kind === 'sample' && props.c.source); draw(); });
  // A click plays the sample through the channel (its volume, pan and mixer insert) at the root
  // pitch, to its end, like a key of the keyboard below; another click restarts it.
  const play = (e: PointerEvent) => {
    if (e.button !== 0) return;
    e.preventDefault();
    const c = props.c;
    be().typingNote({ ch: c.id, key: c.root, vel: 0.9, on: true }).catch(err => say(t('b.pk.fail', { err: String(err) })));
  };
  return <canvas ref={cv} class="chwave" title={t('b.cw.waveTip')} onPointerDown={play} />;
}
