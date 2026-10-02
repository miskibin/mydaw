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
 * is longer than the window. Past the loop sits one extra grayed bar — paint
 * it to grow the pattern — rather than the rest of the four-bar workspace.
 *
 * ## The mode button is a conversion, not a view switch
 *
 * The engine's compiler plays a channel's grid **and** its notes; `mode` is
 * derived from whether there are notes. So converting has to move the content
 * rather than just change what is drawn -- leaving the cells switched on under
 * a converted melody would double every hit. Both directions carry as much as
 * the other representation can hold, and the readout says what was lost.
 * Opening the piano roll (F7, the channel menu) does the same conversion first,
 * so a row of steps is not an empty roll.
 *
 * ## Where in the loop we are
 *
 * A tick sits above the first lane, the width of the step that is sounding --
 * one `translateX` per step, not a wash down every row. See
 * {@link soundingStep} for what the rack claims to know in SONG mode, which is
 * deliberately less than in PAT mode.
 */

import { For, Index, Show, createEffect, createSignal, onCleanup } from 'solid-js';

import type { ChannelClipDto, PatternDto, TrackDto } from '../api/types';
import {
  TICKS_PER_STEP,
  channelClip,
  channelScore,
  displayMode,
  emptyRow,
  isAccent,
  notesToSteps,
  patternTick,
  patternTicks,
  rackDisplaySteps,
  samplesToTicks,
  stepCells,
  withChannel,
  withClearedChannel,
  withContentLength,
  withFilledSteps,
  withLength,
  withInvertedSteps,
  withPastedScore,
  withRandomSteps,
  withRepeatedFirstBar,
  withSetStep,
  withShiftedSteps,
  withStepsConvertedToNotes,
  type ChannelScore,
} from '../lib/pattern';
import { channelColor, channelKind, hasSampleWindow, isAudioClipChannel } from '../lib/channel';
import { allowDrop, droppedAudioFile, droppedAudioPath, importDroppedFile, parseDrop } from '../lib/dnd';
import { useAppState } from '../state';
import { usePatternNav } from '../shell/patternNav';
import { useView } from '../shell/view';
import { alpha, trackColor } from '../theme/tokens';
import { onFrame } from '../ui/frameLoop';
import { Knob, spec } from '../ui/Knob';
import { IconWarn, IconX } from '../ui/icons';
import { InstrumentMenu } from './InstrumentMenu';

/** A step cell, in pixels. Fixed: see the module docs. */
const CELL_W = 18;
const CELL_H = 24;
/** Between two cells of the same bar. */
const CELL_GAP = 3;
/** Between two bars — the only gap, because the beat is shown by colour. */
const BAR_GAP = 8;
/** Cells in one bar of 4/4 at a sixteenth per cell. */
const CELLS_PER_BAR = 16;
/** Cells in one beat, which is what gets the brighter fill. */
const CELLS_PER_BEAT = 4;

const CELL_PITCH = CELL_W + CELL_GAP;
const BAR_PITCH = CELLS_PER_BAR * CELL_W + (CELLS_PER_BAR - 1) * CELL_GAP + BAR_GAP;

/** The lengths a pattern may be, which is what the picker offers. */
const LENGTHS = [16, 32, 64];

/**
 * Fill spacings the channel menu offers, in cells.
 *
 * A cell is a sixteenth, so 4 is a hit on each beat, 2 is eighths, 16 is the
 * downbeat of each bar. 3 and 6 are the FL extras for patterns that are not
 * on the beat.
 */
const FILL_INTERVALS: { interval: number; meta: string }[] = [
  { interval: 2, meta: '1/8' },
  { interval: 3, meta: '' },
  { interval: 4, meta: 'beat' },
  { interval: 6, meta: '' },
  { interval: 8, meta: '1/2' },
  { interval: 16, meta: 'bar' },
];

/** Where the channel-name menu opens, and who it is talking about. */
interface ChannelMenuState {
  channel: number;
  name: string;
  x: number;
  y: number;
}

/**
 * In-memory channel-score clipboard: survives switching patterns and closing
 * the menu, not a restart. The piano roll keeps its own note clipboard.
 */
let channelScoreClipboard: ChannelScore | null = null;

function scoreDetail(score: ChannelScore): string {
  const hits = score.steps.filter((cell) => cell.on).length;
  const notes = score.notes.length;
  if (notes > 0 && hits > 0) return `${hits} steps · ${notes} notes`;
  if (notes > 0) return `${notes} note${notes === 1 ? '' : 's'}`;
  if (hits > 0) return `${hits} step${hits === 1 ? '' : 's'}`;
  return 'empty';
}

/** Keep a context menu inside the window, matching the mixer's effect picker. */
function menuPosition(event: MouseEvent, width: number, height: number): { x: number; y: number } {
  return {
    x: Math.max(8, Math.min(event.clientX, window.innerWidth - width - 8)),
    y: Math.max(8, Math.min(event.clientY, window.innerHeight - height - 8)),
  };
}

