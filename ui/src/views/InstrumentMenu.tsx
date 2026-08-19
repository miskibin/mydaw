/**
 * Choosing what a channel plays -- either replacing one channel's instrument or
 * picking the instrument for a new one.
 *
 * One list for three placement mechanisms. A built-in is a name the engine
 * looks up; a CLAP or VST3 instrument is a bundle loaded from disk, which can
 * fail in ways a built-in cannot. The list hides that difference from the user
 * and keeps it visible in the code, which is the right way round.
 *
 * VST3 instruments are folded in here from the plugin scan rather than from
 * `get_sources`, which deliberately does not list them.
 */

import { For, Show, createMemo, createSignal, onMount } from 'solid-js';

import { useAppState } from '../state';

interface Choice {
  id: string;
  label: string;
  detail: string;
  /** How it is placed: which command, and with what. */
  place: (track: number) => Promise<unknown>;
}

export function InstrumentMenu(props: {
  /** The channel being changed, or `null` when adding one. */
  track: number | null;
  onClose: () => void;
  onAdd: (sourceId: string) => void;
}): ReturnType<typeof Show> {
  const app = useAppState();
  const [filter, setFilter] = createSignal('');

  // The plugin list is cache-only until Rescan; opening this menu must not
  // load unlicensed plugins just to populate a picker.
  onMount(() => void app.loadDevices());

  const choices = createMemo<Choice[]>(() => {
    const fromSources = app.sources().map<Choice>((source) => ({
      id: source.id,
      label: source.label,
      detail:
        source.kind === 'clap'
          ? `CLAP · ${source.vendor || source.pluginId}`
          : source.id.startsWith('sampler:')
            ? 'sampler'
            : 'built-in',
      place: (track) =>
        source.kind === 'clap' && source.bundlePath && source.pluginId
          ? app.backend.setTrackSourceClap(track, source.bundlePath, source.pluginId)
          : app.backend.setTrackSource(track, source.id),
    }));
    const fromVst3 = app
      .vst3Plugins()
      .filter((plugin) => plugin.kind === 'instrument')
      .map<Choice>((plugin) => ({
        id: `vst3:${plugin.id}`,
        label: plugin.name || plugin.id,
        detail: `VST3 · ${plugin.vendor || 'unknown vendor'}`,
        place: (track) => app.backend.setTrackSourceVst3(track, plugin.path, plugin.id),
      }));
    const needle = filter().trim().toLowerCase();
    const all = [...fromSources, ...fromVst3];
    return needle
      ? all.filter(
          (choice) =>
            choice.label.toLowerCase().includes(needle) ||
            choice.detail.toLowerCase().includes(needle),
        )
      : all;
  });

  const currentSource = () =>
    props.track === null ? null : (app.tracks()[props.track]?.sourceId ?? null);

  function pick(choice: Choice): void {
    props.onClose();
    if (props.track === null) {
      props.onAdd(choice.id);
      return;
    }
    const track = props.track;
    void app.mixerCommand(() => choice.place(track) as Promise<never>);
    app.say('Channel · Instrument', choice.label);
  }

  return (
    <div class="wv-scrim wv-scrim--dim" onClick={() => props.onClose()}>
      <div class="wv-modal" onClick={(event) => event.stopPropagation()}>
        <div class="wv-modal__head">
          <span>
            {props.track === null
              ? 'Add a channel'
              : `Replace instrument — ${app.tracks()[props.track]?.name ?? ''}`}
          </span>
          <span class="wv-spacer" />
          <input
            class="wv-modal__filter"
            placeholder="filter…"
            value={filter()}
            ref={(el) => queueMicrotask(() => el.focus())}
            onInput={(event) => setFilter(event.currentTarget.value)}
            onKeyDown={(event) => {
              event.stopPropagation();
              if (event.key === 'Escape') props.onClose();
            }}
          />
        </div>
        <div class="wv-modal__body">
          <Show when={app.scanning()}>
            <div class="wv-modal__row wv-disabled">scanning installed plugins…</div>
          </Show>
          <For each={choices()}>
            {(choice) => (
              <button
                type="button"
                class="wv-modal__row"
                classList={{ 'is-active': choice.id === currentSource() }}
                onClick={() => pick(choice)}
              >
                <span class="wv-swatch" style={{ background: 'var(--acc)' }} />
                <span>{choice.label}</span>
                <span class="wv-spacer" />
                <span class="wv-modal__kind">{choice.detail}</span>
              </button>
            )}
          </For>
          <Show when={choices().length === 0}>
            <div class="wv-empty">
              Nothing matches “{filter()}”
              <span class="wv-empty__hint">CLAP and VST3 instruments appear here once scanned</span>
            </div>
          </Show>
        </div>
      </div>
    </div>
  );
}
