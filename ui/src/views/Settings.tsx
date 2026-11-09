import { createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import { be } from '../api/backend';
import type { AudioConfig, AudioDevices, AudioHost, AudioStatus, CliStatus, MidiDevice, Vst3Info } from '../api/types';
import * as A from '../state/actions';
import { keyName, midiFlash, midiFollow, midiStatus, selectMidiDevice, setMidiFollow, setMidiTarget } from '../state/midi';
import { S } from '../state/store';
import { say, setU, U } from '../state/ui';
import { Segmented } from '../ui/Knob';
import { setTheme, themeMode, type ThemeMode } from '../ui/theme';

const RATES = [44100, 48000, 88200, 96000];
const BUFFERS = [32, 64, 128, 256, 512, 1024, 2048];

export function Settings() {
  return <Show when={U.settings}><SettingsDialog /></Show>;
}

function SettingsDialog() {
  const [hosts, setHosts] = createSignal<AudioHost[]>([]);
  const [devs, setDevs] = createSignal<AudioDevices>({ outputs: [], inputs: [] });
  const [st, setSt] = createSignal<AudioStatus | null>(null);
  const [cli, setCli] = createSignal<CliStatus | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);
  const loadDevs = (host: string) => be().audioDevices(host).then(setDevs).catch(e => setErr(String(e)));
  let off: (() => void) | null = null;
  onCleanup(() => off?.());
  onMount(async () => {
    try {
      const [h, s] = await Promise.all([be().audioHosts(), be().audioGetConfig()]);
      setHosts(h); setSt(s); await loadDevs(s.host);
    } catch (e) { setErr(String(e)); }
    be().cliStatus().then(setCli).catch(() => {});
    off = await be().on('audio-status', s => setSt(s));
  });
  const apply = async (patch: Partial<AudioConfig>) => {
    const s = st(); if (!s) return;
    const cfg: AudioConfig = { host: s.host, output: s.output, input: s.input, sampleRate: s.sampleRate, bufferSize: s.bufferSize, inputChannel: s.inputChannel ?? null, ...patch };
    setBusy(true); setErr(null);
    try {
      if (patch.host && patch.host !== s.host) { await loadDevs(patch.host); cfg.output = null; cfg.input = null; }
      setSt(await be().audioSetConfig(cfg));
    } catch (e) { setErr(String(e)); }
    setBusy(false);
  };
  const opt = (v: string | number | null) => v == null ? '' : String(v);
  return (
    <div class="overlay" onPointerDown={() => setU('settings', false)}>
      <div class="dialog" style={{ width: 'min(500px,92vw)', 'max-height': '92vh', 'overflow-y': 'auto' }} onPointerDown={e => e.stopPropagation()} onKeyDown={e => e.stopPropagation()}>
        <div class="hd"><span>Ustawienia</span><button onClick={() => setU('settings', false)}>×</button></div>
        <div class="form" style={{ 'margin-bottom': '14px' }}>
          <label>Wygląd</label>
          <Segmented options={['Ciemny', 'Jasny', 'Systemowy']} value={() => (['dark', 'light', 'system'] as ThemeMode[]).indexOf(themeMode())}
            pick={i => { const m = (['dark', 'light', 'system'] as ThemeMode[])[i]; setTheme(m); say('Motyw: ' + ['ciemny', 'jasny', 'systemowy'][i]); }} title="Zapamiętywane na tym komputerze" />
        </div>
        <div class="sechd">Audio</div>
        <Show when={st()} fallback={<span class="note">{err() ?? 'Wczytywanie…'}</span>}>
          {s => (
            <div class="form">
              <label>Sterownik</label>
              <div class="row" style={{ gap: '8px' }}>
                <select class="sel" value={s().host} disabled={busy()} onChange={e => apply({ host: e.currentTarget.value })}>
                  <For each={hosts()}>{h => <option value={h.id}>{h.name}</option>}</For>
                </select>
              </div>
              <Show when={!s().asioCompiled}><span /><span class="note warn">ASIO nie jest wkompilowane w tę wersję — dostępne są pozostałe sterowniki.</span></Show>
              <label>Wyjście</label>
              <select class="sel" value={opt(s().output)} disabled={busy()} onChange={e => apply({ output: e.currentTarget.value || null })}>
                <option value="">Domyślne</option>
                <For each={devs().outputs}>{d => <option value={d}>{d}</option>}</For>
              </select>
              <label>Wejście</label>
              <select class="sel" value={opt(s().input)} disabled={busy()} onChange={e => apply({ input: e.currentTarget.value || null })}>
                <option value="">Domyślne / brak</option>
                <For each={devs().inputs}>{d => <option value={d}>{d}</option>}</For>
              </select>
              <label>Kanał wejścia</label>
              <select class="sel" value={String(s().inputChannel ?? 0)} disabled={busy() || s().inputChannels < 2}
                title={s().inputChannels ? 'Kanał nagrywany i odsłuchiwany — mono, w obu słuchawkach (mikrofon zwykle w wejściu 1)' : 'Wejście otwiera się przy nagrywaniu lub odsłuchu — włącz jedno z nich, by zobaczyć kanały'}
                onChange={e => apply({ inputChannel: +e.currentTarget.value })}>
                <For each={Array.from({ length: Math.max(s().inputChannels, (s().inputChannel ?? 0) + 1) }, (_, i) => i)}>{i => <option value={String(i)}>{'Wejście ' + (i + 1) + ' (mono)'}</option>}</For>
              </select>
              <label>Częstotliwość</label>
              <select class="sel" value={opt(s().sampleRate)} disabled={busy()} onChange={e => apply({ sampleRate: e.currentTarget.value ? +e.currentTarget.value : null })}>
                <option value="">Auto</option>
                <For each={RATES}>{r => <option value={String(r)}>{r} Hz</option>}</For>
              </select>
              <label>Bufor</label>
              <select class="sel" value={opt(s().bufferSize)} disabled={busy()} onChange={e => apply({ bufferSize: e.currentTarget.value ? +e.currentTarget.value : null })}>
                <option value="">Auto</option>
                <For each={BUFFERS}>{b => <option value={String(b)}>{b} próbek</option>}</For>
              </select>
              <label>Stan</label>
              <span class="note mono" style={{ color: s().running ? 'var(--green)' : 'var(--danger)' }}>
                {s().running ? '● działa' : '● zatrzymane'} · {s().actualRate} Hz · {s().actualBuffer} próbek · {s().latencyMs.toFixed(1)} ms · wejść {s().inputChannels}
              </span>
              <Show when={s().error}><span /><span class="note err">{s().error}</span></Show>
              <Show when={s().note}><span /><span class="note">{s().note}</span></Show>
              <Show when={err()}><span /><span class="note err">{err()}</span></Show>
              <label>CLI</label>
              <span class="note">
                <Show when={cli()} fallback="—">
                  {c => <><span class="mono" style={{ color: 'var(--fg-hi)' }}>{c().command}</span> · {c().installed ? 'w PATH' + (c().path ? ' (' + c().path + ')' : '') : 'nie ma w PATH'}{c().note ? ' · ' + c().note : ''}</>}
                </Show>
              </span>
            </div>
          )}
        </Show>
        <MidiSection />
      </div>
    </div>
  );
}

