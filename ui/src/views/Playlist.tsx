/**
 * The arrangement: pattern clips and audio clips along a timeline.
 *
 * Geometry is one number -- {@link BAR_PX} pixels per bar at zoom 1 -- and
 * everything else is derived from the engine's own tick constants, so the
 * ruler, the clips and the playhead cannot disagree about where bar 5 is.
 *
 * ## What a lane is, and is not
 *
 * A playlist lane is **not** a mixer track. It has a name, a mute and an
 * ordering, and that is all the engine gives it. The mock's lane header also
 * carries a volume slider, a solo and a record-arm; none of those exist for a
 * lane, so rather than draw three dead controls this header shows what the lane
 * actually has plus its clip count. A clip's *sound* is the channel's, and its
 * fader is in the mixer. The lane's **height**, though, is nobody's but the
 * view's and costs the engine nothing, so it is draggable: the grip along the
 * bottom edge of the header resizes that one lane, and a double-click puts it
 * back. Heights are kept by lane **id**, because an index would slide onto the
 * neighbour the moment a lane below was removed.
 *
 * ## A pattern clip draws the pattern
 *
 * An audio clip has a waveform; a pattern clip used to have a coloured
 * rectangle, which tells you where a clip is and nothing about which one. So it
 * draws a miniature of the pattern's real content -- every channel's notes and
 * switched-on steps, in that channel's own colour. That content is not in
 * `SequencerState` (which carries counts for every pattern and the full shape of
 * only the active one), so it is fetched per pattern through
 * `get_pattern_content` and cached here, the same arrangement the waveforms
 * already use.
 */

import { For, Index, Show, createEffect, createSignal, onCleanup } from 'solid-js';

import type { ClipDto, PatternDto, PlaylistTrackDto, WaveformDto } from '../api/types';
import {
  TICKS_PER_BAR,
  TICKS_PER_STEP,
  samplesToTicks,
  snapTicks,
  ticksToSamples,
} from '../lib/pattern';
import { useAppState } from '../state';
import {
  DEFAULT_LANE_H,
  PLAYLIST_SNAPS,
  clampLaneHeight,
  clampZoom,
  laneHeightOf,
  nextSnap,
  snapLabel,
  useView,
  type PlaylistTool,
} from '../shell/view';
import { alpha, trackColor } from '../theme/tokens';
import { onFrame } from '../ui/frameLoop';
import { IconEraseTool, IconPaintTool, IconSelectTool, IconX } from '../ui/icons';

/** Pixels one bar covers at zoom 1. */
const BAR_PX = 48;
/** How wide a lane header is. */
const HEAD_W = 176;
/**
 * The key window a clip miniature maps onto its height: `C2` to `B6`.
 *
 * Fixed rather than fitted to each pattern's own range, so a bass line sits low
 * in its clip and a lead sits high -- which is the whole reason to draw the
 * notes rather than a bar. A per-pattern range would put every clip's content
 * in the middle and say nothing.
 */
const MARK_LOW_KEY = 36;
const MARK_HIGH_KEY = 95;
/** Marks past this are a smear, and each one is a DOM node. */
const MAX_CLIP_MARKS = 120;
/** Narrower than this and there is nothing a mark could say. */
const MIN_ART_PX = 22;
/** How far past the last clip the timeline keeps going, in bars. */
const TAIL_BARS = 8;
/** The fewest bars ever drawn, so an empty arrangement still has a ruler. */
const MIN_BARS = 32;

const TOOLS: readonly [PlaylistTool, string, () => ReturnType<typeof IconPaintTool>][] = [
  ['paint', 'Paint clips — click a lane to place the brush', IconPaintTool],
  ['select', 'Select and move clips', IconSelectTool],
  ['delete', 'Delete clips', IconEraseTool],
];

/** One mark inside a clip miniature: fractions of the clip, and a colour. */
interface ClipMark {
  left: number;
  width: number;
  bottom: number;
  colour: string;
}

