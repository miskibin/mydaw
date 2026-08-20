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
 *
 * The miniature is **one pattern wide on the timeline**, tiled if the clip is
 * longer and cropped if it is shorter. Stretching the marks to the clip's box
 * would make a shortened clip look sped up. Audio clips do the same for the
 * waveform: it is drawn at the file's own duration, so trimming crops the
 * wave instead of squeezing it.
 *
 * ## Clicking a clip picks it up
 *
 * FL's rule, and the one thing every arrangement gesture is built on: the clip
 * you last touched is the clip the brush holds. Click the `A` already on the
 * timeline and the next click on empty lane paints another `A` -- no trip to
 * the picker, no hunting for which of eleven patterns this one was. A pattern
 * clip also makes its pattern the active one, so the rack, the picker and the
 * brush cannot disagree about what "the current pattern" means.
 *
 * Shift (or ctrl) while dragging a clip **copies** it instead of moving it.
 *
 * ## The viewport, and the wheel
 *
 * Shift used to zoom, which left the arrangement with no horizontal wheel
 * scroll at all -- the one gesture every OS already spends on sideways travel
 * -- so getting back to bar 1 meant finding a scrollbar. The wheel now reads
 * the way it does everywhere else:
 *
 * | gesture | what it does |
 * | --- | --- |
 * | wheel | scroll the lanes vertically |
 * | shift + wheel, or a sideways wheel | scroll the timeline |
 * | ctrl / alt + wheel | zoom **about the pointer**, so the bar under the cursor stays put |
 * | middle-drag | pan both ways |
 * | `Home` / `End` | the start of the song, and the end of the arrangement |
 * | `Delete` / `Escape` | remove the selected clip, and drop the selection |
 *
 * The ruler is a scrub: press and drag moves the playhead, snapped to the
 * playlist's own snap (alt for free positioning). The playhead is drawn twice,
 * as a line under the lane headers and as a marker in the ruler above them,
 * and it scrolls the view along with it while the transport runs -- until you
 * scroll yourself, which parks the follow for a moment so a view you pulled
 * sideways on purpose is not yanked back.
 */

import { For, Index, Show, createEffect, createSignal, onCleanup, onMount } from 'solid-js';

