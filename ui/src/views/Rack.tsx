/**
 * The channel rack: one row per rack channel, showing what the **active
 * pattern** says to it.
 *
 * ## Step cells are a fixed size, and the lane scrolls
 *
 * The one thing a step sequencer must not do is stretch its cells to fill the
 * width. A 64-step pattern in a narrow window would become 64 three-pixel
 * slivers, and a grid you cannot hit is not a grid. So a cell is always
 * {@link CELL_W} x {@link CELL_H}, cells are grouped in fours by a beat gap and
 * in sixteens by a bar gap, and the lane scrolls horizontally when the pattern
 * is longer than the window.
 *
 * ## The mode button is a conversion, not a view switch
 *
 * The engine's compiler plays a channel's grid **and** its notes; `mode` is
 * derived from whether there are notes. So converting has to move the content
 * rather than just change what is drawn -- leaving the cells switched on under
 * a converted melody would double every hit. Both directions carry as much as
 * the other representation can hold, and the readout says what was lost.
 */

import { For, Index, Show, createSignal } from 'solid-js';

import type { ChannelClipDto, PatternDto, TrackDto } from '../api/types';
import {
  displayMode,
  isAccent,
  notesToSteps,
  patternTicks,
  stepsToNotes,
  withChannel,
  withToggledStep,
} from '../lib/pattern';
import { useAppState } from '../state';
import { useView } from '../shell/view';
import { alpha, trackColor } from '../theme/tokens';
import { IconCaret, IconRoll, IconWarn, IconX } from '../ui/icons';
import { InstrumentMenu } from './InstrumentMenu';

/** A step cell, in pixels. Fixed: see the module docs. */
const CELL_W = 13;
const CELL_H = 22;
/** The lengths a pattern may be, which is what the picker offers. */
const LENGTHS = [16, 32, 64];