function MidiSection() {
  const [devs, setDevs] = createSignal<MidiDevice[]>([]);
  const [err, setErr] = createSignal<string | null>(null);
  const refresh = () => be().midiListDevices().then(setDevs).catch(e => setErr(String(e)));
  onMount(refresh);
  const run = (p: Promise<void>) => { setErr(null); p.catch(e => setErr(String(e))); };
  const st = midiStatus;
  const pitched = () => (S.project.channels ?? []);
  const targetVal = () => (midiFollow() ? 'follow' : String(st()?.target ?? ''));
  const pickTarget = (v: string) => {
    if (v === 'follow') { setMidiFollow(true); const ch = U.patWin?.ch; if (ch != null) run(setMidiTarget(ch)); return; }
    setMidiFollow(false); run(setMidiTarget(v === '' ? null : +v));
  };
  return (
    <>
      <div class="sechd" style={{ 'margin-top': '14px' }}>Klawiatura MIDI</div>
      <div class="form">
        <label>Urządzenie</label>
        <div class="row" style={{ gap: '8px' }}>
          <select class="sel" style={{ flex: 1 }} value={st()?.selected?.id ?? ''} onChange={e => run(selectMidiDevice(e.currentTarget.value || null))}>
            <option value="">Brak</option>
            <Show when={st()?.selected && !devs().some(d => d.id === st()!.selected!.id)}>
              <option value={st()!.selected!.id}>{st()!.selected!.name} (odłączone)</option>
            </Show>
            <For each={devs()}>{d => <option value={d.id}>{d.name}</option>}</For>
          </select>
          <button class="smallbtn" style={{ height: '26px' }} onClick={refresh} title="Odśwież listę urządzeń MIDI">Odśwież</button>
        </div>
        <label>Gra na</label>
        <select class="sel" value={targetVal()} onChange={e => pickTarget(e.currentTarget.value)}
          title="Kanał (instrument), który brzmi z klawiatury. Nagrywanie: uzbrojona ścieżka + R — nuty trafiają do nowego patternu.">
          <option value="follow">Zaznaczony kanał (Channel Rack / piano roll)</option>
          <option value="">Pierwszy instrument</option>
          <For each={pitched()}>{c => <option value={String(c.id)}>{c.name}</option>}</For>
        </select>
        <label>Stan</label>
        <span class="note mono row" style={{ gap: '6px' }}>
          <span class="midiled" classList={{ on: !!st()?.connected, hit: midiFlash() }} />
          <Show when={st()?.connected} fallback={<span style={{ color: 'var(--fg4)' }}>{st()?.selected ? 'czeka na urządzenie' : 'nie wybrano'}</span>}>
            {c => <span style={{ color: 'var(--green)' }}>{c().name}</span>}
          </Show>
          <Show when={st()?.lastNote}>{n => <span>· ostatnia {keyName(n().key)}{n().on ? ' v' + n().velocity : ''}</span>}</Show>
        </span>
        <Show when={st()?.error || err()}><span /><span class="note err">{st()?.error ?? err()}</span></Show>
      </div>
    </>
  );
}