/**
 * A pattern's content as marks, in the clip's own coordinates.
 *
 * A channel that has notes is drawn by pitch, so the shape of the melody is
 * visible; a channel that has only steps has no pitch worth showing (a grid has
 * one key), so its hits are stacked on their own row per channel, low in the
 * clip, the way a drum lane reads in every DAW.
 */
function clipMarks(pattern: PatternDto, colourOf: (channel: number) => string): ClipMark[] {
  const span = Math.max(1, pattern.steps * TICKS_PER_STEP);
  const lanes = Math.max(1, pattern.channels.length);
  const marks: ClipMark[] = [];

  pattern.channels.forEach((clip, index) => {
    const colour = colourOf(clip.channel);
    if (clip.notes.length > 0) {
      for (const note of clip.notes) {
        const key = Math.min(MARK_HIGH_KEY, Math.max(MARK_LOW_KEY, note.key));
        marks.push({
          left: note.start / span,
          width: Math.max(0.005, note.length / span),
          bottom: (key - MARK_LOW_KEY) / (MARK_HIGH_KEY - MARK_LOW_KEY),
          colour,
        });
      }
      return;
    }
    const cells = clip.steps?.steps ?? [];
    const width = 0.7 / Math.max(1, pattern.steps);
    cells.forEach((cell, position) => {
      if (!cell.on) return;
      marks.push({
        left: position / Math.max(1, pattern.steps),
        width,
        // Stacked from the floor, one thin row per channel, so two drum lanes
        // do not land on top of each other and read as one.
        bottom: 0.06 + (index / lanes) * 0.5,
        colour,
      });
    });
  });

  return marks.slice(0, MAX_CLIP_MARKS);
}

