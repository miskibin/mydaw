// Channel settings window (FL "Channel settings"): click a Channel Rack name to see what makes
// the sound — the sampler with its waveform and settings, the synth, or WAVE Keys — plus level,
// pan, root key, mixer routing and a keyboard to play it.
import { createSignal, For, Show } from 'solid-js';
import { VOL_UNITY, type AutoTarget, type Channel, type Id } from '../api/types';
import { clamp, panText, volText } from '../lib/format';
import { t, type TranslationKey } from '../lib/i18n';
import { noteName } from '../lib/pitch';
import { autoDisplay, controlMenu, isAutomated, paramMenu } from '../state/automation';
import { commit, Gesture, wheelEdit } from '../state/edit';
import { audition, chanById, chanOp } from '../state/pattern';
import { insName, S } from '../state/store';
import { say } from '../state/ui';
import { channelIcon, InstIcon } from '../ui/instIcons';
import { Knob } from '../ui/Knob';
import { PianoKeys } from '../ui/PianoKeys';
import { moveWindow } from './FxWindow';
import { KeysPanel } from './KeysPanel';
import { SamplerPanel } from './SamplerPanel';
import { SynthPanel } from './SynthPanel';

type Win = { id: Id; x: number; y: number; w: number };
const [win, setWin] = createSignal<Win | null>(null);
export { win as chanWin };

/** Open the channel's window, or close it when it is already showing that channel. */
export function toggleChannelWindow(id: Id) {
  const w = win();
  if (w && w.id === id) { setWin(null); return; }
  const kind = chanById(id)?.kind, synth = kind === 'synth', wd = synth ? 900 : kind === 'keys' ? 700 : 560;
  setWin(w ? { ...w, id, w: wd } : { id, x: Math.max(20, window.innerWidth / 2 - wd / 2), y: synth ? 70 : 110, w: wd });
}
export const closeChannelWindow = () => setWin(null);

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
  return (
    <div class="win chanwin" style={{ left: props.w.x + 'px', top: props.w.y + 'px', width: props.w.w + 'px', 'z-index': 47 }}>
      <div class="winhd" onPointerDown={e => moveWindow(e, () => props.w, setPos)}>
        <span class="chicon" style={{ color: c().color }}><InstIcon id={channelIcon(c())} size={14} /></span>
        <span class="title">{c().name}</span>
        <span class="sub">{t(c().kind === 'sample' ? 'b.cw.kSampler' : c().kind === 'synth' ? 'b.cw.kSynth' : 'b.cw.kKeys')}</span>
        <div style={{ flex: 1 }} />
        <button class="iconbtn" title={t('b.cw.previewTitle')} onClick={() => audition(c().id, undefined, 0.9, true)}>▶</button>
        <button class="closebtn" title={t('b.cw.close')} onClick={closeChannelWindow}>×</button>
      </div>
      <Show when={c().kind === 'synth'}><SynthPanel c={c()} /></Show>
      <Show when={c().kind === 'keys' && c()}>{k => <KeysPanel c={k() as Extract<Channel, { kind: 'keys' }>} />}</Show>
      <Show when={c().kind === 'sample' && c()}>{k => <SamplerPanel c={k() as Extract<Channel, { kind: 'sample' }>} />}</Show>
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
