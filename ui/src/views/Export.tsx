import { createSignal, Show } from 'solid-js';
import { be } from '../api/backend';
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
        say('Eksport anulowany');
        setBusy(false);
        return;
      }
      const peak = r.stats.silent ? 'cisza' : r.stats.peakDbfs.toFixed(1) + ' dBFS';
      const clip = r.stats.clippedSamples > 0 ? ' · ' + r.stats.clippedSamples + ' clipped' : '';
      say('Wyeksportowano ' + baseName(r.out) + ' · ' + fmtExportLen(r.stats.seconds) + ' · peak ' + peak + clip);
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
        <div class="hd"><span>Eksport</span><button disabled={busy()} onClick={() => setU('exportOpen', false)}>×</button></div>
        <Show when={!busy()} fallback={
          <div style={{ display: 'flex', 'flex-direction': 'column', gap: '14px' }}>
            <span class="note">Eksportuję master → WAV 32-bit float…</span>
            <div class="prog busy"><div /></div>
            <div class="row" style={{ 'justify-content': 'flex-end', gap: '8px' }}>
              <button class="smallbtn" onClick={cancel}>Anuluj</button>
            </div>
          </div>
        }>
          <div class="form" style={{ 'margin-bottom': '12px' }}>
            <label>Zakres</label>
            <Segmented
              options={loopOk() ? ['Utwór', 'Pętla'] : ['Utwór']}
              value={() => (useLoop() && loopOk() ? 1 : 0)}
              pick={i => setUseLoop(i === 1)}
              title={loopOk() ? 'Utwór = cała piosenka; Pętla = zaznaczona pętla' : 'Brak pętli — eksportuje cały utwór'}
            />
            <label>Ogon</label>
            <Segmented
              options={['Utnij', '2 s', '8 s']}
              value={() => tailI()}
              pick={i => setTailI(i as 0 | 1 | 2)}
              title="Ile sekund po końcu zostawić na zanikanie reverbu i delay"
            />
            <label>Próbkowanie</label>
            <Segmented
              options={['48 kHz', '44,1 kHz']}
              value={() => rateI()}
              pick={i => setRateI(i as 0 | 1)}
            />
            <label>Format</label>
            <Segmented options={['WAV', 'MP3']} value={() => (mp3() ? 1 : 0)} pick={i => { setMp3(i === 1); remember(); }}
              title="WAV: bez strat, 32-bit float (do dalszej pracy, masteringu). MP3: mały plik do wysłania / odsłuchu na telefonie" />
            <Show when={mp3()}>
              <label>Jakość MP3</label>
              <Segmented options={KBPS.map(k => k + ' kbps')} value={() => kbI()} pick={i => { setKbI(i); remember(); }} title="320 kbps = najlepsza jakość" />
            </Show>
            <label>Plik</label>
            <span class="note mono">{mp3() ? 'MP3 · stereo · ' + KBPS[kbI()] + ' kbps' : 'WAV · stereo · 32-bit float'}</span>
            <label>Długość</label>
            <span class="note mono">{fmtExportLen(seconds())} · {RATES[rateI()]} Hz</span>
          </div>
          <Show when={err()}><div class="note err" style={{ 'margin-bottom': '10px' }}>{err()}</div></Show>
          <div class="row" style={{ 'justify-content': 'flex-end', gap: '8px' }}>
            <button class="smallbtn" onClick={cancel}>Anuluj</button>
            <button class="smallbtn primary" onClick={() => void start()}>Eksportuj…</button>
          </div>
        </Show>
      </div>
    </div>
  );
}