export function Playlist(): ReturnType<typeof Show> {
  const app = useAppState();
  const view = useView();
  const [waveforms, setWaveforms] = createSignal<Record<number, WaveformDto>>({});
  const pending = new Set<number>();
  const [contents, setContents] = createSignal<Record<number, PatternDto>>({});
  const pendingPatterns = new Set<number>();

  let playheadEl: HTMLDivElement | undefined;
  let scrollEl: HTMLDivElement | undefined;

  const pxPerTick = () => (BAR_PX * view.playlistZoom()) / TICKS_PER_BAR;
  const playlist = () => app.playlist();
  const bars = () => {
    const end = playlist()?.endTicks ?? 0;
    return Math.max(MIN_BARS, Math.ceil(end / TICKS_PER_BAR) + TAIL_BARS);
  };
  const laneWidth = () => bars() * BAR_PX * view.playlistZoom();

  /** The waveform for one source, fetched once and remembered. */
  function waveformFor(sourceId: number): WaveformDto | undefined {
    const have = waveforms()[sourceId];
    if (have) return have;
    if (!pending.has(sourceId)) {
      pending.add(sourceId);
      app.backend
        .getAudioWaveform(sourceId, 512)
        .then((wave) => setWaveforms((all) => ({ ...all, [sourceId]: wave })))
        .catch(() => {})
        .finally(() => pending.delete(sourceId));
    }
    return undefined;
  }

  /**
   * The content of the pattern a clip plays, fetched once and remembered.
   *
   * The **active** pattern is never cached: it is already on the wire in full
   * and it changes under the user's hands, so a clip playing the pattern being
   * edited must redraw as the rack is clicked rather than lag it by however long
   * ago the fetch was.
   */
  function patternContent(id: number): PatternDto | undefined {
    const active = app.pattern();
    if (active && active.id === id) return active;
    const have = contents()[id];
    if (have) return have;
    if (!pendingPatterns.has(id)) {
      pendingPatterns.add(id);
      app.backend
        .getPatternContent(id)
        .then((content) => setContents((all) => ({ ...all, [id]: content })))
        .catch(() => {})
        .finally(() => pendingPatterns.delete(id));
    }
    return undefined;
  }

  /**
   * Drop a cached pattern that can no longer be trusted.
   *
   * Only the active pattern can be edited, so a cached copy goes stale in
   * exactly two ways: it *was* active and the rack has since moved on, or its
   * summary changed under it (a length change, a clone, an undo). Both are
   * cheap to watch, and neither needs the engine to tell us anything new.
   */
  let previousActive = -1;
  const fingerprints = new Map<number, string>();
  createEffect(() => {
    const active = app.sequencer()?.patternSet.activeId ?? -1;
    const stale = new Set<number>();
    if (previousActive >= 0 && previousActive !== active) stale.add(previousActive);
    previousActive = active;
    for (const summary of app.patterns()) {
      const mark = `${summary.steps}:${summary.hitCount}:${summary.noteCount}`;
      if (fingerprints.get(summary.id) !== mark) {
        if (fingerprints.has(summary.id)) stale.add(summary.id);
        fingerprints.set(summary.id, mark);
      }
    }
    if (stale.size === 0) return;
    setContents((all) => {
      const next = { ...all };
      for (const id of stale) delete next[id];
      return next;
    });
  });

  // --- lane heights --------------------------------------------------------

  const laneHeight = (laneId: number) => laneHeightOf(view.laneHeights(), laneId);

  /**
   * Which lane the pointer is over, by asking the document rather than dividing
   * by a lane height -- lanes are individually resizable now, so there is no
   * single height to divide by.
   */
  function laneUnder(clientX: number, clientY: number): number | null {
    for (const element of document.elementsFromPoint(clientX, clientY)) {
      const lane = (element as HTMLElement).closest<HTMLElement>('.wv-pl__lane');
      if (lane?.dataset.lane !== undefined) return Number(lane.dataset.lane);
    }
    return null;
  }

  // The playhead is written to the DOM on the frame loop: it moves every frame
  // and must not go through a signal. It is only meaningful in SONG mode --
  // in PAT mode the transport is looping a pattern, not running the timeline.
  onFrame(() => {
    if (!playheadEl) return;
    if (app.playMode() !== 'song') {
      playheadEl.style.display = 'none';
      return;
    }
    playheadEl.style.display = '';
    const ticks = samplesToTicks(
      app.playhead.positionSamples(),
      app.playhead.getSampleRate(),
      app.bpm(),
    );
    playheadEl.style.transform = `translateX(${ticks * pxPerTick()}px)`;
  });

  const onWheel = (event: WheelEvent) => {
    if (!event.shiftKey) return;
    event.preventDefault();
    view.setPlaylistZoom((zoom) => clampZoom(zoom * (event.deltaY < 0 ? 1.1 : 1 / 1.1)));
  };

  // --- placing -------------------------------------------------------------

  /** Where in the song a pointer at `clientX` is, snapped. */
  function tickAt(clientX: number, lane: HTMLElement): number {
    const rect = lane.getBoundingClientRect();
    const raw = (clientX - rect.left) / pxPerTick();
    return snapTicks(Math.max(0, raw), view.playlistSnap(), true);
  }

  function place(lane: number, start: number, brush = view.brush()): void {
    if (!brush) {
      app.say('Playlist', 'no brush — pick a pattern or a file in the Picker');
      return;
    }
    app.startPlaylistGesture(brush.kind === 'pattern' ? 'Paint pattern clip' : 'Paint audio clip');
    void app
      .sequencerCommand((backend) =>
        brush.kind === 'pattern'
          ? backend.addPatternClip(brush.id, lane, start)
          : backend.addAudioClip(brush.id, lane, start),
      )
      .then(() => app.commitPlaylistGesture());
  }

  function onLanePointerDown(event: PointerEvent, lane: number): void {
    if (event.button !== 0 || view.playlistTool() !== 'paint') return;
    place(lane, tickAt(event.clientX, event.currentTarget as HTMLElement));
  }

  function onLaneDrop(event: DragEvent, lane: number): void {
    event.preventDefault();
    const raw = event.dataTransfer?.getData('text/plain') ?? '';
    const [kind, ref] = raw.split('::');
    const id = Number(ref);
    if (!Number.isFinite(id)) return;
    const start = tickAt(event.clientX, event.currentTarget as HTMLElement);
    if (kind === 'pattern') place(lane, start, { kind: 'pattern', id });
    else if (kind === 'audio') place(lane, start, { kind: 'audio', id });
    else if (kind === 'inst') {
      app.report(new Error('An instrument goes on the rack, not a playlist lane'));
    }
  }

  // --- moving and resizing -------------------------------------------------

  function onClipPointerDown(event: PointerEvent, clip: ClipDto, mode: 'move' | 'resize'): void {
    event.stopPropagation();
    if (event.button !== 0) return;
    if (view.playlistTool() === 'delete') {
      removeClip(clip);
      return;
    }
    view.setSelectedClip(clip.id);
    const startX = event.clientX;
    const startY = event.clientY;
    const originStart = clip.start;
    const originLength = clip.length;
    const originLane = clip.track;
    let moved = false;
    let lastStart = originStart;
    let lastLane = originLane;
    let lastLength = originLength;

    app.startPlaylistGesture(mode === 'move' ? 'Move clip' : 'Resize clip');

    const onMove = (moveEvent: PointerEvent) => {
      const dx = (moveEvent.clientX - startX) / pxPerTick();
      if (!moved && Math.abs(moveEvent.clientX - startX) < 3 && Math.abs(moveEvent.clientY - startY) < 3) {
        return;
      }
      moved = true;
      if (mode === 'resize') {
        const snap = view.playlistSnap() || 1;
        lastLength = Math.max(snap, snapTicks(originLength + dx, view.playlistSnap()));
        app.say(clipName(clip), `${(lastLength / TICKS_PER_BAR).toFixed(2)} bars`);
        // Optimistic: the engine answers with the whole state, which would
        // otherwise arrive a frame late and make the edge stutter.
        (moveEvent.target as HTMLElement | null)?.closest<HTMLElement>('.wv-clip')?.style.setProperty(
          'width',
          `${Math.max(4, lastLength * pxPerTick() - 2)}px`,
        );
        return;
      }
      // The lane under the pointer, not a lane count derived from a fixed
      // height: with resizable lanes the two stop agreeing at the first drag.
      // Off the stack entirely (past the last lane, or over a scrollbar) keeps
      // the last good answer rather than snapping back to where it started.
      lastLane = laneUnder(moveEvent.clientX, moveEvent.clientY) ?? lastLane;
      lastStart = snapTicks(Math.max(0, originStart + dx), view.playlistSnap());
      app.say(
        clipName(clip),
        `bar ${(lastStart / TICKS_PER_BAR + 1).toFixed(2)} · ${playlist()?.tracks[lastLane]?.name ?? ''}`,
      );
    };

    const onUp = () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      if (!moved) {
        app.commitPlaylistGesture();
        return;
      }
      const run =
        mode === 'resize'
          ? app.sequencerCommand((backend) => backend.resizeClip(clip.id, lastLength))
          : app.sequencerCommand((backend) => backend.moveClip(clip.id, lastLane, lastStart));
      void run.then(() => app.commitPlaylistGesture());
    };

    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
  }

  function clipName(clip: ClipDto): string {
    if (clip.kind === 'pattern') {
      return app.patterns().find((pattern) => pattern.id === clip.patternId)?.name ?? 'Clip';
    }
    return app.audioSources().find((source) => source.id === clip.sourceId)?.name ?? 'Audio';
  }

  function removeClip(clip: ClipDto): void {
    app.startPlaylistGesture('Delete clip');
    void app
      .sequencerCommand((backend) => backend.removeClip(clip.id))
      .then(() => app.commitPlaylistGesture());
  }

  // --- lanes ---------------------------------------------------------------

  function removeLane(index: number, name: string): void {
    const lost = playlist()?.clips.filter((clip) => clip.track === index).length ?? 0;
    void app.sequencerCommand((backend) => backend.removePlaylistTrack(index));
    app.clearHistory(`removing ${name}`);
    app.say('Lane · Delete', lost > 0 ? `${name} and ${lost} clip${lost > 1 ? 's' : ''}` : name);
  }

  const brushLabel = () => {
    const brush = view.brush();
    if (!brush) return 'No brush — pick one on the right';
    if (brush.kind === 'pattern') {
      return `Brush · ${app.patterns().find((p) => p.id === brush.id)?.name ?? '?'}`;
    }
    return `Brush · ${app.audioSources().find((s) => s.id === brush.id)?.name ?? '?'}`;
  };

  const brushColour = () => {
    const brush = view.brush();
    if (!brush) return 'var(--dim)';
    if (brush.kind === 'pattern') {
      const pattern = app.patterns().find((p) => p.id === brush.id);
      return pattern?.color ?? trackColor(brush.id);
    }
    return 'var(--acc)';
  };

  onCleanup(() => pending.clear());

  return (
    <div class="wv-main">
      <div class="wv-viewbar">
        <span class="wv-viewbar__title">PLAYLIST</span>
        <div class="wv-seg wv-seg--plain">
          <For each={TOOLS}>
            {([id, title, Icon]) => (
              <button
                type="button"
                class="wv-tool"
                classList={{ 'is-active': view.playlistTool() === id }}
                title={title}
                onClick={() => view.setPlaylistTool(id)}
              >
                <Icon />
              </button>
            )}
          </For>
        </div>
        <button
          type="button"
          class="wv-chip wv-chip--sunk"
          title="What clip edges snap to"
          onClick={() => view.setPlaylistSnap(nextSnap(PLAYLIST_SNAPS, view.playlistSnap()))}
        >
          <span class="wv-chip__tag">SNAP</span>
          {snapLabel(PLAYLIST_SNAPS, view.playlistSnap())}
        </button>
        <div class="wv-zoom">
          <button
            type="button"
            class="wv-zoom__btn"
            title="Zoom out (shift-scroll)"
            onClick={() => view.setPlaylistZoom((zoom) => clampZoom(zoom / 1.25))}
          >
            −
          </button>
          <button
            type="button"
            class="wv-zoom__btn"
            title="Zoom in (shift-scroll)"
            onClick={() => view.setPlaylistZoom((zoom) => clampZoom(zoom * 1.25))}
          >
            +
          </button>
        </div>
        <div class="wv-spacer" />
        <span class="wv-brush" style={{ background: alpha(brushColour(), 0.14), color: brushColour() }}>
          <span class="wv-swatch" style={{ background: brushColour() }} />
          {brushLabel()}
        </span>
      </div>

      <div class="wv-pl" ref={scrollEl} onWheel={onWheel}>
        <div class="wv-pl__inner" style={{ width: `${HEAD_W + laneWidth()}px` }}>
          <div class="wv-pl__ruler">
            <div class="wv-pl__corner" style={{ width: `${HEAD_W}px` }}>
              {playlist()?.tracks.length ?? 0} lanes · {playlist()?.clips.length ?? 0} clips
            </div>
            <div
              class="wv-pl__ruler-track"
              style={{ width: `${laneWidth()}px` }}
              title="Click to move the playhead"
              onPointerDown={(event) => {
                const rect = event.currentTarget.getBoundingClientRect();
                const ticks = Math.max(0, (event.clientX - rect.left) / pxPerTick());
                app.seek(
                  ticksToSamples(ticks, app.playhead.getSampleRate(), app.bpm()),
                );
                app.say('Transport · Seek', `bar ${(ticks / TICKS_PER_BAR + 1).toFixed(2)}`);
              }}
            >
              <Index each={Array.from({ length: bars() })}>
                {(_, bar) => (
                  <span
                    class="wv-pl__mark"
                    classList={{ 'is-major': bar % 4 === 0 }}
                    style={{
                      left: `${bar * BAR_PX * view.playlistZoom()}px`,
                      width: `${BAR_PX * view.playlistZoom()}px`,
                    }}
                  >
                    {bar % 4 === 0 ? bar + 1 : ''}
                  </span>
                )}
              </Index>
            </div>
          </div>

          <Index each={playlist()?.tracks ?? []}>
            {(lane, index) => (
              <div class="wv-pl__row">
                <div
                  class="wv-pl__head"
                  style={{ width: `${HEAD_W}px`, height: `${laneHeight(lane().id)}px` }}
                >
                  <button
                    type="button"
                    class="wv-led"
                    classList={{ 'is-muted': lane().muted }}
                    title={lane().muted ? 'Unmute this lane' : 'Mute this lane'}
                    onClick={() => {
                      void app.sequencerCommand((backend) =>
                        backend.setPlaylistTrackMute(index, !lane().muted),
                      );
                      app.say(`${lane().name} · Lane`, lane().muted ? 'Active' : 'Muted');
                    }}
                  />
                  <LaneName
                    name={lane().name}
                    onRename={(name) =>
                      void app.sequencerCommand((backend) =>
                        backend.renamePlaylistTrack(index, name),
                      )
                    }
                  />
                  <span class="wv-pl__count">
                    {playlist()?.clips.filter((clip) => clip.track === index).length ?? 0}
                  </span>
                  <button
                    type="button"
                    class="wv-pl__del"
                    disabled={(playlist()?.tracks.length ?? 1) <= 1}
                    classList={{ 'wv-disabled': (playlist()?.tracks.length ?? 1) <= 1 }}
                    title={
                      (playlist()?.tracks.length ?? 1) <= 1
                        ? 'An arrangement must keep at least one lane'
                        : 'Remove this lane and every clip on it'
                    }
                    onClick={() => removeLane(index, lane().name)}
                  >
                    <IconX />
                  </button>
                  <LaneGrip lane={lane()} height={laneHeight(lane().id)} />
                </div>
                <div
                  class="wv-pl__lane"
                  data-lane={index}
                  classList={{ 'is-muted': lane().muted, 'is-painting': view.playlistTool() === 'paint' }}
                  style={{
                    width: `${laneWidth()}px`,
                    height: `${laneHeight(lane().id)}px`,
                    'background-size': `${BAR_PX * view.playlistZoom() * 4}px 100%, ${BAR_PX * view.playlistZoom()}px 100%`,
                  }}
                  onPointerDown={(event) => onLanePointerDown(event, index)}
                  onDragOver={(event) => event.preventDefault()}
                  onDrop={(event) => onLaneDrop(event, index)}
                  onContextMenu={(event) => event.preventDefault()}
                >
                  <For each={playlist()?.clips.filter((clip) => clip.track === index) ?? []}>
                    {(clip) => (
                      <Clip
                        clip={clip}
                        pxPerTick={pxPerTick()}
                        name={clipName(clip)}
                        colour={
                          clip.kind === 'pattern'
                            ? (app.patterns().find((p) => p.id === clip.patternId)?.color ??
                              trackColor(clip.patternId ?? 0))
                            : 'var(--acc)'
                        }
                        waveform={clip.kind === 'audio' ? waveformFor(clip.sourceId!) : undefined}
                        marks={() => {
                          if (clip.kind !== 'pattern' || clip.patternId === undefined) return [];
                          if (clip.length * pxPerTick() < MIN_ART_PX) return [];
                          const content = patternContent(clip.patternId);
                          if (!content) return [];
                          return clipMarks(content, (channel) => {
                            const track = app.tracks()[channel];
                            return track ? trackColor(track.id) : trackColor(channel);
                          });
                        }}
                        selected={view.selectedClip() === clip.id}
                        onDown={(event, mode) => onClipPointerDown(event, clip, mode)}
                        onDelete={() => removeClip(clip)}
                        onOpen={() => {
                          if (clip.kind !== 'pattern' || clip.patternId === undefined) return;
                          void app.sequencerCommand((backend) =>
                            backend.setActivePattern(clip.patternId!),
                          );
                          view.go('rack');
                        }}
                      />
                    )}
                  </For>
                </div>
              </div>
            )}
          </Index>

          <div class="wv-pl__row">
            <button
              type="button"
              class="wv-pl__add"
              style={{ width: `${HEAD_W}px` }}
              disabled={(playlist()?.tracks.length ?? 0) >= (playlist()?.maxTracks ?? 16)}
              classList={{
                'wv-disabled': (playlist()?.tracks.length ?? 0) >= (playlist()?.maxTracks ?? 16),
              }}
              title={
                (playlist()?.tracks.length ?? 0) >= (playlist()?.maxTracks ?? 16)
                  ? 'The engine allows no more lanes'
                  : 'Add a lane'
              }
              onClick={() => void app.sequencerCommand((backend) => backend.addPlaylistTrack())}
            >
              + Add lane
            </button>
            <div class="wv-pl__addfill" style={{ width: `${laneWidth()}px` }} />
          </div>

          <div class="wv-pl__playhead" ref={playheadEl} style={{ left: `${HEAD_W}px` }} />
        </div>

        <Show when={(playlist()?.clips.length ?? 0) === 0}>
          <div class="wv-pl__hint">
            <strong>The arrangement is empty.</strong>
            <span class="wv-empty__hint">
              Pick a pattern in the Picker on the right, then click a lane with the paint tool — or
              drag one straight onto a lane. Switch the transport to SONG to hear it.
            </span>
          </div>
        </Show>
      </div>
    </div>
  );
}

