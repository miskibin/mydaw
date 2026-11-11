// The sampler's page of the channel window (FL "Channel settings", Ableton "Simpler"): the
// sample's waveform with the part that plays, how it plays (one-shot or gate, reverse,
// normalise, cut group) and its pitch and volume envelope. Every control is one undo step.
import { createEffect, onCleanup, onMount, Show } from 'solid-js';
import { SAMPLER, samplerDefault, type Channel, type SamplerState } from '../api/types';
import { baseName, clamp } from '../lib/format';
import { lang, t, type TranslationKey } from '../lib/i18n';
import { attackFromT, attackToT, decayFromT, decayToT, envelopePoints, msText, releaseFromT, releaseToT } from '../lib/sampler';
import { commit, Gesture, wheelEdit } from '../state/edit';
import { chanById, chanOp, chanSource, replaceWithFile } from '../state/pattern';
import { getPeaks, peakRange, peaksVersion } from '../state/peaks';
import { say } from '../state/ui';
import { track } from '../ui/drag';
import { fitCanvas } from '../ui/frameLoop';
import { Knob } from '../ui/Knob';
import { rgba, T, themeVersion } from '../ui/theme';

type Smp = Extract<Channel, { kind: 'sample' }>;

/** The channel as it is now (a drag keeps editing the live one, not the one it started with). */
const live = (id: number): Smp | undefined => { const c = chanById(id); return c?.kind === 'sample' ? c : undefined; };
const settings = (c: Smp): SamplerState => c.sampler ?? samplerDefault();
const patchOp = (c: Smp, patch: Partial<SamplerState>) => chanOp(c, { sampler: { ...settings(c), ...patch } } as Partial<Channel>);

