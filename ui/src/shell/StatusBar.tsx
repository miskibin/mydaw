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

import { useAppState } from '../state';
import { useView } from './view';

export function StatusBar(): ReturnType<typeof Show> {
  const app = useAppState();
  const view = useView();

  const hint = () =>
    view.rollOpen()
      ? 'shift-drag or the Select tool marquees · shift-click adds to the selection · Ctrl+A all · Ctrl+D duplicate · right-click deletes'
      : 'F5 playlist · F6 rack · F7 piano roll · F9 mixer · Space play · Ctrl+Z undo';

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

      <Show when={app.param()}>
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
