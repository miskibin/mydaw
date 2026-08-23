import { For, Show, createSignal, onCleanup, onMount } from 'solid-js';

import { useTheme } from '../theme/ThemeProvider';
import { THEME_OPTIONS, accentFor, paletteFor, type ThemeName } from '../theme/tokens';
import { useAppState } from '../state';
import { useView } from '../shell/view';
import type { MidiPortDto } from '../api/types';
import {
  clampPreviewVolumePercent,
  DEFAULT_PREVIEW_VOLUME_PERCENT,
} from '../lib/previewVolume';
import {
  AUDIO_BUFFER_CHOICES,
  audioBufferLabel,
  audioBufferMs,
} from '../lib/audioOutput';
import { inputDeviceNote } from '../lib/recording';
import { peakFraction, formatDb, MIN_DB } from '../panels/mixer/scale';
import { onFrame } from '../ui/frameLoop';
import type { AudioOutput } from '../api/types';
import '../styles/settings.css';

const FONTS = [
  { name: 'geist' as const, label: 'Geist', note: 'Domyślny, neutralny' },
  { name: 'inter' as const, label: 'Inter', note: 'Czytelny i kompaktowy' },
  { name: 'mono' as const, label: 'Geist Mono', note: 'Techniczny, równy rytm' },
];

const MIDI_POLL_MS = 400;
const MIDI_LIVE_MS = 250;

function statusNote(port: MidiPortDto): string {
  if (port.status === 'connected') return 'Sparowany, nuty grają wybrany kanał w Racku';
  if (port.status === 'offline') return 'Sparowany, ale odłączony';
  if (port.status === 'error') return port.lastError ?? 'Nie udało się otworzyć portu';
  return 'Wykryty — włącz, żeby sparować';
}

