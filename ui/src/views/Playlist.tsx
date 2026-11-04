/**
 * The arrangement: pattern clips and audio clips along a timeline.
 *
 * Geometry is one number -- {@link BAR_PX} pixels per bar at zoom 1 -- and
 * everything else is derived from the engine's own tick constants, so the
 * ruler, the clips and the playhead cannot disagree about where bar 5 is.
 *
 * ## What a lane is, and is not
 *
 * A playlist lane is **not** a mixer track. It has a name, a mute, a solo and
 * an ordering, and that is all the engine gives it. Volume, pan and record-arm
 * live on mixer strips — a pattern clip on this row plays through the channels
 * it contains, so there is no single fader for the lane. Mute and solo are
 * arrangement-scope: they drop the lane's clips at compile time the same way
 * mixer solo folds into an effective mute. A clip's *sound* is the channel's,
 * and its fader is in the mixer. The lane's **height**, though, is nobody's
 * but the view's and costs the engine nothing, so it is draggable: the grip
 * along the bottom edge of the header resizes that one lane, and a
 * double-click puts it back. Heights are kept by lane **id**, because an
 * index would slide onto the neighbour the moment a lane below was removed.
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
 * Shift while dragging a clip **copies** it instead of moving it. Ctrl while
 * the paint tool is up drops into select, the way FL's draw tool does.
 * Double-click opens the rack (Alt for the piano roll) and must not paint.
 *
 * ## Editing audio clips
 *
 * An audio clip is a window onto a recording, and every edit keeps the audio
 * where it is in time -- the arithmetic is in `lib/playlistEdit.ts`:
 *
 * - the **left edge** slip-trims: start and length move, the file offset
 *   advances by the same time, and the trim stops at the file's first frame;
 * - the **slice tool** (`C`) cuts the clip under the pointer on the grid (Alt
 *   for off it), and the clip menu cuts at the right-click or the playhead;
 * - the clip menu's **volume** is the clip's own trim, shown after its name
 *   and in the height of its waveform.
 *
 * Each is one undo entry. Pattern clips have no file offset, so trimming their
 * head or slicing them would change the music; they keep the right edge only.
 *
 * ## Selection
 *
 * The playlist keeps a set of clip ids, not a single highlight. The select
 * tool (or paint + Ctrl) rubber-bands a marquee; Shift adds. Ctrl+C / X / V
 * copy, cut and paste the set; Ctrl+B duplicates it to the right; Ctrl+D and
 * a click on empty lane drop it. Dragging a selected clip moves the group.
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
 * | `Delete` / `Escape` | remove the selected clips, and drop the selection |
 * | `Ctrl+A / C / X / V / B / D` | select all, copy, cut, paste, duplicate right, deselect |
 *
 * The lanes are the scroller and the add-lane row sits under them, the same
 * stack as the channel rack: both scrollbars land on the lanes box, directly
 * above the button, and leftover panel is below it rather than swallowing it.
 *
 * The ruler is a scrub: press and drag moves the playhead, snapped to the
 * playlist's own snap (alt for free positioning). The playhead is drawn twice,
 * as a line under the lane headers and as a marker in the ruler above them,
 * and it scrolls the view along with it while the transport runs -- until you
 * scroll yourself, which parks the follow for a moment so a view you pulled
 * sideways on purpose is not yanked back.
 */

import { For, Index, Show, createEffect, createSignal, on, onCleanup, onMount } from 'solid-js';

import type { Backend } from '../api/backend';
import type {
  AudioSourceDto,
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
import {
  CLIP_GAIN_MAX_DB,
  CLIP_GAIN_MIN_DB,
  clampClipGain,
  clampGroupMove,
  clipScore,
  clipsIntersecting,
  duplicateToRight,
  formatClipGain,
  gainPeaks,
  latestOnly,
  pasteAtPlayhead,
  splitAudioClip,
  splitPoint,
  trimClipStart,
  unionClipIds,
  type AudioClipWindow,
  type PlaylistClipScore,
} from '../lib/playlistEdit';
import { channelColor } from '../lib/channel';
import { overviewWaveformPath } from '../lib/sampleWaveform';
import { allowDrop, droppedAudioFile, droppedAudioPath, importDroppedFile, parseDrop } from '../lib/dnd';
import {
  clampScroll,
  foldToLoop,
  followScroll,
  wheelIntent,
  wheelPixels,
  zoomAnchoredScroll,
} from '../lib/timeline';
import { laneSilenced } from '../lib/playlistLane';
import { canSplitByChannel } from '../lib/pattern';
import { isRepeatClick } from '../lib/navigate';
import { useAppState } from '../state';
import { usePatternNav } from '../shell/patternNav';
import {
  DEFAULT_LANE_H,
  PLAYLIST_SNAPS,
  clampLaneHeight,
  clampZoom,
  laneHeightOf,
  useView,
  type Brush,
  type PlaylistTool,
} from '../shell/view';
import { trackColor, wash } from '../theme/tokens';
import { onFrame } from '../ui/frameLoop';
import { SnapSelect } from '../ui/SnapSelect';
import { NumberField } from '../ui/NumberField';
import {
  IconEraseTool,
  IconPaintTool,
  IconRewind,
  IconSelectTool,
  IconSliceTool,
  IconX,
} from '../ui/icons';

/** Pixels one bar covers at zoom 1. */
const BAR_PX = 48;
/** How wide a lane header is. */
const HEAD_W = 200;
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
  ['paint', 'Paint clips — click a lane to place the brush. Hold Ctrl to select.', IconPaintTool],
  ['select', 'Select clips — drag a box, Shift to add, then move or Shift-drag to copy', IconSelectTool],
  ['slice', 'Slice audio clips (C) — click a clip to cut it on the grid, Alt to cut off the grid', IconSliceTool],
  ['delete', 'Delete clips', IconEraseTool],
];

