// The sample window of an audio clip — the same window as a sampler channel's (FL: an audio clip
// is a channel with sampler settings): the waveform with the part the clip plays and its fades,
// the file, how it plays (reverse, normalise, keep pitch / resample, fit to a length) and its
// pitch, level and fades. The mixer insert (of the clip's track) sits in the header. Every
// control is one undo step; a click on the waveform previews the clip.
import { createEffect, For, onCleanup, onMount, Show } from 'solid-js';
import { be } from '../api/backend';
import { CLIP_PITCH, type Clip, type Source } from '../api/types';
import { FIT_BEATS, fitClip, TOTAL } from '../lib/clipOps';
import { baseName, clamp, dbg, dbText, panText } from '../lib/format';
import { t, type TranslationKey } from '../lib/i18n';
import * as A from '../state/actions';
import { autoDisplay, controlMenu, isAutomated, paramMenu } from '../state/automation';
import { commit, Gesture, wheelEdit } from '../state/edit';
import { getPeaks, peakRange, peaksVersion } from '../state/peaks';
import { plain, S } from '../state/store';
import { tele } from '../state/telemetry';
import { say, setU, U } from '../state/ui';
import { track } from '../ui/drag';
import { addFrameTask, fitCanvas, Layer } from '../ui/frameLoop';
import { guessIcon, InstIcon } from '../ui/instIcons';
import { InsertPicker } from '../ui/InsertPicker';
import { Knob } from '../ui/Knob';
import { rgba, T, theme } from '../ui/theme';
import { moveWindow } from './FxWindow';

export function SampleEditor() {
  const clip = () => U.editor ? S.project.clips.find(c => c.id === U.editor!.clipId) : undefined;
  const src = () => { const c = clip(); return c && c.kind === 'audio' ? S.project.sources.find(s => s.id === c.ref) : undefined; };
  return (
    <Show when={U.editor && clip() && src()}>
      <EditorBody clip={clip()!} src={src()!} />
    </Show>
  );
}

/** Height of the waveform (px). */
const WAVE_H = 120;

/** "1 beat", "2 beats", "1 bar", "2 bars" … */
const fitLabel = (b: number) => b < 4 ? t(b === 1 ? 'd.smp.fit.beat' : 'd.smp.fit.beats', { n: b }) : t(b === 4 ? 'd.smp.fit.bar' : 'd.smp.fit.bars', { n: b / 4 });

