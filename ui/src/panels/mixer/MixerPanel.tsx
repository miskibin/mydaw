import {
  For,
  Index,
  Show,
  createSignal,
  onCleanup,
  onMount,
  type Accessor,
  type JSX,
} from 'solid-js';
import { useAppState } from '../../state';
import type { TrackDto } from '../../api/types';
import type { PanelProps } from '../types';
import { faderFraction, faderToDb, formatDb, panLabel, peakFraction } from './scale';

/**
 * The mixer: one strip per track, and the master on the right.
 *
 * # Why DOM and not a canvas
 * Same answer as the step sequencer, for the same reason. A strip is a fader, a
 * pan slider, two buttons and a name -- controls that want keyboard focus,
 * labels and the browser's own hit-testing. The *meters* inside them are the
 * only part that moves at frame rate, and those are two `<div>`s per channel
 * whose height is written from an animation frame; painting a whole canvas per
 * strip to move four rectangles would cost more than it saved.
 *
 * # Where the numbers come from
 * Two different streams, deliberately kept apart:
 *
 * - **Control state** (`app.mixer()`) is a Solid signal, written once per
 *   command. Every mixer command answers with the whole `MixerState`, so a
 *   click re-renders the strips and nothing else.
 * - **Levels** come off the `MeterBus`, which is a plain mutable holder, read
 *   from this panel's own `requestAnimationFrame` loop. Routing 30-60 frames a
 *   second through a signal would re-run every dependent computation at that
 *   rate for a number that only paints a rectangle.
 *
 * # Faders are exponential, not linear
 * A linear dB fader spends half its travel between -60 and -30 dB, where
 * nothing musical happens. `scale.ts` maps travel to dB with a curve, so the
 * loud end -- the part you actually mix in -- gets most of the throw.
 *
 * # `<Index>`, not `<For>`
 * Every mixer command answers with a whole new `MixerState`, so every `TrackDto`
 * is a new object each time. `<For>` keys by reference and would tear the strips
 * down and rebuild them on every edit -- which, mid-drag, replaces the very
 * `<input>` the pointer is captured on and ends the gesture. `<Index>` keys by
 * position and hands each strip an accessor, so a fader move updates one
 * attribute and touches nothing else. Same reasoning as the sequencer grid.
 */
export function MixerPanel(_props: PanelProps): JSX.Element {
  const app = useAppState();
  const [addOpen, setAddOpen] = createSignal(false);

  return (
    <div class="mixer">
      <Show
        when={app.mixer()}
        fallback={<p class="mixer__empty">Waiting for the project…</p>}
      >
        {(mixer) => (
          <>
            <div class="mixer__strips">
              <Index each={mixer().tracks}>
                {(track) => <TrackStrip track={track} />}
              </Index>
              <MasterStrip gainDb={mixer().master.gainDb} />
            </div>

            <div class="mixer__footer">
              <Show
                when={mixer().canEditLayout}
                fallback={
                  <span
                    class="mixer__hint"
                    title="A rebuild would have to recreate every node, and a hosted CLAP plugin can only be activated once."
                  >
                    Track layout is locked while a CLAP plugin is loaded
                  </span>
                }
              >
                <span class="mixer__hint">
                  {mixer().tracks.length}/{mixer().maxTracks} tracks · click a name to
                  play it from the keyboard
                </span>
                <div class="mixer__add">
                  <button
                    type="button"
                    class="mixer__add-button"
                    disabled={mixer().tracks.length >= mixer().maxTracks}
                    aria-expanded={addOpen()}
                    title="Add a track"
                    onClick={() => setAddOpen((open) => !open)}
                  >
                    + Track
                  </button>
                  <Show when={addOpen()}>
                    <div class="mixer__add-menu" role="menu">
                      <For each={app.sources().filter((source) => source.available)}>
                        {(source) => (
                          <button
                            type="button"
                            role="menuitem"
                            class="mixer__add-item"
                            title={source.description}
                            onClick={() => {
                              setAddOpen(false);
                              app.mixerCommand((backend) => backend.addTrack(source.id));
                            }}
                          >
                            {source.label}
                          </button>
                        )}
                      </For>
                    </div>
                  </Show>
                </div>
              </Show>
            </div>
          </>
        )}
      </Show>
    </div>
  );
}