export function Rack(): ReturnType<typeof Show> {
  const app = useAppState();
  const view = useView();
  const [instrumentFor, setInstrumentFor] = createSignal<number | null>(null);
  const [renaming, setRenaming] = createSignal<number | null>(null);
  const [dragOver, setDragOver] = createSignal(false);

  const pattern = () => app.pattern();
  const activeId = () => app.sequencer()?.patternSet.activeId ?? 0;

  /** The clip the active pattern puts on `channel`, if any. */
  const clipFor = (channel: number): ChannelClipDto | undefined =>
    pattern()?.channels.find((clip) => clip.channel === channel);

  function selectChannel(index: number): void {
    view.setStrip({ kind: 'track', index });
    app.selectTrack(index);
  }

  // --- steps ---------------------------------------------------------------

  function toggleStep(channel: number, index: number, accent: boolean): void {
    const key = app.channelKey(channel);
    app.editPatternOnce(accent ? 'Accent step' : 'Toggle step', (current) =>
      withToggledStep(current, channel, index, accent, key),
    );
  }

  // --- the mode button -----------------------------------------------------

  function toggleMode(channel: number, name: string): void {
    const current = pattern();
    if (!current) return;
    const clip = clipFor(channel);
    const key = app.channelKey(channel);
    const toRoll = displayMode(clip) === 'steps';

    if (toRoll) {
      const row = clip?.steps;
      const converted = row ? stepsToNotes(row, current.steps) : [];
      app.editPatternOnce('Convert to piano roll', (target) =>
        withChannel(target, channel, key, (existing) => ({
          ...existing,
          // The cells are cleared, not kept: the compiler would play both.
          steps: existing.steps
            ? { ...existing.steps, steps: existing.steps.steps.map((cell) => ({ ...cell, on: false })) }
            : existing.steps,
          notes: converted,
        })),
      );
      app.say(
        `${name} · Mode`,
        converted.length > 0 ? `Piano roll — ${converted.length} steps became notes` : 'Piano roll',
      );
      return;
    }

    const notes = clip?.notes ?? [];
    app.editPatternOnce('Convert to steps', (target) =>
      withChannel(target, channel, key, (existing) => ({
        ...existing,
        steps: existing.steps
          ? notesToSteps(notes, existing.steps, target.steps)
          : existing.steps,
        notes: [],
      })),
    );
    app.say(
      `${name} · Mode`,
      notes.length > 0
        ? `Steps — ${notes.length} notes folded onto the grid, lengths dropped`
        : 'Steps',
    );
  }

  // --- channels ------------------------------------------------------------

  function removeChannel(track: TrackDto): void {
    if (app.tracks().length <= 1) return;
    void app.mixerCommand((backend) => backend.removeTrack(track.index));
    // Removing a track renumbers every pattern's channels, so no entry below
    // could be replayed against the arrangement that is left.
    app.clearHistory(`removing ${track.name}`);
    app.say('Channel · Delete', track.name);
  }

  function addFromDrop(event: DragEvent): void {
    event.preventDefault();
    setDragOver(false);
    const raw = event.dataTransfer?.getData('text/plain') ?? '';
    const [kind, ref] = raw.split('::');
    if (kind === 'audio') {
      // Honest refusal: an audio file is a clip on a lane, not an instrument.
      app.report(
        new Error('Audio files go on a playlist lane, not the rack — switch to PLAYLIST and drop it there'),
      );
      return;
    }
    if (kind !== 'inst' || !ref) return;
    placeSource(ref);
  }

  /** Add a channel playing `sourceId`, hosted plugins included. */
  function placeSource(sourceId: string): void {
    const source = app.sources().find((entry) => entry.id === sourceId);
    const vst3 = app.vst3Plugins().find((plugin) => `vst3:${plugin.id}` === sourceId);
    void app.mixerCommand(async (backend) => {
      // A hosted plugin cannot be a track's source at creation -- the engine
      // takes a built-in id and then swaps -- so it is two commands, in one
      // funnel, so only the final state is adopted.
      const label = source?.label ?? vst3?.name ?? 'Channel';
      const created = await backend.addTrack('poly-synth', label);
      const index = created.tracks.length - 1;
      if (source && source.kind === 'clap' && source.bundlePath && source.pluginId) {
        return backend.setTrackSourceClap(index, source.bundlePath, source.pluginId);
      }
      if (vst3) return backend.setTrackSourceVst3(index, vst3.path, vst3.id);
      if (source && source.kind === 'internal' && source.id !== 'poly-synth') {
        return backend.setTrackSource(index, source.id);
      }
      return created;
    });
  }

  return (
    <div class="wv-main">
      <div class="wv-viewbar">
        <span class="wv-viewbar__title">CHANNEL RACK</span>
        <div class="wv-pattabs">
          <For each={app.patterns()}>
            {(summary) => (
              <Show
                when={renaming() === summary.id}
                fallback={
                  <button
                    type="button"
                    class="wv-pattab"
                    classList={{ 'is-active': summary.id === activeId() }}
                    style={
                      summary.id === activeId()
                        ? {
                            background: alpha(summary.color ?? trackColor(summary.id), 0.16),
                            'box-shadow': `inset 0 0 0 1px ${alpha(summary.color ?? trackColor(summary.id), 0.6)}`,
                          }
                        : undefined
                    }
                    title="Click to edit this pattern · double-click to rename"
                    onClick={() =>
                      void app.sequencerCommand((backend) => backend.setActivePattern(summary.id))
                    }
                    onDblClick={() => setRenaming(summary.id)}
                  >
                    <span
                      class="wv-swatch"
                      style={{ background: summary.color ?? trackColor(summary.id) }}
                    />
                    {summary.name}
                  </button>
                }
              >
                <input
                  class="wv-pattab__input"
                  value={summary.name}
                  ref={(el) => queueMicrotask(() => el.select())}
                  onKeyDown={(event) => {
                    event.stopPropagation();
                    if (event.key === 'Escape') setRenaming(null);
                    if (event.key !== 'Enter') return;
                    const name = event.currentTarget.value.trim();
                    setRenaming(null);
                    if (name && name !== summary.name) {
                      void app.sequencerCommand((b) => b.renamePattern(summary.id, name));
                    }
                  }}
                  onBlur={(event) => {
                    const name = event.currentTarget.value.trim();
                    setRenaming(null);
                    if (name && name !== summary.name) {
                      void app.sequencerCommand((b) => b.renamePattern(summary.id, name));
                    }
                  }}
                />
              </Show>
            )}
          </For>
          <button
            type="button"
            class="wv-dashed"
            title="Add an empty pattern"
            onClick={() => void app.sequencerCommand((backend) => backend.addPattern())}
          >
            +
          </button>
        </div>

        <div class="wv-seg" title="How long the pattern loops. Shortening keeps what is past the end.">
          <For each={LENGTHS}>
            {(steps) => (
              <button
                type="button"
                class="wv-seg__btn wv-seg__btn--len"
                classList={{ 'is-active': pattern()?.steps === steps }}
                onClick={() => {
                  void app.sequencerCommand((backend) =>
                    backend.setPatternLength(activeId(), steps),
                  );
                  app.clearHistory('changing the pattern length');
                }}
              >
                {steps}
              </button>
            )}
          </For>
        </div>

        <div class="wv-spacer" />
        <span class="wv-viewbar__meta">
          {app.tracks().length} ch · {(pattern()?.steps ?? 16) / 16} bar
          {(pattern()?.steps ?? 16) / 16 === 1 ? '' : 's'}
        </span>
      </div>

      <div
        class="wv-rack"
        onDragOver={(event) => {
          event.preventDefault();
          if (!dragOver()) setDragOver(true);
        }}
        onDragLeave={(event) => {
          if (event.currentTarget.contains(event.relatedTarget as Node)) return;
          setDragOver(false);
        }}
        onDrop={addFromDrop}
      >
        <Index each={app.tracks()}>
          {(track) => (
            <RackRow
              track={track()}
              pattern={pattern()}
              selected={(() => {
                const strip = view.strip();
                return strip.kind === 'track' && strip.index === track().index;
              })()}
              onSelect={() => selectChannel(track().index)}
              onToggleStep={(index, accent) => toggleStep(track().index, index, accent)}
              onOpenRoll={() => {
                selectChannel(track().index);
                view.openRoll(track().index);
              }}
              onInstrument={() => setInstrumentFor(track().index)}
              onToggleMode={() => toggleMode(track().index, track().name)}
              onRemove={() => removeChannel(track())}
              canRemove={app.tracks().length > 1}
            />
          )}
        </Index>

        <div
          class="wv-rack__drop"
          classList={{ 'is-over': dragOver() }}
          onClick={() => setInstrumentFor(-1)}
          title="Add a channel"
        >
          {dragOver()
            ? 'release to add a channel'
            : app.tracks().length >= (app.mixer()?.maxTracks ?? 16)
              ? `the engine allows ${app.mixer()?.maxTracks ?? 16} channels, and they are all in use`
              : '+ add a channel — or drag an instrument here'}
        </div>
      </div>

      <Show when={instrumentFor() !== null}>
        <InstrumentMenu
          track={instrumentFor()! >= 0 ? instrumentFor()! : null}
          onClose={() => setInstrumentFor(null)}
          onAdd={placeSource}
        />
      </Show>
    </div>
  );
}