/** A lane's name, renamed in place. */
function LaneName(props: { name: string; onRename: (name: string) => void }): ReturnType<typeof Show> {
  const [editing, setEditing] = createSignal(false);
  return (
    <Show
      when={editing()}
      fallback={
        <button
          type="button"
          class="wv-pl__name"
          title="Double-click to rename"
          onDblClick={() => setEditing(true)}
        >
          {props.name}
        </button>
      }
    >
      <input
        class="wv-pl__nameinput"
        value={props.name}
        ref={(el) => queueMicrotask(() => el.select())}
        onKeyDown={(event) => {
          event.stopPropagation();
          if (event.key === 'Escape') setEditing(false);
          if (event.key !== 'Enter') return;
          const name = event.currentTarget.value.trim();
          setEditing(false);
          if (name && name !== props.name) props.onRename(name);
        }}
        onBlur={(event) => {
          const name = event.currentTarget.value.trim();
          setEditing(false);
          if (name && name !== props.name) props.onRename(name);
        }}
      />
    </Show>
  );
}

/**
 * The grip that resizes one lane.
 *
 * A strip along the bottom edge of the header rather than of the lane itself:
 * the lane is a drawing surface and a five-pixel band of it that paints nothing
 * when you click would be a hole in the paint tool.
 */
