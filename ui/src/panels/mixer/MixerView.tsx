import { Index, Show, type JSX } from 'solid-js';
import { useAppState } from '../../state';
import { trackColor } from '../../theme/tokens';
import { Meter } from '../../ui/Meter';
import { FADER_TRAVEL_PX, KNOB_TRAVEL_PX, startDrag } from '../../ui/drag';
import { faderFraction, faderToDb, formatDb, MAX_DB, MIN_DB, panLabel } from './scale';
import type { MasterDto, TrackDto } from '../../api/types';

/** Rotation of a pan knob's pointer, in degrees. Hard left is −130°. */
function panDegrees(pan: number): number {
  return Math.max(-1, Math.min(1, pan)) * 130;
}

/**
 * A knob, drawn exactly as the mock's `knob()` helper draws one.
 *
 * `size` is the diameter; the pointer is 34% of it, which is what keeps a 24 px
 * pan knob and a 30 px send knob looking like the same control.
 */
function Knob(props: {
  size: number;
  degrees: number;
  color: string;
  title: string;
  label: string;
  onDrag: (event: PointerEvent) => void;
  onReset?: () => void;
}): JSX.Element {
  return (
    <span
      class="wv-knob"
      role="slider"
      tabindex="-1"
      aria-label={props.label}
      aria-valuetext={props.title}
      title={props.title}
      style={{ width: `${props.size}px`, height: `${props.size}px` }}
      onPointerDown={props.onDrag}
      onDblClick={() => props.onReset?.()}
    >
      <span
        class="wv-knob__pointer"
        style={{
          height: `${Math.round(props.size * 0.34)}px`,
          background: props.color,
          transform: `translateX(-50%) rotate(${props.degrees}deg)`,
        }}
      />
    </span>
  );
}

/**
 * A fader: the sunk track and the handle that rides it.
 *
 * The handle's travel is `100% - 12px` rather than `100%`, so at unity the
 * handle sits *inside* the track instead of half out of the top of it. The mock
 * writes `top: n%` and gets away with it because its numbers never reach the
 * ends.
 */
function Fader(props: {
  db: number;
  min: number;
  max: number;
  label: string;
  onChange: (db: number) => void;
}): JSX.Element {
  const fraction = () => faderFraction(props.db);

  return (
    <div
      class="wv-fader"
      role="slider"
      tabindex="0"
      aria-label={props.label}
      aria-valuemin={props.min}
      aria-valuemax={props.max}
      aria-valuenow={Number(props.db.toFixed(1))}
      aria-valuetext={`${formatDb(props.db)} dB`}
      title="Drag to set the level, double-click for 0 dB"
      onPointerDown={(event) => {
        event.stopPropagation();
        startDrag(event, {
          from: fraction(),
          // The whole throw in `FADER_TRAVEL_PX` pixels, in *travel* rather than
          // dB, so the curve in `scale.ts` is the one thing that decides how the
          // decibels are spaced.
          perPixel: 1 / FADER_TRAVEL_PX,
          min: 0,
          max: 1,
          onChange: (value) => props.onChange(faderToDb(value)),
        });
      }}
      onDblClick={(event) => {
        event.stopPropagation();
        props.onChange(0);
      }}
      onKeyDown={(event) => {
        const step = event.shiftKey ? 0.1 : 1;
        if (event.key === 'ArrowUp') props.onChange(Math.min(props.max, props.db + step));
        else if (event.key === 'ArrowDown') props.onChange(Math.max(props.min, props.db - step));
        else return;
        event.preventDefault();
      }}
    >
      <span
        class="wv-fader__handle"
        style={{ top: `calc((1 - ${fraction().toFixed(4)}) * (100% - 12px))` }}
      />
    </div>
  );
}

/** The bottom panel's MIXER tab: one strip per track, then the master. */
export function MixerView(): JSX.Element {
  const app = useAppState();

  return (
    <div class="wv-mixer">
      {/*
        `Index`, not `For`. Every mixer command answers with a whole fresh
        `MixerState`, so every `TrackDto` is a new object and a keyed `For` would
        tear down and rebuild every strip on every pointer move of a fader drag.
        A strip's identity here is its *position*, which is exactly what `Index`
        keys on -- so a drag updates one inline style instead of rebuilding a
        dozen elements sixty times a second.
      */}
      <div class="wv-mixer__scroll">
        <Index each={app.mixer()?.tracks ?? []}>
          {(track) => <TrackStrip track={track()} />}
        </Index>
      </div>
      <div class="wv-mixer__master">
        <Show when={app.mixer()?.master}>
          {(master) => <MasterStrip master={master()} />}
        </Show>
      </div>
    </div>
  );
}

