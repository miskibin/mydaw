import { createSignal, onCleanup, onMount, Show } from 'solid-js';
import { be } from '../api/backend';
import { t } from '../lib/i18n';
import { baseName } from '../lib/format';
import { exportRange, exportSeconds, fmtExportLen, hasLoop, stemInserts } from '../lib/export';
import type { ExportAudioOptions, ExportProgress } from '../api/types';
import { S } from '../state/store';
import { say, setU, U } from '../state/ui';
import { Segmented } from '../ui/Knob';

const TAILS = [0, 2, 8, 0] as const;
const WRAP = 3;
const RATES = [48000, 44100] as const;
const KBPS = [320, 256, 192, 128] as const;
const FKEY = 'wave.export.fmt';

export function Export() {
  return <Show when={U.exportOpen}><ExportDialog /></Show>;
}

function ExportDialog() {
  const saved = (() => { try { return (JSON.parse(localStorage.getItem(FKEY) ?? '{}') ?? {}) as { mp3?: boolean; stemsMp3?: boolean; kb?: number; stems?: boolean; master?: boolean; wavBits?: number; dither?: boolean }; } catch { return {}; } })();
  const loopOk = () => hasLoop(S.project.loop);
  const [useLoop, setUseLoop] = createSignal(false);
  const [tailI, setTailI] = createSignal<0 | 1 | 2 | 3>(1); // 2 s
  const [rateI, setRateI] = createSignal(0); // 48 kHz
  const [mixMp3, setMixMp3] = createSignal(!!saved.mp3);
  const [stemsMp3, setStemsMp3] = createSignal(!!saved.stemsMp3);
  const [kbI, setKbI] = createSignal(Math.max(0, KBPS.indexOf((saved.kb ?? 320) as 320)));
  const [stems, setStems] = createSignal(!!saved.stems);
  const [master, setMaster] = createSignal(saved.master !== false);
  const [wavBits, setWavBits] = createSignal<24 | 32>(saved.wavBits === 24 ? 24 : 32);
  const [dither, setDither] = createSignal(saved.dither === true);
  const remember = () => { try { localStorage.setItem(FKEY, JSON.stringify({ mp3: mixMp3(), stemsMp3: stemsMp3(), kb: KBPS[kbI()], stems: stems(), master: master(), wavBits: wavBits(), dither: dither() })); } catch { /* ignore */ } };
  const mp3 = () => stems() ? stemsMp3() : mixMp3();
  const setMp3 = (value: boolean) => { (stems() ? setStemsMp3 : setMixMp3)(value); remember(); };
  const wav = () => !mp3();
  const canWrap = () => wav() && useLoop() && loopOk();
  const tailChoice = () => tailI() === WRAP && !canWrap() ? 1 : tailI();
  const tailSeconds = () => TAILS[tailChoice()];
  const wavDepth = () => wavBits() === 24 ? '24-bit PCM' : '32-bit float';
  const [prog, setProg] = createSignal<ExportProgress | null>(null);
  onMount(() => {
    const un = be().on('export-progress', p => setProg(p));
    onCleanup(() => { void un.then(f => f()); });
  });
  const nStems = () => stemInserts(S.project).length;
  const nFiles = () => nStems() + (master() ? 1 : 0);
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);

  const range = () => exportRange(S.project, useLoop() && loopOk());
  const seconds = () => exportSeconds(range().from, range().to, S.project.bpm, tailSeconds());
  const close = () => { if (!busy()) setU('exportOpen', false); };

  const start = async () => {
    if (busy()) return;
    setBusy(true); setU('exportBusy', true); setErr(null); setProg(null);
    try {
      const common = { useLoop: useLoop() && loopOk(), tail: tailSeconds(), sampleRate: RATES[rateI()] };
      const options: ExportAudioOptions = mp3()
        ? { ...common, format: 'mp3', mp3Kbps: KBPS[kbI()] }
        : { ...common, format: 'wav', wavBits: wavBits(), dither: wavBits() === 24 && dither(), wrapTail: tailChoice() === WRAP };
      if (stems()) {
        const r = await be().exportStems({ ...options, includeMaster: master() });
        setU('exportBusy', false);
        if (!U.exportOpen) return; // dialog was closed (Escape) while bouncing
        if (r.cancelled) {
          say(t('a.ex.cancelled'));
          setBusy(false);
          return;
        }
        const clipped = r.files.reduce((n, f) => n + f.stats.clippedSamples, 0);
        const clip = clipped > 0 ? ' · ' + t('a.ex.clipped', { n: clipped }) : '';
        const skipped = r.skipped.length ? ' · ' + t('a.ex.stemsSkipped', { n: r.skipped.length, names: r.skipped.join(', ') }) : '';
        say(t('a.ex.doneStems', { n: r.files.length, dir: baseName(r.dir), len: fmtExportLen(r.files[0].stats.seconds), clip, skipped }));
        setU('exportOpen', false);
        return;
      }
      const r = await be().exportAudio(options);
      setU('exportBusy', false);
      if (!U.exportOpen) return; // dialog was closed (Escape) while bouncing
      if (r.cancelled) {
        say(t('a.ex.cancelled'));
        setBusy(false);
        return;
      }
      const peak = r.stats.silent ? t('a.ex.silence') : r.stats.peakDbfs.toFixed(1) + ' dBFS';
      const clip = r.stats.clippedSamples > 0 ? ' · ' + t('a.ex.clipped', { n: r.stats.clippedSamples }) : '';
      say(t('a.ex.done', { name: baseName(r.out), len: fmtExportLen(r.stats.seconds), peak, clip }));
      setU('exportOpen', false);
    } catch (e) {
      setU('exportBusy', false);
      if (!U.exportOpen) return;
      setErr(String(e));
      setBusy(false);
    }
  };

  const cancel = () => {
    if (busy()) {
      void be().exportCancel().catch(() => {});
      return;
    }
    setU('exportOpen', false);
  };

  return (
    <div class="overlay" onPointerDown={() => close()}>
      <div class="dialog" role="dialog" aria-modal="true" aria-label={t('a.ex.title')} style={{ width: 'min(420px,92vw)' }} onPointerDown={e => e.stopPropagation()} onKeyDown={e => { if (e.key !== 'Escape') e.stopPropagation(); }}>
        <div class="hd"><span>{t('a.ex.title')}</span><button disabled={busy()} onClick={() => setU('exportOpen', false)}>×</button></div>
        <Show when={!busy()} fallback={
          <div style={{ display: 'flex', 'flex-direction': 'column', gap: '14px' }}>
            <span class="note">{stems()
              ? (prog() && prog()!.name ? t('a.ex.busyStems', { done: prog()!.done + 1, total: prog()!.total, name: prog()!.name }) : t('a.ex.busyStemsStart'))
              : t('a.ex.busy')}</span>
            <Show when={stems() && prog()} fallback={<div class="prog busy"><div /></div>}>
              <div class="prog"><div style={{ width: Math.round((prog()!.done / Math.max(1, prog()!.total)) * 100) + '%' }} /></div>
            </Show>
            <div class="row" style={{ 'justify-content': 'flex-end', gap: '8px' }}>
              <button class="smallbtn" onClick={cancel}>{t('a.ex.cancel')}</button>
            </div>
          </div>
        }>
          <div class="form" style={{ 'margin-bottom': '12px' }}>
            <label>{t('a.ex.output')}</label>
            <Segmented
              options={[t('a.ex.mix'), t('a.ex.stems')]}
              value={() => (stems() ? 1 : 0)}
              pick={i => { setStems(i === 1); remember(); }}
              title={t('a.ex.outputTip')}
            />
            <label>{t('a.ex.range')}</label>
            <Segmented
              options={loopOk() ? [t('a.ex.song'), t('a.ex.loop')] : [t('a.ex.song')]}
              value={() => (useLoop() && loopOk() ? 1 : 0)}
              pick={i => setUseLoop(i === 1)}
              title={loopOk() ? t('a.ex.rangeTip') : t('a.ex.rangeTipNoLoop')}
            />
            <label>{t('a.ex.tail')}</label>
            <Segmented
              options={canWrap() ? [t('a.ex.cut'), '2 s', '8 s', t('a.ex.wrap')] : [t('a.ex.cut'), '2 s', '8 s']}
              value={tailChoice}
              pick={i => setTailI(i as 0 | 1 | 2 | 3)}
              title={t(canWrap() ? 'a.ex.wrapTip' : 'a.ex.tailTip')}
            />
            <label>{t('a.ex.rate')}</label>
            <Segmented
              options={['48 kHz', '44,1 kHz']}
              value={() => rateI()}
              pick={i => setRateI(i as 0 | 1)}
            />
            <Show when={stems()}>
              <label>{t('a.ex.master')}</label>
              <Segmented options={[t('a.ex.masterOn'), t('a.ex.masterOff')]} value={() => (master() ? 0 : 1)} pick={i => { setMaster(i === 0); remember(); }}
                title={t('a.ex.masterTip')} />
            </Show>
            <label>{t('a.ex.format')}</label>
            <Segmented options={['WAV', 'MP3']} value={() => (mp3() ? 1 : 0)} pick={i => setMp3(i === 1)}
              title={t('a.ex.formatTip')} />
            <Show when={mp3()}>
              <label>{t('a.ex.mp3q')}</label>
              <Segmented options={KBPS.map(k => k + ' kbps')} value={() => kbI()} pick={i => { setKbI(i); remember(); }} title={t('a.ex.mp3qTip')} />
            </Show>
            <Show when={wav()}>
              <label>{t('a.ex.bitDepth')}</label>
              <Segmented options={['32-bit float', '24-bit PCM']} value={() => wavBits() === 24 ? 1 : 0}
                pick={i => { setWavBits(i === 1 ? 24 : 32); remember(); }} />
              <Show when={wavBits() === 24}>
                <label>{t('a.ex.dither')}</label>
                <Segmented options={[t('a.ex.ditherOff'), t('a.ex.ditherOn')]} value={() => dither() ? 1 : 0}
                  pick={i => { setDither(i === 1); remember(); }} title={t('a.ex.ditherTip')} />
              </Show>
            </Show>
            <label>{t('a.ex.file')}</label>
            <span class="note mono">{stems()
              ? mp3()
                ? t('a.ex.fileStemsMp3', { n: nFiles(), kbps: KBPS[kbI()] })
                : t('a.ex.fileStems', { n: nFiles(), depth: wavDepth() })
              : mp3() ? t('a.ex.fileMp3', { kbps: KBPS[kbI()] }) : t('a.ex.fileWav', { depth: wavDepth() })}</span>
            <label>{t('a.ex.length')}</label>
            <span class="note mono">{fmtExportLen(seconds())} · {RATES[rateI()]} Hz</span>
          </div>
          <Show when={stems() && nFiles() === 0}><div class="note err" style={{ 'margin-bottom': '10px' }}>{t('a.ex.stemsNone')}</div></Show>
          <Show when={err()}><div class="note err" style={{ 'margin-bottom': '10px' }}>{err()}</div></Show>
          <div class="row" style={{ 'justify-content': 'flex-end', gap: '8px' }}>
            <button class="smallbtn" onClick={cancel}>{t('a.ex.cancel')}</button>
            <button class="smallbtn primary" disabled={stems() && nFiles() === 0} onClick={() => void start()}>{stems() ? t('a.ex.goStems') : t('a.ex.go')}</button>
          </div>
        </Show>
      </div>
    </div>
  );
}