/** In-memory clip clipboard: survives switching views, not a restart. */
let playlistClipboard: PlaylistClipScore[] = [];

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
  const nav = usePatternNav();
  const [waveforms, setWaveforms] = createSignal<Record<number, WaveformDto>>({});
  const pending = new Set<number>();
  const [contents, setContents] = createSignal<Record<number, PatternDto>>({});
  const pendingPatterns = new Set<number>();
  const [marquee, setMarquee] = createSignal<{ x: number; y: number; w: number; h: number } | null>(
    null,
  );
  const [clipMenu, setClipMenu] = createSignal<{
    clip: ClipDto;
    x: number;
    y: number;
    /** The song tick under the pointer when the menu opened: "Split here". */
    tick: number;
  } | null>(null);
  /**
   * A left-edge trim in progress: the clip as it will be when the pointer is
   * released. Drawn through the normal clip props rather than by poking the
   * element, so the waveform shifts with the offset and a refused command
   * snaps back by itself.
   */
  const [trimming, setTrimming] = createSignal<({ id: number } & AudioClipWindow) | null>(null);
  /** Where the slice tool would cut the clip under the pointer. */
  const [cutHover, setCutHover] = createSignal<{ id: number; at: number } | null>(null);

  let playheadEl: HTMLDivElement | undefined;
  /** The playhead's marker in the ruler, above the sticky lane headers. */
  let cursorEl: HTMLDivElement | undefined;
  let scrollEl: HTMLDivElement | undefined;

  const pxPerTick = () => (BAR_PX * view.playlistZoom()) / TICKS_PER_BAR;
  const playlist = () => app.playlist();

  // A take that just landed is selected, so Delete, Ctrl+Z's neighbour and the
  // right-click Mute act on it with no hunting.
  createEffect(on(app.lastTake, (take) => {
    if (take) view.setSelectedClips([take.clipId]);
  }, { defer: true }));
  const bars = () => {
    const end = playlist()?.endTicks ?? 0;
    return Math.max(MIN_BARS, Math.ceil(end / TICKS_PER_BAR) + TAIL_BARS);
  };
  const laneWidth = () => bars() * BAR_PX * view.playlistZoom();
  /** The widest the scroller can be pushed, headers included. */
  const maxScroll = () =>
    Math.max(0, HEAD_W + laneWidth() - (scrollEl?.clientWidth ?? 0));
  const lanesFull = () =>
    (playlist()?.tracks.length ?? 0) >= (playlist()?.maxTracks ?? 16);

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

  createEffect(() => {
    const have = new Set((playlist()?.clips ?? []).map((clip) => clip.id));
    const current = view.selectedClips();
    const next = current.filter((id) => have.has(id));
    if (next.length === current.length) return;
    view.setSelectedClips(next);
  });

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
      if (view.view() !== 'playlist' || view.rollOpen()) return;
      const target = event.target as HTMLElement | null;
      if (event.defaultPrevented || target?.closest('[aria-label="Browser"], .wv-playlist-picker, .wv-plug, .wv-menu, .wv-modal')) return;
      if (
        target &&
        (target.tagName === 'INPUT' ||
          target.tagName === 'TEXTAREA' ||
          target.tagName === 'SELECT' ||
          target.isContentEditable)
      ) {
        return;
      }
      const ctrl = event.ctrlKey || event.metaKey;
      if (ctrl && !event.altKey && !event.shiftKey) {
        const key = event.key.toLowerCase();
        if (key === 'a') {
          event.preventDefault();
          selectAllClips();
          return;
        }
        if (key === 'c') {
          event.preventDefault();
          copySelection();
          return;
        }
        if (key === 'x') {
          event.preventDefault();
          void cutSelection();
          return;
        }
        if (key === 'v') {
          event.preventDefault();
          void pasteClipboard();
          return;
        }
        if (key === 'b') {
          event.preventDefault();
          void duplicateSelection();
          return;
        }
        if (key === 'd') {
          event.preventDefault();
          deselectClips();
          return;
        }
      }
      if (ctrl || event.altKey) return;
      if (!event.shiftKey && !view.typingKeyboard()) {
        const tool = {
          KeyP: 'paint',
          KeyB: 'paint',
          KeyE: 'select',
          KeyC: 'slice',
          KeyD: 'delete',
        } as const;
        const next = tool[event.code as keyof typeof tool];
        if (next) {
          event.preventDefault();
          view.setPlaylistTool(next);
          return;
        }
      }
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
          event.preventDefault();
          void deleteSelection();
          break;
        case 'Backspace':
          event.preventDefault();
          view.togglePlaylistSnap();
          break;
        case 'Escape':
          deselectClips();
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

  /** Select tool, or paint with Ctrl held (FL: draw drops into select). */
  function isSelectGesture(event: MouseEvent): boolean {
    if (view.playlistTool() === 'select') return true;
    return view.playlistTool() === 'paint' && (event.ctrlKey || event.metaKey) && !event.shiftKey;
  }

  function laneBoxesNow(): { index: number; top: number; bottom: number }[] {
    return Array.from(scrollEl?.querySelectorAll<HTMLElement>('.wv-pl__lane') ?? []).map((lane) => {
      const box = lane.getBoundingClientRect();
      return { index: Number(lane.dataset.lane), top: box.top, bottom: box.bottom };
    });
  }

  function rawTickAt(clientX: number): number {
    const lane = scrollEl?.querySelector<HTMLElement>('.wv-pl__lane');
    if (!lane) return 0;
    const rect = lane.getBoundingClientRect();
    return Math.max(0, (clientX - rect.left) / pxPerTick());
  }

  function playheadTicks(): number {
    return foldToLoop(
      samplesToTicks(app.playhead.positionSamples(), app.playhead.getSampleRate(), app.bpm()),
      app.playMode() === 'song' && app.looping() ? (playlist()?.endTicks ?? 0) : 0,
    );
  }

  function selectedClipDtos(): ClipDto[] {
    const ids = new Set(view.selectedClips());
    return (playlist()?.clips ?? []).filter((clip) => ids.has(clip.id));
  }

  /**
   * Rubber-band on an empty lane. A click (no drag) deselects; Shift keeps
   * the current set and unions whatever the box covers.
   */
  function beginMarquee(event: MouseEvent, originLane: number): void {
    event.preventDefault();
    const originX = event.clientX;
    const originY = event.clientY;
    const originTick = rawTickAt(originX);
    const additive = event.shiftKey;
    let dragged = false;

    const paintBox = (clientX: number, clientY: number): void => {
      const x = Math.min(originX, clientX);
      const y = Math.min(originY, clientY);
      setMarquee({
        x,
        y,
        w: Math.abs(clientX - originX),
        h: Math.abs(clientY - originY),
      });
    };

    const onMove = (move: PointerEvent) => {
      if (!dragged && Math.abs(move.clientX - originX) < 4 && Math.abs(move.clientY - originY) < 4) {
        return;
      }
      dragged = true;
      paintBox(move.clientX, move.clientY);
    };

    const onUp = (up: PointerEvent) => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      window.removeEventListener('pointercancel', onUp);
      document.body.style.removeProperty('user-select');
      setMarquee(null);
      if (!dragged) {
        if (!additive) deselectClips();
        return;
      }
      const endLane = laneUnder(up.clientY, laneBoxesNow()) ?? originLane;
      const hits = clipsIntersecting(playlist()?.clips ?? [], {
        track0: originLane,
        track1: endLane,
        start: originTick,
        end: rawTickAt(up.clientX),
      });
      const next = additive ? unionClipIds(view.selectedClips(), hits) : hits;
      view.setSelectedClips(next);
      app.say('Playlist · Select', next.length === 0 ? 'none' : `${next.length}`);
    };

    document.body.style.setProperty('user-select', 'none');
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
    window.addEventListener('pointercancel', onUp);
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

  function onLanePointerDown(event: MouseEvent, lane: number): void {
    if (event.button !== 0) return;
    if (isRepeatClick(event.detail)) return;
    if (isSelectGesture(event)) {
      beginMarquee(event, lane);
      return;
    }
    if (view.playlistTool() !== 'paint') return;
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

  function onClipPointerDown(event: MouseEvent, clip: ClipDto, mode: 'move' | 'resize'): void {
    event.stopPropagation();
    if (event.button !== 0) return;
    // Before the repeat-click guard: two quick cuts in a row are two cuts.
    if (view.playlistTool() === 'slice') {
      sliceAtPointer(event, clip);
      return;
    }
    if (isRepeatClick(event.detail)) return;
    if (view.playlistTool() === 'delete') {
      removeClip(clip);
      return;
    }

    const shift = event.shiftKey;
    const current = view.selectedClips();
    if (shift && mode === 'move') {
      view.setSelectedClips(unionClipIds(current, [clip.id]));
    } else if (!current.includes(clip.id)) {
      view.setSelectedClips([clip.id]);
    }

    pickUp(clip);

    const groupIds = new Set(
      mode === 'resize'
        ? [clip.id]
        : view.selectedClips().includes(clip.id)
          ? view.selectedClips()
          : [clip.id],
    );
    const group = (playlist()?.clips ?? []).filter((entry) => groupIds.has(entry.id));
    const copying = mode === 'move' && event.shiftKey;
    const startX = event.clientX;
    const startY = event.clientY;
    const originStart = clip.start;
    const originLength = clip.length;
    const originLane = clip.track;
    let moved = false;
    let lastStart = originStart;
    let lastLane = originLane;
    let lastLength = originLength;
    let lastDelta = { dTrack: 0, dStart: 0 };
    const clipEl = (event.currentTarget as HTMLElement).closest<HTMLElement>('.wv-clip');
    const laneBoxes = laneBoxesNow();
    const ghosts = group.map((entry) => {
      const el = scrollEl?.querySelector<HTMLElement>(`[data-clip-id="${entry.id}"]`);
      const laneEl = el?.closest<HTMLElement>('.wv-pl__lane');
      return {
        clip: entry,
        el,
        originTop: laneEl?.getBoundingClientRect().top ?? 0,
        originLeft: entry.start,
        originTrack: entry.track,
      };
    });

    app.startPlaylistGesture(
      mode === 'resize'
        ? 'Resize clip'
        : copying
          ? group.length > 1
            ? 'Copy clips'
            : 'Copy clip'
          : group.length > 1
            ? 'Move clips'
            : 'Move clip',
    );

    const paint = (): void => {
      if (mode === 'resize') {
        if (!clipEl) return;
        clipEl.style.pointerEvents = 'none';
        clipEl.style.zIndex = '8';
        clipEl.style.width = `${Math.max(4, lastLength * pxPerTick() - 2)}px`;
        return;
      }
      for (const ghost of ghosts) {
        if (!ghost.el) continue;
        ghost.el.style.pointerEvents = 'none';
        ghost.el.style.zIndex = '8';
        ghost.el.style.left = `${(ghost.originLeft + lastDelta.dStart) * pxPerTick()}px`;
        const target = laneBoxes.find((lane) => lane.index === ghost.originTrack + lastDelta.dTrack);
        ghost.el.style.transform = target ? `translateY(${target.top - ghost.originTop}px)` : '';
      }
    };

    const resetGhosts = (keepTransform: boolean): void => {
      for (const ghost of ghosts) {
        if (!ghost.el) continue;
        ghost.el.style.pointerEvents = '';
        if (!keepTransform) {
          ghost.el.style.left = `${ghost.originLeft * pxPerTick()}px`;
          ghost.el.style.transform = '';
          ghost.el.style.zIndex = '';
        } else if (lastDelta.dTrack === 0) {
          ghost.el.style.transform = '';
        }
      }
    };

    let announced = '';
    const announce = (value: string): void => {
      if (value === announced) return;
      announced = value;
      app.say(group.length > 1 ? `${group.length} clips` : clipName(clip), value);
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
      lastDelta = clampGroupMove(
        group,
        lastLane - originLane,
        lastStart - originStart,
        playlist()?.tracks.length ?? 0,
      );
      lastStart = originStart + lastDelta.dStart;
      lastLane = originLane + lastDelta.dTrack;
      announce(
        `bar ${(lastStart / TICKS_PER_BAR + 1).toFixed(2)} · ${playlist()?.tracks[lastLane]?.name ?? ''}`,
      );
      paint();
    };

    const onUp = () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      if (!moved) {
        resetGhosts(false);
        // No resize happened. Clearing Solid's inline width collapses this
        // absolutely positioned clip, so the second click hits the lane.
        app.commitPlaylistGesture();
        activate(clip);
        return;
      }
      if (mode === 'resize') {
        if (clipEl) {
          clipEl.style.pointerEvents = '';
          clipEl.style.zIndex = '';
        }
        void app
          .sequencerCommand((backend) => backend.resizeClip(clip.id, lastLength))
          .then(() => app.commitPlaylistGesture());
        return;
      }
      if (copying) {
        resetGhosts(false);
        void placeScores(
          group.map((entry) => ({
            ...clipScore(entry),
            track: entry.track + lastDelta.dTrack,
            start: entry.start + lastDelta.dStart,
          })),
          group.length > 1 ? 'Copy clips' : 'Copy clip',
          false,
        ).then(() => app.commitPlaylistGesture());
        return;
      }
      resetGhosts(true);
      void app
        .sequencerCommand(async (backend) => {
          let state = await backend.getPattern();
          for (const entry of group) {
            state = await backend.moveClip(
              entry.id,
              entry.track + lastDelta.dTrack,
              entry.start + lastDelta.dStart,
            );
          }
          return state;
        })
        .then(() => app.commitPlaylistGesture());
    };

    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
  }

  // --- audio clip edits: trim the head, slice, volume ----------------------

  function sourceOf(clip: ClipDto): AudioSourceDto | undefined {
    return app.audioSources().find((source) => source.id === clip.sourceId);
  }

  function windowOf(clip: ClipDto): AudioClipWindow {
    return { start: clip.start, length: clip.length, offsetFrames: clip.offsetFrames ?? 0 };
  }

  /**
   * Drag an audio clip's left edge: FL's slip-trim.
   *
   * The start and the length move together so the right edge stays put, and
   * the offset into the file advances by the same time, so the audio stays
   * where it was on the timeline and only the window onto it changes. One undo
   * entry for the whole drag. Pattern clips have no offset to advance -- trimming
   * their head would shift the music -- so they have no left grip at all.
   */
  function onTrimPointerDown(event: MouseEvent, clip: ClipDto): void {
    event.stopPropagation();
    if (event.button !== 0) return;
    const tool = view.playlistTool();
    if (tool === 'slice') {
      sliceAtPointer(event, clip);
      return;
    }
    if (isRepeatClick(event.detail)) return;
    if (tool === 'delete') {
      removeClip(clip);
      return;
    }
    if (clip.kind !== 'audio') return;
    event.preventDefault();
    if (!view.selectedClips().includes(clip.id)) view.setSelectedClips([clip.id]);
    pickUp(clip);

    const origin = windowOf(clip);
    const source = sourceOf(clip);
    const file = { frames: source?.frames ?? 0, sampleRate: source?.sampleRate ?? 0 };
    const startX = event.clientX;
    let moved = false;
    let last = origin;
    let announced = '';
    app.startPlaylistGesture('Trim clip start');

    const onMove = (move: PointerEvent) => {
      if (!moved && Math.abs(move.clientX - startX) < 3) return;
      moved = true;
      const raw = origin.start + (move.clientX - startX) / pxPerTick();
      last = trimClipStart(origin, file, app.bpm(), raw, view.playlistSnap(), move.altKey);
      setTrimming({ id: clip.id, ...last });
      const seconds = file.sampleRate > 0 ? last.offsetFrames / file.sampleRate : 0;
      const value = `bar ${(last.start / TICKS_PER_BAR + 1).toFixed(2)} · ${seconds.toFixed(2)} s in`;
      if (value !== announced) {
        announced = value;
        app.say(`${clipName(clip)} · Trim start`, value);
      }
    };

    const onUp = () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      window.removeEventListener('pointercancel', onUp);
      document.body.style.removeProperty('cursor');
      document.body.style.removeProperty('user-select');
      const final = last;
      if (!moved || (final.start === origin.start && final.offsetFrames === origin.offsetFrames)) {
        setTrimming(null);
        app.commitPlaylistGesture();
        if (!moved) activate(clip);
        return;
      }
      void app
        .sequencerCommand(async (backend) => {
          // Shrink before moving right and move before growing left, so no
          // step in between asks the engine for a clip past the song's end.
          if (final.start > origin.start) {
            await backend.resizeClip(clip.id, final.length);
            await backend.moveClip(clip.id, clip.track, final.start);
          } else {
            await backend.moveClip(clip.id, clip.track, final.start);
            await backend.resizeClip(clip.id, final.length);
          }
          return backend.setClipOffset(clip.id, final.offsetFrames);
        })
        .then(() => {
          setTrimming(null);
          app.commitPlaylistGesture();
        });
    };

    document.body.style.setProperty('cursor', 'ew-resize');
    document.body.style.setProperty('user-select', 'none');
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
    window.addEventListener('pointercancel', onUp);
  }

  /** A slice in flight: a second click must not cut the clip it is replacing. */
  let slicing = false;

  /**
   * Cut an audio clip in two at song tick `at`, as one undo entry.
   *
   * The right half is added *before* the original is shortened, so an engine
   * that refuses the new clip (the clip limit, say) leaves the arrangement as
   * it was instead of with half a vocal missing. Gain carries over to both.
   */
  async function sliceClip(clip: ClipDto, at: number): Promise<void> {
    if (slicing || clip.kind !== 'audio' || clip.sourceId === undefined) return;
    const sourceId = clip.sourceId;
    const { left, right } = splitAudioClip(
      windowOf(clip),
      at,
      { sampleRate: sourceOf(clip)?.sampleRate ?? 0 },
      app.bpm(),
    );
    slicing = true;
    setCutHover(null);
    app.startPlaylistGesture('Slice clip');
    try {
      await app.sequencerCommand(async (backend) => {
        const before = new Set((playlist()?.clips ?? []).map((other) => other.id));
        const added = await backend.addAudioClip(
          sourceId,
          clip.track,
          right.start,
          right.length,
          right.offsetFrames,
          clip.gainDb ?? 0,
        );
        // A muted take stays muted on both sides of the cut.
        const fresh = added.playlist.clips.find((other) => !before.has(other.id));
        if (clip.muted && fresh) await backend.setClipMuted(fresh.id, true);
        return backend.resizeClip(clip.id, left.length);
      });
    } finally {
      app.commitPlaylistGesture();
      slicing = false;
    }
    app.say(`${clipName(clip)} · Slice`, `bar ${(at / TICKS_PER_BAR + 1).toFixed(2)}`);
  }

  /** Where the slice tool cuts `clip` for a pointer at `clientX`, or `null`. */
  function cutAt(clip: ClipDto, clientX: number, free: boolean): number | null {
    return splitPoint(clip, rawTickAt(clientX), view.playlistSnap(), free);
  }

  function sliceAtPointer(event: MouseEvent, clip: ClipDto): void {
    event.preventDefault();
    if (clip.kind !== 'audio') {
      app.say('Playlist · Slice', 'pattern clips always start at step 1 — slice cuts audio clips');
      return;
    }
    const at = cutAt(clip, event.clientX, event.altKey);
    if (at === null) {
      app.say(`${clipName(clip)} · Slice`, 'that grid line is the clip’s edge — Alt cuts off the grid');
      return;
    }
    void sliceClip(clip, at);
  }

  /** The slice tool's cut line follows the pointer over an audio clip. */
  function hoverCut(event: PointerEvent | null, clip: ClipDto): void {
    if (!event || view.playlistTool() !== 'slice' || clip.kind !== 'audio') {
      if (cutHover()) setCutHover(null);
      return;
    }
    const at = cutAt(clip, event.clientX, event.altKey);
    const current = cutHover();
    if (at === null) {
      if (current) setCutHover(null);
      return;
    }
    if (current?.id === clip.id && current.at === at) return;
    setCutHover({ id: clip.id, at });
  }

  /**
   * The playhead as a cut into `clip`, or `null` when it is not inside it.
   *
   * Only in SONG mode: in PAT mode the transport is counting through a
   * pattern, and its position is not a place on the arrangement.
   */
  function playheadCut(clip: ClipDto): number | null {
    if (app.playMode() !== 'song') return null;
    return splitPoint(clip, playheadTicks(), 0, true);
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
    if (clip.kind === 'audio' && clip.sourceId !== undefined) {
      void focusSampleChannel(clip.sourceId).catch(app.report);
      return;
    }
    if (clip.kind !== 'pattern' || clip.patternId === undefined) return;
    if (app.sequencer()?.patternSet.activeId !== clip.patternId) {
      void app.sequencerCommand((backend) => backend.setActivePattern(clip.patternId!));
    }
  }

  /** The same clip again at `lane`/`start`, length, offset and trim kept. */
  function cloneScore(backend: Backend, score: PlaylistClipScore): Promise<SequencerState> {
    const lanes = playlist()?.tracks.length ?? 1;
    const track = Math.min(Math.max(0, score.track), Math.max(0, lanes - 1));
    if (score.kind === 'pattern' && score.patternId !== undefined) {
      return backend.addPatternClip(score.patternId, track, score.start, score.length);
    }
    if (score.sourceId === undefined) return backend.getPattern();
    return backend.addAudioClip(
      score.sourceId,
      track,
      score.start,
      score.length,
      score.offsetFrames ?? 0,
      score.gainDb ?? 0,
    );
  }

  async function placeScores(
    scores: PlaylistClipScore[],
    label: string,
    startGesture = true,
  ): Promise<number[]> {
    if (scores.length === 0) return [];
    const before = new Set((playlist()?.clips ?? []).map((clip) => clip.id));
    if (startGesture) app.startPlaylistGesture(label);
    await app.sequencerCommand(async (backend) => {
      let state = await backend.getPattern();
      for (const score of scores) {
        state = await cloneScore(backend, score);
      }
      return state;
    });
    if (startGesture) app.commitPlaylistGesture();
    const created = (playlist()?.clips ?? [])
      .filter((clip) => !before.has(clip.id))
      .map((clip) => clip.id);
    if (created.length > 0) view.setSelectedClips(created);
    return created;
  }

  function deselectClips(): void {
    if (view.selectedClips().length === 0) return;
    view.setSelectedClips([]);
    app.say('Playlist · Select', 'none');
  }

  function selectAllClips(): void {
    const ids = (playlist()?.clips ?? []).map((clip) => clip.id);
    view.setSelectedClips(ids);
    app.say('Playlist · Select', ids.length === 0 ? 'none' : `${ids.length}`);
  }

  function copySelection(): void {
    const clips = selectedClipDtos();
    if (clips.length === 0) return;
    playlistClipboard = clips.map(clipScore);
    app.say('Playlist · Copy', `${clips.length} clip${clips.length > 1 ? 's' : ''}`);
  }

  async function removeSelected(label: string): Promise<number> {
    const clips = selectedClipDtos();
    if (clips.length === 0) return 0;
    app.startPlaylistGesture(label);
    await app.sequencerCommand(async (backend) => {
      let state = await backend.getPattern();
      for (const clip of clips) {
        state = await backend.removeClip(clip.id);
      }
      return state;
    });
    app.commitPlaylistGesture();
    view.setSelectedClips([]);
    return clips.length;
  }

  async function cutSelection(): Promise<void> {
    const clips = selectedClipDtos();
    if (clips.length === 0) return;
    playlistClipboard = clips.map(clipScore);
    const count = await removeSelected(clips.length > 1 ? 'Cut clips' : 'Cut clip');
    app.say('Playlist · Cut', `${count} clip${count > 1 ? 's' : ''}`);
  }

  async function pasteClipboard(): Promise<void> {
    if (playlistClipboard.length === 0) return;
    const placed = pasteAtPlayhead(playlistClipboard, playheadTicks());
    await placeScores(placed, placed.length > 1 ? 'Paste clips' : 'Paste clip');
    app.say('Playlist · Paste', `${placed.length} clip${placed.length > 1 ? 's' : ''}`);
  }

  async function duplicateSelection(): Promise<void> {
    const clips = selectedClipDtos();
    if (clips.length === 0) return;
    const copies = duplicateToRight(clips.map(clipScore));
    await placeScores(copies, copies.length > 1 ? 'Duplicate clips' : 'Duplicate clip');
    app.say('Playlist · Duplicate', `${copies.length} clip${copies.length > 1 ? 's' : ''}`);
  }

  async function deleteSelection(): Promise<void> {
    const count = await removeSelected(
      view.selectedClips().length > 1 ? 'Delete clips' : 'Delete clip',
    );
    if (count === 0) return;
    app.say('Playlist · Delete', `${count} clip${count > 1 ? 's' : ''}`);
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
    const channel = await focusSampleChannel(sourceId);
    if (channel !== null) view.openSampleEditor(channel);
  }

  /** Point the rack, mixer and inspector at the channel that owns this file. */
  async function focusSampleChannel(sourceId: number): Promise<number | null> {
    const find = () =>
      app.tracks().find((entry) => entry.engine === 'clip' && entry.audioSourceId === sourceId);
    let channel = find();
    if (!channel) {
      await app.mixerCommand((backend) => backend.getTracks());
      channel = find();
    }
    if (!channel) {
      app.say('Playlist', 'that file has no channel yet');
      return null;
    }
    const index = channel.index;
    view.setStrip({ kind: 'track', index });
    view.setSideOpen(true);
    view.setSideTab('inspector');
    await app.mixerCommand((backend) => backend.setSelectedTrack(index));
    return index;
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

  function clipCanSplit(clip: ClipDto): boolean {
    if (clip.kind !== 'pattern' || clip.patternId === undefined) return false;
    const content = patternContent(clip.patternId);
    if (content) return canSplitByChannel(content);
    return (app.patterns().find((pattern) => pattern.id === clip.patternId)?.voiceCount ?? 0) > 1;
  }

  function openClipMenu(event: MouseEvent, clip: ClipDto): void {
    event.preventDefault();
    event.stopPropagation();
    setClipMenu({
      clip,
      x: Math.max(8, Math.min(event.clientX, window.innerWidth - 240)),
      y: Math.max(8, Math.min(event.clientY, window.innerHeight - 220)),
      tick: rawTickAt(event.clientX),
    });
  }

  /** The menu's clip as the engine has it now, not as it was when it opened. */
  const liveMenuClip = (): ClipDto | undefined => {
    const id = clipMenu()?.clip.id;
    return (playlist()?.clips ?? []).find((clip) => clip.id === id);
  };
  const menuCut = (): number | null => {
    const menu = clipMenu();
    const clip = liveMenuClip();
    if (!menu || !clip) return null;
    return splitPoint(clip, menu.tick, view.playlistSnap());
  };
  const menuPlayheadCut = (): number | null => {
    const clip = liveMenuClip();
    return clip ? playheadCut(clip) : null;
  };

  // A menu for a clip that has gone -- an undo rebuilt it under a new id, or a
  // CLI edit removed it -- would act on nothing; close it instead.
  createEffect(() => {
    const menu = clipMenu();
    if (!menu) return;
    if (!(playlist()?.clips ?? []).some((clip) => clip.id === menu.clip.id)) setClipMenu(null);
  });

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
        <SnapSelect value={view.playlistSnap()} choices={PLAYLIST_SNAPS} onChange={view.setPlaylistSnap} label="Playlist snap" />
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

      <div class="wv-pl">
        <div
          class="wv-pl__scroll"
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
              <span class="wv-pl__corner-bar">BAR</span>
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
              <div
                class="wv-pl__row"
                classList={{ 'is-silenced': laneSilenced(playlist()?.tracks ?? [], index) }}
              >
                <div
                  class="wv-pl__head"
                  style={{ width: `${HEAD_W}px`, height: `${laneHeight(lane().id)}px` }}
                >
                  <div class="wv-pl__head-top">
                    <span class="wv-pl__num">{index + 1}</span>
                    <LaneName
                      name={lane().name}
                      onRename={(name) =>
                        void app.sequencerCommand((backend) =>
                          backend.renamePlaylistTrack(index, name),
                        )
                      }
                    />
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
                  </div>
                  <div class="wv-pl__head-btns">
                    <button
                      type="button"
                      class="wv-pill wv-pill--solo"
                      classList={{ 'is-on': lane().soloed }}
                      title={
                        lane().soloed
                          ? 'Unsolo this lane'
                          : 'Solo this lane — other lanes silent in SONG'
                      }
                      aria-pressed={lane().soloed}
                      onClick={() => {
                        app.startPlaylistGesture(`${lane().name} · Solo`);
                        void app
                          .sequencerCommand((backend) =>
                            backend.setPlaylistTrackSolo(index, !lane().soloed),
                          )
                          .then(() => app.commitPlaylistGesture());
                        app.say(`${lane().name} · Lane`, lane().soloed ? 'Active' : 'Soloed');
                      }}
                    >
                      S
                    </button>
                    <button
                      type="button"
                      class="wv-pill wv-pill--mute"
                      classList={{ 'is-on': lane().muted }}
                      title={lane().muted ? 'Unmute this lane' : 'Mute this lane'}
                      aria-pressed={lane().muted}
                      onClick={() => {
                        app.startPlaylistGesture(`${lane().name} · Mute`);
                        void app
                          .sequencerCommand((backend) =>
                            backend.setPlaylistTrackMute(index, !lane().muted),
                          )
                          .then(() => app.commitPlaylistGesture());
                        app.say(`${lane().name} · Lane`, lane().muted ? 'Active' : 'Muted');
                      }}
                    >
                      M
                    </button>
                  </div>
                  <LaneGrip lane={lane()} height={laneHeight(lane().id)} />
                </div>
                <div
                  class="wv-pl__lane"
                  data-lane={index}
                  classList={{
                    'is-painting': view.playlistTool() === 'paint',
                    'is-slicing': view.playlistTool() === 'slice',
                  }}
                  style={{
                    width: `${laneWidth()}px`,
                    height: `${laneHeight(lane().id)}px`,
                    'background-size': `${BAR_PX * view.playlistZoom() * 4}px 100%, ${BAR_PX * view.playlistZoom()}px 100%`,
                  }}
                  // MouseEvent.detail carries the click count; PointerEvent.detail is 0.
                  onMouseDown={(event) => onLanePointerDown(event, index)}
                  onPointerDown={(event) => {
                    if (event.pointerType !== 'mouse') {
                      event.preventDefault(); // suppress the compatibility mousedown
                      onLanePointerDown(event, index);
                    }
                  }}
                  onDragOver={(event) => allowDrop(event)}
                  onDrop={(event) => onLaneDrop(event, index)}
                  onContextMenu={(event) => event.preventDefault()}
                >
                  <For
                    each={(playlist()?.clips.filter((clip) => clip.track === index) ?? []).map(
                      (clip) => clip.id,
                    )}
                  >
                    {(id) => {
                      const clip = () => playlist()?.clips.find((entry) => entry.id === id);
                      return (
                        <Show when={clip()}>
                          {(current) => (
                            <Clip
                              clip={(() => {
                                const trim = trimming();
                                const live = current();
                                return trim?.id === live.id
                                  ? {
                                      ...live,
                                      start: trim.start,
                                      length: trim.length,
                                      offsetFrames: trim.offsetFrames,
                                    }
                                  : live;
                              })()}
                              cutAt={
                                view.playlistTool() === 'slice' && cutHover()?.id === current().id
                                  ? (cutHover()?.at ?? null)
                                  : null
                              }
                              onHover={(event) => hoverCut(event, current())}
                              pxPerTick={pxPerTick()}
                              name={clipName(current())}
                              colour={
                                current().kind === 'pattern'
                                  ? (app.patterns().find((p) => p.id === current().patternId)
                                      ?.color ?? trackColor(current().patternId ?? 0))
                                  : trackColor(current().sourceId ?? 0)
                              }
                              waveform={
                                current().kind === 'audio'
                                  ? waveformFor(current().sourceId!)
                                  : undefined
                              }
                              waveTicks={
                                current().kind === 'audio'
                                  ? (app.audioSources().find(
                                      (source) => source.id === current().sourceId,
                                    )?.durationTicks ?? 0)
                                  : 0
                              }
                              marks={() => {
                                const patternId = current().patternId;
                                if (current().kind !== 'pattern' || patternId === undefined) {
                                  return [];
                                }
                                if (current().length * pxPerTick() < MIN_ART_PX) return [];
                                const content = patternContent(patternId);
                                if (!content) return [];
                                return clipMarks(content, (channel) => {
                                  const track = app.tracks()[channel];
                                  return track ? channelColor(track) : trackColor(channel);
                                });
                              }}
                              patternTicks={
                                current().kind === 'pattern' && current().patternId !== undefined
                                  ? (app.patterns().find(
                                      (pattern) => pattern.id === current().patternId,
                                    )?.lengthTicks ?? 0)
                                  : 0
                              }
                              selected={view.selectedClips().includes(current().id)}
                              onDown={(event, mode) =>
                                mode === 'trim'
                                  ? onTrimPointerDown(event, current())
                                  : onClipPointerDown(event, current(), mode)
                              }
                              onMenu={(event) => openClipMenu(event, current())}
                              onOpen={(altKey) => {
                                // Two quick cuts are a double-click; with the
                                // slice tool up they mean "cut", not "open".
                                if (view.playlistTool() === 'slice') return;
                                const sourceId = current().sourceId;
                                if (current().kind === 'audio' && sourceId !== undefined) {
                                  void openSample(sourceId).catch(app.report);
                                  return;
                                }
                                const patternId = current().patternId;
                                if (current().kind !== 'pattern' || patternId === undefined) {
                                  return;
                                }
                                void nav.openPattern(patternId, {
                                  altKey,
                                  surface: 'clip',
                                });
                              }}
                            />
                          )}
                        </Show>
                      );
                    }}
                  </For>
                </div>
              </div>
            )}
          </Index>

          <div class="wv-pl__playhead" ref={playheadEl} style={{ left: `${HEAD_W}px` }} />
          </div>
        </div>

        <button
          type="button"
          class="wv-pl__add wv-can-disable"
          disabled={lanesFull()}
          title={
            lanesFull()
              ? `The engine allows ${playlist()?.maxTracks ?? 16} lanes, and they are all in use`
              : 'Add a lane'
          }
          onClick={() => void app.sequencerCommand((backend) => backend.addPlaylistTrack())}
        >
          {lanesFull()
            ? `the engine allows ${playlist()?.maxTracks ?? 16} lanes, and they are all in use`
            : '+ add a lane'}
        </button>

        <Show when={(playlist()?.clips.length ?? 0) === 0}>
          <div class="wv-pl__hint">
            <strong>The arrangement is empty.</strong>
            <span class="wv-empty__hint">
              Select a pattern on the left, then click a lane to place it.
                  Press Space in the playlist to hear the arrangement.
            </span>
          </div>
        </Show>
        <Show when={marquee()}>
          {(box) => (
            <div
              class="wv-pl__marquee"
              style={{
                left: `${box().x}px`,
                top: `${box().y}px`,
                width: `${Math.max(1, box().w)}px`,
                height: `${Math.max(1, box().h)}px`,
              }}
            />
          )}
        </Show>
        <Show when={clipMenu()}>
          {(menu) => (
            <>
              <div class="wv-scrim" onClick={() => setClipMenu(null)} onContextMenu={(event) => {
                event.preventDefault();
                setClipMenu(null);
              }} />
              <div
                class="wv-menu wv-menu--clip"
                style={{ left: `${menu().x}px`, top: `${menu().y}px` }}
                onClick={(event) => event.stopPropagation()}
              >
                <button
                  type="button"
                  class="wv-menu__row"
                  onClick={() => {
                    const clip = menu().clip;
                    setClipMenu(null);
                    if (clip.kind === 'audio' && clip.sourceId !== undefined) {
                      void openSample(clip.sourceId).catch(app.report);
                      return;
                    }
                    if (clip.kind !== 'pattern' || clip.patternId === undefined) return;
                    void nav.openPattern(clip.patternId, { altKey: false, surface: 'clip' });
                  }}
                >
                  Open
                </button>
                <Show when={menu().clip.kind === 'pattern' && menu().clip.patternId !== undefined}>
                  <button
                    type="button"
                    class="wv-menu__row"
                    onClick={() => {
                      const id = menu().clip.patternId;
                      setClipMenu(null);
                      if (id === undefined) return;
                      void nav.openPattern(id, { altKey: true, surface: 'clip' });
                    }}
                  >
                    Open piano roll
                  </button>
                  <button
                    type="button"
                    class="wv-menu__row wv-can-disable"
                    disabled={!clipCanSplit(menu().clip)}
                    title={
                      clipCanSplit(menu().clip)
                        ? 'One pattern per channel, spread onto consecutive lanes'
                        : 'Needs a pattern that speaks for more than one channel'
                    }
                    onClick={() => {
                      const id = menu().clip.patternId;
                      setClipMenu(null);
                      if (id === undefined) return;
                      app.splitPatternByChannel(id);
                    }}
                  >
                    Split by channel
                  </button>
                </Show>
                <Show when={menu().clip.kind === 'audio'}>
                  <button
                    type="button"
                    class="wv-menu__row wv-can-disable"
                    disabled={menuCut() === null}
                    title={
                      menuCut() === null
                        ? 'The nearest grid line is the clip’s own edge — right-click further in'
                        : 'Cut the clip in two on the grid line nearest where you right-clicked'
                    }
                    onClick={() => {
                      const at = menuCut();
                      const clip = liveMenuClip();
                      setClipMenu(null);
                      if (clip && at !== null) void sliceClip(clip, at);
                    }}
                  >
                    Split here
                    <span class="wv-menu__meta">C tool</span>
                  </button>
                  <button
                    type="button"
                    class="wv-menu__row wv-can-disable"
                    disabled={menuPlayheadCut() === null}
                    title={
                      menuPlayheadCut() === null
                        ? app.playMode() === 'song'
                          ? 'The playhead is not inside this clip'
                          : 'The playlist playhead runs in SONG mode'
                        : 'Cut the clip in two where the playhead is'
                    }
                    onClick={() => {
                      const clip = liveMenuClip();
                      setClipMenu(null);
                      if (!clip) return;
                      const at = playheadCut(clip);
                      if (at !== null) void sliceClip(clip, at);
                    }}
                  >
                    Split at playhead
                  </button>
                  <button
                    type="button"
                    class="wv-menu__row"
                    title="Silence this clip (and the rest of the selection) without deleting it — set an older take aside"
                    onClick={() => {
                      const clip = liveMenuClip() ?? menu().clip;
                      setClipMenu(null);
                      const selected = view.selectedClips();
                      const ids = selected.includes(clip.id)
                        ? (playlist()?.clips ?? [])
                            .filter((other) => other.kind === 'audio' && selected.includes(other.id))
                            .map((other) => other.id)
                        : [clip.id];
                      void app.setClipMuted(ids, !clip.muted);
                    }}
                  >
                    {menu().clip.muted ? 'Unmute' : 'Mute'}
                  </button>
                  <ClipVolumeRow clipId={menu().clip.id} name={clipName(menu().clip)} />
                </Show>
                <div class="wv-menu__rule" />
                <button
                  type="button"
                  class="wv-menu__row wv-menu__row--danger"
                  onClick={() => {
                    const clip = menu().clip;
                    setClipMenu(null);
                    removeClip(clip);
                  }}
                >
                  Delete
                </button>
              </div>
            </>
          )}
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

