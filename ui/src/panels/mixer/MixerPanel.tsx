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
import type {
  ClapSlot,
  ClapTarget,
  MasterDto,
  MissingPlugin,
  TrackDto,
} from '../../api/types';
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
 * # Plugins live in the strips
 * A track's instrument and the master's insert are both picked from a `<select>`
 * in the strip that owns them, because that is where the answer to "what is this
 * strip playing" belongs -- the CLAP browser lists what is *installed*, which is
 * a different question. The dropdown *replaces* the old static source line
 * rather than sitting under it: the select already renders the name, a second
 * copy of it would only compete for the 24 vertical pixels a strip can spare,
 * and a `<select>` cannot legally live inside the strip header, which is a
 * `<button>`.
 *
 * A placed plugin's editor is a *native* window, so the strip's editor button is
 * a request to the Rust main thread rather than something rendered here. Several
 * can be open at once -- one per strip -- and a plugin whose `hasEditor` is
 * false gets no button at all, because "this plugin has no editor" is an
 * ordinary fact about a plugin rather than an error to be discovered by clicking.
 *
 * # `<Index>`, not `<For>`
 * Every mixer command answers with a whole new `MixerState`, so every `TrackDto`
 * is a new object each time. `<For>` keys by reference and would tear the strips
 * down and rebuild them on every edit -- which, mid-drag, replaces the very
 * `<input>` the pointer is captured on and ends the gesture. `<Index>` keys by
 * position and hands each strip an accessor, so a fader move updates one
 * attribute and touches nothing else. Same reasoning as the sequencer grid, and
 * the same reasoning applies again inside a strip: `app.sources()` is re-fetched
 * after every command, so its `<option>`s are built with `<Index>` too. The
 * master's effect list is the one exception -- `app.masterEffects()` is fetched
 * once at startup and never replaced, so `<For>` is right for it.
 */
