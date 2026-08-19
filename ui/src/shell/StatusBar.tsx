import { Show, type JSX } from 'solid-js';
import { useAppState } from '../state';

/**
 * The bottom strip: what the audio device is, what went wrong, and which
 * backend is answering.
 *
 * BUFFER and LATENCY come from `get_stream_info`, i.e. from the block size the
 * device actually opened with -- not from a setting somebody typed. CPU is not
 * here: the mock design shows one, and MYDAW has no DSP load figure yet. An
 * invented percentage in a status bar is worse than an absent one.
 */
export function StatusBar(): JSX.Element {
  const app = useAppState();

  const info = () => app.streamInfo();
  const running = () => info()?.running === true;

  return (
    <footer class="wv-status">
      <span>
        DEVICE{' '}
        <b>{running() ? (info()?.deviceName ?? 'unknown') : 'none'}</b>
      </span>
      <span>
        BUFFER <b>{running() ? info()?.bufferFrames : '—'}</b>
      </span>
      <span>
        LATENCY <b>{running() ? `${info()?.latencyMs.toFixed(1)} ms` : '—'}</b>
      </span>
      <span>
        RATE <b>{running() ? `${((info()?.sampleRate ?? 0) / 1000).toFixed(1)} kHz` : '—'}</b>
      </span>

      <span class="wv-spacer" />

      <Show when={app.kernelError()}>
        {(message) => (
          <span class="wv-status__error" title={message()}>
            {message()}
          </span>
        )}
      </Show>
      <Show when={app.project().missingPlugins > 0}>
        <span class="wv-status__error">
          {app.project().missingPlugins} plugin
          {app.project().missingPlugins === 1 ? '' : 's'} missing
        </span>
      </Show>

      <span>
        BACKEND <b>{app.backend.kind}</b>
      </span>
      <span class="wv-disabled" title="Autosave — coming soon">
        AUTOSAVE <b>off</b>
      </span>
    </footer>
  );
}
