import { createSignal, Show } from 'solid-js';
import { be } from '../api/backend';
import { t } from '../lib/i18n';
import { baseName } from '../lib/format';
import { exportRange, exportSeconds, fmtExportLen, hasLoop } from '../lib/export';
import { S } from '../state/store';
import { say, setU, U } from '../state/ui';
import { Segmented } from '../ui/Knob';

const TAILS = [0, 2, 8] as const;
const RATES = [48000, 44100] as const;
const KBPS = [320, 256, 192, 128] as const;
const FKEY = 'wave.export.fmt';
const saved = (() => { try { return JSON.parse(localStorage.getItem(FKEY) ?? '{}') as { mp3?: boolean; kb?: number }; } catch { return {}; } })();

export function Export() {
  return <Show when={U.exportOpen}><ExportDialog /></Show>;
}

function ExportDialog() {
  const loopOk = () => hasLoop(S.project.loop);
  const [useLoop, setUseLoop] = createSignal(false);
  const [tailI, setTailI] = createSignal(1); // 2 s
  const [rateI, setRateI] = createSignal(0); // 48 kHz
  const [mp3, setMp3] = createSignal(!!saved.mp3);
  const [kbI, setKbI] = createSignal(Math.max(0, KBPS.indexOf((saved.kb ?? 320) as 320)));
  const remember = () => { try { localStorage.setItem(FKEY, JSON.stringify({ mp3: mp3(), kb: KBPS[kbI()] })); } catch { /* ignore */ } };
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);

  const range = () => exportRange(S.project, useLoop() && loopOk());
  const seconds = () => exportSeconds(range().from, range().to, S.project.bpm, TAILS[tailI()]);
  const close = () => { if (!busy()) setU('exportOpen', false); };

  const start = async () => {
    if (busy()) return;
    setBusy(true); setU('exportBusy', true); setErr(null);
    try {
      const r = await be().exportAudio({
        useLoop: useLoop() && loopOk(),
        tail: TAILS[tailI()],
        sampleRate: RATES[rateI()],
        format: mp3() ? 'mp3' : 'wav',
        ...(mp3() ? { mp3Kbps: KBPS[kbI()] } : {}),
      });
      setU('exportBusy', false);
      if (!U.exportOpen) return; // dialog was closed (Escape) while bouncing
      if (r.cancelled) {
        say(t('a.ex.cancelled'));
        setBusy(false);
        return;
      }
      const peak = r.stats.silent ? t('a.ex.silence') : r.stats.peakDbfs.toFixed(1) + ' dBFS';
      const clip = r.stats.clippedSamples > 0 ? ' · ' + r.stats.clippedSamples + ' clipped' : '';
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
      <div class="dialog" style={{ width: 'min(420px,92vw)' }} onPointerDown={e => e.stopPropagation()} onKeyDown={e => e.stopPropagation()}>
        <div class="hd"><span>{t('a.ex.title')}</span><button disabled={busy()} onClick={() => setU('exportOpen', false)}>×</button></div>
        <Show when={!busy()} fallback={
          <div style={{ display: 'flex', 'flex-direction': 'column', gap: '14px' }}>
            <span class="note">{t('a.ex.busy')}</span>
            <div class="prog busy"><div /></div>
            <div class="row" style={{ 'justify-content': 'flex-end', gap: '8px' }}>
              <button class="smallbtn" onClick={cancel}>{t('a.ex.cancel')}</button>
            </div>
          </div>
        }>
          <div class="form" style={{ 'margin-bottom': '12px' }}>
            <label>{t('a.ex.range')}</label>
            <Segmented
              options={loopOk() ? [t('a.ex.song'), t('a.ex.loop')] : [t('a.ex.song')]}
              value={() => (useLoop() && loopOk() ? 1 : 0)}
              pick={i => setUseLoop(i === 1)}
              title={loopOk() ? t('a.ex.rangeTip') : t('a.ex.rangeTipNoLoop')}
            />
            <label>{t('a.ex.tail')}</label>
            <Segmented
              options={[t('a.ex.cut'), '2 s', '8 s']}
              value={() => tailI()}
              pick={i => setTailI(i as 0 | 1 | 2)}
              title={t('a.ex.tailTip')}
            />
            <label>{t('a.ex.rate')}</label>
            <Segmented
              options={['48 kHz', '44,1 kHz']}
              value={() => rateI()}
              pick={i => setRateI(i as 0 | 1)}
            />
            <label>{t('a.ex.format')}</label>
            <Segmented options={['WAV', 'MP3']} value={() => (mp3() ? 1 : 0)} pick={i => { setMp3(i === 1); remember(); }}
              title={t('a.ex.formatTip')} />
            <Show when={mp3()}>
              <label>{t('a.ex.mp3q')}</label>
              <Segmented options={KBPS.map(k => k + ' kbps')} value={() => kbI()} pick={i => { setKbI(i); remember(); }} title={t('a.ex.mp3qTip')} />
            </Show>
            <label>{t('a.ex.file')}</label>
            <span class="note mono">{mp3() ? t('a.ex.fileMp3', { kbps: KBPS[kbI()] }) : t('a.ex.fileWav')}</span>
            <label>{t('a.ex.length')}</label>
            <span class="note mono">{fmtExportLen(seconds())} · {RATES[rateI()]} Hz</span>
          </div>
          <Show when={err()}><div class="note err" style={{ 'margin-bottom': '10px' }}>{err()}</div></Show>
          <div class="row" style={{ 'justify-content': 'flex-end', gap: '8px' }}>
            <button class="smallbtn" onClick={cancel}>{t('a.ex.cancel')}</button>
            <button class="smallbtn primary" onClick={() => void start()}>{t('a.ex.go')}</button>
          </div>
        </Show>
      </div>
    </div>
  );
}
