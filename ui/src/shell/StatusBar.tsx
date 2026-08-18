import { Show, type JSX } from 'solid-js';
import { useAppState } from '../state';
import { noteName } from '../lib/keyboard';
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

      {/*
        The piano readout. `octave` is the only way to know which notes A-J will
        play, and the held list is the only feedback that a key press reached
        the engine at all -- worth a few characters of chrome, especially while
        the instrument chain is behind an environment variable.
      */}
      <span
        class="statusbar__item statusbar__item--mono statusbar__item--notes"
        classList={{ 'is-playing': app.heldNotes().length > 0 }}
        title="Computer keyboard piano: A W S E D F T G Y H U J, Z/X to shift octave"
      >
        <span class="statusbar__label">oct</span> {app.octave()}
        <Show when={app.heldNotes().length > 0} fallback={<span class="statusbar__muted"> —</span>}>
          {' '}
          {app.heldNotes().map(noteName).join(' ')}
        </Show>
      </span>

      <span class="statusbar__item statusbar__item--mono">
        {app.playing() ? 'PLAYING' : 'STOPPED'}
      </span>
      <span class="statusbar__item statusbar__item--mono">
        {Math.round(app.positionSeconds() * app.playhead.getSampleRate()).toLocaleString()} spl
      </span>
    </footer>
  );
}