/**
 * One audio clip's own volume, in the clip's right-click menu.
 *
 * A trim on top of the channel fader, for evening out takes on one lane: the
 * loud chorus take down 3 dB without touching the verse on the same channel.
 * The slider and the readout move it live, so it can be set by ear while the
 * song plays; each drag, nudge or typed value is **one** undo entry. Values
 * go to the engine newest-first through {@link latestOnly}, and the undo entry
 * is taken only once the last of them has landed.
 */
function ClipVolumeRow(props: { clipId: number; name: string }): ReturnType<typeof Show> {
  const app = useAppState();
  const live = () => app.playlist()?.clips.find((clip) => clip.id === props.clipId)?.gainDb ?? 0;
  const [local, setLocal] = createSignal<number | null>(null);
  const value = () => local() ?? clampClipGain(live());
  const send = latestOnly((db: number) =>
    app.sequencerCommand((backend) => backend.setClipGain(props.clipId, db)),
  );
  let open = false;

  const input = (db: number): void => {
    const next = clampClipGain(db);
    if (!open) {
      open = true;
      app.startPlaylistGesture('Clip volume');
    }
    setLocal(next);
    send.push(next);
    app.say(`${props.name} · Volume`, formatClipGain(next) || '0.0 dB');
  };

  const commit = async (): Promise<void> => {
    if (!open) return;
    open = false;
    await send.settled();
    setLocal(null);
    app.commitPlaylistGesture();
  };
  // Closing the menu mid-gesture (a wheel nudge still waiting to commit) must
  // not leave the gesture open.
  onCleanup(() => void commit());

  const format = (db: number) => `${db > 0 ? '+' : db < 0 ? '−' : ''}${Math.abs(db).toFixed(1)} dB`;

  return (
    <div class="wv-clipgain" title="This clip's volume, on top of its channel's fader. Double-click the slider for 0 dB.">
      <span class="wv-clipgain__label">Volume</span>
      <input
        class="wv-clipgain__slider"
        type="range"
        min={CLIP_GAIN_MIN_DB}
        max={CLIP_GAIN_MAX_DB}
        step="0.1"
        value={value()}
        aria-label="Clip volume"
        aria-valuetext={format(value())}
        onInput={(event) => input(Number(event.currentTarget.value))}
        onChange={() => void commit()}
        onDblClick={() => {
          input(0);
          void commit();
        }}
      />
      <NumberField
        class="wv-clipgain__value"
        value={value}
        onInput={input}
        onCommit={() => void commit()}
        min={CLIP_GAIN_MIN_DB}
        max={CLIP_GAIN_MAX_DB}
        step={0.1}
        wheelStep={0.5}
        format={format}
        title="Drag or scroll to change, double-click to type a dB value"
        aria-label="Clip volume in dB"
      />
    </div>
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
  /** Where the slice tool would cut, in song ticks; `null` for no cut line. */
  cutAt: number | null;
  /** The pointer moving over the clip, or leaving it (`null`). */
  onHover: (event: PointerEvent | null) => void;
  onDown: (event: MouseEvent, mode: 'move' | 'resize' | 'trim') => void;
  onMenu: (event: MouseEvent) => void;
  onOpen: (altKey: boolean) => void;
}): ReturnType<typeof Show> {
  /**
   * The waveform as one SVG path, drawn from the engine's own min/max pairs
   * and scaled by the clip's volume, so a clip turned down looks it.
   */
  const path = () => overviewWaveformPath(gainPeaks(props.waveform?.peaks ?? [], props.clip.gainDb));
  const gain = () => (props.clip.kind === 'audio' ? formatClipGain(props.clip.gainDb) : '');

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
      data-clip-id={props.clip.id}
      classList={{ 'is-selected': props.selected, 'is-muted': props.clip.muted === true }}
      style={{
        left: `${props.clip.start * props.pxPerTick}px`,
        width: `${Math.max(6, props.clip.length * props.pxPerTick - 4)}px`,
        background: wash(props.colour, 0.28),
        'box-shadow': `inset 0 0 0 ${props.selected ? 2 : 1}px ${wash(props.colour, props.selected ? 1 : 0.7)}`,
      }}
      onMouseDown={(event) => props.onDown(event, 'move')}
      onPointerDown={(event) => {
        if (event.pointerType !== 'mouse') {
          event.preventDefault();
          props.onDown(event, 'move');
        }
      }}
      onDblClick={(event) => {
        event.preventDefault();
        event.stopPropagation();
        props.onOpen(event.altKey);
      }}
      onContextMenu={(event) => {
        event.preventDefault();
        event.stopPropagation();
        props.onMenu(event);
      }}
      onPointerMove={(event) => props.onHover(event)}
      onPointerLeave={() => props.onHover(null)}
      title={
        props.clip.kind === 'pattern'
          ? `${props.name} — click to load the brush, drag to move (shift-drag to copy), drag the right edge to resize, double-click to edit (Alt for the piano roll), right-click for more`
          : `${props.clip.muted ? 'Muted · ' : ''}${props.name}${gain() ? ` (${gain()})` : ''} — click to load the brush, drag to move (shift-drag to copy), drag either edge to trim, C for the slice tool, double-click to open the sample window, right-click for volume and more. Audio clips play through their own channel's strip.`
      }
    >
      <span class="wv-clip__label">
        {props.name}
        <Show when={gain()}>
          <span class="wv-clip__gain">{gain()}</span>
        </Show>
      </span>
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
      <Show when={props.cutAt !== null}>
        <span
          class="wv-clip__cut"
          style={{ left: `${((props.cutAt ?? 0) - props.clip.start) * props.pxPerTick}px` }}
        />
      </Show>
      <Show when={props.clip.kind === 'audio'}>
        <span
          class="wv-clip__grip wv-clip__grip--left"
          title="Drag to trim the start — the audio stays in place (Alt for off the grid)"
          onMouseDown={(event) => props.onDown(event, 'trim')}
          onPointerDown={(event) => {
            if (event.pointerType !== 'mouse') {
              event.preventDefault();
              props.onDown(event, 'trim');
            }
          }}
        />
      </Show>
      <span class="wv-clip__grip"
        onMouseDown={(event) => props.onDown(event, 'resize')}
        onPointerDown={(event) => {
          if (event.pointerType !== 'mouse') {
            event.preventDefault();
            props.onDown(event, 'resize');
          }
        }}
      />
    </div>
  );
}
