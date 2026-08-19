import { Show, type JSX } from 'solid-js';
import { useAppState } from '../state';
import { DRUM_PAD_HINT, noteName } from '../lib/keyboard';
import { registeredPanelKeys } from '../panels/registry';

/** What the computer keyboard does right now, given the selected source. */
const PIANO_HINT = 'A W S E D F T G Y H U J, Z/X to shift octave';

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
        What the computer keyboard is currently playing, and what it can play.
        The hint changes with the source because the drum pads only exist for
        the kit -- a static hint would be wrong three quarters of the time.
      */}
      <Show when={app.drumPadsActive()}>
        <span class="statusbar__item statusbar__item--hint" title="Drum pads, in addition to the piano keys">
          {DRUM_PAD_HINT}
        </span>
      </Show>

      {/*
        The piano readout. `octave` is the only way to know which notes A-J will
        play, and the held list is the only feedback that a key press reached
        the engine at all.
      */}
      <span
        class="statusbar__item statusbar__item--mono statusbar__item--notes"
        classList={{ 'is-playing': app.heldNotes().length > 0 }}
        title={
          app.drumPadsActive()
            ? `Piano: ${PIANO_HINT}. Drum pads: ${DRUM_PAD_HINT}`
            : `Computer keyboard piano: ${PIANO_HINT}`
        }
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
