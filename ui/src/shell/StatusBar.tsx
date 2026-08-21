/**
 * The status bar: what the device is, what the last gesture did, and what went
 * wrong.
 *
 * The mock's CPU meter is **not** here. The engine publishes no CPU figure, and
 * a number invented in the webview would be the most quietly dishonest thing in
 * the whole shell -- a load meter is the one readout people trust without
 * Buffer and latency are the open stream's last callback, not the engine
 * allocation ceiling. Backend (WASAPI/ASIO) is part of DEVICE.
 */

import { Show, createSignal, onCleanup, onMount } from 'solid-js';

import { midiKeyFor, noteName } from '../lib/keyboard';
import { useAppState } from '../state';
import { useView } from './view';
import type { MidiStatusDto } from '../api/types';

const MIDI_POLL_MS = 400;
const MIDI_LIVE_MS = 250;

export function StatusBar(): ReturnType<typeof Show> {
  const app = useAppState();
  const view = useView();
  const [midi, setMidi] = createSignal<MidiStatusDto>();
  const [now, setNow] = createSignal(Date.now());

  onMount(() => {
    let alive = true;
    const tick = (): void => {
      setNow(Date.now());
      void app.backend
        .getMidiStatus()
        .then((status) => {
          if (alive) setMidi(status);
        })
        .catch(() => {
          /* the status bar is a readout, not an error surface */
        });
    };
    tick();
    const id = setInterval(tick, MIDI_POLL_MS);
    const streamId = setInterval(() => {
      void app.refreshStreamInfo();
    }, MIDI_POLL_MS);
    onCleanup(() => {
      alive = false;
      clearInterval(id);
      clearInterval(streamId);
    });
  });

  const midiLive = () => {
    const status = midi();
    if (!status || status.enabledCount === 0) return false;
    return now() - status.activityAtMs < MIDI_LIVE_MS;
  };

  const midiLabel = () => {
    const status = midi();
    if (!status || status.enabledCount === 0) return 'off';
    return status.activeName ?? `${status.enabledCount} on`;
  };

  const qName = () => noteName(midiKeyFor('KeyQ', app.octave()) ?? 60);

  /** Held typing-keyboard notes, low to high — the live footer readout. */
  const sounding = () =>
    [...new Set(app.heldNotes())]
      .sort((a, b) => a - b)
      .map(noteName)
      .join(' · ');

  const hint = () =>
    view.rollOpen()
      ? view.typingKeyboard()
        ? `typing keyboard on · Q is ${qName()} · tools off · Esc closes`
        : 'left draw · shift stretch · right erase · ctrl select · alt unsnap · scrollbar edges zoom · F fit notes · P/B/E/D/C'
      : view.typingKeyboard()
        ? `typing keyboard on · Q is ${qName()} · 2 is C# · W is D · scroll the key button for octave · Ctrl+T off`
        : 'F5 playlist · F6 rack · F7 piano roll · F8 browser · F9 mixer · Tab playlist/rack · Space play · Ctrl+T keys · Ctrl+Z undo';

  return (
    <footer class="wv-status">
      <span>
        DEVICE{' '}
        <span class="wv-status__value">
          {app.streamInfo()?.running
            ? `${app.streamInfo()!.backend.toUpperCase()} · ${app.streamInfo()!.deviceName || 'none'}`
            : 'none'}
        </span>
      </span>
      <span>
        BUFFER{' '}
        <span class="wv-status__value">
          {app.streamInfo()?.running
            ? app.streamInfo()!.bufferFrames || '…'
            : '—'}
        </span>
      </span>
      <span>
        LATENCY{' '}
        <span class="wv-status__value">
          {app.streamInfo()?.running ? `${app.streamInfo()!.latencyMs.toFixed(1)} ms` : '—'}
        </span>
      </span>
      <span class="wv-status__midi">
        MIDI{' '}
        <span class="wv-status__midi-lamp" classList={{ 'is-live': midiLive() }} />
        <span class="wv-status__value">{midiLabel()}</span>
      </span>

      <Show when={sounding()}>
        <div class="wv-readout wv-readout--note">
          <span class="wv-readout__name">{app.heldNotes().length > 1 ? 'CHORD' : 'NOTE'}</span>
          <span class="wv-readout__value">{sounding()}</span>
        </div>
      </Show>

      <Show when={app.param() && !sounding()}>
        <div class="wv-readout">
          <span class="wv-readout__name">{app.param()!.name}</span>
          <span class="wv-readout__value">{app.param()!.value}</span>
        </div>
      </Show>

      <div class="wv-spacer" />

      <Show when={app.kernelError()}>
        <span class="wv-status__error" title={app.kernelError()!}>
          {app.kernelError()}
        </span>
      </Show>
      <Show when={!app.kernelError()}>
        <span>{hint()}</span>
      </Show>

      <Show when={app.backend.kind === 'mock'}>
        <span class="wv-status__value" title="No engine behind this window: the browser mock is answering">
          MOCK
        </span>
      </Show>
    </footer>
  );
}