function TrackStrip(props: { track: TrackDto }): JSX.Element {
  const app = useAppState();

  const selected = () => {
    const selection = app.selection();
    return selection.kind === 'track' && selection.index === props.track.index;
  };

  const bounds = () => ({
    min: app.mixer()?.minGainDb ?? MIN_DB,
    max: app.mixer()?.maxGainDb ?? MAX_DB,
  });

  const peak = () => {
    const meters = app.meters.latest?.tracks[props.track.index];
    return Math.max(meters?.peakL ?? 0, meters?.peakR ?? 0);
  };

  return (
    <div
      class="wv-strip"
      classList={{ 'is-selected': selected(), 'is-muted': props.track.silenced }}
      role="group"
      aria-label={`${props.track.name} strip`}
      onPointerDown={() => app.select({ kind: 'track', index: props.track.index })}
    >
      <div class="wv-strip__head">
        <span class="wv-strip__dot" style={{ background: trackColor(props.track.id) }} />
        <span class="wv-strip__name" title={props.track.name}>
          {props.track.name}
        </span>
      </div>

      <div class="wv-strip__inserts">
        <span class="wv-insert" title={props.track.sourceLabel}>
          {props.track.sourceLabel}
        </span>
        <span
          class="wv-insert wv-insert--add wv-disabled"
          title="Per-track inserts — coming soon. Master insert only for now."
        >
          +
        </span>
      </div>

      <div class="wv-strip__body">
        <div class="wv-strip__panrow">
          <Knob
            size={24}
            degrees={panDegrees(props.track.pan)}
            color={selected() ? 'var(--acc)' : 'var(--dim)'}
            label={`${props.track.name} pan`}
            title={`Pan ${panLabel(props.track.pan)} — drag to change, double-click to centre`}
            onDrag={(event) => {
              event.stopPropagation();
              startDrag(event, {
                from: props.track.pan,
                perPixel: 2 / KNOB_TRAVEL_PX,
                min: -1,
                max: 1,
                onChange: (pan) =>
                  app.mixerCommand((api) =>
                    api.setTrackPan(props.track.index, Math.round(pan * 100) / 100),
                  ),
              });
            }}
            onReset={() => app.mixerCommand((api) => api.setTrackPan(props.track.index, 0))}
          />
          <span class="wv-strip__pan">{panLabel(props.track.pan)}</span>
          <span class="wv-spacer" />
          <button
            type="button"
            class="wv-pill wv-pill--mute"
            classList={{ 'is-on': props.track.muted }}
            title={`Mute ${props.track.name}`}
            aria-pressed={props.track.muted}
            onClick={(event) => {
              event.stopPropagation();
              app.mixerCommand((api) =>
                api.setTrackMute(props.track.index, !props.track.muted),
              );
            }}
          >
            M
          </button>
        </div>

        <div class="wv-strip__faders">
          <Fader
            db={props.track.gainDb}
            min={bounds().min}
            max={bounds().max}
            label={`${props.track.name} level`}
            onChange={(db) => app.mixerCommand((api) => api.setTrackGain(props.track.index, db))}
          />
          <Meter read={() => (props.track.silenced ? 0 : peak())} />
        </div>

        <div class="wv-strip__readout">
          <span class="wv-strip__gain">{formatDb(props.track.gainDb)}</span>
          <span class="wv-strip__out">MASTER</span>
        </div>
      </div>
    </div>
  );
}

function MasterStrip(props: { master: MasterDto }): JSX.Element {
  const app = useAppState();

  const selected = () => app.selection().kind === 'master';
  const bounds = () => ({
    min: app.mixer()?.minGainDb ?? MIN_DB,
    max: app.mixer()?.maxGainDb ?? MAX_DB,
  });

  return (
    <div
      class="wv-strip wv-strip--master"
      classList={{ 'is-selected': selected() }}
      role="group"
      aria-label="Master strip"
      onPointerDown={() => app.select({ kind: 'master' })}
    >
      <div class="wv-strip__head">
        <span class="wv-strip__mastername">MASTER</span>
      </div>
      <div class="wv-strip__body">
        <div class="wv-strip__faders">
          <Fader
            db={props.master.gainDb}
            min={bounds().min}
            max={bounds().max}
            label="Master level"
            onChange={(db) => app.mixerCommand((api) => api.setMasterGain(db))}
          />
          <Meter read={() => app.meters.latest?.peakL ?? 0} />
          <Meter read={() => app.meters.latest?.peakR ?? 0} />
        </div>
        <span class="wv-strip__gain">{formatDb(props.master.gainDb)} dB</span>
        <span class="wv-strip__out wv-disabled" title="Loudness metering — coming soon">
          — LUFS
        </span>
      </div>
    </div>
  );
}