function LaneGrip(props: { lane: PlaylistTrackDto; height: number }): ReturnType<typeof Show> {
  const view = useView();
  const app = useAppState();

  const setHeight = (height: number) =>
    view.setLaneHeights((all) => ({ ...all, [props.lane.id]: clampLaneHeight(height) }));

  return (
    <span
      class="wv-pl__grip"
      title={`${props.lane.name} — drag to resize this lane, double-click for ${DEFAULT_LANE_H} px`}
      onPointerDown={(event) => {
        if (event.button !== 0) return;
        event.preventDefault();
        event.stopPropagation();
        const startY = event.clientY;
        const startHeight = props.height;
        const onMove = (move: PointerEvent) => {
          const next = clampLaneHeight(startHeight + (move.clientY - startY));
          setHeight(next);
          app.say(`${props.lane.name} · Lane height`, `${next} px`);
        };
        const onUp = () => {
          window.removeEventListener('pointermove', onMove);
          window.removeEventListener('pointerup', onUp);
          document.body.style.removeProperty('cursor');
          document.body.style.removeProperty('user-select');
        };
        document.body.style.setProperty('cursor', 'ns-resize');
        document.body.style.setProperty('user-select', 'none');
        window.addEventListener('pointermove', onMove);
        window.addEventListener('pointerup', onUp);
      }}
      onDblClick={(event) => {
        event.stopPropagation();
        view.setLaneHeights((all) => {
          const next = { ...all };
          delete next[props.lane.id];
          return next;
        });
        app.say(`${props.lane.name} · Lane height`, `${DEFAULT_LANE_H} px`);
      }}
    />
  );
}