export function SamplerPanel(props: { c: Smp }) {
  const c = () => live(props.c.id) ?? props.c;
  const s = () => settings(c());
  const src = () => chanSource(c());
  const set = (patch: Partial<SamplerState>, label: TranslationKey, heard: string) => {
    commit([patchOp(c(), patch)], t(label));
    say(c().name + ' · ' + heard);
  };
  const knob = (key: string, lab: TranslationKey, title: TranslationKey, tv: () => number, text: () => string, toPatch: (v: number) => Partial<SamplerState>, def: number, zero?: number, dim?: () => boolean) => {
    let g: Gesture | null = null;
    const reset = () => commit([patchOp(c(), toPatch(def))], t(lab));
    return (
      <div classList={{ smpdim: !!dim?.() }}>
        <Knob key={'smp' + key + props.c.id} label={t(lab)} title={t(title)} t={tv} text={text} zero={zero != null ? () => zero : undefined}
          onStart={() => { g = new Gesture(t(lab)); }}
          onChange={v => { g?.update([patchOp(c(), toPatch(v))]); say(c().name + ' · ' + t(lab).toLowerCase() + ' ' + text()); }}
          onEnd={() => { g?.end(); g = null; }}
          onReset={reset}
          onWheel={v => wheelEdit('smp' + key, t(lab), [patchOp(c(), toPatch(v))])} />
      </div>
    );
  };
  const semiSpan = SAMPLER.semi.max - SAMPLER.semi.min, fineSpan = SAMPLER.fine.max - SAMPLER.fine.min;
  return (
    <>
      <Wave c={c()} />
      <div class="chsrc">
        <span class="ell" title={src()?.path ?? ''}>{src() ? baseName(src()!.path) : t('b.cw.noFile')}</span>
        <Show when={src()}>{x => <span class="mono">{(x().dur * (s().end - s().start)).toFixed(2)} s · {Math.round(x().sr / 100) / 10} kHz · {x().ch === 1 ? 'mono' : 'stereo'}</span>}</Show>
        <button class="smallbtn" onClick={() => void replaceWithFile(c())} title={t('b.cw.changeSampleTitle')}>{t('b.cw.changeSampleBtn')}</button>
      </div>
      <div class="smprow">
        <div class="seg smpmode" role="radiogroup" aria-label={t('b.sm.mode')}>
          <button role="radio" aria-checked={!s().gate} classList={{ on: !s().gate }} title={t('b.sm.oneshotTitle')} onClick={() => s().gate && set({ gate: false }, 'b.sm.mode', t('b.sm.oneshot'))}>{t('b.sm.oneshot')}</button>
          <button role="radio" aria-checked={s().gate} classList={{ on: s().gate }} title={t('b.sm.gateTitle')} onClick={() => !s().gate && set({ gate: true }, 'b.sm.mode', t('b.sm.gate'))}>{t('b.sm.gate')}</button>
        </div>
        <button class="smptog" aria-pressed={s().reverse} classList={{ on: s().reverse }} title={t('b.sm.reverseTitle')} onClick={() => set({ reverse: !s().reverse }, 'b.sm.reverse', t(s().reverse ? 'b.sm.reverseOff' : 'b.sm.reverseOn'))}>{t('b.sm.reverse')}</button>
        <button class="smptog" aria-pressed={s().norm} classList={{ on: s().norm }} title={t('b.sm.normTitle')} onClick={() => set({ norm: !s().norm }, 'b.sm.norm', t(s().norm ? 'b.sm.normOff' : 'b.sm.normOn'))}>{t('b.sm.norm')}</button>
        <label class="smpcut" title={t('b.sm.cutTitle')}>
          <span>{t('b.sm.cut')}</span>
          <select class="sel" value={String(s().cut)} onChange={e => { const cut = +e.currentTarget.value; set({ cut }, 'b.sm.cut', cut ? t('b.sm.cutSay', { n: cut }) : t('b.sm.cutNoneSay')); }}>
            <option value="0">{t('b.sm.cutNone')}</option>
            {Array.from({ length: SAMPLER.cutGroups }, (_, i) => <option value={String(i + 1)}>{i + 1}</option>)}
          </select>
        </label>
      </div>
      <div class="smpknobs">
        <div class="smpgrp">
          {knob('semi', 'b.sm.pitch', 'b.sm.pitchTitle', () => (s().semi - SAMPLER.semi.min) / semiSpan, () => (s().semi > 0 ? '+' : '') + s().semi + ' st', v => ({ semi: Math.round(SAMPLER.semi.min + clamp(v, 0, 1) * semiSpan) }), 0.5, 0.5)}
          {knob('fine', 'b.sm.fine', 'b.sm.fineTitle', () => (s().fine - SAMPLER.fine.min) / fineSpan, () => (s().fine > 0 ? '+' : '') + Math.round(s().fine) + ' ct', v => ({ fine: Math.round(SAMPLER.fine.min + clamp(v, 0, 1) * fineSpan) }), 0.5, 0.5)}
        </div>
        <div class="smpgrp">
          {knob('att', 'b.sm.attack', 'b.sm.attackTitle', () => attackToT(s().attack), () => msText(s().attack), v => ({ attack: attackFromT(v) }), 0)}
          {knob('dec', 'b.sm.decay', 'b.sm.decayTitle', () => decayToT(s().decay), () => msText(s().decay), v => ({ decay: decayFromT(v) }), 0)}
          {knob('sus', 'b.sm.sustain', 'b.sm.sustainTitle', () => s().sustain, () => Math.round(s().sustain * 100) + ' %', v => ({ sustain: Math.round(clamp(v, 0, 1) * 100) / 100 }), 1)}
          {knob('rel', 'b.sm.release', s().gate ? 'b.sm.releaseTitle' : 'b.sm.releaseOff', () => releaseToT(s().release), () => msText(s().release), v => ({ release: releaseFromT(v) }), releaseToT(SAMPLER.release.def), undefined, () => !s().gate)}
        </div>
      </div>
    </>
  );
}

/** Pixels around a start/end marker that grab it. */
const GRAB = 7;

