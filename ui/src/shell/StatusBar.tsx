/**
 * The status bar: what the device is, what the last gesture did, and what went
 * wrong.
 *
 * The mock's CPU meter is **not** here. The engine publishes no CPU figure, and
 * a number invented in the webview would be the most quietly dishonest thing in
 * the whole shell -- a load meter is the one readout people trust without
 * checking. Buffer and latency are real, and come from the open stream.
 */

import { Show } from 'solid-js';

import { midiKeyFor, noteName } from '../lib/keyboard';
import { useAppState } from '../state';
import { useView } from './view';

export function StatusBar(): ReturnType<typeof Show> {
  const app = useAppState();
  const view = useView();

  const qName = () => noteName(midiKeyFor('KeyQ', app.octave()) ?? 60);

  /** Held typing-keyboard notes, low to high — the live footer readout. */
  const sounding = () =>
    [...new Set(app.heldNotes())]
      .sort((a, b) => a - b)
      .map(noteName)
      .join(' · ');

  const hint = () => {
    if (view.rollOpen()) {
      return view.typingKeyboard()
        ? `typing keyboard on · Q is ${qName()} · tools off · Esc closes`
        : 'left draw · shift stretch · right erase · ctrl select · alt unsnap · scrollbar edges zoom · P/B/E/D/C';
    }
    if (view.typingKeyboard()) {
      return `typing keyboard on · Q is ${qName()} · 2 is C# · W is D · scroll the key button for octave · Ctrl+T off`;
    }
    if (view.view() === 'playlist') {
      return 'click a clip to paint it next · drag a run · Shift-drag stretch · P/B paint · E select · D delete · Ctrl+B duplicate';
    }
    return 'F5 playlist · F6 rack · F7 piano roll · F9 mixer · Space play · Ctrl+T keys · Ctrl+Z undo';
  };

  return (
    <footer class="wv-status">
      <span>
        DEVICE{' '}
        <span class="wv-status__value">
          {app.streamInfo()?.running ? app.streamInfo()!.deviceName : 'none'}
        </span>
      </span>
      <span>
        BUFFER{' '}
        <span class="wv-status__value">
          {app.streamInfo()?.running ? app.streamInfo()!.bufferFrames : '—'}
        </span>
      </span>
      <span>
        LATENCY{' '}
        <span class="wv-status__value">
          {app.streamInfo()?.running ? `${app.streamInfo()!.latencyMs.toFixed(1)} ms` : '—'}
        </span>
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