import type { Backend } from '../api/backend';
import type {
  ClipDto,
  PatternDto,
  PlaylistTrackDto,
  SequencerState,
  WaveformDto,
} from '../api/types';
import {
  TICKS_PER_BAR,
  TICKS_PER_STEP,
  audioWaveLayout,
  clipArtLayout,
  samplesToTicks,
  snapTicks,
  ticksToSamples,
} from '../lib/pattern';
import { allowDrop, droppedAudioFile, droppedAudioPath, importDroppedFile, parseDrop } from '../lib/dnd';
import {
  clampScroll,
  foldToLoop,
  followScroll,
  wheelIntent,
  wheelPixels,
  zoomAnchoredScroll,
} from '../lib/timeline';
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
  type Brush,
  type PlaylistTool,
} from '../shell/view';
import { trackColor, wash } from '../theme/tokens';
import { onFrame } from '../ui/frameLoop';
import { IconEraseTool, IconPaintTool, IconRewind, IconSelectTool, IconX } from '../ui/icons';

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
/** What one notch of a zooming wheel, or one press of the zoom buttons, is worth. */
const ZOOM_STEP = 1.15;
/** How long a scroll of your own parks the playhead-follow. */
const FOLLOW_HOLD_MS = 2_500;

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
 * A pattern's content as marks, in **one pattern's** coordinates (0..1).
 *
 * The clip then draws these at the pattern's timeline width, so shortening the
 * clip crops and lengthening it loops — the marks never squeeze or stretch.
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
  /** The playhead's marker in the ruler, above the sticky lane headers. */
  let cursorEl: HTMLDivElement | undefined;
  let scrollEl: HTMLDivElement | undefined;

  const pxPerTick = () => (BAR_PX * view.playlistZoom()) / TICKS_PER_BAR;
  const playlist = () => app.playlist();
  const bars = () => {
    const end = playlist()?.endTicks ?? 0;
    return Math.max(MIN_BARS, Math.ceil(end / TICKS_PER_BAR) + TAIL_BARS);
  };
  const laneWidth = () => bars() * BAR_PX * view.playlistZoom();
  /** The widest the scroller can be pushed, headers included. */
  const maxScroll = () =>
    Math.max(0, HEAD_W + laneWidth() - (scrollEl?.clientWidth ?? 0));

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
   * Which lane the pointer is over, by Y against each lane's box.
   *
   * `elementsFromPoint` would hit the clip being dragged, and dividing by a
   * single lane height is wrong now that lanes resize independently.
   */
  function laneUnder(clientY: number, lanes: readonly { index: number; top: number; bottom: number }[]): number | null {
    for (const lane of lanes) {
      if (clientY >= lane.top && clientY < lane.bottom) return lane.index;
    }
    return null;
  }

  // --- the viewport --------------------------------------------------------

  /** The last sideways position this view put there itself. */
  let knownScroll = 0;
  /** Wall clock after which the follow may move the view again. */
  let heldUntil = 0;

  /**
   * Park the follow when *you* scrolled sideways.
   *
   * Compared against what the follow last wrote rather than acting on every
   * `scroll` event, because scrolling the lanes **vertically** fires one too --
   * and a follow that switched itself off every time you looked at a lane
   * further down would be off for good.
   */
  function onScroll(): void {
    if (!scrollEl) return;
    const left = scrollEl.scrollLeft;
    if (Math.abs(left - knownScroll) <= 1) return;
    knownScroll = left;
    heldUntil = performance.now() + FOLLOW_HOLD_MS;
  }

  /** Scroll to `left` without the follow reading it back as a hand on the wheel. */
  function scrollTo(left: number): void {
    if (!scrollEl) return;
    scrollEl.scrollLeft = clampScroll(left, maxScroll());
    knownScroll = scrollEl.scrollLeft;
  }

  /**
   * Zoom about a point, `clientX` in the window or the middle of the timeline.
   *
   * Zooming and leaving `scrollLeft` alone -- what the buttons used to do --
   * zooms about bar 1, which throws whatever you were looking at off the side
   * of the screen and turns every zoom into a hunt for your place again.
   */
  function zoomBy(factor: number, clientX?: number): void {
    const before = pxPerTick();
    const next = clampZoom(view.playlistZoom() * factor);
    if (next === view.playlistZoom()) return;
    const box = scrollEl?.getBoundingClientRect();
    const pointerOffset =
      clientX !== undefined && box
        ? clientX - box.left
        : HEAD_W + Math.max(0, ((box?.width ?? 0) - HEAD_W) / 2);
    const scrollLeft = scrollEl?.scrollLeft ?? 0;
    view.setPlaylistZoom(next);
    app.say('Playlist · Zoom', `${Math.round(next * 100)}%`);
    if (!scrollEl) return;
    const nextPxPerTick = (BAR_PX * next) / TICKS_PER_BAR;
    scrollTo(
      zoomAnchoredScroll({
        scrollLeft,
        pointerOffset,
        headerPx: HEAD_W,
        pxPerTick: before,
        nextPxPerTick,
        // From the numbers rather than from `scrollWidth`, which is only right
        // once the browser has laid the wider lanes out.
        maxScroll: Math.max(0, HEAD_W + bars() * BAR_PX * next - scrollEl.clientWidth),
      }),
    );
  }

  /** The playhead to bar 1, and the view with it. */
  function toStart(): void {
    app.seek(0);
    scrollTo(0);
    heldUntil = 0;
    app.say('Transport · Seek', 'bar 1');
  }

  function seekTicks(ticks: number): void {
    app.seek(ticksToSamples(Math.max(0, ticks), app.playhead.getSampleRate(), app.bpm()));
    app.say('Transport · Seek', `bar ${(Math.max(0, ticks) / TICKS_PER_BAR + 1).toFixed(2)}`);
  }

  /**
   * Middle-drag pans, the way it does in every DAW and every map.
   *
   * On the scroller rather than on the lanes, so panning works over the ruler
   * and the headers too -- and past a lane's right edge, which is where you end
   * up when the thing you were dragging towards is off screen.
   */
  function onPanPointerDown(event: PointerEvent): void {
    if (event.button !== 1 || !scrollEl) return;
    const el = scrollEl;
    event.preventDefault();
    const startX = event.clientX;
    const startY = event.clientY;
    const fromLeft = el.scrollLeft;
    const fromTop = el.scrollTop;
    const onMove = (move: PointerEvent) => {
      el.scrollLeft = clampScroll(fromLeft - (move.clientX - startX), maxScroll());
      el.scrollTop = Math.max(0, fromTop - (move.clientY - startY));
    };
    const onUp = () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      window.removeEventListener('pointercancel', onUp);
      document.body.style.removeProperty('cursor');
    };
    document.body.style.setProperty('cursor', 'grabbing');
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
    window.addEventListener('pointercancel', onUp);
  }

  // Registered by hand with `{ passive: false }`: a JSX `onWheel` may end up
  // passive, and a passive `preventDefault()` is a silent no-op -- see `drag.ts`.
  onMount(() => {
    const el = scrollEl;
    if (!el) return;
    const onWheel = (event: WheelEvent) => {
      const intent = wheelIntent(event);
      // Plain wheel is the lanes scrolling vertically, which the browser
      // already does better than we would.
      if (intent === 'scrollY') return;
      event.preventDefault();
      if (intent === 'zoom') {
        const delta = event.deltaY !== 0 ? event.deltaY : event.deltaX;
        if (delta === 0) return;
        zoomBy(delta < 0 ? ZOOM_STEP : 1 / ZOOM_STEP, event.clientX);
        return;
      }
      el.scrollLeft = clampScroll(el.scrollLeft + wheelPixels(event), maxScroll());
    };
    el.addEventListener('wheel', onWheel, { passive: false });
    el.addEventListener('scroll', onScroll, { passive: true });
    onCleanup(() => {
      el.removeEventListener('wheel', onWheel);
      el.removeEventListener('scroll', onScroll);
    });
  });

  // The playhead is written to the DOM on the frame loop: it moves every frame
  // and must not go through a signal. It is only meaningful in SONG mode --
  // in PAT mode the transport is looping a pattern, not running the timeline.
  onFrame(() => {
    const song = app.playMode() === 'song';
    if (playheadEl) playheadEl.style.display = song ? '' : 'none';
    if (cursorEl) cursorEl.style.display = song ? '' : 'none';
    if (!song) return;
    // The engine loops the arrangement by repeating it, so the position it
    // reports keeps climbing past the end of the song; the playhead has to be
    // folded back into the loop or it walks off the timeline.
    const ticks = foldToLoop(
      samplesToTicks(app.playhead.positionSamples(), app.playhead.getSampleRate(), app.bpm()),
      app.looping() ? (playlist()?.endTicks ?? 0) : 0,
    );
    const x = ticks * pxPerTick();
    if (playheadEl) playheadEl.style.transform = `translateX(${x}px)`;
    if (cursorEl) cursorEl.style.transform = `translateX(${x}px)`;

    if (!scrollEl || !app.playing() || !view.playlistFollow()) return;
    if (performance.now() < heldUntil) return;
    const next = followScroll({
      playheadPx: x,
      scrollLeft: scrollEl.scrollLeft,
      viewportPx: scrollEl.clientWidth,
      headerPx: HEAD_W,
      maxScroll: maxScroll(),
    });
    if (next === null) return;
    scrollTo(next);
  });

  // --- the keyboard --------------------------------------------------------

  onMount(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      if (target && (target.tagName === 'INPUT' || target.isContentEditable)) return;
      if (event.ctrlKey || event.metaKey || event.altKey) return;
      switch (event.key) {
        case 'Home':
          event.preventDefault();
          toStart();
          break;
        case 'End': {
          event.preventDefault();
          const end = playlist()?.endTicks ?? 0;
          seekTicks(end);
          scrollTo(end * pxPerTick() - 200);
          break;
        }
        case 'Delete':
        case 'Backspace': {
          const clip = playlist()?.clips.find((entry) => entry.id === view.selectedClip());
          if (!clip) return;
          event.preventDefault();
          removeClip(clip);
          view.setSelectedClip(null);
          break;
        }
        case 'Escape':
          view.setSelectedClip(null);
          break;
        default:
          break;
      }
    };
    window.addEventListener('keydown', onKeyDown);
    onCleanup(() => window.removeEventListener('keydown', onKeyDown));
  });

  /**
   * Move the playhead from the ruler, and keep moving it with the pointer.
   *
   * Snapped to the playlist's snap, which is what makes bar 1 reachable: with
   * the ruler reading raw pixels, "the start" was a single pixel somewhere
   * under the lane headers, and every click near it landed on bar 1.03.
   */
  function onRulerPointerDown(event: PointerEvent): void {
    if (event.button !== 0) return;
    const track = event.currentTarget as HTMLElement;
    event.preventDefault();
    let last = -1;
    const seekAt = (clientX: number, free: boolean) => {
      // Measured per move, not once: the ruler scrolls under the pointer when
      // the wheel or the follow moves the view mid-drag.
      const rect = track.getBoundingClientRect();
      const raw = Math.max(0, (clientX - rect.left) / pxPerTick());
      const ticks = free ? Math.round(raw) : snapTicks(raw, view.playlistSnap());
      if (ticks === last) return;
      last = ticks;
      seekTicks(ticks);
    };
    seekAt(event.clientX, event.altKey);
    const onMove = (move: PointerEvent) => seekAt(move.clientX, move.altKey);
    const onUp = () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      window.removeEventListener('pointercancel', onUp);
      document.body.style.removeProperty('cursor');
      document.body.style.removeProperty('user-select');
    };
    document.body.style.setProperty('cursor', 'ew-resize');
    document.body.style.setProperty('user-select', 'none');
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
    window.addEventListener('pointercancel', onUp);
  }

  // --- placing -------------------------------------------------------------

  /** Where in the song a pointer at `clientX` is, snapped. */
  function tickAt(clientX: number, lane: HTMLElement): number {
    const rect = lane.getBoundingClientRect();
    const raw = (clientX - rect.left) / pxPerTick();
    return snapTicks(Math.max(0, raw), view.playlistSnap(), true);
  }

  /**
   * What painting places: the brush, or the pattern the rack is editing.
   *
   * There is always a current pattern, so "no brush" was never a state worth
   * refusing a click over -- it only ever meant *the shell* had not been told
   * which one, and the engine had. Falling back to the active pattern is also
   * what makes the picker optional: click a clip, paint another one.
   */
  const brushOrActive = (): Brush => {
    const picked = view.brush();
    if (picked) return picked;
    const active = app.sequencer()?.patternSet.activeId;
    return active === undefined ? null : { kind: 'pattern', id: active };
  };

  function place(lane: number, start: number, brush = brushOrActive()): void {
    if (!brush) {
      app.say('Playlist', 'no brush — pick a pattern or a file in the Picker');
      return;
    }
    app.startPlaylistGesture(brush.kind === 'pattern' ? 'Paint pattern clip' : 'Paint audio clip');
    const patternTicks =
      brush.kind === 'pattern'
        ? app.sequencer()?.patternSet.patterns.find((pattern) => pattern.id === brush.id)?.lengthTicks
        : undefined;
    void app
      .sequencerCommand((backend) =>
        brush.kind === 'pattern'
          ? backend.addPatternClip(brush.id, lane, start, patternTicks)
          : backend.addAudioClip(brush.id, lane, start),
      )
      .then(() => app.commitPlaylistGesture());
  }

  function onLanePointerDown(event: PointerEvent, lane: number): void {
    if (event.button !== 0) return;
    // Empty lane, select tool: the selection is a thing you can put down.
    if (view.playlistTool() !== 'paint') {
      view.setSelectedClip(null);
      return;
    }
    place(lane, tickAt(event.clientX, event.currentTarget as HTMLElement));
  }

  function onLaneDrop(event: DragEvent, lane: number): void {
    event.preventDefault();
    const start = tickAt(event.clientX, event.currentTarget as HTMLElement);
    const filePath = droppedAudioPath(event);
    if (filePath) {
      placeFile(lane, start, filePath);
      return;
    }
    // An OS drop arrives as bytes, not a path; import them, then place the
    // written file exactly as a library `file::` drop would be.
    const file = droppedAudioFile(event);
    if (file) {
      void importDroppedFile(file, (name, bytes) => app.backend.importDroppedAudio(name, bytes))
        .then((path) => placeFile(lane, start, path))
        .catch(app.report);
      return;
    }
    const payload = parseDrop(event.dataTransfer?.getData('text/plain') ?? '');
    if (!payload) return;
    if (payload.kind === 'inst' || payload.kind === 'file') {
      if (payload.kind === 'inst') {
        app.report(new Error('An instrument goes on the rack, not a playlist lane'));
      }
      return;
    }
    const id = Number(payload.ref);
    if (!Number.isFinite(id)) return;
    if (payload.kind === 'pattern') place(lane, start, { kind: 'pattern', id });
    else if (payload.kind === 'audio') place(lane, start, { kind: 'audio', id });
  }

  function placeFile(lane: number, start: number, path: string): void {
    app.startPlaylistGesture('Paint audio clip');
    void app.backend
      .loadAudioSource(path)
      .then(async (source) => {
        await app.refreshAudioSources();
        view.setBrush({ kind: 'audio', id: source.id });
        return app.sequencerCommand((backend) => backend.addAudioClip(source.id, lane, start));
      })
      .then(() => app.commitPlaylistGesture())
      .catch(app.report);
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
    pickUp(clip);
    // FL's clone: hold shift (or ctrl) and the drag leaves the original where
    // it was and drops a copy where you let go.
    const copying = mode === 'move' && (event.shiftKey || event.ctrlKey);
    const startX = event.clientX;
    const startY = event.clientY;
    const originStart = clip.start;
    const originLength = clip.length;
    const originLane = clip.track;
    let moved = false;
    let lastStart = originStart;
    let lastLane = originLane;
    let lastLength = originLength;
    const clipEl = (event.currentTarget as HTMLElement).closest<HTMLElement>('.wv-clip');
    const originLaneEl = clipEl?.closest<HTMLElement>('.wv-pl__lane');
    const originTop = originLaneEl?.getBoundingClientRect().top ?? 0;
    const laneBoxes = Array.from(scrollEl?.querySelectorAll<HTMLElement>('.wv-pl__lane') ?? []).map(
      (lane) => {
        const box = lane.getBoundingClientRect();
        return { index: Number(lane.dataset.lane), top: box.top, bottom: box.bottom };
      },
    );

    app.startPlaylistGesture(
      mode === 'resize' ? 'Resize clip' : copying ? 'Copy clip' : 'Move clip',
    );

    const paint = (): void => {
      if (!clipEl) return;
      clipEl.style.pointerEvents = 'none';
      clipEl.style.zIndex = '8';
      if (mode === 'resize') {
        clipEl.style.width = `${Math.max(4, lastLength * pxPerTick() - 2)}px`;
        return;
      }
      clipEl.style.left = `${lastStart * pxPerTick()}px`;
      const target = laneBoxes.find((lane) => lane.index === lastLane);
      clipEl.style.transform = target ? `translateY(${target.top - originTop}px)` : '';
    };

    let announced = '';
    const announce = (value: string): void => {
      if (value === announced) return;
      announced = value;
      app.say(clipName(clip), value);
    };

    const onMove = (moveEvent: PointerEvent) => {
      const dx = (moveEvent.clientX - startX) / pxPerTick();
      if (!moved && Math.abs(moveEvent.clientX - startX) < 3 && Math.abs(moveEvent.clientY - startY) < 3) {
        return;
      }
      moved = true;
      if (mode === 'resize') {
        const snap = view.playlistSnap() || 1;
        lastLength = Math.max(snap, snapTicks(originLength + dx, view.playlistSnap()));
        announce(`${(lastLength / TICKS_PER_BAR).toFixed(2)} bars`);
        paint();
        return;
      }
      lastLane = laneUnder(moveEvent.clientY, laneBoxes) ?? lastLane;
      lastStart = snapTicks(Math.max(0, originStart + dx), view.playlistSnap());
      announce(
        `bar ${(lastStart / TICKS_PER_BAR + 1).toFixed(2)} · ${playlist()?.tracks[lastLane]?.name ?? ''}`,
      );
      paint();
    };

    const onUp = () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      if (clipEl) {
        clipEl.style.pointerEvents = '';
        if (!moved || lastLane === originLane) clipEl.style.transform = '';
        if (!moved) clipEl.style.zIndex = '';
      }
      if (!moved) {
        app.commitPlaylistGesture();
        activate(clip);
        return;
      }
      if (copying) {
        // The original never moved, so put its box back: the drag painted it
        // to the drop point and only the *engine's* answer redraws a clip.
        if (clipEl) {
          clipEl.style.left = `${originStart * pxPerTick()}px`;
          clipEl.style.transform = '';
          clipEl.style.zIndex = '';
        }
        void app
          .sequencerCommand((backend) => cloneClip(backend, clip, lastLane, lastStart))
          .then(() => app.commitPlaylistGesture());
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

  /**
   * Load the brush from a clip, the way clicking a clip does in FL.
   *
   * Local and instant: no round trip, because this runs on *pointer down* and
   * a state echo landing mid-gesture would rebuild the very element the drag is
   * holding. What the engine has to be told -- which pattern is now the current
   * one -- waits for {@link activate}, on a press that turned out to be a click.
   */
  function pickUp(clip: ClipDto): void {
    const brush: Brush =
      clip.kind === 'pattern' && clip.patternId !== undefined
        ? { kind: 'pattern', id: clip.patternId }
        : clip.kind === 'audio' && clip.sourceId !== undefined
          ? { kind: 'audio', id: clip.sourceId }
          : null;
    if (!brush) return;
    const already = view.brush();
    view.setBrush(brush);
    if (already?.kind !== brush.kind || already.id !== brush.id) {
      app.say('Playlist · Brush', clipName(clip));
    }
  }

  /**
   * A click on a pattern clip makes its pattern the current one.
   *
   * So the rack, the picker and the brush share one idea of "current" -- click
   * the clip, then F6, and you are editing what you clicked.
   */
  function activate(clip: ClipDto): void {
    if (clip.kind !== 'pattern' || clip.patternId === undefined) return;
    if (app.sequencer()?.patternSet.activeId === clip.patternId) return;
    void app.sequencerCommand((backend) => backend.setActivePattern(clip.patternId!));
  }

  /** The same clip again at `lane`/`start`, length, offset and trim kept. */
  function cloneClip(
    backend: Backend,
    clip: ClipDto,
    lane: number,
    start: number,
  ): Promise<SequencerState> {
    if (clip.kind === 'pattern') {
      return backend.addPatternClip(clip.patternId!, lane, start, clip.length);
    }
    return backend.addAudioClip(
      clip.sourceId!,
      lane,
      start,
      clip.length,
      clip.offsetFrames ?? 0,
      clip.gainDb ?? 0,
    );
  }

  /**
   * Open the sample window for an audio clip, from the playlist.
   *
   * The channel is looked up twice on purpose: `add_audio_clip` creates the
   * Audio Clip channel but answers with the *sequencer* state, so a clip
   * dropped a moment ago is on a channel this shell's mixer copy has never
   * heard of. One refetch turns "nothing happens when I double-click" into the
   * window it should have opened.
   */
  async function openSample(sourceId: number): Promise<void> {
    const find = () =>
      app.tracks().find((entry) => entry.engine === 'clip' && entry.audioSourceId === sourceId);
    let channel = find();
    if (!channel) {
      await app.mixerCommand((backend) => backend.getTracks());
      channel = find();
    }
    if (!channel) {
      app.say('Playlist', 'that file has no channel yet');
      return;
    }
    const index = channel.index;
    await app.mixerCommand((backend) => backend.setSelectedTrack(index));
    view.openSampleEditor(index);
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
    const brush = brushOrActive();
    if (!brush) return 'No brush — pick one on the right';
    if (brush.kind === 'pattern') {
      return `Brush · ${app.patterns().find((p) => p.id === brush.id)?.name ?? '?'}`;
    }
    return `Brush · ${app.audioSources().find((s) => s.id === brush.id)?.name ?? '?'}`;
  };

  const brushColour = () => {
    const brush = brushOrActive();
    if (!brush) return 'var(--dim)';
    if (brush.kind === 'pattern') {
      const pattern = app.patterns().find((p) => p.id === brush.id);
      return pattern?.color ?? trackColor(brush.id);
    }
    return trackColor(brush.id);
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
          class="wv-tool"
          title="Back to the start (Home)"
          onClick={toStart}
        >
          <IconRewind />
        </button>
        <button
          type="button"
          class="wv-chip wv-chip--sunk"
          title="What clip edges and the ruler snap to — hold alt on the ruler for none"
          onClick={() => view.setPlaylistSnap(nextSnap(PLAYLIST_SNAPS, view.playlistSnap()))}
        >
          <span class="wv-chip__tag">SNAP</span>
          {snapLabel(PLAYLIST_SNAPS, view.playlistSnap())}
        </button>
        <button
          type="button"
          class="wv-chip wv-chip--sunk"
          classList={{ 'is-active': view.playlistFollow() }}
          title="Scroll the arrangement along with the playhead while it plays"
          onClick={() => {
            const on = !view.playlistFollow();
            view.setPlaylistFollow(on);
            app.say('Playlist · Follow', on ? 'on' : 'off');
          }}
        >
          <span class="wv-chip__tag">FOLLOW</span>
          {view.playlistFollow() ? 'on' : 'off'}
        </button>
        <div class="wv-zoom">
          <button
            type="button"
            class="wv-zoom__btn"
            title="Zoom out (ctrl-scroll over the lanes)"
            onClick={() => zoomBy(1 / 1.25)}
          >
            −
          </button>
          <button
            type="button"
            class="wv-zoom__btn"
            title="Zoom in (ctrl-scroll over the lanes)"
            onClick={() => zoomBy(1.25)}
          >
            +
          </button>
        </div>
        <div class="wv-spacer" />
        <span class="wv-brush" style={{ background: wash(brushColour(), 0.14), color: brushColour() }}>
          <span class="wv-swatch" style={{ background: brushColour() }} />
          {brushLabel()}
        </span>
      </div>

      <div
        class="wv-pl"
        ref={scrollEl}
        onPointerDown={onPanPointerDown}
        onAuxClick={(event) => event.button === 1 && event.preventDefault()}
      >
        <div class="wv-pl__inner" style={{ width: `${HEAD_W + laneWidth()}px` }}>
          <div class="wv-pl__ruler">
            <button
              type="button"
              class="wv-pl__corner"
              style={{ width: `${HEAD_W}px` }}
              title="Back to the start (Home)"
              onClick={toStart}
            >
              <IconRewind />
              {playlist()?.tracks.length ?? 0} lanes · {playlist()?.clips.length ?? 0} clips
            </button>
            <div
              class="wv-pl__ruler-track"
              style={{ width: `${laneWidth()}px` }}
              title="Drag to move the playhead — snapped, hold alt for none"
              onPointerDown={onRulerPointerDown}
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
              <div class="wv-pl__cursor" ref={cursorEl} />
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
                  onDragOver={(event) => allowDrop(event)}
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
                            : trackColor(clip.sourceId ?? 0)
                        }
                        waveform={clip.kind === 'audio' ? waveformFor(clip.sourceId!) : undefined}
                        waveTicks={
                          clip.kind === 'audio'
                            ? (app.audioSources().find((source) => source.id === clip.sourceId)
                                ?.durationTicks ?? 0)
                            : 0
                        }
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
                        patternTicks={
                          clip.kind === 'pattern'
                            ? (app.patterns().find((pattern) => pattern.id === clip.patternId)
                                ?.lengthTicks ?? 0)
                            : 0
                        }
                        selected={view.selectedClip() === clip.id}
                        onDown={(event, mode) => onClipPointerDown(event, clip, mode)}
                        onDelete={() => removeClip(clip)}
                        onOpen={() => {
                          if (clip.kind === 'audio' && clip.sourceId !== undefined) {
                            void openSample(clip.sourceId).catch(app.report);
                            return;
                          }
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
              Click a lane with the paint tool to place the current pattern — or drag one from the
              Picker. Afterwards, clicking any clip loads it into the brush, so the next click
              paints the same thing. Switch the transport to SONG to hear it.
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
  /** The file's duration on the timeline; 0 when this is not an audio clip. */
  waveTicks: number;
  /** The pattern's content as marks. Empty for an audio clip or a narrow one. */
  marks: () => ClipMark[];
  /** Ticks in one loop of the pattern; 0 when this is not a pattern clip. */
  patternTicks: number;
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

  const art = () => clipArtLayout(props.clip.length, props.patternTicks, props.pxPerTick);
  const wave = () =>
    audioWaveLayout(
      props.waveTicks > 0 ? props.waveTicks : props.clip.length,
      props.waveform?.frames ?? 0,
      props.clip.offsetFrames ?? 0,
      props.pxPerTick,
    );
  const cycles = () => Array.from({ length: art().cycles }, (_, cycle) => cycle);

  return (
    <div
      class="wv-clip"
      classList={{ 'is-selected': props.selected }}
      style={{
        left: `${props.clip.start * props.pxPerTick}px`,
        width: `${Math.max(6, props.clip.length * props.pxPerTick - 2)}px`,
        background: wash(props.colour, 0.18),
        'box-shadow': `inset 0 0 0 1px ${wash(props.colour, props.selected ? 1 : 0.55)}`,
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
          ? `${props.name} — click to load the brush, drag to move (shift-drag to copy), drag the right edge to resize, double-click to open it in the rack, right-click to delete`
          : `${props.name} — click to load the brush, drag to move (shift-drag to copy), double-click to open the sample window, right-click to delete. Audio clips play through their own channel's strip.`
      }
    >
      <span class="wv-clip__label">{props.name}</span>
      <Show when={props.clip.kind === 'audio' && props.waveform}>
        <svg
          class="wv-clip__wave"
          viewBox="0 0 100 100"
          preserveAspectRatio="none"
          style={{
            width: `${wave().wavePx}px`,
            left: `${-wave().offsetPx}px`,
          }}
        >
          <path d={path()} fill="currentColor" />
        </svg>
      </Show>
      <Show when={props.clip.kind === 'pattern' && props.patternTicks > 0}>
        <div class="wv-clip__art">
          <For each={cycles()}>
            {(cycle) => (
              <div
                class="wv-clip__cycle"
                style={{
                  left: `${cycle * art().cyclePx}px`,
                  width: `${art().cyclePx}px`,
                }}
              >
                <For each={props.marks()}>
                  {(mark) => (
                    <span
                      class="wv-clip__mark"
                      style={{
                        left: `${mark.left * 100}%`,
                        width: `${mark.width * 100}%`,
                        bottom: `${mark.bottom * 84}%`,
                        background: wash(mark.colour, 0.85),
                      }}
                    />
                  )}
                </For>
              </div>
            )}
          </For>
        </div>
      </Show>
      <span class="wv-clip__grip" onPointerDown={(event) => props.onDown(event, 'resize')} />
    </div>
  );
}
