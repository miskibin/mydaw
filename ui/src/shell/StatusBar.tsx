import { Show, createSignal, onCleanup, onMount, type JSX } from 'solid-js';
import { useAppState } from '../state';
import { DRUM_PAD_HINT, noteName } from '../lib/keyboard';
import { registeredPanelKeys } from '../panels/registry';
import type { AssetStatusDto } from '../api/types';

/** How often the asset line refreshes while a download runs. */
const ASSET_POLL_MS = 500;

const megabytes = (bytes: number): number => Math.round(bytes / 1_000_000);

const percent = (status: AssetStatusDto): number =>
  status.totalBytes > 0 ? Math.floor((100 * status.downloadedBytes) / status.totalBytes) : 0;

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

  // A local signal rather than app state: nothing else in the UI cares, and the
  // whole affordance disappears once the content is installed. Polled while a
  // transfer runs (the command is metadata plus three atomic loads), and left
  // alone otherwise.
  const [assets, setAssets] = createSignal<AssetStatusDto | undefined>();
  const refresh = async () => {
    try {
      const next = await app.backend.getAssetStatus();
      const appeared = next.instrumentReady && !assets()?.instrumentReady;
      setAssets(next);
      // A newly loaded instrument is a new entry in the source picker, and
      // nothing else would tell it.
      if (appeared) app.refresh();
    } catch {
      // A backend without the command is a backend with nothing to offer here;
      // leaving the line hidden is the right failure.
    }
  };
  onMount(() => {
    void refresh();
    // Polled only while something is in flight -- a transfer, or the decode
    // that follows one. An installed, loaded session polls nothing.
    const timer = setInterval(() => {
      const status = assets();
      if (!status || status.instrumentReady) return;
      if (status.state !== 'idle' || status.installed) void refresh();
    }, ASSET_POLL_MS);
    onCleanup(() => clearInterval(timer));
  });
  /**
   * The status, but only while there is something to say about it: before the
   * content is playable, and only when this build has content to offer.
   * Returning the object rather than a boolean is what gives `<Show>`'s child
   * the narrowed value.
   */
  const pending = (): AssetStatusDto | undefined => {
    const status = assets();
    return status && !status.instrumentReady && status.fileCount > 0 ? status : undefined;
  };
  const download = async () => {
    try {
      setAssets(await app.backend.startAssetDownload());
    } catch {
      void refresh();
    }
  };

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

      {/*
        Which project this is. The window title says it too, but a maximised DAW
        is mostly a window whose title bar you cannot see, and this line always
        can be. The path is the tooltip rather than the label: it is what tells
        two projects of the same name apart, and it is far too long for a bar
        that also has to fit the piano readout.
      */}
      <span
        class="statusbar__item"
        title={app.project().path ?? 'This project has never been saved'}
      >
        project: <strong>{app.project().name}</strong>
      </span>

      {/*
        A count here and the names on the strips: this is the line that says
        *something* is wrong with the project you just opened, and the mixer is
        where you find out what. Nothing was lost -- the references are still in
        the file -- so this is a warning rather than an error.
      */}
      <Show when={app.project().missingPlugins > 0}>
        <span
          class="statusbar__item statusbar__item--warn"
          title="The mixer strips say which. Install them and reopen the project to get them back."
        >
          {app.project().missingPlugins}{' '}
          {app.project().missingPlugins === 1 ? 'plugin' : 'plugins'} missing
        </span>
      </Show>

      {/*
        The bundled sample content. Absent from a checkout on purpose (131 MB of
        CC-BY piano), so a first run has to offer to fetch it -- and must not
        fetch it unasked, which is why this is a button and not an effect.
        Disappears for good once installed, so the common case costs one line of
        nothing. Deliberately plain: a full-width banner would be a permanent
        tax on every session after the first.
      */}
      <Show when={pending()}>
        {(status) => (
          <span
            class="statusbar__item statusbar__item--hint"
            title={status().error || `${status().instrumentName}, verified against pinned checksums`}
          >
            <Show
              when={!status().installed && status().state !== 'downloading'}
              fallback={
                status().installed
                  ? `Loading ${status().instrumentName}…`
                  : `Downloading ${status().instrumentName}… ${percent(status())}%`
              }
            >
              <button class="link-button" type="button" onClick={() => void download()}>
                Download {status().instrumentName} ({megabytes(status().totalBytes)} MB)
              </button>
            </Show>
          </span>
        )}
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