export function MixerPanel(_props: PanelProps): JSX.Element {
  const app = useAppState();
  const [addOpen, setAddOpen] = createSignal(false);

  /**
   * A new track always starts on a built-in. `add_track` names its source by id,
   * and a CLAP instrument is *placed* rather than named -- so the way to a new
   * CLAP track is: add one, then pick the plugin in its own dropdown.
   */
  const startingSources = () => app.sources().filter((source) => source.kind === 'internal');

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
              <MasterStrip master={mixer().master} />
            </div>

            <div class="mixer__footer">
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
                    <For each={startingSources()}>
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

  const builtIns = () => app.sources().filter((source) => source.kind === 'internal');
  const instruments = () => app.sources().filter((source) => source.kind === 'clap');

  /**
   * Apply whatever the dropdown landed on. The id round-trips through
   * `app.sources()` rather than being decoded here, because *how* a source is
   * placed (named, or loaded from a bundle) is the source's own business and is
   * decided once, in `placeSource`.
   */
  const pick = (id: string) => {
    const source = app.sources().find((entry) => entry.id === id);
    if (source) app.placeSource(track().index, source);
  };

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
      </button>

      <select
        class="strip__picker"
        value={track().sourceId}
        aria-label={`${track().name} source`}
        title={`${track().name} plays ${track().sourceLabel}`}
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
              // The vendor rides in the tooltip rather than in the option text:
              // a strip is a few dozen pixels wide, and "Name — Vendor" would be
              // ellipsised down to neither.
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

      <Show when={track().missing}>
        {(missing) => <MissingPluginBadge missing={missing} />}
      </Show>

      <Show when={track().clap}>
        {(clap) => (
          <ClapEditorButton slot={clap} target={{ slot: 'track', index: track().index }} />
        )}
      </Show>

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
        <Show when={(app.mixer()?.tracks.length ?? 0) > 1}>
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

/** The master strip: a fader, a meter, a CLAP insert, and no pan. */
function MasterStrip(props: { master: MasterDto }): JSX.Element {
  const app = useAppState();

  /**
   * `''` is the "None" option. The plugin *id* is the option value because it is
   * unique across the whole scan, and the bundle path is looked back up from it
   * -- one `.clap` file can export several plugins, so the pair is what the
   * placement command needs and neither half alone would do.
   */
  const pick = (id: string) => {
    if (id === '') {
      app.mixerCommand((backend) => backend.clearMasterEffect());
      return;
    }
    const plugin = app.masterEffects().find((entry) => entry.id === id);
    if (plugin) {
      app.mixerCommand((backend) => backend.setMasterEffectClap(plugin.path, plugin.id));
    }
  };

  return (
    <div class="strip strip--master">
      <div class="strip__header strip__header--master">
        <span class="strip__name">Master</span>
      </div>

      <select
        class="strip__picker"
        value={props.master.clap?.pluginId ?? ''}
        aria-label="Master insert effect"
        title={
          props.master.clap
            ? `Master insert: ${props.master.clap.name}`
            : 'No effect on the master insert'
        }
        onChange={(event) => pick(event.currentTarget.value)}
      >
        <option value="">No insert</option>
        <optgroup label="CLAP effects">
          {/* Fetched once at startup and never replaced, so `<For>` is right
              here -- see the panel docs on `<Index>` vs `<For>`. */}
          <For each={app.masterEffects()}>
            {(plugin) => (
              <option
                value={plugin.id}
                title={plugin.vendor ? `${plugin.name} — ${plugin.vendor}` : plugin.name}
              >
                {plugin.name || plugin.id}
              </option>
            )}
          </For>
        </optgroup>
      </select>

      <Show when={props.master.missing}>
        {(missing) => <MissingPluginBadge missing={missing} />}
      </Show>

      <Show when={props.master.clap}>
        {(clap) => <ClapEditorButton slot={clap} target={{ slot: 'master' }} />}
      </Show>

      <div class="strip__body">
        <StripMeter track={null} />
        <input
          class="strip__fader"
          type="range"
          min="0"
          max="1"
          step="0.001"
          value={faderFraction(props.master.gainDb)}
          aria-label="Master fader"
          title={`Master — ${formatDb(props.master.gainDb)}`}
          onInput={(event) =>
            app.mixerCommand((backend) =>
              backend.setMasterGain(faderToDb(Number.parseFloat(event.currentTarget.value))),
            )
          }
        />
      </div>

      <span class="strip__readout">{formatDb(props.master.gainDb)}</span>
      <span class="strip__readout strip__readout--pan">stereo</span>
      <div class="strip__buttons strip__buttons--master" />
    </div>
  );
}

/**
 * The hole a plugin left when the project asked for it and it would not load.
 *
 * On the strip rather than in a dialog at load time, for three reasons: a
 * project can be missing several plugins and a dialog would have to list them
 * out of context, the strip is where the question "why is this track silent"
 * gets asked, and the answer stays visible for as long as it is true. The status
 * bar's count is what says to come and look.
 *
 * Not a button. There is nothing to click yet -- the fix is to install the
 * plugin and reopen the project -- and a control that did nothing would be worse
 * than a label that says so. The track keeps everything else it had, and the
 * reference is still in the project file, so this is a repair notice rather than
 * an obituary.
 *
 * The strip is 78px wide, so the badge shows the name and ellipsises; the format,
 * the loader's reason and the path the project expected all ride in the `title`,
 * which is where the sentence that actually explains it belongs.
 */
function MissingPluginBadge(props: { missing: Accessor<MissingPlugin> }): JSX.Element {
  const missing = props.missing;
  return (
    <span
      class="strip__missing"
      title={`${missing().name} (${missing().format}) could not be loaded: ${missing().reason}\nExpected at ${missing().path}`}
    >
      ⚠ {missing().name}
    </span>
  );
}

/**
 * Opens or closes one placed plugin's native editor window.
 *
 * Renders nothing at all when the plugin has no editor: a disabled button would
 * suggest something is temporarily wrong, when in fact this plugin will never
 * have a window. `slot` is a `<Show>` accessor, so it re-reads `guiOpen` from
 * whatever `MixerState` arrived last rather than keeping a second copy of it.
 */
function ClapEditorButton(props: { slot: Accessor<ClapSlot>; target: ClapTarget }): JSX.Element {
  const app = useAppState();

  return (
    <Show when={props.slot().hasEditor}>
      <button
        type="button"
        class="strip__editor"
        classList={{ 'is-on': props.slot().guiOpen }}
        aria-pressed={props.slot().guiOpen}
        aria-label={`${props.slot().name} editor`}
        title={
          props.slot().guiOpen
            ? `Close ${props.slot().name}'s editor window`
            : `Open ${props.slot().name}'s editor in its own window`
        }
        onClick={() => app.toggleClapGui(props.target, !props.slot().guiOpen)}
      >
        {props.slot().guiOpen ? 'Close' : 'Editor'}
      </button>
    </Show>
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
