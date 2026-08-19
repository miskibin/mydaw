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
 *
 * ## Where in the loop we are
 *
 * Two overlays are laid over the whole stack of rows -- a column the width of
 * one cell over the step that is sounding, and a hairline at the exact
 * position -- rather than a class on the cell in every lane. One `translateX`
 * per frame instead of one per row per frame, and the roll-mode previews get
 * the moving line for free because they share the lane's coordinate system.
 * See {@link soundingStep} for what the rack claims to know in SONG mode, which
 * is deliberately less than in PAT mode.
 */

import { For, Index, Show, createEffect, createSignal, onCleanup } from 'solid-js';

import type { ChannelClipDto, PatternDto, TrackDto } from '../api/types';
import {
  TICKS_PER_STEP,
  displayMode,
  emptyRow,
  isAccent,
  notesToSteps,
  patternTick,
  patternTicks,
  samplesToTicks,
  stepCells,
  stepsToNotes,
  withChannel,
  withToggledStep,
} from '../lib/pattern';
import { useAppState } from '../state';
import { useView } from '../shell/view';
import { alpha, trackColor } from '../theme/tokens';
import { onFrame } from '../ui/frameLoop';
import { IconCaret, IconRoll, IconWarn, IconX } from '../ui/icons';
import { InstrumentMenu } from './InstrumentMenu';

/** A step cell, in pixels. Fixed: see the module docs. */
const CELL_W = 13;
const CELL_H = 22;
/** Between two cells of the same bar. From the mock's rack lane. */
const CELL_GAP = 1.5;
/** Between two bars -- the only gap, because the beat is shown by colour. */
const BAR_GAP = 5;
/** Cells in one bar of 4/4 at a sixteenth per cell. */
const CELLS_PER_BAR = 16;
/** Cells in one beat, which is what gets the brighter fill. */
const CELLS_PER_BEAT = 4;

const CELL_PITCH = CELL_W + CELL_GAP;
const BAR_PITCH = CELLS_PER_BAR * CELL_W + (CELLS_PER_BAR - 1) * CELL_GAP + BAR_GAP;

/** The lengths a pattern may be, which is what the picker offers. */
const LENGTHS = [16, 32, 64];

/** Where cell `index`'s left edge sits inside a lane, in pixels. */
function cellX(index: number): number {
  const bar = Math.floor(index / CELLS_PER_BAR);
  return bar * BAR_PITCH + (index - bar * CELLS_PER_BAR) * CELL_PITCH;
}

/** How wide a lane of `steps` cells is. The roll preview matches it exactly. */
function laneWidth(steps: number): number {
  return steps <= 0 ? 0 : cellX(steps - 1) + CELL_W;
}

/**
 * Where a *fractional* step position sits, in pixels.
 *
 * Interpolated between one cell's left edge and the next rather than from a
 * pixels-per-tick constant, so the hairline stays inside the cell the column is
 * highlighting even across a bar gap, which is wider than a cell gap.
 */