/** "VST3…" from the add-effect menu: a searchable list of scanned plugins. */
export function VstPicker() {
  return <Show when={U.vstPicker}>{v => <VstPickerDialog insId={v().insId} />}</Show>;
}
function VstPickerDialog(props: { insId: number }) {
  const [list, setList] = createSignal<Vst3Info[] | null>(null);
  const [q, setQ] = createSignal('');
  const [err, setErr] = createSignal<string | null>(null);
  const scan = (rescan: boolean) => { setList(null); setErr(null); be().vst3Scan(rescan).then(setList).catch(e => { setErr(String(e)); setList([]); }); };
  onMount(() => scan(false));
  let input!: HTMLInputElement;
  onMount(() => setTimeout(() => input?.focus(), 0));
  const shown = () => { const s = q().toLowerCase(); return (list() ?? []).filter(p => !s || (p.name + ' ' + p.vendor + ' ' + p.category).toLowerCase().includes(s)); };
  const close = () => setU('vstPicker', null);
  return (
    <div class="overlay" onPointerDown={close}>
      <div class="dialog" style={{ width: 'min(440px,92vw)', display: 'flex', 'flex-direction': 'column', gap: '10px' }} onPointerDown={e => e.stopPropagation()}>
        <div class="hd" style={{ 'margin-bottom': 0 }}><span>Dodaj VST3</span><button onClick={close}>×</button></div>
        <div class="row" style={{ gap: '8px' }}>
          <input ref={input} class="textin" style={{ flex: 1 }} placeholder="Szukaj…" value={q()} onInput={e => setQ(e.currentTarget.value)}
            onKeyDown={e => { e.stopPropagation(); if (e.key === 'Escape') close(); if (e.key === 'Enter' && shown()[0]) { A.addVst(props.insId, shown()[0]); close(); } }} />
          <button class="smallbtn" style={{ height: '26px' }} onClick={() => scan(true)} title="Przeskanuj foldery VST3 ponownie">Skanuj ponownie</button>
        </div>
        <Show when={err()}><span class="note err">{err()}</span></Show>
        <div class="plist" style={{ height: '300px' }}>
          <Show when={list()} fallback={<span class="note">Skanowanie…</span>}>
            <For each={shown()} fallback={<span class="note">Nie znaleziono pluginów VST3</span>}>
              {p => (
                <div class="pi" onClick={() => { A.addVst(props.insId, p); close(); say('Dodano ' + p.name); }} title={p.path}>
                  <span class="tile" style={{ width: '20px', height: '20px', background: '#9a8cf026', color: '#9a8cf0' }}>
                    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><path d="M4 6h16v12H4z M8 10v4 M12 9v6 M16 11v2" /></svg>
                  </span>
                  <span class="ell" style={{ flex: 1, font: '500 12px var(--sans)', color: 'var(--fg-hi)' }}>{p.name}</span>
                  <span class="ell" style={{ font: '11px var(--sans)', color: 'var(--fg4)', 'max-width': '140px' }}>{p.vendor}</span>
                </div>
              )}
            </For>
          </Show>
        </div>
      </div>
    </div>
  );
}