/** One clip: a coloured block with either the pattern it plays or a waveform. */
function Clip(props: {
  clip: ClipDto;
  pxPerTick: number;
  name: string;
  colour: string;
  waveform: WaveformDto | undefined;
  /** The pattern's content as marks. Empty for an audio clip or a narrow one. */
  marks: () => ClipMark[];
  selected: boolean;
  onDown: (event: PointerEvent, mode: 'move' | 'resize') => void;
  onDelete: () => void;
  onOpen: () => void;
}): ReturnType<typeof Show> {
  /** The waveform as one SVG path, drawn from the engine's own min/max pairs. */
  const path = () => {
    const wave = props.waveform;
    if (!wave || wave.peaks.length === 0) return '';
    const step = 100 / wave.peaks.length;
    const top = wave.peaks.map((pair, index) => `${index * step},${50 - pair[1] * 46}`);
    const bottom = wave.peaks
      .map((pair, index) => `${(wave.peaks.length - 1 - index) * step},${50 - pair[0] * 46}`)
      .reverse()
      .reverse();
    return `M${top.join(' L')} L${bottom.join(' L')} Z`;
  };

  return (
    <div
      class="wv-clip"
      classList={{ 'is-selected': props.selected }}
      style={{
        left: `${props.clip.start * props.pxPerTick}px`,
        width: `${Math.max(6, props.clip.length * props.pxPerTick - 2)}px`,
        background: alpha(props.colour === 'var(--acc)' ? '#4f8ef7' : props.colour, 0.18),
        'box-shadow': `inset 0 0 0 1px ${alpha(props.colour === 'var(--acc)' ? '#4f8ef7' : props.colour, props.selected ? 1 : 0.55)}`,
      }}
      onPointerDown={(event) => props.onDown(event, 'move')}
      onDblClick={() => props.onOpen()}
      onContextMenu={(event) => {
        event.preventDefault();
        event.stopPropagation();
        props.onDelete();
      }}
      title={
        props.clip.kind === 'pattern'
          ? `${props.name} — drag to move, drag the right edge to resize, double-click to edit, right-click to delete`
          : `${props.name} — audio clip. Audio clips play into the master bus; per-lane routing is not implemented yet.`
      }
    >
      <span class="wv-clip__label">{props.name}</span>
      <Show when={props.clip.kind === 'audio' && props.waveform}>
        <svg class="wv-clip__wave" viewBox="0 0 100 100" preserveAspectRatio="none">
          <path d={path()} fill="currentColor" />
        </svg>
      </Show>
      <Show when={props.clip.kind === 'pattern'}>
        <div class="wv-clip__art">
          <For each={props.marks()}>
            {(mark) => (
              <span
                class="wv-clip__mark"
                style={{
                  left: `${mark.left * 100}%`,
                  width: `${mark.width * 100}%`,
                  bottom: `${mark.bottom * 84}%`,
                  background: alpha(mark.colour, 0.85),
                }}
              />
            )}
          </For>
        </div>
      </Show>
      <span class="wv-clip__grip" onPointerDown={(event) => props.onDown(event, 'resize')} />
    </div>
  );
}
