import { createSignal, Index, Show, type JSX } from 'solid-js';
import { formatTimecode } from '../lib/playhead';
import { useAppState } from '../state';

/** Tempo bounds, mirroring `MIN_BPM`/`MAX_BPM` in `crates/rt-core`. */
const MIN_BPM = 20;
const MAX_BPM = 999;

/** Beats per pixel of vertical drag. One BPM per 4 px is the usual DAW feel. */
const PIXELS_PER_BPM = 4;

/**
 * Top chrome: transport buttons wired to the backend abstraction, the
 * instrument selector, the tempo control, and the position readout.
 *
 * # The tempo control
 * A number input *and* a vertical drag, because both are how people set a
 * tempo: typing 174 is exact, and dragging is how you find out that 128 was
 * better. Both funnel into the same `setBpm`, which clamps here and is rejected
 * by Rust if it somehow still escapes the range -- a tempo of 0 would divide the
 * scheduler's step length by zero, so it is checked in both languages.
 *
 * # The source selector is a dropdown now
 * It used to be a row of segmented buttons, on the grounds that the list was
 * short, fixed, and worth reading at a glance. The list is no longer any of
 * those things: it is the three built-ins plus every CLAP instrument installed
 * on the machine, which on a working system is dozens and has no upper bound.
 * A button row would either overflow the bar or squeeze the transport off it.
 * A `<select>` costs a click to *browse* but still shows the current choice
 * without one, which is the part that mattered.
 *
 * The optgroups are the same two the mixer strip uses -- `Built in` and
 * `CLAP instruments` -- because they are the same list, and a user who learns
 * the shape of it in one place should recognise it in the other.
 */
export function TransportBar(): JSX.Element {
  const app = useAppState();
  /** Text being typed, while the input has focus. `null` means "show the state". */
  const [draft, setDraft] = createSignal<string | null>(null);

  const clampBpm = (value: number) => Math.min(MAX_BPM, Math.max(MIN_BPM, value));

  /** Name of the track the selector and the keyboard address. */
  const selectedTrackName = () => {
    const mixer = app.mixer();
    return mixer?.tracks.find((track) => track.selected)?.name;
  };

  const builtIns = () => app.sources().filter((source) => source.kind === 'internal');
  const instruments = () => app.sources().filter((source) => source.kind === 'clap');

  /** The source currently on the selected track, `''` before the list arrives. */
  const currentSourceId = () => app.currentSource()?.id ?? '';

  /**
   * Put the chosen source on the selected track. Which track that is comes from
   * the mixer rather than being remembered here, so this bar and the mixer panel
   * can never disagree about who the keyboard is playing.
   */
  const pick = (id: string) => {
    const source = app.sources().find((entry) => entry.id === id);
    const track = app.mixer()?.selected;
    if (source && track !== undefined) app.placeSource(track, source);
  };

  /** Accept what was typed, ignoring anything that is not a tempo. */
  const commitDraft = () => {
    const text = draft();
    setDraft(null);
    if (text === null) return;
    const parsed = Number.parseFloat(text);
    if (Number.isFinite(parsed)) app.setBpm(clampBpm(parsed));
  };

  /**
   * Vertical drag: pointer capture so the gesture survives leaving the input,
   * and up is faster. The value is applied continuously -- a tempo you cannot
   * hear until you let go is a tempo you cannot dial in.
   */
  const startDrag = (event: PointerEvent) => {
    if (event.button !== 0) return;
    const target = event.currentTarget as HTMLElement;
    const startY = event.clientY;
    const startBpm = app.bpm();
    let moved = false;

    const move = (moveEvent: PointerEvent) => {
      const delta = Math.round((startY - moveEvent.clientY) / PIXELS_PER_BPM);
      if (delta === 0 && !moved) return;
      moved = true;
      app.setBpm(clampBpm(startBpm + delta));
    };
    const end = () => {
      target.removeEventListener('pointermove', move);
      target.removeEventListener('pointerup', end);
      target.removeEventListener('pointercancel', end);
      // A drag that moved must not also land as a click-to-focus-and-select.
      if (moved) target.blur();
    };

    target.setPointerCapture(event.pointerId);
    target.addEventListener('pointermove', move);
    target.addEventListener('pointerup', end);
    target.addEventListener('pointercancel', end);
  };

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
          {/*
            The selector sets the source of the *selected track* -- the same one
            the keyboard plays and the same one the mixer highlights. Naming the
            track here is what keeps that from being a surprise; the mixer panel
            is where you change which track it is.
          */}
          <label class="source__label" for="source" title="The source of the selected track">
            {selectedTrackName() ?? 'Source'}
          </label>
          {/*
            `<Index>`, not `<For>`: the whole list is re-fetched after every
            mixer command, so each `SourceDto` is a new object every time and
            `<For>` would rebuild every option on a fader move. The order is
            fixed (built-ins, then the scan), so keying by position is right.
          */}
          <select
            id="source"
            class="source__select"
            value={currentSourceId()}
            aria-label="Instrument source for the selected track"
            title={app.currentSource()?.description ?? 'The source of the selected track'}
            onChange={(event) => pick(event.currentTarget.value)}
          >
            <optgroup label="Built in">
              <Index each={builtIns()}>
                {(source) => (
                  <option value={source().id} title={source().description}>
                    {source().label}
                  </option>
                )}
              </Index>
            </optgroup>
            <optgroup label="CLAP instruments">
              <Index each={instruments()}>
                {(source) => (
                  <option
                    value={source().id}
                    title={
                      source().vendor
                        ? `${source().label} — ${source().vendor}`
                        : source().label
                    }
                  >
                    {source().label}
                  </option>
                )}
              </Index>
            </optgroup>
          </select>
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
          <label class="readout__label" for="tempo">
            Tempo
          </label>
          <span class="readout__value readout__value--mono tempo">
            <input
              id="tempo"
              class="tempo__input"
              type="number"
              inputmode="decimal"
              min={MIN_BPM}
              max={MAX_BPM}
              step="0.1"
              value={draft() ?? app.bpm().toFixed(1)}
              title={`Tempo in BPM (${MIN_BPM}–${MAX_BPM}). Type a value, or drag up and down.`}
              onPointerDown={startDrag}
              onInput={(event) => setDraft(event.currentTarget.value)}
              onBlur={commitDraft}
              onKeyDown={(event) => {
                if (event.key === 'Enter') event.currentTarget.blur();
                if (event.key === 'Escape') {
                  setDraft(null);
                  event.currentTarget.blur();
                }
              }}
            />
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