export function Settings() {
  const theme = useTheme();
  const view = useView();
  const app = useAppState();
  const [ports, setPorts] = createSignal<MidiPortDto[]>([]);
  const [activityAtMs, setActivityAtMs] = createSignal(0);
  const [activeName, setActiveName] = createSignal<string | null>(null);
  const [now, setNow] = createSignal(Date.now());
  const [previewPercent, setPreviewPercent] = createSignal(DEFAULT_PREVIEW_VOLUME_PERCENT);
  const [audio, setAudio] = createSignal<AudioOutput>();
  const [audioBusy, setAudioBusy] = createSignal(false);
  const [inputBusy, setInputBusy] = createSignal(false);

  // --- audio input ---------------------------------------------------------

  const inputStatus = () => app.inputStatus();
  const defaultDevice = () => app.inputDevices().find((device) => device.isDefault);
  /** Whether `id` is the device actually open right now. */
  const isOpen = (id: string): boolean => {
    const status = inputStatus();
    if (!status || status.state === 'idle') return false;
    return status.deviceId === id || (status.deviceId === null && defaultDevice()?.id === id);
  };

  /**
   * Run one input gesture, with the rows disabled while it is in flight.
   *
   * Opening a capture device is a device call, not a signal write: it can take
   * a moment, and a second click during that moment is how two streams get
   * opened on one device.
   */
  async function runInput(gesture: () => Promise<void>): Promise<void> {
    if (inputBusy()) return;
    setInputBusy(true);
    try {
      await gesture();
    } finally {
      setInputBusy(false);
    }
  }

  // The level moves continuously, so it is painted on the frame loop rather
  // than from a signal -- the rule `ui/frameLoop.ts` exists for.
  let settingsRmsEl: HTMLSpanElement | undefined;
  let settingsPeakEl: HTMLSpanElement | undefined;
  let settingsDbEl: HTMLSpanElement | undefined;
  onFrame(() => {
    const status = inputStatus();
    if (!status) return;
    if (settingsRmsEl) settingsRmsEl.style.width = `${peakFraction(status.rms) * 100}%`;
    if (settingsPeakEl) settingsPeakEl.style.left = `${peakFraction(status.peak) * 100}%`;
    if (settingsDbEl) {
      const db = status.peak > 0 ? 20 * Math.log10(status.peak) : MIN_DB;
      settingsDbEl.textContent = `${formatDb(db)} dB`;
    }
  });

  async function refreshMidi(): Promise<void> {
    try {
      const [list, status] = await Promise.all([
        app.backend.listMidiPorts(),
        app.backend.getMidiStatus(),
      ]);
      setPorts(list);
      setActivityAtMs(status.activityAtMs);
      setActiveName(status.activeName);
      setNow(Date.now());
    } catch (error) {
      app.report(error);
    }
  }

  onMount(() => {
    void refreshMidi();
    void app.backend
      .getPreviewSettings()
      .then((settings) => setPreviewPercent(clampPreviewVolumePercent(settings.volumePercent)))
      .catch(app.report);
    void app.backend
      .getAudioSettings()
      .then(setAudio)
      .catch(app.report);
    const id = setInterval(() => {
      void refreshMidi();
    }, MIDI_POLL_MS);
    onCleanup(() => clearInterval(id));
  });

  async function toggle(port: MidiPortDto): Promise<void> {
    try {
      setPorts(await app.backend.setMidiPortEnabled(port.name, !port.enabled));
    } catch (error) {
      app.report(error);
    }
  }

  async function applyAudio(preferAsio: boolean, bufferFrames: number): Promise<void> {
    if (audioBusy()) return;
    setAudioBusy(true);
    try {
      const next = await app.backend.setAudioSettings(preferAsio, bufferFrames);
      setAudio(next);
      await app.refreshStreamInfo();
    } catch (error) {
      app.report(error);
    } finally {
      setAudioBusy(false);
    }
  }

  const paired = () => ports().filter((port) => port.enabled).length;
  const live = (port: MidiPortDto) =>
    port.enabled &&
    port.connected &&
    now() - activityAtMs() < MIDI_LIVE_MS &&
    (activeName() === port.name || paired() === 1);

  return (
    <main class="wv-settings">
      <div class="wv-settings__header">
        <div>
          <p class="wv-settings__eyebrow">WAVE / PREFERENCES</p>
          <h1>Settings</h1>
          <p class="wv-settings__lede">Audio, MIDI i wygląd. Zmiany zapisują się automatycznie.</p>
        </div>
        <button type="button" class="wv-settings__close" onClick={() => {
          const back = view.previousView();
          view.go(back === 'settings' ? 'rack' : back);
        }}>Back to studio</button>
      </div>

      <section class="wv-settings__section">
        <div class="wv-settings__section-head">
          <div>
            <h2>Audio output</h2>
            <p>
              Buffer is the delay between a key and the sound. Smaller is tighter and harder on the CPU.
              ASIO bypasses the Windows mixer when a driver is present; if it cannot open, WAVE stays on WASAPI and says so.
            </p>
          </div>
          <span class="wv-settings__value">
            {audio()?.stream.running
              ? `${audio()!.stream.backend.toUpperCase()} · ${audio()!.stream.latencyMs.toFixed(1)} ms`
              : 'OFF'}
          </span>
        </div>

        <div class="wv-audio-driver">
          <button
            type="button"
            class="wv-font-row"
            classList={{ 'is-active': audio() ? !audio()!.preferAsio : true }}
            disabled={audioBusy()}
            aria-pressed={audio() ? !audio()!.preferAsio : true}
            onClick={() => void applyAudio(false, audio()?.bufferFrames ?? 0)}
          >
            <span>
              <strong>WASAPI</strong>
              <small>Shared with Windows. YouTube and Discord keep playing.</small>
            </span>
            <Show when={audio() ? !audio()!.preferAsio : true}><span class="wv-font-row__check">ACTIVE</span></Show>
          </button>
          <button
            type="button"
            class="wv-font-row"
            classList={{ 'is-active': !!audio()?.preferAsio }}
            disabled={audioBusy()}
            aria-pressed={!!audio()?.preferAsio}
            onClick={() => void applyAudio(true, audio()?.bufferFrames ?? 0)}
          >
            <span>
              <strong>ASIO</strong>
              <small>
                {audio()?.asioCompiled
                  ? 'Lowest latency. Needs your interface driver or ASIO4ALL. Other apps typically go silent.'
                  : 'This WAVE build has no ASIO host yet. The switch is remembered; WAVE stays on WASAPI until a build with ASIO opens a driver.'}
              </small>
            </span>
            <Show when={!!audio()?.preferAsio}><span class="wv-font-row__check">ON</span></Show>
          </button>
        </div>

        <p class="wv-settings__subhead">Buffer</p>
        <div class="wv-buffer-list" role="group" aria-label="Audio buffer size">
          <For each={[...AUDIO_BUFFER_CHOICES]}>{(frames) => (
            <button
              type="button"
              class="wv-buffer-chip"
              classList={{ 'is-active': (audio()?.bufferFrames ?? 0) === frames }}
              disabled={audioBusy()}
              aria-pressed={(audio()?.bufferFrames ?? 0) === frames}
              onClick={() => void applyAudio(audio()?.preferAsio ?? false, frames)}
            >
              <strong>{audioBufferLabel(frames)}</strong>
              <small>
                {frames === 0
                  ? 'Host default'
                  : audioBufferMs(frames, audio()?.stream.sampleRate ?? 48_000)}
              </small>
            </button>
          )}</For>
        </div>

        <Show when={audio()?.stream.running}>
          <p class="wv-audio-readout">
            Running {audio()!.stream.backend.toUpperCase()}
            {audio()!.stream.deviceName ? ` · ${audio()!.stream.deviceName}` : ''}
            {audio()!.stream.bufferFrames
              ? ` · ${audio()!.stream.bufferFrames} frames · ${audio()!.stream.latencyMs.toFixed(1)} ms`
              : ' · measuring callback…'}
          </p>
        </Show>
        <Show when={!!audio()?.stream.note}>
          <p class="wv-audio-note">{audio()!.stream.note}</p>
        </Show>
      </section>

      <section class="wv-settings__section">
        <div class="wv-settings__section-head">
          <div>
            <h2>Audio input</h2>
            <p>
              Where the REC button records from. WAVE opens this device only while the transport is
              armed, so nothing else on the machine loses the microphone in between. A take is
              converted to the project’s sample rate as it is captured, so an interface running at a
              different rate still lines up frame for frame.
            </p>
          </div>
          <span class="wv-settings__value">
            {inputStatus() && inputStatus()!.state !== 'idle'
              ? `${inputStatus()!.state.toUpperCase()} · ${(inputStatus()!.deviceSampleRate / 1000).toFixed(1)} kHz`
              : app.inputDevices().length > 0
                ? `${app.inputDevices().length} FOUND`
                : 'NONE'}
          </span>
        </div>

        <Show
          when={app.inputDevices().length > 0}
          fallback={
            <p class="wv-settings__empty">
              No capture device. Plug in an audio interface or a USB microphone — and on Windows,
              check that WAVE is allowed to use the microphone in Privacy settings — then refresh.
            </p>
          }
        >
          <div class="wv-input-list">
            <button
              type="button"
              class="wv-midi-row"
              classList={{ 'is-on': (inputStatus()?.deviceId ?? null) === null }}
              aria-pressed={(inputStatus()?.deviceId ?? null) === null}
              disabled={inputBusy()}
              onClick={() => void runInput(() => app.setInputDevice(null))}
            >
              <span class="wv-midi-row__lamp" />
              <span>
                <strong>System default</strong>
                <small>
                  {defaultDevice()
                    ? `Follows Windows — currently ${defaultDevice()!.name}`
                    : 'Follows whatever the system calls the default input'}
                </small>
              </span>
              <span class="wv-midi-row__check">
                {(inputStatus()?.deviceId ?? null) === null ? 'ACTIVE' : ''}
              </span>
            </button>
            <For each={app.inputDevices()}>{(device) => (
              <button
                type="button"
                class="wv-midi-row"
                classList={{
                  'is-on': inputStatus()?.deviceId === device.id,
                  'is-live': isOpen(device.id),
                }}
                aria-pressed={inputStatus()?.deviceId === device.id}
                disabled={inputBusy()}
                onClick={() => void runInput(() => app.setInputDevice(device.id))}
              >
                <span class="wv-midi-row__lamp" classList={{ 'is-live': isOpen(device.id) }} />
                <span>
                  <strong>{device.name}</strong>
                  <small>{inputDeviceNote(device.channels, device.sampleRate)}</small>
                </span>
                <span class="wv-midi-row__check">
                  {inputStatus()?.deviceId === device.id ? 'ACTIVE' : ''}
                </span>
              </button>
            )}</For>
          </div>
        </Show>

        <div class="wv-input-monitor">
          <button
            type="button"
            class="wv-settings__refresh"
            disabled={inputBusy() || app.inputDevices().length === 0}
            onClick={() => void runInput(() => app.setInputArmed(inputStatus()?.state === 'idle'))}
          >
            {inputStatus() && inputStatus()!.state !== 'idle' ? 'Stop monitoring' : 'Monitor level'}
          </button>
          <Show
            when={inputStatus() && inputStatus()!.state !== 'idle'}
            fallback={
              <span class="wv-input-monitor__hint">
                Arm the input to check the level before you record.
              </span>
            }
          >
            <span class="wv-rec__meter" aria-hidden="true">
              <span class="wv-rec__rms" ref={settingsRmsEl} />
              <span class="wv-rec__peak" ref={settingsPeakEl} />
            </span>
            <span class="wv-input-monitor__db" ref={settingsDbEl}>
              -∞ dB
            </span>
          </Show>
        </div>

        <Show when={!!inputStatus()?.fault}>
          <p class="wv-audio-note">{inputStatus()!.fault}</p>
        </Show>
        <Show when={inputStatus() && inputStatus()!.state !== 'idle'}>
          <p class="wv-audio-readout">
            Recording from {inputStatus()!.deviceName} ·{' '}
            {inputDeviceNote(inputStatus()!.deviceChannels, inputStatus()!.deviceSampleRate)} · takes
            are written at {(inputStatus()!.sampleRate / 1000).toFixed(1)} kHz, up to{' '}
            {Math.round(inputStatus()!.maxSeconds / 60)} minutes
          </p>
        </Show>
        <button
          type="button"
          class="wv-settings__refresh"
          onClick={() => void runInput(() => app.refreshInputDevices())}
        >
          Refresh
        </button>
      </section>

      <section class="wv-settings__section">
        <div class="wv-settings__section-head">
          <div>
            <h2>MIDI controllers</h2>
            <p>Nuty z włączonego kontrolera grają wybrany kanał w Racku. MIDI Learn na pokrętła przyjdzie później.</p>
          </div>
          <span class="wv-settings__value">{paired() ? `${paired()} ON` : 'OFF'}</span>
        </div>
        <Show
          when={ports().length > 0}
          fallback={
            <p class="wv-settings__empty">Brak portów MIDI. Podłącz klawiaturę USB albo utwórz port loopMIDI, potem odśwież.</p>
          }
        >
          <div class="wv-midi-list">
            <For each={ports()}>{(port) => (
              <button
                type="button"
                class="wv-midi-row"
                classList={{ 'is-on': port.enabled, 'is-live': live(port) }}
                aria-pressed={port.enabled}
                onClick={() => void toggle(port)}
              >
                <span class="wv-midi-row__lamp" classList={{ 'is-live': live(port) }} />
                <span>
                  <strong>{port.name}</strong>
                  <small>{statusNote(port)}</small>
                </span>
                <span class="wv-midi-row__check">{port.enabled ? 'ON' : 'OFF'}</span>
              </button>
            )}</For>
          </div>
        </Show>
        <button type="button" class="wv-settings__refresh" onClick={() => void refreshMidi()}>
          Refresh
        </button>
      </section>

      <section class="wv-settings__section">
        <div class="wv-settings__section-head">
          <div>
            <h2>Preview volume</h2>
            <p>Sounds and Audio previews play at this percentage of the current master volume.</p>
          </div>
          <span class="wv-settings__value">{Math.round(previewPercent())}%</span>
        </div>
        <label class="wv-preview-vol">
          <span>0%</span>
          <input
            type="range"
            min="0"
            max="100"
            step="1"
            value={previewPercent()}
            aria-label="Preview volume as a percent of master"
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={Math.round(previewPercent())}
            aria-valuetext={`${Math.round(previewPercent())} percent of master volume`}
            onInput={(event) => {
              const next = clampPreviewVolumePercent(Number(event.currentTarget.value));
              setPreviewPercent(next);
              void app.backend.setPreviewVolume(next).catch(app.report);
            }}
          />
          <span>100%</span>
        </label>
      </section>

      <section class="wv-settings__section">
        <div class="wv-settings__section-head"><div><h2>Color theme</h2><p>Wybierz charakter powierzchni roboczej.</p></div><span class="wv-settings__value">{theme.name().toUpperCase()}</span></div>
        <div class="wv-theme-grid">
          <For each={THEME_OPTIONS}>{(option) => (
            <button type="button" class="wv-theme-card" classList={{ 'is-active': theme.name() === option.name }} aria-pressed={theme.name() === option.name} onClick={() => theme.setTheme(option.name as ThemeName)}>
              <span class="wv-theme-card__preview" style={{ background: paletteFor(option.name).sunk }}>
                <i style={{ background: accentFor(theme.accent(), option.name) }} />
                <i style={{ background: paletteFor(option.name).line2 }} />
                <i style={{ background: paletteFor(option.name).live }} />
              </span>
              <span class="wv-theme-card__copy"><strong>{option.label}</strong><small>{option.description}</small></span>
              <Show when={theme.name() === option.name}><span class="wv-theme-card__check">●</span></Show>
            </button>
          )}</For>
        </div>
      </section>

      <section class="wv-settings__section">
        <div class="wv-settings__section-head"><div><h2>Interface font</h2><p>Font wpływa na wszystkie etykiety i kontrolki WAVE.</p></div><span class="wv-settings__value">{theme.font() === 'mono' ? 'MONO' : theme.font().toUpperCase()}</span></div>
        <div class="wv-font-list">
          <For each={FONTS}>{(font) => <button type="button" class="wv-font-row" classList={{ 'is-active': theme.font() === font.name }} onClick={() => theme.setFont(font.name)}><span class={`wv-font-row__sample wv-font-row__sample--${font.name}`}>Aa</span><span><strong>{font.label}</strong><small>{font.note}</small></span><Show when={theme.font() === font.name}><span class="wv-font-row__check">ACTIVE</span></Show></button>}</For>
        </div>
      </section>
    </main>
  );
}

export default Settings;