/** Where cell `index`'s left edge sits inside a lane, in pixels. */
function cellX(index: number): number {
  const bar = Math.floor(index / CELLS_PER_BAR);
  return bar * BAR_PITCH + (index - bar * CELLS_PER_BAR) * CELL_PITCH;
}

/** How wide a lane of `steps` cells is. The roll preview matches it exactly. */
function laneWidth(steps: number): number {
  return steps <= 0 ? 0 : cellX(steps - 1) + CELL_W;
}

export function Rack(props: { onClose?: () => void } = {}): ReturnType<typeof Show> {
  const app = useAppState();
  const view = useView();
  const nav = usePatternNav();
  const [instrumentFor, setInstrumentFor] = createSignal<number | null>(null);
  const [dragOver, setDragOver] = createSignal(false);
  /** Whether a render-to-sample is in flight. One at a time. */
  const [rendering, setRendering] = createSignal(false);
  const [channelMenu, setChannelMenu] = createSignal<ChannelMenuState | null>(null);

  const pattern = () => app.pattern();
  const activeId = () => app.sequencer()?.patternSet.activeId ?? 0;

  /** The clip the active pattern puts on `channel`, if any. */
  const clipFor = (channel: number): ChannelClipDto | undefined =>
    pattern()?.channels.find((clip) => clip.channel === channel);

  // --- where in the loop we are --------------------------------------------

  let rowsEl: HTMLDivElement | undefined;
  let pipEl: HTMLDivElement | undefined;

  /**
   * How far the lanes start from the left edge of the row stack.
   *
   * Measured rather than added up from the LED and the name box, because a
   * constant here would be a second opinion about the row's layout that the
   * CSS could drift away from without anything failing. Cached: it only
   * moves when the row's chrome does.
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
    if (pipEl) pipEl.style.display = 'none';
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
    const paint = () => {
      if (!pipEl) return;
      const position = soundingStep(current);
      if (position === null) {
        hidePosition();
        lastStep = -1;
        return;
      }
      // An explicit value, not `''`: the stylesheet's own `display: none` is
      // what hides this, so clearing the inline style would un-hide nothing.
      const left = laneOffset();
      const step = Math.min(current.steps - 1, Math.max(0, Math.floor(position)));
      if (step === lastStep) return;
      lastStep = step;
      pipEl.style.display = 'block';
      pipEl.style.transform = `translateX(${left + cellX(step)}px)`;
    };

    paint();
    if (!app.playing()) return;
    onFrame(paint);
    onCleanup(hidePosition);
  });

  function selectChannel(index: number): void {
    view.setStrip({ kind: 'track', index });
    view.setSideTab('inspector');
    app.selectTrack(index);
  }

  // --- steps ---------------------------------------------------------------

  function paintStep(channel: number, index: number, on: boolean, accent: boolean): void {
    const key = app.channelKey(channel);
    app.editPattern((current) => {
      const needed = index < 16 ? 16 : index < 32 ? 32 : 64;
      const resized = needed > current.steps ? withLength(current, needed) : current;
      return withContentLength(withSetStep(resized, channel, index, on, accent, key));
    });
  }

  function repeatFirstBar(channel: number, name: string): void {
    const key = app.channelKey(channel);
    app.editPatternOnce('Repeat first bar', (current) =>
      withRepeatedFirstBar(current, channel, key),
    );
    app.say(`${name} · Repeat`, 'first bar copied through the pattern');
  }

  /**
   * Channel Rack keys while this editor has focus:
   * Ctrl+C / Ctrl+X / Ctrl+V copy, cut and paste the selected channel's score;
   * Ctrl+Shift+Left/Right rotate its steps.
   */
  createEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      if (view.view() !== 'rack' || view.rollOpen()) return;
      if (event.defaultPrevented || target?.closest('[aria-label="Browser"], .wv-plug:not(.wv-rack-window), .wv-menu, .wv-modal')) return;
      if (
        target &&
        (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.tagName === 'SELECT' || target.isContentEditable)
      ) {
        return;
      }
      if (!(event.ctrlKey || event.metaKey) || event.altKey) return;
      const strip = view.strip();
      if (strip.kind !== 'track') return;
      const track = app.tracks()[strip.index];
      if (!track || isAudioClipChannel(track)) return;

      if (event.shiftKey) {
        if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return;
        event.preventDefault();
        const delta = event.key === 'ArrowLeft' ? -1 : 1;
        const key = app.channelKey(strip.index);
        app.editPatternOnce(delta < 0 ? 'Shift steps left' : 'Shift steps right', (current) =>
          withShiftedSteps(current, strip.index, delta, key),
        );
        app.say(`${track.name} · Shift`, delta < 0 ? '1 step left' : '1 step right');
        return;
      }

      const key = event.key.toLowerCase();
      if (key !== 'c' && key !== 'x' && key !== 'v') return;
      event.preventDefault();
      const current = pattern();
      if (!current) return;
      if (key === 'c' || key === 'x') {
        channelScoreClipboard = channelScore(current, strip.index);
        app.say(`${track.name} · ${key === 'x' ? 'Cut' : 'Copy'}`, scoreDetail(channelScoreClipboard));
        if (key === 'c') return;
        const trigger = app.channelKey(strip.index);
        app.editPatternOnce('Cut', (target) => withClearedChannel(target, strip.index, trigger));
        return;
      }
      const score = channelScoreClipboard;
      if (!score) return;
      const trigger = app.channelKey(strip.index);
      app.editPatternOnce('Paste', (target) =>
        withPastedScore(target, strip.index, score, trigger),
      );
      app.say(`${track.name} · Paste`, scoreDetail(score));
    };
    window.addEventListener('keydown', onKeyDown);
    onCleanup(() => window.removeEventListener('keydown', onKeyDown));
  });

  // --- the mode button -----------------------------------------------------

  function toggleMode(channel: number, name: string): void {
    const current = pattern();
    if (!current) return;
    const clip = clipFor(channel);
    const key = app.channelKey(channel);
    const toRoll = displayMode(clip) === 'steps';

    if (toRoll) {
      const after = withStepsConvertedToNotes(current, channel, key);
      app.editPatternOnce('Convert to piano roll', (target) =>
        withStepsConvertedToNotes(target, channel, key),
      );
      const count = channelClip(after, channel)?.notes.length ?? 0;
      app.say(
        `${name} · Mode`,
        count > 0 ? `Piano roll — ${count} steps became notes` : 'Piano roll',
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
    const strip = view.strip();
    const selected = strip.kind === 'track' ? strip.index : null;
    const nextSelected =
      selected === null
        ? null
        : selected === track.index
          ? Math.max(0, track.index - 1)
          : selected > track.index
            ? selected - 1
            : selected;
    if (nextSelected !== null) view.setStrip({ kind: 'track', index: nextSelected });
    void app.mixerCommand(async (backend) => {
      const state = await backend.removeTrack(track.index);
      return nextSelected === null ? state : backend.setSelectedTrack(nextSelected);
    });
    // Removing a track renumbers every pattern's channels, so no entry below
    // could be replayed against the arrangement that is left.
    app.clearHistory(`removing ${track.name}`);
    app.say('Channel · Delete', track.name);
  }

  function addFromDrop(event: DragEvent): void {
    event.preventDefault();
    setDragOver(false);
    const filePath = droppedAudioPath(event);
    if (filePath) {
      placeSample(filePath);
      return;
    }
    // An OS drop arrives as bytes, not a path; import them, then place the
    // written file exactly as a library `file::` drop would be.
    const file = droppedAudioFile(event);
    if (file) {
      void importDroppedFile(file, (name, bytes) => app.backend.importDroppedAudio(name, bytes))
        .then((path) => placeSample(path))
        .catch(app.report);
      return;
    }
    const payload = parseDrop(event.dataTransfer?.getData('text/plain') ?? '');
    if (!payload) return;
    if (payload.kind === 'audio') {
      const source = app.audioSources().find((entry) => String(entry.id) === payload.ref);
      if (source?.path) {
        placeSample(source.path);
        return;
      }
      app.report(new Error('That audio file is no longer loaded'));
      return;
    }
    if (payload.kind === 'file') {
      app.report(new Error('That file is not an audio sample this build can play'));
      return;
    }
    if (payload.kind === 'pattern') {
      app.report(new Error('A pattern goes on a playlist lane, not the rack'));
      return;
    }
    placeSource(payload.ref);
  }

  /** Decode a file first, then atomically swap this row to its one-shot source. */
  async function replaceWithSample(track: TrackDto, path: string): Promise<void> {
    const loaded = await app.backend.loadAudioSource(path);
    await app.mixerCommand((backend) =>
      backend.setTrackSource(track.index, `oneshot:${loaded.id}`),
    );
    await app.refreshAudioSources();
    selectChannel(track.index);
    app.say(`${track.name} · Replace`, loaded.name);
  }

  /** Replace an existing row with a browser instrument, including hosted ones. */
  async function replaceWithSource(track: TrackDto, sourceId: string): Promise<void> {
    const source = app.sources().find((entry) => entry.id === sourceId);
    const vst3 = app.vst3Plugins().find((plugin) => `vst3:${plugin.id}` === sourceId);
    await app.mixerCommand((backend) => {
      if (source?.kind === 'clap' && source.bundlePath && source.pluginId) {
        return backend.setTrackSourceClap(track.index, source.bundlePath, source.pluginId);
      }
      if (vst3) return backend.setTrackSourceVst3(track.index, vst3.path, vst3.id);
      return backend.setTrackSource(track.index, sourceId);
    });
    selectChannel(track.index);
    app.say(`${track.name} · Replace`, source?.label ?? vst3?.name ?? sourceId);
  }

  /** A drop on a row replaces that row; the rack background still adds a row. */
  function replaceFromDrop(event: DragEvent, track: TrackDto): void {
    event.preventDefault();
    event.stopPropagation();
    setDragOver(false);

    const filePath = droppedAudioPath(event);
    if (filePath) {
      void replaceWithSample(track, filePath).catch(app.report);
      return;
    }

    // Capture the File synchronously: WebView2 clears DataTransfer after this
    // handler returns, before its bytes have been read.
    const file = droppedAudioFile(event);
    if (file) {
      void importDroppedFile(file, (name, bytes) => app.backend.importDroppedAudio(name, bytes))
        .then((path) => replaceWithSample(track, path))
        .catch(app.report);
      return;
    }

    const payload = parseDrop(event.dataTransfer?.getData('text/plain') ?? '');
    if (!payload) return;
    if (payload.kind === 'audio') {
      const source = app.audioSources().find((entry) => String(entry.id) === payload.ref);
      if (source?.path) void replaceWithSample(track, source.path).catch(app.report);
      else app.report(new Error('That audio file is no longer loaded'));
      return;
    }
    if (payload.kind === 'inst') {
      void replaceWithSource(track, payload.ref).catch(app.report);
      return;
    }
    if (payload.kind === 'pattern') {
      app.report(new Error('A pattern goes on a playlist lane, not a rack channel'));
    }
  }

  /**
   * Bounce this pattern through the mixer and drop it back in as a channel.
   *
   * "Resampling", the second half of what a sample editor is for: a hat pattern
   * with its filter and reverb becomes one file you can reverse, trim and pitch
   * as a unit. The engine renders one loop offline through the real graph and
   * lands the result exactly as a dropped sample lands — so the new channel is
   * a normal one-shot, and this opens its editor the way an import would.
   *
   * Deliberately **not** undoable: it adds a channel, and adding or removing
   * channels is the one structural edit `ui/lib/undo.ts` cannot replay. Clearing
   * the history for a purely additive operation would cost the user their note
   * edits, so the entry is simply never recorded — the channel's own delete
   * button is the way back.
   */
  function renderToSample(): void {
    if (rendering()) return;
    setRendering(true);
    void app.backend
      .renderPatternToSample()
      .then((result) =>
        app
          .mixerCommand(async () => result.mixer)
          .then(() => app.refreshAudioSources())
          .then(() => {
            view.openSampleEditor(result.track);
            const seconds = result.sampleRate > 0 ? result.frames / result.sampleRate : 0;
            app.say(
              'Render to sample',
              result.hostedChannels > 0
                ? `${result.name} · ${seconds.toFixed(2)} s · ${result.hostedChannels} hosted plugin${
                    result.hostedChannels === 1 ? '' : 's'
                  } could not be rendered`
                : `${result.name} · ${seconds.toFixed(2)} s`,
            );
          }),
      )
      .catch(app.report)
      .finally(() => setRendering(false));
  }

  function placeSample(path: string): void {
    void app.mixerCommand((backend) => backend.addSampleChannel(path)).then(() => {
      void app.refreshAudioSources();
    });
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
      <div class="wv-viewbar" data-window-drag>
        <span class="wv-viewbar__title">CHANNEL RACK</span>
        <div class="wv-pattabs">
          <Index each={app.patterns()}>
            {(summary) => (
              <button
                type="button"
                class="wv-pattab"
                classList={{ 'is-active': summary().id === activeId() }}
                style={
                  summary().id === activeId()
                    ? {
                        background: alpha(summary().color ?? trackColor(summary().id), 0.16),
                        'box-shadow': `inset 0 0 0 1px ${alpha(summary().color ?? trackColor(summary().id), 0.6)}`,
                      }
                    : undefined
                }
                title="Click to edit this pattern · double-click for the piano roll · F2 to rename"
                onClick={() =>
                  void app.sequencerCommand((backend) => backend.setActivePattern(summary().id))
                }
                onDblClick={(event) => {
                  event.preventDefault();
                  void nav.openPattern(summary().id, { altKey: event.altKey, surface: 'tab' });
                }}
              >
                <span
                  class="wv-swatch"
                  style={{ background: summary().color ?? trackColor(summary().id) }}
                />
                {summary().name}
              </button>
            )}
          </Index>
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
        <button
          type="button"
          class="wv-chip"
          disabled={rendering() || app.tracks().length === 0}
          title={
            rendering()
              ? 'Rendering this pattern…'
              : 'Render to sample — bounce one loop of this pattern through the mixer and drop it back in as a new sample channel'
          }
          onClick={renderToSample}
        >
          {rendering() ? 'Rendering…' : 'Render to sample'}
        </button>
        <span class="wv-viewbar__meta">
          {app.tracks().length} ch · {(pattern()?.steps ?? 16) / 16} bar
          {(pattern()?.steps ?? 16) / 16 === 1 ? '' : 's'}
        </span>
        <Show when={props.onClose}>
          <button type="button" class="wv-chip" aria-label="Close Channel Rack" title="Close Channel Rack (F6 to reopen)" onClick={() => props.onClose?.()}>×</button>
        </Show>
      </div>

      <div
        class="wv-rack"
        onDragOver={(event) => {
          allowDrop(event);
          if (!dragOver()) setDragOver(true);
        }}
        onDragLeave={(event) => {
          if (event.currentTarget.contains(event.relatedTarget as Node)) return;
          setDragOver(false);
        }}
        onDrop={addFromDrop}
      >
        <div class="wv-rack__rows" ref={rowsEl}>
          <div class="wv-rack__pip" ref={pipEl} style={{ width: `${CELL_W}px` }} />

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
                onPaintStep={(index, on, accent) => paintStep(track().index, index, on, accent)}
                onOpenRoll={() => {
                  selectChannel(track().index);
                  app.promoteStepsToRoll(track().index);
                  view.openRoll(track().index);
                }}
                onOpenSample={() => view.openSampleEditor(track().index)}
                onReplaceDrop={(event) => replaceFromDrop(event, track())}
                onToggleMode={() => toggleMode(track().index, track().name)}
                onRepeat={() => repeatFirstBar(track().index, track().name)}
                onRemove={() => removeChannel(track())}
                onChannelMenu={(event) => {
                  selectChannel(track().index);
                  setChannelMenu({
                    channel: track().index,
                    name: track().name,
                    ...menuPosition(event, 216, 560),
                  });
                }}
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

      <Show when={channelMenu()}>
        {(menu) => (
          <ChannelMenu
            menu={menu()}
            close={() => setChannelMenu(null)}
            onReplace={() => setInstrumentFor(menu().channel)}
          />
        )}
      </Show>
    </div>
  );
}

/** Keep channel controls visible while scrolling a long pattern. */
function RackRow(props: {
  track: TrackDto;
  pattern: PatternDto | undefined;
  selected: boolean;
  canRemove: boolean;
  onSelect: () => void;
  onPaintStep: (index: number, on: boolean, accent: boolean) => void;
  onOpenRoll: () => void;
  onOpenSample: () => void;
  onReplaceDrop: (event: DragEvent) => void;
  onToggleMode: () => void;
  onRepeat: () => void;
  onRemove: () => void;
  onChannelMenu: (event: MouseEvent) => void;
}): ReturnType<typeof Show> {
  const app = useAppState();
  const colour = () => channelColor(props.track);
  const clip = () => props.pattern?.channels.find((entry) => entry.channel === props.track.index);
  const isRoll = () => displayMode(clip()) === 'roll';
  const length = () => props.pattern?.steps ?? 16;
  /** Looping cells plus one grayed bar -- see `rackDisplaySteps`. */
  const shown = () => rackDisplaySteps(length());
  /** Always a full lane -- see `stepCells` for the three ways a clip arrives. */
  const steps = () => stepCells(clip(), shown());
  const [localDb, setLocalDb] = createSignal<number | null>(null);
  const [localPan, setLocalPan] = createSignal<number | null>(null);
  const [replaceOver, setReplaceOver] = createSignal(false);
  const gainDb = () => localDb() ?? props.track.gainDb;
  const pan = () => localPan() ?? props.track.pan;
  const panDef = spec('pan', 'Pan', '', -1, 1, 0);
  const volumeDef = () =>
    spec(
      'volume',
      'Volume',
      'dB',
      app.mixer()?.minGainDb ?? -60,
      app.mixer()?.maxGainDb ?? 6,
      0,
    );

  function setGain(value: number): void {
    setLocalDb(value);
    void app.backend.setTrackGain(props.track.index, value).catch(app.report);
  }

  function commitGain(): void {
    void app.mixerCommand((backend) => backend.getTracks()).finally(() => setLocalDb(null));
  }

  function setPan(value: number): void {
    setLocalPan(value);
    void app.backend.setTrackPan(props.track.index, value).catch(app.report);
  }

  function commitPan(): void {
    void app.mixerCommand((backend) => backend.getTracks()).finally(() => setLocalPan(null));
  }

  let laneEl: HTMLDivElement | undefined;
  let paint: { on: boolean; accent: boolean; seen: Set<number>; last: number } | null = null;

  function stepAt(clientX: number, clientY: number): number | null {
    const hit = document.elementFromPoint(clientX, clientY);
    const step = hit instanceof Element ? hit.closest('.wv-step') : null;
    if (!step || !laneEl?.contains(step)) return null;
    const index = Number((step as HTMLElement).dataset.index);
    return Number.isInteger(index) ? index : null;
  }

  function applyPaint(index: number): void {
    if (!paint) return;
    const from = paint.last;
    const lo = Math.min(from, index);
    const hi = Math.max(from, index);
    for (let step = lo; step <= hi; step += 1) {
      if (paint.seen.has(step)) continue;
      paint.seen.add(step);
      props.onPaintStep(step, paint.on, paint.accent);
    }
    paint.last = index;
  }

  function beginPaint(event: PointerEvent, index: number, wasOn: boolean): void {
    if (event.button !== 0) return;
    event.preventDefault();
    event.stopPropagation();
    const accent = event.shiftKey;
    const on = !wasOn;
    paint = { on, accent, seen: new Set(), last: index };
    app.startPatternGesture(on ? (accent ? 'Accent steps' : 'Paint steps') : 'Erase steps');
    applyPaint(index);

    const move = (moveEvent: PointerEvent) => {
      const next = stepAt(moveEvent.clientX, moveEvent.clientY);
      if (next !== null) applyPaint(next);
    };
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
      window.removeEventListener('pointercancel', up);
      document.body.style.removeProperty('user-select');
      paint = null;
      app.commitPatternGesture();
    };
    document.body.style.setProperty('user-select', 'none');
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
    window.addEventListener('pointercancel', up);
  }

  onCleanup(() => {
    if (!paint) return;
    paint = null;
    app.commitPatternGesture();
  });

  return (
    <div
      class="wv-chrow"
      classList={{
        'is-selected': props.selected,
        'is-muted': props.track.muted,
        'is-replace': replaceOver(),
      }}
      onClick={() => props.onSelect()}
      onContextMenu={(event) => {
        event.preventDefault();
        props.onChannelMenu(event);
      }}
      onDragOver={(event) => {
        event.stopPropagation();
        allowDrop(event);
        if (!replaceOver()) setReplaceOver(true);
      }}
      onDragLeave={(event) => {
        if (event.currentTarget.contains(event.relatedTarget as Node)) return;
        setReplaceOver(false);
      }}
      onDrop={(event) => {
        setReplaceOver(false);
        props.onReplaceDrop(event);
      }}
    >
      <Show when={replaceOver()}>
        <span class="wv-chrow__replace">REPLACE {props.track.name}</span>
      </Show>
      <div class="wv-chrow__controls">
      <button
        type="button"
        class="wv-led"
        aria-label={`${props.track.muted ? 'Unmute' : 'Mute'} ${props.track.name}`}
        classList={{ 'is-muted': props.track.muted }}
        style={{ background: props.track.muted ? 'transparent' : colour() }}
        title={
          props.track.muted
            ? `Unmute this channel · ${channelKind(props.track).title}`
            : `Mute this channel · ${channelKind(props.track).title}`
        }
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
          title={`${
            props.track.clap?.hasEditor || props.track.vst3?.hasEditor
              ? 'Open this instrument’s own window'
              : 'Open this instrument’s window'
          } · right-click for piano roll and fills`}
          onClick={(event) => {
            event.stopPropagation();
            // A double-click includes two clicks. Open/toggle only once.
            if (event.detail > 1) return;
            props.onSelect();
            const slot = props.track.clap ?? props.track.vst3;
            if (slot?.hasEditor) {
              app.toggleGui({ slot: 'track', index: props.track.index }, !slot.guiOpen);
            } else if (hasSampleWindow(props.track)) {
              props.onOpenSample();
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
      </div>

      <div class="wv-chrow__vol" onClick={(event) => event.stopPropagation()}>
        <Knob
          def={panDef}
          value={pan}
          onInput={setPan}
          onCommit={commitPan}
          owner={props.track.name}
          size={22}
          color={colour()}
          title="Channel pan — double-click to centre"
        />
      </div>
      <div class="wv-chrow__vol" onClick={(event) => event.stopPropagation()}>
        <Knob
          def={volumeDef()}
          value={gainDb}
          onInput={setGain}
          onCommit={commitGain}
          owner={props.track.name}
          size={22}
          color={colour()}
          title={`Channel volume ${gainDb().toFixed(1)} dB — linked to the sample inspector and mixer`}
        />
      </div>

      <button
        type="button"
        class="wv-chrow__repeat"
        disabled={length() <= 16 || isAudioClipChannel(props.track)}
        title="Repeat this channel's first bar through the pattern"
        onClick={(event) => {
          event.stopPropagation();
          props.onRepeat();
        }}
      >
        ↪
      </button>

      </div>
      <div class="wv-chrow__lane" ref={laneEl} style={{ width: `${laneWidth(shown())}px` }}>
        <Show
          when={!isAudioClipChannel(props.track)}
          fallback={
            <div
              class="wv-chrow__clip"
              style={{ width: `${laneWidth(shown())}px` }}
              title="This channel plays playlist clips of its file"
            >
              playlist
            </div>
          }
        >
        <Show
          when={!isRoll()}
          fallback={
            <button
              type="button"
              class="wv-chrow__preview"
              style={{
                width: `${laneWidth(shown())}px`,
                'background-size': `${BAR_PITCH}px 100%`,
              }}
              title="Open the piano roll for this channel"
              onClick={(event) => {
                event.stopPropagation();
                props.onOpenRoll();
              }}
            >
              <NotePreview clip={clip()} pattern={props.pattern} colour={colour()} />
              <Show when={length() < shown()}>
                <span
                  class="wv-chrow__tail"
                  style={{ left: `${cellX(length()) - BAR_GAP}px` }}
                />
              </Show>
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
                  data-index={index}
                  classList={{
                    'is-beat': beat,
                    'is-unused': index >= length(),
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
                  title={`Step ${index + 1} — drag to paint, shift-drag for an accent`}
                  aria-label={`${props.track.name} step ${index + 1}`}
                  aria-pressed={cell().on}
                  onPointerDown={(event) => {
                    event.stopPropagation();
                    beginPaint(event, index, cell().on);
                  }}
                  onClick={(event) => event.stopPropagation()}
                  onKeyDown={(event) => {
                    if (event.key !== 'Enter' || event.repeat || event.ctrlKey || event.metaKey || event.altKey) return;
                    event.preventDefault(); event.stopPropagation();
                    app.startPatternGesture(cell().on ? 'Erase step' : 'Add step');
                    props.onPaintStep(index, !cell().on, event.shiftKey);
                    app.commitPatternGesture();
                  }}
                />
              );
            }}
          </Index>
        </Show>
        </Show>
      </div>

      <button
        type="button"
        class="wv-chrow__mode"
        disabled={isAudioClipChannel(props.track)}
        title={
          isAudioClipChannel(props.track)
            ? 'Audio clips have no step grid'
            : isRoll()
              ? 'Fold these notes back onto the step grid — one key, no note lengths'
              : 'Turn these steps into notes and edit them in the piano roll'
        }
        onClick={(event) => {
          event.stopPropagation();
          if (isAudioClipChannel(props.track)) return;
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

/**
 * Right-click on a channel. Piano roll and replace live here so the name box
 * stays a name. The fills are FL's: every Nth cell on, the rest off, starting
 * at the left edge. Copy / paste move the score between channels or patterns.
 * Invert / shift / random rewrite the same grid. Notes go with them — the
 * compiler would play both.
 */
function ChannelMenu(props: {
  menu: ChannelMenuState;
  close: () => void;
  onReplace: () => void;
}): ReturnType<typeof Show> {
  const app = useAppState();
  const view = useView();
  const track = () => app.tracks()[props.menu.channel];
  const clipLocked = () => {
    const current = track();
    return !current || isAudioClipChannel(current);
  };

  function edit(
    label: string,
    run: (pattern: PatternDto, key: number) => PatternDto,
    detail: string,
  ): void {
    const key = app.channelKey(props.menu.channel);
    app.editPatternOnce(label, (current) => run(current, key));
    app.say(`${props.menu.name} · ${label}`, detail);
    props.close();
  }

  function fill(interval: number, meta: string): void {
    const label = interval === 1 ? 'Fill all' : `Fill every ${interval}`;
    const detail = meta || (interval === 1 ? 'every step' : `every ${interval} steps`);
    edit(label, (current, key) => withFilledSteps(current, props.menu.channel, interval, key), detail);
  }

  function invert(): void {
    edit(
      'Invert',
      (current, key) => withInvertedSteps(current, props.menu.channel, key),
      'on ↔ off',
    );
  }

  function shift(delta: number): void {
    const label = delta < 0 ? 'Shift left' : 'Shift right';
    edit(label, (current, key) => withShiftedSteps(current, props.menu.channel, delta, key), '1 step');
  }

  function randomise(): void {
    edit(
      'Random',
      (current, key) => withRandomSteps(current, props.menu.channel, Date.now(), key),
      'new pattern',
    );
  }

  function clear(): void {
    edit(
      'Clear',
      (current, key) => withClearedChannel(current, props.menu.channel, key),
      'empty',
    );
  }

  function copy(): void {
    const current = app.pattern();
    if (!current || clipLocked()) return;
    channelScoreClipboard = channelScore(current, props.menu.channel);
    app.say(`${props.menu.name} · Copy`, scoreDetail(channelScoreClipboard));
    props.close();
  }

  function cut(): void {
    const current = app.pattern();
    if (!current || clipLocked()) return;
    const score = channelScore(current, props.menu.channel);
    channelScoreClipboard = score;
    edit('Cut', (pattern, key) => withClearedChannel(pattern, props.menu.channel, key), scoreDetail(score));
  }

  function paste(): void {
    const score = channelScoreClipboard;
    if (!score || clipLocked()) return;
    edit(
      'Paste',
      (current, key) => withPastedScore(current, props.menu.channel, score, key),
      scoreDetail(score),
    );
  }

  return (
    <div
      class="wv-scrim"
      onClick={props.close}
      onContextMenu={(event) => {
        event.preventDefault();
        props.close();
      }}
    >
      <div
        class="wv-menu wv-menu--chrow"
        style={{ left: `${props.menu.x}px`, top: `${props.menu.y}px` }}
        onClick={(event) => event.stopPropagation()}
      >
        <button
          type="button"
          class="wv-menu__row"
          disabled={clipLocked()}
          title={
            clipLocked()
              ? 'Audio clips play from the playlist, not the piano roll'
              : 'Turn these steps into notes and open the piano roll (F7)'
          }
          onClick={() => {
            if (clipLocked()) return;
            app.promoteStepsToRoll(props.menu.channel);
            view.openRoll(props.menu.channel);
            props.close();
          }}
        >
          <span>Piano roll</span>
          <span class="wv-spacer" />
          <span class="wv-menu__meta">F7</span>
        </button>
        <button
          type="button"
          class="wv-menu__row"
          title="Replace this channel's instrument"
          onClick={() => {
            props.close();
            props.onReplace();
          }}
        >
          Replace instrument
        </button>
        <Show when={track() && hasSampleWindow(track()!)}>
          <button
            type="button"
            class="wv-menu__row"
            title="Open the sample window for this channel"
            onClick={() => {
              view.openSampleEditor(props.menu.channel);
              props.close();
            }}
          >
            Sample editor
          </button>
        </Show>
        <div class="wv-menu__rule" />
        <For each={FILL_INTERVALS}>
          {(entry) => (
            <button
              type="button"
              class="wv-menu__row"
              title={`Turn on step 1 and every ${entry.interval} after — the rest off`}
              onClick={() => fill(entry.interval, entry.meta)}
            >
              <span>Fill every {entry.interval}</span>
              <span class="wv-spacer" />
              <Show when={entry.meta}>
                <span class="wv-menu__meta">{entry.meta}</span>
              </Show>
            </button>
          )}
        </For>
        <button
          type="button"
          class="wv-menu__row"
          title="Turn every step on"
          onClick={() => fill(1, 'every step')}
        >
          <span>Fill all</span>
        </button>
        <div class="wv-menu__rule" />
        <button
          type="button"
          class="wv-menu__row"
          disabled={clipLocked()}
          title={
            clipLocked()
              ? 'Audio clips play from the playlist, not a step score'
              : "Copy this channel's steps and notes (Ctrl+C)"
          }
          onClick={copy}
        >
          <span>Copy</span>
          <span class="wv-spacer" />
          <span class="wv-menu__meta">Ctrl+C</span>
        </button>
        <button
          type="button"
          class="wv-menu__row"
          disabled={clipLocked()}
          title={
            clipLocked()
              ? 'Audio clips play from the playlist, not a step score'
              : "Cut this channel's steps and notes (Ctrl+X)"
          }
          onClick={cut}
        >
          <span>Cut</span>
          <span class="wv-spacer" />
          <span class="wv-menu__meta">Ctrl+X</span>
        </button>
        <button
          type="button"
          class="wv-menu__row"
          disabled={clipLocked() || channelScoreClipboard === null}
          title={
            clipLocked()
              ? 'Audio clips play from the playlist, not a step score'
              : channelScoreClipboard === null
                ? 'Copy a channel first'
                : 'Paste the copied steps and notes onto this channel (Ctrl+V)'
          }
          onClick={paste}
        >
          <span>Paste</span>
          <span class="wv-spacer" />
          <span class="wv-menu__meta">Ctrl+V</span>
        </button>
        <button type="button" class="wv-menu__row" title="Flip every step" onClick={invert}>
          Invert
        </button>
        <button
          type="button"
          class="wv-menu__row"
          title="Move every hit one step earlier"
          onClick={() => shift(-1)}
        >
          Shift left
        </button>
        <button
          type="button"
          class="wv-menu__row"
          title="Move every hit one step later"
          onClick={() => shift(1)}
        >
          Shift right
        </button>
        <button
          type="button"
          class="wv-menu__row"
          title="A new random pattern on this channel"
          onClick={randomise}
        >
          Random
        </button>
        <button
          type="button"
          class="wv-menu__row"
          title="Turn every step off and drop any notes"
          onClick={clear}
        >
          Clear
        </button>
      </div>
    </div>
  );
}

/** A channel's notes at a glance, laid out over the same span as the step lane. */
function NotePreview(props: {
  clip: ChannelClipDto | undefined;
  pattern: PatternDto | undefined;
  colour: string;
}): ReturnType<typeof Show> {
  const notes = () => props.clip?.notes ?? [];
  const span = () => rackDisplaySteps(props.pattern?.steps ?? 16) * TICKS_PER_STEP;
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