function EditorBody(props: { clip: Clip; src: Source }) {
  let cv!: HTMLCanvasElement, prevLine!: HTMLDivElement;
  const E = () => U.editor!;
  const c = () => S.project.clips.find(x => x.id === E().clipId) ?? props.clip;
  const src = () => S.project.sources.find(s => s.id === c().ref) ?? props.src;
  const spb = () => 60 / S.project.bpm;
  const W = () => E().w - 24, H = () => WAVE_H;
  // beats of the clip: a stretched clip shows its source stretched (offset and len are in them)
  const st = () => c().stretch ?? 1;
  const durB = () => src().dur / spb() * st();
  /** Seconds of the source at clip beat `b`. */
  const srcSec = (b: number) => b * spb() / st();
  /** The preview plays the rendered audio once it is ready (else the source at its own speed). */
  const rendered = () => (c().pitch || (st() !== 1 && !c().resample)) && !(c().id in S.stretching);
  const previewDur = () => src().dur * (rendered() && !c().resample ? st() : 1);
  const pxB = () => W() / durB();
  const x0 = () => c().offset * pxB(), x1 = () => (c().offset + c().len) * pxB();
  const fi = () => c().fadeIn / spb() * pxB(), fo = () => c().fadeOut / spb() * pxB();
  const tr = () => S.project.tracks[c().track];

  // The waveform is a cached layer (keyed by size, peaks and gain); region and fades are drawn
  // over it, so dragging a fade or a region edge costs the same for a 4 s or a 4 min file.
  const wave = new Layer();
  let dirty = true, themeEpoch = -1;
  createEffect(() => { W(); c().offset; c().len; c().fadeIn; c().fadeOut; c().gain; c().norm; c().rev; c().stretch; src().color; peaksVersion(); dirty = true; });
  onMount(() => {
    const stop = addFrameTask(() => {
      if (themeEpoch !== theme.epoch) { themeEpoch = theme.epoch; dirty = true; }
      if (dirty) { dirty = false; draw(); }
      const f = tele.frame;
      if (f.previewing && f.previewPos >= 0) { prevLine.style.opacity = '1'; prevLine.style.transform = `translateX(${f.previewPos / previewDur() * W()}px)`; }
      else if (prevLine.style.opacity !== '0') prevLine.style.opacity = '0';
    });
    onCleanup(stop);
  });
  const draw = () => {
    const w = W(), h = H(), dpr = window.devicePixelRatio || 1;
    fitCanvas(cv, w, h);
    const g = cv.getContext('2d')!;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    const pk = getPeaks(src().id, c().norm, c().rev);
    const key = [src().id, c().norm, c().rev, c().gain, src().color, pk ? pk.length : 0, peaksVersion()].join('|');
    g.drawImage(wave.get(w, h, key, (l, lw, lh) => {
      l.fillStyle = T.viz; l.fillRect(0, 0, lw, lh);
      l.fillStyle = T.vizGrid2; l.fillRect(0, Math.floor(lh / 2), lw, 1);
      if (!pk) return;
      const bins = pk.length / 2, mid = lh / 2, gn = dbg(c().gain);
      l.fillStyle = src().color; l.beginPath();
      for (let x = 0; x < lw; x++) {
        const a = Math.floor(x / lw * bins), b = Math.max(a + 1, Math.floor((x + 1) / lw * bins));
        const [mn, mx] = peakRange(pk, a, b);
        const y1 = mid - clamp(mx * gn, 0, 1) * mid * 0.95, y2 = mid - clamp(mn * gn, -1, 0) * mid * 0.95;
        l.rect(x, y1, 1, Math.max(0.6, y2 - y1));
      }
      l.fill();
    }) as CanvasImageSource, 0, 0, w, h);
    const a = x0(), b = x1(), fI = fi(), fO = fo();
    g.fillStyle = T.shade; g.beginPath();
    g.moveTo(a, h); g.lineTo(a + fI, 0); g.lineTo(a, 0); g.closePath();
    g.moveTo(b - fO, 0); g.lineTo(b, h); g.lineTo(b, 0); g.closePath(); g.fill();
    g.strokeStyle = T.ink; g.lineWidth = 1.5; g.beginPath();
    if (fI) { g.moveTo(a, h); g.lineTo(a + fI, 0); }
    if (fO) { g.moveTo(b - fO, 0); g.lineTo(b, h); }
    g.stroke();
    g.fillStyle = rgba(T.viz, 0.78); g.fillRect(0, 0, a, h); g.fillRect(b, 0, w - b, h);
    // the markers, as on a sampler's waveform: start (flag at the top) and end (flag at the bottom)
    g.fillStyle = T.fgHi;
    g.fillRect(Math.round(a), 0, 1, h); g.fillRect(Math.round(a), 0, 6, 6);
    g.fillRect(Math.round(b) - 1, 0, 1, h); g.fillRect(Math.round(b) - 6, h - 6, 6, 6);
  };

  const put = (patch: Partial<Clip>) => ({ op: 'putClips' as const, clips: [{ ...plain(c()), ...patch }] });
  /** A click on the waveform (not on a marker or fade handle) plays the clip, or stops it. */
  const preview = () => U.previewing ? void be().previewStop() : void be().previewClip(c().id).catch(e => say(String(e)));
  const waveDown = (e: PointerEvent) => {
    if (e.button !== 0) return;
    const kind = (e.target as HTMLElement).dataset.ed;
    if (!kind) { preview(); return; }
    e.preventDefault(); e.stopPropagation();
    const o = plain(c()), s = spb(), db = durB(), px = pxB(), sx = e.clientX;
    const g = new Gesture(kind === 'fi' || kind === 'fo' ? 'Fade' : 'Region');
    track(ev => {
      const dB = (ev.clientX - sx) / px, n = { ...o };
      if (kind === 'l') { const no = clamp(o.offset + dB, Math.max(0, o.offset - o.start), o.offset + o.len - 1 / 16); n.offset = no; n.start = o.start + (no - o.offset); n.len = o.len - (no - o.offset); }
      else if (kind === 'r') n.len = clamp(o.len + dB, 1 / 16, Math.min(db - o.offset, TOTAL - o.start));
      else if (kind === 'fi') n.fadeIn = clamp(o.fadeIn + dB * s, 0, o.len * s - o.fadeOut);
      else if (kind === 'fo') n.fadeOut = clamp(o.fadeOut - dB * s, 0, o.len * s - o.fadeIn);
      g.update([{ op: 'putClips', clips: [n] }]);
      say(kind === 'fi' ? 'Fade in ' + Math.round(n.fadeIn * 1000) + ' ms' : kind === 'fo' ? 'Fade out ' + Math.round(n.fadeOut * 1000) + ' ms' : 'Region ' + srcSec(n.offset).toFixed(3) + ' – ' + srcSec(n.offset + n.len).toFixed(3) + ' s');
    }, () => g.end(), kind === 'l' || kind === 'r' ? 'ew-resize' : 'col-resize');
  };

  // ---------- knobs ----------
  type K = 'gain' | 'pan' | 'fadeIn' | 'fadeOut' | 'semi' | 'fine';
  /** Pitch as whole semitones (toward 0) and cents, like the sampler's PITCH and FINE. */
  const semi = () => Math.trunc(Math.round((c().pitch ?? 0) * 100) / 100);
  const fine = () => Math.round(((c().pitch ?? 0) - semi()) * 100);
  const pitchOf = (s: number, f: number) => Math.round(clamp(s + f / 100, -CLIP_PITCH, CLIP_PITCH) * 100) / 100;
  const get = (k: K): number => k === 'semi' ? semi() : k === 'fine' ? fine() : c()[k] || 0;
  const spec = (k: K): [number, number, number] => {
    const mx = Math.max(0.01, c().len * spb());
    return ({ gain: [-24, 12, 0], pan: [-1, 1, 0], fadeIn: [0, mx, 0], fadeOut: [0, mx, 0], semi: [-CLIP_PITCH, CLIP_PITCH, 0], fine: [-100, 100, 0] } as Record<K, [number, number, number]>)[k];
  };
  const text = (k: K, v: number) => k === 'gain' ? dbText(v) : k === 'pan' ? panText(v) : k === 'semi' ? (v > 0 ? '+' : '') + v + ' st' : k === 'fine' ? (v > 0 ? '+' : '') + v + ' ct' : Math.round(v * 1000) + ' ms';
  /** The clip with knob `k` at value `v` (within what the other settings leave). */
  const patchOf = (k: K, v: number): Partial<Clip> => {
    const cc = c(), mx = cc.len * spb();
    if (k === 'semi') return { pitch: pitchOf(Math.round(v), fine()) };
    if (k === 'fine') return { pitch: pitchOf(semi(), Math.round(v)) };
    if (k === 'fadeIn') return { fadeIn: Math.min(v, mx - cc.fadeOut) };
    if (k === 'fadeOut') return { fadeOut: Math.min(v, mx - cc.fadeIn) };
    return { [k]: v };
  };
  const labels: Record<K, TranslationKey> = { gain: 'd.smp.k.vol', pan: 'd.smp.k.pan', fadeIn: 'd.smp.k.fadein', fadeOut: 'd.smp.k.fadeout', semi: 'd.smp.k.pitch', fine: 'd.smp.k.fine' };
  const tips: Partial<Record<K, TranslationKey>> = { semi: 'd.smp.k.pitchTip', fine: 'd.smp.k.fineTip' };
  const knob = (k: K) => {
    let g: Gesture | null = null;
    const label = t(labels[k]);
    // VOL and PAN are automatable (the clip's own gain and pan); the rest is rendered or shapes the clip
    const target = () => k === 'gain' ? { kind: 'clipGain' as const, id: c().id } : k === 'pan' ? { kind: 'clipPan' as const, id: c().id } : null;
    const v = () => { const tg = target(); return (tg && autoDisplay(tg)) ?? get(k); };
    const reset = () => commit([put(patchOf(k, spec(k)[2]))], label);
    const fromT = (tv: number) => { const [lo, hi] = spec(k); return lo + tv * (hi - lo); };
    return <Knob key={'ed' + k} label={label} title={t(tips[k] ?? 'd.smp.knob.tip')}
      automated={() => { const tg = target(); return !!tg && isAutomated(tg); }}
      onMenu={(x, y) => { const tg = target(); if (tg) paramMenu(x, y, tg, reset, src().color); else controlMenu(x, y, src().name + ' · ' + label, reset, t(k === 'semi' || k === 'fine' ? 'd.smp.noAutoPitch' : 'a.au.noFade')); }}
      t={() => { const [lo, hi] = spec(k); return clamp((v() - lo) / (hi - lo), 0, 1); }}
      zero={() => { const [lo, hi] = spec(k); return lo < 0 ? -lo / (hi - lo) : 0; }}
      text={() => text(k, v())}
      onStart={() => { g = new Gesture(label); }}
      onChange={tv => { const p = patchOf(k, fromT(tv)); g?.update([put(p)]); say(label + ' ' + text(k, k === 'semi' || k === 'fine' ? get(k) : (p[k as keyof Clip] as number))); }}
      onEnd={() => { g?.end(); g = null; }}
      onReset={reset}
      onWheel={tv => {
        // a wheel tick is less than a semitone: step PITCH by at least one
        let nv = fromT(tv);
        if (k === 'semi') { const d = nv - get(k); nv = get(k) + Math.sign(d) * Math.max(1, Math.round(Math.abs(d))); }
        wheelEdit('ed' + k, label, [put(patchOf(k, nv))]);
      }} />;
  };

  // ---------- fit to a length ----------
  const fit = (b: number) => {
    const n = fitClip(plain(c()), b); if (!n) return;
    commit([{ op: 'putClips', clips: [n] }], t('d.smp.fit.op'));
    say(t('d.smp.fit.say', { len: fitLabel(b), k: (n.stretch ?? 1).toFixed(2) }) + (n.resample ? '' : t('d.smp.fit.keep')));
  };
  const fitTip = (b: number) => fitClip(plain(c()), b) ? t('d.smp.fit.tip', { len: fitLabel(b) }) : t('d.smp.fit.no', { len: fitLabel(b) });

  const resize = (e: PointerEvent) => {
    if (e.button !== 0) return; e.preventDefault(); e.stopPropagation();
    const sx = e.clientX, o = { ...E() };
    track(ev => setU('editor', ed => ed && { ...ed, w: clamp(o.w + ev.clientX - sx, 520, window.innerWidth - 20) }), null, 'ew-resize');
  };
  const handle = (left: () => number, kind: string, title: string) => (
    <div data-ed={kind} title={title} style={{ position: 'absolute', top: 0, bottom: 0, left: left() + 'px', width: '9px', 'margin-left': '-4px', cursor: 'ew-resize' }} />
  );
  const busy = () => c().id in S.stretching;
  return (
    <div class="win chanwin smpwin" style={{ left: E().x + 'px', top: E().y + 'px', width: E().w + 'px', 'z-index': 40 }} data-editor>
      <div class="winhd" onPointerDown={e => moveWindow(e, () => E(), (x, y) => setU('editor', ed => ed && { ...ed, x, y }))}>
        <span class="chicon" style={{ color: src().color }}><InstIcon id={guessIcon(src().name) ?? 'sample'} size={14} /></span>
        <span class="title ell" style={{ 'min-width': '0', flex: '0 1 auto' }} title={src().name}>{src().name}</span>
        <span class="sub">{t('d.smp.kind', { track: tr()?.name ?? '' })}</span>
        <div style={{ flex: 1 }} />
        <InsertPicker value={tr()?.insert ?? 0} master ops={id => { const op = A.trackOp(c().track, { insert: id ?? 0 }); return op ? [op] : []; }}
          label={t('d.smp.routing')} wheelKey="edroute" title={t('d.smp.mixer.tip', { track: tr()?.name ?? '' })} />
        <button class="closebtn" title={t('d.smp.close')} onClick={A.closeEditor}>×</button>
      </div>
      <div onPointerDown={waveDown} title={t('d.smp.waveTip')} class="smpwave" style={{ width: W() + 'px', height: H() + 'px' }}>
        <canvas ref={cv} style={{ position: 'absolute', left: 0, top: 0, width: W() + 'px', height: H() + 'px', 'pointer-events': 'none' }} />
        {handle(x0, 'l', t('d.smp.regstart'))}
        {handle(x1, 'r', t('d.smp.regend'))}
        <div data-ed="fi" title={t('d.smp.fadein.tip')} style={{ position: 'absolute', top: 0, left: x0() + fi() + 'px', width: '11px', height: '11px', 'margin-left': '-1px', background: 'var(--fg-hi)', 'border-radius': '0 0 4px 0', cursor: 'col-resize' }} />
        <div data-ed="fo" title={t('d.smp.fadeout.tip')} style={{ position: 'absolute', top: 0, left: x1() - fo() + 'px', width: '11px', height: '11px', 'margin-left': '-10px', background: 'var(--fg-hi)', 'border-radius': '0 0 0 4px', cursor: 'col-resize' }} />
        <div ref={prevLine} style={{ position: 'absolute', top: 0, bottom: 0, left: 0, width: '1px', background: 'var(--playhead)', opacity: 0, 'pointer-events': 'none' }} />
      </div>
      <div class="chsrc">
        <span class="ell" title={src().path}>{baseName(src().path)}</span>
        <span class="mono">{srcSec(c().offset).toFixed(2) + '–' + srcSec(c().offset + c().len).toFixed(2) + ' of ' + src().dur.toFixed(2) + ' s · ' + Math.round(src().sr / 100) / 10 + ' kHz · ' + (src().ch === 2 ? 'stereo' : 'mono')}</span>
      </div>
      <div class="smprow">
        <button class="smptog" aria-pressed={c().rev} classList={{ on: c().rev }} title={t('d.smp.rev.tip')} onClick={() => { commit([put({ rev: !c().rev })], 'Reverse'); say(c().rev ? t('d.smp.rev.on') : t('d.smp.rev.off')); }}>{t('b.sm.reverse')}</button>
        <button class="smptog" aria-pressed={c().norm} classList={{ on: c().norm }} title={t('d.smp.norm.tip')} onClick={() => { commit([put({ norm: !c().norm })], 'Normalize'); say(c().norm ? t('d.smp.norm.on') : t('d.smp.norm.off')); }}>{t('b.sm.norm')}</button>
        <div class="seg smpmode" role="radiogroup" aria-label={t('d.smp.method')} title={t('d.smp.methodTip')}>
          <button role="radio" aria-checked={!c().resample} classList={{ on: !c().resample }} onClick={() => A.setStretchMethod(c(), false)}>{t('a.pl.keepPitch')}</button>
          <button role="radio" aria-checked={!!c().resample} classList={{ on: !!c().resample }} onClick={() => A.setStretchMethod(c(), true)}>{t('a.pl.resample')}</button>
        </div>
        <span class="smpstat mono" classList={{ bad: !!S.stretching[c().id] }} title={S.stretching[c().id] || ''}>
          {busy() ? (S.stretching[c().id] ? t('d.smp.renderFail') : t('d.smp.rendering')) : st() !== 1 ? '×' + st().toFixed(2) : ''}
        </span>
      </div>
      <div class="smprow smpfit">
        <span class="tag9">{t('d.smp.fit')}</span>
        <div class="seg">
          <For each={FIT_BEATS}>{b => (
            <button classList={{ on: Math.abs(c().len - b) < 1e-6 }} disabled={!fitClip(plain(c()), b)} title={fitTip(b)} onClick={() => fit(b)}>{fitLabel(b)}</button>
          )}</For>
        </div>
      </div>
      <div class="smpknobs">
        <div class="smpgrp">{knob('semi')}{knob('fine')}</div>
        <div class="smpgrp">{knob('gain')}{knob('pan')}</div>
        <div class="smpgrp">{knob('fadeIn')}{knob('fadeOut')}</div>
      </div>
      <div style={{ height: '14px' }} />
      <div class="resizer" onPointerDown={resize} title={t('d.smp.resize')} />
    </div>
  );
}
