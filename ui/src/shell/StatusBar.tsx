import { Show, type JSX } from 'solid-js';
import { useAppState } from '../state';
import { registeredPanelKeys } from '../panels/registry';

const KERNEL_LABEL = {
  connecting: 'Connecting to kernel',
  ready: 'Kernel ready',
  error: 'Kernel unreachable',
} as const;

/** Bottom chrome: kernel lifecycle state, backend in use, and counts. */
export function StatusBar(): JSX.Element {
  const app = useAppState();
  const startedCount = () => app.plugins().filter((p) => p.state === 'started').length;
  const unknownPanels = () =>
    app.panels().filter((p) => !registeredPanelKeys().includes(p.component)).length;

  return (
    <footer class="statusbar">
      <span class="statusbar__item statusbar__item--kernel">
        <span class={`state-dot state-dot--kernel-${app.kernelState()}`} aria-hidden="true" />
        {KERNEL_LABEL[app.kernelState()]}
      </span>

      <span class="statusbar__item">
        backend: <strong>{app.backend.kind}</strong>
      </span>

      <span class="statusbar__item">
        plugins: {startedCount()}/{app.plugins().length} started
      </span>

      <span class="statusbar__item">panels: {app.panels().length}</span>

      <Show when={unknownPanels() > 0}>
        <span class="statusbar__item statusbar__item--warn">
          {unknownPanels()} unregistered
        </span>
      </Show>

      <span class="statusbar__spacer" />

      <Show when={app.kernelError()}>
        {(message) => <span class="statusbar__item statusbar__item--warn">{message()}</span>}
      </Show>

      <span class="statusbar__item statusbar__item--mono">
        {app.playing() ? 'PLAYING' : 'STOPPED'}
      </span>
      <span class="statusbar__item statusbar__item--mono">
        {Math.round(app.positionSeconds() * app.playhead.getSampleRate()).toLocaleString()} spl
      </span>
    </footer>
  );
}