/** One track's strip. `track` is an `<Index>` accessor -- see the panel docs. */
function TrackStrip(props: { track: Accessor<TrackDto> }): JSX.Element {
  const app = useAppState();
  const track = props.track;

  return (
    <div
      class="strip"
      classList={{
        'is-selected': track().selected,
        'is-silenced': track().silenced,
      }}
    >
      <button
        type="button"
        class="strip__header"
        aria-pressed={track().selected}
        title={`${track().name} — ${track().sourceLabel}. Click to play this track from the keyboard (note channel ${track().index}).`}
        onClick={() =>
          app.mixerCommand((backend) => backend.setSelectedTrack(track().index))
        }
      >
        <span class="strip__name">{track().name}</span>
        <span class="strip__source">{track().sourceLabel}</span>
      </button>

      <div class="strip__body">
        <StripMeter track={track().index} />
        <input
          class="strip__fader"
          type="range"
          min="0"
          max="1"
          step="0.001"
          value={faderFraction(track().gainDb)}
          aria-label={`${track().name} fader`}
          title={`${track().name} — ${formatDb(track().gainDb)}`}
          onInput={(event) =>
            app.mixerCommand((backend) =>
              backend.setTrackGain(
                track().index,
                faderToDb(Number.parseFloat(event.currentTarget.value)),
              ),
            )
          }
        />
      </div>

      <span class="strip__readout">{formatDb(track().gainDb)}</span>

      <input
        class="strip__pan"
        type="range"
        min="-1"
        max="1"
        step="0.01"
        value={track().pan}
        aria-label={`${track().name} pan`}
        title={`Pan — ${panLabel(track().pan)}`}
        onInput={(event) =>
          app.mixerCommand((backend) =>
            backend.setTrackPan(track().index, Number.parseFloat(event.currentTarget.value)),
          )
        }
        onDblClick={() =>
          app.mixerCommand((backend) => backend.setTrackPan(track().index, 0))
        }
      />
      <span class="strip__readout strip__readout--pan">{panLabel(track().pan)}</span>

      <div class="strip__buttons">
        <button
          type="button"
          class="strip__toggle strip__toggle--mute"
          classList={{ 'is-on': track().muted }}
          aria-pressed={track().muted}
          title="Mute (a 10 ms fade, not a cut)"
          onClick={() =>
            app.mixerCommand((backend) =>
              backend.setTrackMute(track().index, !track().muted),
            )
          }
        >
          M
        </button>
        <button
          type="button"
          class="strip__toggle strip__toggle--solo"
          classList={{ 'is-on': track().soloed }}
          aria-pressed={track().soloed}
          title="Solo (silences every track that is not soloed)"
          onClick={() =>
            app.mixerCommand((backend) =>
              backend.setTrackSolo(track().index, !track().soloed),
            )
          }
        >
          S
        </button>
        <Show when={app.mixer()?.canEditLayout && (app.mixer()?.tracks.length ?? 0) > 1}>
          <button
            type="button"
            class="strip__toggle strip__toggle--remove"
            title={`Remove ${track().name}`}
            onClick={() =>
              app.mixerCommand((backend) => backend.removeTrack(track().index))
            }
          >
            ×
          </button>
        </Show>
      </div>
    </div>
  );
}

/** The master strip: a fader, a meter, and no pan. */
function MasterStrip(props: { gainDb: number }): JSX.Element {
  const app = useAppState();

  return (
    <div class="strip strip--master">
      <div class="strip__header strip__header--master">
        <span class="strip__name">Master</span>
        <span class="strip__source">sum</span>
      </div>

      <div class="strip__body">
        <StripMeter track={null} />
        <input
          class="strip__fader"
          type="range"
          min="0"
          max="1"
          step="0.001"
          value={faderFraction(props.gainDb)}
          aria-label="Master fader"
          title={`Master — ${formatDb(props.gainDb)}`}
          onInput={(event) =>
            app.mixerCommand((backend) =>
              backend.setMasterGain(faderToDb(Number.parseFloat(event.currentTarget.value))),
            )
          }
        />
      </div>

      <span class="strip__readout">{formatDb(props.gainDb)}</span>
      <span class="strip__readout strip__readout--pan">stereo</span>
      <div class="strip__buttons strip__buttons--master" />
    </div>
  );
}

/**
 * A two-bar peak meter driven from the meter bus.
 *
 * `track` is the track index, or `null` for the master bus. The heights are
 * written straight onto the elements' styles from an animation frame -- no
 * signal, no re-render, no allocation per frame.
 */
function StripMeter(props: { track: number | null }): JSX.Element {
  const app = useAppState();
  let left: HTMLDivElement | undefined;
  let right: HTMLDivElement | undefined;

  onMount(() => {
    let raf = 0;
    const tick = () => {
      const frame = app.meters.latest;
      let peakL = 0;
      let peakR = 0;
      if (frame) {
        if (props.track === null) {
          peakL = frame.peakL;
          peakR = frame.peakR;
        } else {
          const meter = frame.tracks[props.track];
          if (meter) {
            peakL = meter.peakL;
            peakR = meter.peakR;
          }
        }
      }
      if (left) left.style.height = `${peakFraction(peakL) * 100}%`;
      if (right) right.style.height = `${peakFraction(peakR) * 100}%`;
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    onCleanup(() => cancelAnimationFrame(raf));
  });

  return (
    <div
      class="strip__meter"
      aria-hidden="true"
      title={props.track === null ? 'Master level' : 'Track level (pre-master)'}
    >
      <div class="strip__meter-track">
        <div class="strip__meter-fill" ref={left} />
      </div>
      <div class="strip__meter-track">
        <div class="strip__meter-fill" ref={right} />
      </div>
    </div>
  );
}
