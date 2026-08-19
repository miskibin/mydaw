import { For, Show, type JSX } from 'solid-js';
import { formatTimecode } from '../lib/playhead';
import { useAppState } from '../state';

/** Placeholder tempo until a transport plugin owns it (Phase 2). */
const PLACEHOLDER_BPM = 128;

/**
 * Top chrome: transport buttons wired to the backend abstraction, the
 * instrument selector, and the position and tempo readouts. Tempo is a
 * placeholder -- nothing on the Rust side reports it yet.
 *
 * The source selector is segmented buttons rather than a dropdown because the
 * list is short, fixed, and worth reading at a glance: which instrument is
 * playing is the single most important piece of state on this bar, and a
 * collapsed `<select>` would hide it behind a click. An unavailable source
 * (`CLAP`, when no bundle was named at startup) stays visible and disabled --
 * "you could have one" is useful, "it does not exist" is not.
 */
export function TransportBar(): JSX.Element {
  const app = useAppState();

  return (
    <header class="transport">
      <div class="transport__brand">
        <span class="transport__logo">MYDAW</span>
        <span class="transport__phase">phase 0</span>
      </div>

      <div class="transport__controls" role="group" aria-label="Transport">
        <button
          type="button"
          class="transport__button transport__button--play"
          classList={{ 'is-active': app.playing() }}
          aria-pressed={app.playing()}
          title="Play (backend: transport_play)"
          onClick={() => app.play()}
        >
          <svg viewBox="0 0 14 14" width="13" height="13" aria-hidden="true">
            <path d="M3 1.5 L12 7 L3 12.5 Z" fill="currentColor" />
          </svg>
          <span>Play</span>
        </button>
        <button
          type="button"
          class="transport__button"
          title="Stop (backend: transport_stop)"
          onClick={() => app.stop()}
        >
          <svg viewBox="0 0 14 14" width="13" height="13" aria-hidden="true">
            <rect x="3" y="3" width="8" height="8" fill="currentColor" />
          </svg>
          <span>Stop</span>
        </button>
      </div>

      <Show when={app.sources().length > 0}>
        <div class="source">
          <span class="source__label">Source</span>
          <div class="segmented" role="group" aria-label="Instrument source">
            <For each={app.sources()}>
              {(source) => (
                <button
                  type="button"
                  class="segmented__button"
                  classList={{ 'is-active': source.selected }}
                  disabled={!source.available}
                  aria-pressed={source.selected}
                  title={
                    source.available
                      ? source.description
                      : `${source.description} — not loaded in this session`
                  }
                  onClick={() => app.selectSource(source.id)}
                >
                  {source.label}
                </button>
              )}
            </For>
          </div>
        </div>
      </Show>

      <div class="transport__readouts">
        <div class="readout">
          <span class="readout__label">Position</span>
          <span class="readout__value readout__value--mono">
            {formatTimecode(app.positionSeconds())}
          </span>
        </div>
        <div class="readout">
          <span class="readout__label">Tempo</span>
          <span class="readout__value readout__value--mono" title="Placeholder — no tempo service yet">
            {PLACEHOLDER_BPM.toFixed(2)}
            <small> BPM</small>
          </span>
        </div>
        <div class="readout">
          <span class="readout__label">Signature</span>
          <span class="readout__value readout__value--mono">4/4</span>
        </div>
      </div>
    </header>
  );
}
