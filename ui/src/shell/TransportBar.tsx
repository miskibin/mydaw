import { type JSX } from 'solid-js';
import { formatTimecode } from '../lib/playhead';
import { useAppState } from '../state';

/** Placeholder tempo until a transport plugin owns it (Phase 2). */
const PLACEHOLDER_BPM = 128;

/**
 * Top chrome: transport buttons wired to the backend abstraction, plus the
 * position and tempo readouts. Tempo is a placeholder -- nothing on the Rust
 * side reports it yet.
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