/** The waveform: the part that plays is bright, the rest dimmed; drag the two markers to trim. */
function Wave(props: { c: Smp }) {
  let cv!: HTMLCanvasElement;
  const s = () => settings(live(props.c.id) ?? props.c);
  const draw = () => {
    if (!cv) return;
    const w = cv.clientWidth, h = cv.clientHeight; if (!w || !h) return;
    fitCanvas(cv, w, h);
    const g = cv.getContext('2d')!, dpr = window.devicePixelRatio || 1;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    g.clearRect(0, 0, w, h);
    g.fillStyle = T.viz; g.fillRect(0, 0, w, h);
    g.fillStyle = T.vizGrid; g.fillRect(0, Math.round(h / 2), w, 1);
    const pk = getPeaks(props.c.source);
    if (!pk) { g.fillStyle = T.fg5; g.font = '11px sans-serif'; g.fillText(t('b.cw.loading'), 10, h / 2 - 6); return; }
    const st = s(), x0 = st.start * w, x1 = st.end * w;
    const n = pk.length >> 1, mid = h / 2, amp = h / 2 - 4;
    for (let x = 0; x < w; x++) {
      const [mn, mx] = peakRange(pk, Math.floor(x / w * n), Math.floor((x + 1) / w * n));
      g.fillStyle = rgba(props.c.color, x + 1 > x0 && x < x1 ? 0.9 : 0.22);
      g.fillRect(x, mid - mx * amp, 1, Math.max(1, (mx - mn) * amp));
    }
    // the volume envelope over the part, in the direction it plays
    const src = chanSource(props.c), partSec = (src?.dur ?? 0) * (st.end - st.start) / 2 ** ((st.semi + st.fine / 100) / 12);
    const pts = envelopePoints(st, partSec);
    if (pts.length && x1 - x0 > 2) {
      g.strokeStyle = T.fgHi; g.globalAlpha = 0.75; g.lineWidth = 1.25; g.beginPath();
      pts.forEach(([u, v], i) => {
        const x = st.reverse ? x1 - u * (x1 - x0) : x0 + u * (x1 - x0), y = 4 + (1 - v) * (h - 8);
        if (i) g.lineTo(x, y); else g.moveTo(x, y);
      });
      g.stroke(); g.globalAlpha = 1;
    }
    // the markers: start (solid flag at the top) and end (flag at the bottom)
    g.fillStyle = T.fgHi;
    g.fillRect(Math.round(x0), 0, 1, h); g.fillRect(Math.round(x0), 0, 6, 6);
    g.fillRect(Math.min(w - 1, Math.round(x1) - 1), 0, 1, h); g.fillRect(Math.round(x1) - 6, h - 6, 6, 6);
    // which way it plays
    g.font = '10px sans-serif'; g.fillStyle = T.fg3;
    g.fillText(st.reverse ? '◀' : '▶', st.reverse ? Math.max(x0 + 8, x1 - 20) : Math.min(x1 - 14, x0 + 10), h - 6);
  };
  const near = (e: PointerEvent): 'start' | 'end' | null => {
    const r = cv.getBoundingClientRect(), x = e.clientX - r.left, st = s();
    const ds = Math.abs(x - st.start * r.width), de = Math.abs(x - st.end * r.width);
    if (Math.min(ds, de) > GRAB) return null;
    // where they touch, the side of the pointer decides
    return ds < de || (ds === de && x < st.start * r.width) ? 'start' : 'end';
  };
  const down = (e: PointerEvent) => {
    if (e.button !== 0) return;
    const which = near(e); if (!which) return;
    e.preventDefault();
    const r = cv.getBoundingClientRect();
    const g = new Gesture(t('b.sm.region'));
    track(ev => {
      const c = live(props.c.id); if (!c) return;
      const st = settings(c), f = Math.round(clamp((ev.clientX - r.left) / r.width, 0, 1) * 1000) / 1000;
      const patch = which === 'start' ? { start: Math.min(f, st.end - SAMPLER.minSpan) } : { end: Math.max(f, st.start + SAMPLER.minSpan) };
      g.update([patchOp(c, patch)]);
      say(c.name + ' · ' + t(which === 'start' ? 'b.sm.startSay' : 'b.sm.endSay', { pct: Math.round((patch.start ?? patch.end ?? 0) * 100) }));
    }, () => g.end(), 'ew-resize');
  };
  const hover = (e: PointerEvent) => { cv.style.cursor = near(e) ? 'ew-resize' : 'default'; };
  /** Double click: play the whole file again. */
  const reset = () => { const c = live(props.c.id); if (c && (settings(c).start !== 0 || settings(c).end !== 1)) commit([patchOp(c, { start: 0, end: 1 })], t('b.sm.region')); };
  onMount(() => {
    const ro = new ResizeObserver(draw); ro.observe(cv);
    onCleanup(() => ro.disconnect());
  });
  createEffect(() => {
    peaksVersion(); themeVersion(); lang();
    void props.c.color; void props.c.source;
    const st = s(); void [st.start, st.end, st.reverse, st.attack, st.decay, st.sustain, st.semi, st.fine, st.gate];
    draw();
  });
  return <canvas ref={cv} class="chwave" title={t('b.sm.waveTitle')} onPointerDown={down} onPointerMove={hover} onDblClick={reset} />;
}