/** One channel: LED, name, roll button, its lane, the mode button, delete. */
function RackRow(props: {
  track: TrackDto;
  pattern: PatternDto | undefined;
  selected: boolean;
  canRemove: boolean;
  onSelect: () => void;
  onToggleStep: (index: number, accent: boolean) => void;
  onOpenRoll: () => void;
  onInstrument: () => void;
  onToggleMode: () => void;
  onRemove: () => void;
}): ReturnType<typeof Show> {
  const app = useAppState();
  const colour = () => trackColor(props.track.id);
  const clip = () => props.pattern?.channels.find((entry) => entry.channel === props.track.index);
  const isRoll = () => displayMode(clip()) === 'roll';
  const steps = () => clip()?.steps?.steps ?? [];

  /** Beat and bar gaps, so a cell's position is readable at a glance. */
  const gapFor = (index: number): string =>
    index % 16 === 0 ? '9px' : index % 4 === 0 ? '4px' : '1.5px';

  return (
    <div
      class="wv-chrow"
      classList={{ 'is-selected': props.selected, 'is-muted': props.track.muted }}
      onClick={() => props.onSelect()}
    >
      <button
        type="button"
        class="wv-led"
        classList={{ 'is-muted': props.track.muted }}
        style={{ background: props.track.muted ? 'transparent' : colour() }}
        title={props.track.muted ? 'Unmute this channel' : 'Mute this channel'}
        onClick={(event) => {
          event.stopPropagation();
          void app.mixerCommand((backend) =>
            backend.setTrackMute(props.track.index, !props.track.muted),
          );
          app.say(`${props.track.name} · Mute`, props.track.muted ? 'Active' : 'Muted');
        }}
      />

      <div class="wv-chrow__name">
        <button
          type="button"
          class="wv-chrow__label"
          title={
            props.track.clap?.hasEditor || props.track.vst3?.hasEditor
              ? 'Open this instrument’s own window'
              : props.track.sourceLabel
          }
          onClick={(event) => {
            event.stopPropagation();
            props.onSelect();
            const slot = props.track.clap ?? props.track.vst3;
            if (slot?.hasEditor) {
              app.toggleGui({ slot: 'track', index: props.track.index }, !slot.guiOpen);
            }
          }}
        >
          {props.track.name}
        </button>
        <Show when={props.track.missing}>
          <span class="wv-chrow__warn" title={`${props.track.missing!.name}: ${props.track.missing!.reason}`}>
            <IconWarn />
          </span>
        </Show>
        <button
          type="button"
          class="wv-chrow__caret"
          title="Replace this channel's instrument"
          onClick={(event) => {
            event.stopPropagation();
            props.onSelect();
            props.onInstrument();
          }}
        >
          <IconCaret />
        </button>
      </div>

      <button
        type="button"
        class="wv-chrow__roll"
        classList={{ 'is-on': isRoll() }}
        title="Open the piano roll for this channel (F7)"
        onClick={(event) => {
          event.stopPropagation();
          props.onOpenRoll();
        }}
      >
        <IconRoll />
      </button>

      <div class="wv-chrow__lane">
        <Show
          when={!isRoll()}
          fallback={
            <button
              type="button"
              class="wv-chrow__preview"
              title="Open the piano roll for this channel"
              onClick={(event) => {
                event.stopPropagation();
                props.onOpenRoll();
              }}
            >
              <NotePreview clip={clip()} pattern={props.pattern} colour={colour()} />
            </button>
          }
        >
          <Index each={steps()}>
            {(cell, index) => (
              <button
                type="button"
                class="wv-step"
                classList={{ 'is-on': cell().on, 'is-accent': cell().on && isAccent(cell().velocity) }}
                style={{
                  width: `${CELL_W}px`,
                  height: `${CELL_H}px`,
                  'margin-left': index === 0 ? '0' : gapFor(index),
                  background: cell().on ? colour() : undefined,
                }}
                title={`Step ${index + 1} — click to toggle, shift-click for an accent`}
                onClick={(event) => {
                  event.stopPropagation();
                  props.onToggleStep(index, event.shiftKey);
                }}
              />
            )}
          </Index>
        </Show>
      </div>

      <button
        type="button"
        class="wv-chrow__mode"
        title={
          isRoll()
            ? 'Fold these notes back onto the step grid — one key, no note lengths'
            : 'Turn these steps into notes and edit them in the piano roll'
        }
        onClick={(event) => {
          event.stopPropagation();
          props.onToggleMode();
        }}
      >
        {isRoll() ? 'roll' : 'steps'}
      </button>

      <button
        type="button"
        class="wv-chrow__del"
        disabled={!props.canRemove}
        classList={{ 'wv-disabled': !props.canRemove }}
        title={
          props.canRemove
            ? `Remove ${props.track.name} — its patterns and sends go with it`
            : 'A project must keep at least one channel'
        }
        onClick={(event) => {
          event.stopPropagation();
          props.onRemove();
        }}
      >
        <IconX />
      </button>
    </div>
  );
}

/** A channel's notes at a glance, laid out over the pattern's whole length. */
function NotePreview(props: {
  clip: ChannelClipDto | undefined;
  pattern: PatternDto | undefined;
  colour: string;
}): ReturnType<typeof Show> {
  const notes = () => props.clip?.notes ?? [];
  const span = () => (props.pattern ? patternTicks(props.pattern) : 3840);
  const range = () => {
    const keys = notes().map((note) => note.key);
    const low = keys.length > 0 ? Math.min(...keys) : 60;
    const high = keys.length > 0 ? Math.max(...keys) : 72;
    return { low, span: Math.max(6, high - low + 1) };
  };

  return (
    <Show when={notes().length > 0} fallback={<span class="wv-chrow__empty">empty — click to draw</span>}>
      <For each={notes()}>
        {(note) => (
          <span
            class="wv-chrow__note"
            style={{
              left: `${(note.start / span()) * 100}%`,
              width: `${Math.max(0.6, (note.length / span()) * 100)}%`,
              bottom: `${((note.key - range().low) / range().span) * 74 + 8}%`,
              background: alpha(props.colour, 0.35 + note.velocity * 0.6),
            }}
          />
        )}
      </For>
    </Show>
  );
}
