import { createEffect, For, onCleanup, onMount, Show } from 'solid-js';
import { be } from '../api/backend';
import type { Clip, Source } from '../api/types';
import { TOTAL } from '../lib/clipOps';
import { clamp, dbg, dbText, panText } from '../lib/format';
import * as A from '../state/actions';
import { commit, Gesture, wheelEdit } from '../state/edit';
import { getPeaks, peakRange, peaksVersion } from '../state/peaks';
import { insName, S } from '../state/store';
import { tele } from '../state/telemetry';
import { say, setU, U } from '../state/ui';
import { track } from '../ui/drag';
import { addFrameTask, fitCanvas } from '../ui/frameLoop';
import { Knob } from '../ui/Knob';
import { moveWindow } from './FxWindow';

const plain = <T,>(v: T): T => JSON.parse(JSON.stringify(v));

export function SampleEditor() {
  const clip = () => U.editor ? S.project.clips.find(c => c.id === U.editor!.clipId) : undefined;
  const src = () => { const c = clip(); return c && c.kind === 'audio' ? S.project.sources.find(s => s.id === c.ref) : undefined; };
  return (
    <Show when={U.editor && clip() && src()}>
      <EditorBody clip={clip()!} src={src()!} />
    </Show>
  );
}

function EditorBody(props: { clip: Clip; src: Source }) {
  let cv!: HTMLCanvasElement, prevLine!: HTMLDivElement;
  const E = () => U.editor!;
  const c = () => S.project.clips.find(x => x.id === E().clipId) ?? props.clip;
  const src = () => S.project.sources.find(s => s.id === c().ref) ?? props.src;
  const spb = () => 60 / S.project.bpm;
  const W = () => E().w - 32, H = () => Math.max(80, E().h - 210);
  const durB = () => src().dur / spb();
  const pxB = () => W() / durB();
  const x0 = () => c().offset * pxB(), x1 = () => (c().offset + c().len) * pxB();
  const fi = () => c().fadeIn / spb() * pxB(), fo = () => c().fadeOut / spb() * pxB();
  const tr = () => S.project.tracks[c().track];

  let dirty = true;
  createEffect(() => { W(); H(); c().offset; c().len; c().fadeIn; c().fadeOut; c().gain; c().norm; c().rev; src().color; peaksVersion(); dirty = true; });
  onMount(() => {
    const stop = addFrameTask(() => {
      if (dirty) { dirty = false; draw(); }
      const f = tele.frame;
      if (f.previewing && f.previewPos >= 0) { prevLine.style.opacity = '1'; prevLine.style.transform = `translateX(${f.previewPos / src().dur * W()}px)`; }
      else if (prevLine.style.opacity !== '0') prevLine.style.opacity = '0';
    });
    onCleanup(stop);
  });
  const draw = () => {
    const w = W(), h = H(), dpr = window.devicePixelRatio || 1;
    fitCanvas(cv, w, h);
    const g = cv.getContext('2d')!;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    g.fillStyle = '#0e1116'; g.fillRect(0, 0, w, h);
    g.fillStyle = '#1c222a'; g.fillRect(0, Math.floor(h / 2), w, 1);
    const pk = getPeaks(src().id, c().norm, c().rev);
    if (pk) {
      const bins = pk.length / 2, mid = h / 2, gn = dbg(c().gain);
      g.fillStyle = src().color; g.beginPath();
      for (let x = 0; x < w; x++) {
        const a = Math.floor(x / w * bins), b = Math.max(a + 1, Math.floor((x + 1) / w * bins));
        const [mn, mx] = peakRange(pk, a, b);
        const y1 = mid - clamp(mx * gn, 0, 1) * mid * 0.95, y2 = mid - clamp(mn * gn, -1, 0) * mid * 0.95;
        g.rect(x, y1, 1, Math.max(0.6, y2 - y1));
      }
      g.fill();
    }
    const a = x0(), b = x1(), fI = fi(), fO = fo();
    g.fillStyle = 'rgba(0,0,0,.55)'; g.beginPath();
    g.moveTo(a, h); g.lineTo(a + fI, 0); g.lineTo(a, 0); g.closePath();
    g.moveTo(b - fO, 0); g.lineTo(b, h); g.lineTo(b, 0); g.closePath(); g.fill();
    g.strokeStyle = '#f2f4f7'; g.lineWidth = 1.5; g.beginPath();
    if (fI) { g.moveTo(a, h); g.lineTo(a + fI, 0); }
    if (fO) { g.moveTo(b - fO, 0); g.lineTo(b, h); }
    g.stroke();
    g.fillStyle = 'rgba(10,12,15,.72)'; g.fillRect(0, 0, a, h); g.fillRect(b, 0, w - b, h);
    g.fillStyle = '#4f8ef7'; g.fillRect(Math.round(a), 0, 1, h); g.fillRect(Math.round(b) - 1, 0, 1, h);
  };

  const put = (patch: Partial<Clip>) => ({ op: 'putClips' as const, clips: [{ ...plain(c()), ...patch }] });
  const waveDown = (e: PointerEvent) => {
    if (e.button !== 0) return;
    const kind = (e.target as HTMLElement).dataset.ed; if (!kind) return;
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
      say(kind === 'fi' ? 'Fade in ' + Math.round(n.fadeIn * 1000) + ' ms' : kind === 'fo' ? 'Fade out ' + Math.round(n.fadeOut * 1000) + ' ms' : 'Region ' + (n.offset * s).toFixed(3) + ' – ' + ((n.offset + n.len) * s).toFixed(3) + ' s');
    }, () => g.end(), kind === 'l' || kind === 'r' ? 'ew-resize' : 'col-resize');
  };

  type K = 'gain' | 'pan' | 'fadeIn' | 'fadeOut';
  const spec = (k: K): [number, number, number] => { const mx = Math.max(0.01, c().len * spb()); return { gain: [-24, 12, 0], pan: [-1, 1, 0], fadeIn: [0, mx, 0], fadeOut: [0, mx, 0] }[k] as [number, number, number]; };
  const text = (k: K, v: number) => k === 'gain' ? dbText(v) : k === 'pan' ? panText(v) : Math.round(v * 1000) + ' ms';
  const limit = (k: K, v: number) => { const cc = c(), mx = cc.len * spb(); return k === 'fadeIn' ? Math.min(v, mx - cc.fadeOut) : k === 'fadeOut' ? Math.min(v, mx - cc.fadeIn) : v; };
  const knobs: { k: K; label: string }[] = [{ k: 'gain', label: 'VOL' }, { k: 'pan', label: 'PAN' }, { k: 'fadeIn', label: 'FADE IN' }, { k: 'fadeOut', label: 'FADE OUT' }];

  const resize = (e: PointerEvent) => {
    if (e.button !== 0) return; e.preventDefault(); e.stopPropagation();
    const sx = e.clientX, sy = e.clientY, o = { ...E() };
    track(ev => setU('editor', ed => ed && { ...ed, w: clamp(o.w + ev.clientX - sx, 600, window.innerWidth - 20), h: clamp(o.h + ev.clientY - sy, 340, window.innerHeight - 20) }), null, 'nwse-resize');
  };
  const previewing = () => U.previewing;
  const handle = (left: () => number, kind: string, title: string) => (
    <div data-ed={kind} title={title} style={{ position: 'absolute', top: 0, bottom: 0, left: left() + 'px', width: '9px', 'margin-left': '-4px', cursor: 'ew-resize' }} />
  );
  return (
    <div class="win" style={{ left: E().x + 'px', top: E().y + 'px', width: E().w + 'px', height: E().h + 'px', 'z-index': 40 }} data-editor>
      <div class="winhd" style={{ height: '32px', padding: '0 6px 0 12px' }} onPointerDown={e => moveWindow(e, () => E(), (x, y) => setU('editor', ed => ed && { ...ed, x, y }))}>
        <span style={{ width: '8px', height: '8px', 'border-radius': '50%', background: src().color }} />
        <span class="title">{src().name}</span>
        <span class="sub">{tr()?.name ?? ''}</span>
        <div style={{ flex: 1 }} />
        <button class="closebtn" title="Zamknij (Esc)" onClick={A.closeEditor}>×</button>
      </div>
      <div class="row mono" style={{ height: '28px', flex: 'none', 'justify-content': 'space-between', gap: '12px', padding: '0 16px', 'font-size': '10px', color: 'var(--fg4)' }}>
        <span>{src().dur.toFixed(3) + ' s · ' + src().sr + ' Hz · ' + (src().ch === 2 ? 'stereo' : 'mono') + ' · peak ' + src().peakDb.toFixed(1) + ' dBFS'}</span>
        <span>REGION {(c().offset * spb()).toFixed(3) + ' – ' + ((c().offset + c().len) * spb()).toFixed(3) + ' s'}</span>
      </div>
      <div onPointerDown={waveDown} style={{ position: 'relative', flex: 'none', margin: '0 16px', width: W() + 'px', height: H() + 'px', border: '1px solid #252b34', 'border-radius': '3px', overflow: 'hidden' }}>
        <canvas ref={cv} style={{ position: 'absolute', left: 0, top: 0, width: W() + 'px', height: H() + 'px', 'pointer-events': 'none' }} />
        {handle(x0, 'l', 'Początek regionu')}
        {handle(x1, 'r', 'Koniec regionu')}
        <div data-ed="fi" title="Fade in — przeciągnij" style={{ position: 'absolute', top: 0, left: x0() + fi() + 'px', width: '11px', height: '11px', 'margin-left': '-1px', background: '#f2f4f7', 'border-radius': '0 0 4px 0', cursor: 'col-resize' }} />
        <div data-ed="fo" title="Fade out — przeciągnij" style={{ position: 'absolute', top: 0, left: x1() - fo() + 'px', width: '11px', height: '11px', 'margin-left': '-10px', background: '#f2f4f7', 'border-radius': '0 0 0 4px', cursor: 'col-resize' }} />
        <div ref={prevLine} style={{ position: 'absolute', top: 0, bottom: 0, left: 0, width: '1px', background: '#eef1f5', opacity: 0, 'pointer-events': 'none' }} />
      </div>
      <div style={{ flex: 1, 'min-height': 0, display: 'flex', 'flex-wrap': 'wrap', 'align-content': 'flex-start', 'align-items': 'center', gap: '12px 18px', padding: '14px 16px' }}>
        <div class="row" style={{ gap: '16px' }}>
          <For each={knobs}>
            {kn => {
              let g: Gesture | null = null;
              const v = () => c()[kn.k] || 0;
              return <Knob key={'ed' + kn.k} label={kn.label} title="Przeciągnij ↕ · Shift = precyzyjnie · 2× klik = reset"
                t={() => { const [lo, hi] = spec(kn.k); return clamp((v() - lo) / (hi - lo), 0, 1); }}
                zero={() => { const [lo, hi] = spec(kn.k); return lo < 0 ? -lo / (hi - lo) : 0; }}
                text={() => text(kn.k, v())}
                onStart={() => { g = new Gesture(kn.label); }}
                onChange={t => { const [lo, hi] = spec(kn.k); const nv = limit(kn.k, lo + t * (hi - lo)); g?.update([put({ [kn.k]: nv })]); say(kn.label + ' ' + text(kn.k, nv)); }}
                onEnd={() => { g?.end(); g = null; }}
                onReset={() => commit([put({ [kn.k]: spec(kn.k)[2] })], kn.label)}
                onWheel={t => { const [lo, hi] = spec(kn.k); const nv = limit(kn.k, lo + t * (hi - lo)); wheelEdit('ed' + kn.k, kn.label, [put({ [kn.k]: nv })]); }} />;
            }}
          </For>
        </div>
        <div style={{ width: '1px', 'align-self': 'stretch', background: '#252b34' }} />
        <div style={{ display: 'flex', 'flex-direction': 'column', gap: '6px' }}>
          <ToggleBtn on={c().norm} label="NORMALIZE" title="Normalizuj do −0.3 dBFS (nieniszcząco)" onClick={() => { commit([put({ norm: !c().norm })], 'Normalize'); say(c().norm ? 'Normalize do −0.3 dBFS (nieniszcząco)' : 'Normalize wył.'); }} />
          <ToggleBtn on={c().rev} label="REVERSE" title="Odwróć (nieniszcząco)" onClick={() => { commit([put({ rev: !c().rev })], 'Reverse'); say(c().rev ? 'Reverse wł. (nieniszcząco)' : 'Reverse wył.'); }} />
        </div>
        <div style={{ display: 'flex', 'flex-direction': 'column', gap: '6px' }}>
          <button onClick={() => previewing() ? be().previewStop() : be().previewClip(c().id).catch(e => say(String(e)))}
            style={{ height: '22px', padding: '0 12px', border: 0, 'border-radius': '3px', background: previewing() ? '#4ec08a' : '#252b34', color: previewing() ? '#07231a' : '#f2f4f7', font: '500 11px var(--sans)', cursor: 'pointer' }}>
            {previewing() ? 'Stop' : 'Podgląd'}</button>
          <button class="smallbtn" style={{ height: '22px', padding: '0 12px' }} title="Wytnij wszystko poza regionem (nowa kopia pliku)"
            onClick={() => be().trimClip(c().id).then(() => say('Przycięto do regionu · zapisano jako kopię, oryginał nietknięty (Ctrl+Z cofa)')).catch(e => say('Nie przycięto: ' + String(e)))}>Przytnij</button>
        </div>
        <div style={{ flex: 1 }} />
        <div class="row" style={{ gap: '6px' }}>
          <span class="tag9">MIXER</span>
          <select class="sel" style={{ height: '26px' }} value={String(tr()?.insert ?? 0)}
            onChange={e => { const id = +e.currentTarget.value; A.setTrack(c().track, { insert: id }, 'Routing'); setU('mixSel', id); }}>
            <For each={S.project.inserts}>{m => <option value={String(m.id)}>{insName(m)}</option>}</For>
          </select>
        </div>
      </div>
      <div onPointerDown={resize} title="Zmień rozmiar" style={{ position: 'absolute', right: 0, bottom: 0, width: '14px', height: '14px', cursor: 'nwse-resize', background: 'linear-gradient(135deg,transparent 50%,#4a5360 50%,#4a5360 60%,transparent 60%,transparent 70%,#4a5360 70%,#4a5360 80%,transparent 80%)' }} />
    </div>
  );
}

function ToggleBtn(p: { on: boolean; label: string; title: string; onClick: () => void }) {
  return (
    <button onClick={p.onClick} title={p.title} style={{ display: 'flex', 'align-items': 'center', gap: '7px', height: '22px', padding: '0 8px', border: 0, 'border-radius': '3px', background: '#0e1013', color: p.on ? '#f2f4f7' : '#9aa2ad', font: '600 10px var(--mono)', 'letter-spacing': '.06em', cursor: 'pointer', 'white-space': 'nowrap' }}>
      <span style={{ width: '8px', height: '8px', 'border-radius': '2px', background: p.on ? '#4ec08a' : '#2c333d' }} />{p.label}
    </button>
  );
}