function positionX(step: number): number {
  const whole = Math.floor(step);
  const left = cellX(whole);
  return left + (step - whole) * (cellX(whole + 1) - left);
}

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

  // --- where in the loop we are --------------------------------------------

  let rowsEl: HTMLDivElement | undefined;
  let columnEl: HTMLDivElement | undefined;
  let lineEl: HTMLDivElement | undefined;

  /**
   * How far the lanes start from the left edge of the row stack.
   *
   * Measured rather than added up from the LED, the name box and the roll
   * button, because a constant here would be a second opinion about the row's
   * layout that the CSS could drift away from without anything failing. Cached:
   * it only moves when the row's chrome does.
   */
  let laneLeft = -1;
  function laneOffset(): number {
    if (laneLeft > 0) return laneLeft;
    const lane = rowsEl?.querySelector<HTMLElement>('.wv-chrow__lane');
    // A miss is not cached: the first paint can land before the engine's first
    // answer has put any rows on screen, and caching the zero it measures then
    // would pin the playhead to the left edge for the life of the view.
    if (!lane) return 0;
    laneLeft = lane.offsetLeft;
    return laneLeft;
  }
  createEffect(() => {
    // Anything that can change the row's leading controls re-measures.
    app.tracks().length;
    pattern()?.steps;
    laneLeft = -1;
  });

  /**
   * Which step of the active pattern is sounding, fractionally, or `null`.
   *
   * **PAT mode** is unambiguous: the transport is looping this pattern and the
   * position inside the loop *is* the answer.
   *
   * **SONG mode** is not. The active pattern may be on the timeline in a dozen
   * places, in none, or twice at once, and a rack lit up from a clip the user
   * is not looking at is a lie told sixty times a second. So the rack answers
   * only when exactly one unmuted clip of this pattern is under the playhead --
   * then there is one true answer -- and stays dark otherwise.
   *
   * Called from the frame loop, so every signal read here is untracked.
   */
  function soundingStep(current: PatternDto): number | null {
    const samples = app.playhead.positionSamples();
    const rate = app.playhead.getSampleRate();
    const bpm = app.bpm();
    if (app.playMode() === 'pat') {
      return patternTick(samples, rate, bpm, current) / TICKS_PER_STEP;
    }
    const list = app.playlist();
    if (!list) return null;
    const songTick = samplesToTicks(samples, rate, bpm);
    const sounding = list.clips.filter(
      (clip) =>
        clip.kind === 'pattern' &&
        clip.patternId === current.id &&
        songTick >= clip.start &&
        songTick < clip.start + clip.length &&
        list.tracks[clip.track]?.muted !== true,
    );
    if (sounding.length !== 1) return null;
    const loop = patternTicks(current);
    if (loop <= 0) return null;
    return (((songTick - sounding[0]!.start) % loop) + loop) % loop / TICKS_PER_STEP;
  }

  function hidePosition(): void {
    if (columnEl) columnEl.style.display = 'none';
    if (lineEl) lineEl.style.display = 'none';
  }

  // Subscribed to the frame loop only while the transport runs: a stopped rack
  // costs nothing, which is the whole contract `onFrame` is built on. It is
  // still painted **once** when the transport stops, because where you stopped
  // is exactly the thing you wanted to see -- what would be dishonest is a
  // marker that kept moving, not one that stayed put.
  createEffect(() => {
    const current = pattern();
    // Tracked so the one paint a stopped transport gets is repeated once the
    // rows exist: the very first can land before `<Index>` has rendered any,
    // and `laneOffset` has nothing to measure until it has.
    app.tracks().length;
    if (!current) {
      hidePosition();
      return;
    }
    let lastStep = -1;
    let lastX = Number.NaN;
    const paint = () => {
      if (!columnEl || !lineEl) return;
      const position = soundingStep(current);
      if (position === null) {
        hidePosition();
        lastStep = -1;
        return;
      }
      // An explicit value, not `''`: the stylesheet's own `display: none` is
      // what hides these, so clearing the inline style would un-hide nothing.
      columnEl.style.display = 'block';
      lineEl.style.display = 'block';
      const left = laneOffset();
      // The column moves once per step; the line moves every frame. Writing
      // only what changed keeps a stopped-on-a-step rack down to one property.
      const step = Math.min(current.steps - 1, Math.max(0, Math.floor(position)));
      if (step !== lastStep) {
        lastStep = step;
        columnEl.style.transform = `translateX(${left + cellX(step)}px)`;
      }
      const x = left + positionX(position);
      if (x !== lastX) {
        lastX = x;
        lineEl.style.transform = `translateX(${x}px)`;
      }
    };

    paint();
    if (!app.playing()) return;
    onFrame(paint);
    onCleanup(hidePosition);
  });

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
        // A clip can have no grid at all; fold onto a fresh one rather than
        // dropping the notes into a row that is not there.
        steps: notesToSteps(notes, existing.steps ?? emptyRow(key, target.steps), target.steps),
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
        <div class="wv-rack__rows" ref={rowsEl}>
          {/* Under the cells in the source order but painted over them, which
              is what makes it read as a wash on the column rather than a block
              in front of it. Both are inert to the pointer. */}
          <div class="wv-rack__col" ref={columnEl} style={{ width: `${CELL_W}px` }} />
          <div class="wv-rack__line" ref={lineEl} />

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
        </div>

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
  const length = () => props.pattern?.steps ?? 16;
  /** Always a full lane -- see `stepCells` for the three ways a clip arrives. */
  const steps = () => stepCells(clip(), length());

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

      <div class="wv-chrow__lane" style={{ width: `${laneWidth(length())}px` }}>
        <Show
          when={!isRoll()}
          fallback={
            <button
              type="button"
              class="wv-chrow__preview"
              style={{
                width: `${laneWidth(length())}px`,
                'background-size': `${BAR_PITCH}px 100%`,
              }}
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
            {(cell, index) => {
              // The beat is shown by colour, not by a wider gap: the mock tints
              // every fourth cell and separates only the bars, which keeps the
              // lane one readable block instead of four ragged clusters -- and
              // it is what lets the playhead column be a single element.
              const beat = index % CELLS_PER_BEAT === 0;
              return (
                <button
                  type="button"
                  class="wv-step"
                  classList={{
                    'is-beat': beat,
                    'is-on': cell().on,
                    'is-accent': cell().on && isAccent(cell().velocity),
                  }}
                  style={{
                    width: `${CELL_W}px`,
                    height: `${CELL_H}px`,
                    'margin-left':
                      index === 0
                        ? '0'
                        : `${index % CELLS_PER_BAR === 0 ? BAR_GAP : CELL_GAP}px`,
                    background: cell().on
                      ? beat
                        ? colour()
                        : alpha(colour(), 0.8)
                      : undefined,
                  }}
                  title={`Step ${index + 1} — click to toggle, shift-click for an accent`}
                  onClick={(event) => {
                    event.stopPropagation();
                    props.onToggleStep(index, event.shiftKey);
                  }}
                />
              );
            }}
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
