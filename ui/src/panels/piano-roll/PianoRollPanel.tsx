import { createEffect, createSignal, onCleanup, onMount, Show, type JSX } from 'solid-js';
import { useAppState } from '../../state';
import type { MelodicNoteDto, PatternDto } from '../../api/types';
import {
  currentTick,
  FALLBACK_SAMPLE_RATE,
  MAX_NOTES,
  noteAt,
  noteName,
  PATTERN_LENGTHS,
  patternTicks,
  snapTicks,
  STEP_VELOCITY,
  TICKS_PER_STEP,
  withNote,
  withoutNote,
  withoutNotes,
  withPatternLength,
  withReplacedNote,
} from '../../lib/pattern';
import type { PanelProps } from '../types';
import {
  clampScroll,
  contentHeight,
  contentWidth,
  DEFAULT_CENTRE_KEY,
  DEFAULT_ZOOM,
  fitZoom,
  geometryFor,
  gridHeight,
  gridWidth,
  gripFor,
  inGrid,
  KEY_COLUMN_WIDTH,
  PianoRollRenderer,
  readRollColors,
  scrollToCentre,
  TOP_KEY,
  xToTick,
  yToKey,
  ZOOM_LEVELS,
  type NoteGrip,
  type RollGeometry,
} from './draw';

/**
 * Snap options, in ticks. `0` means no snap at all -- a note goes exactly where
 * it is dropped, which is how a swung hi-hat or a nudged bass note gets written.
 */
const SNAP_OPTIONS: readonly { label: string; ticks: number }[] = [
  { label: '1/4', ticks: TICKS_PER_STEP * 4 },
  { label: '1/8', ticks: TICKS_PER_STEP * 2 },
  { label: '1/16', ticks: TICKS_PER_STEP },
  { label: 'Off', ticks: 0 },
];

/** Length a click gives a new note when snapping is off. One sixteenth. */
const FREE_NOTE_TICKS = TICKS_PER_STEP;

/** What a pointer is currently doing. */
interface Drag {
  index: number;
  grip: NoteGrip;
  /** The note as it was when the drag started, so every move is absolute. */
  origin: MelodicNoteDto;
  /** The tick the pointer went down on, unsnapped. */
  grabTick: number;
  pointerId: number;
}

/**
 * The piano roll: a canvas editor for the pattern's melodic notes.
 *
 * # What it edits, and where that goes
 * Exactly the same `PatternDto` the step sequencer edits, through the same
 * debounced `set_pattern` -- so a note drawn here and a kick clicked there are
 * one message to Rust, one validation, one `basedrop` hand-off to the audio
 * thread, and one thing in the project file. There is no second path and no
 * second scheduler; see `plugins/sequencer/src/schedule.rs` for how the two
 * halves merge.
 *
 * # Why the canvas and the state are kept apart
 * `draw.ts` owns the geometry and the painting and knows nothing about Solid;
 * this component owns the pointer, the scroll and the selection. That is what
 * lets "where is tick 960?" have exactly one answer, shared by the code that
 * paints a note and the code that decides which note was clicked -- the classic
 * way a roll ends up with a one-pixel disagreement between what you see and what
 * you grab.
 *
 * # Redrawing
 * A `createEffect` redraws whenever anything the picture depends on changes, and
 * an animation-frame loop runs **only while the transport does**, because the
 * playhead is the only thing that moves on its own. An idle roll is a static
 * canvas: no rAF, no wakeups, nothing on the profile.
 */
export function PianoRollPanel(_props: PanelProps): JSX.Element {
  const app = useAppState();

  const [zoom, setZoom] = createSignal(DEFAULT_ZOOM);
  const [snap, setSnap] = createSignal(TICKS_PER_STEP);
  const [selected, setSelected] = createSignal(-1);
  const [scrollX, setScrollX] = createSignal(0);
  const [scrollY, setScrollY] = createSignal(0);
  const [size, setSize] = createSignal({ width: 0, height: 0 });
  const [playheadTick, setPlayheadTick] = createSignal<number | null>(null);

  let canvas: HTMLCanvasElement | undefined;
  let host: HTMLDivElement | undefined;
  let renderer: PianoRollRenderer | undefined;
  let drag: Drag | undefined;
  let settled = false;

  /** The track a new note plays: the selected one, whose index is its channel. */
  const channel = () => app.mixer()?.selected ?? 0;
  const trackName = () => {
    const mixer = app.mixer();
    const track = mixer?.tracks[mixer.selected];
    return track ? track.name : 'no track';
  };

  const geometry = (): RollGeometry =>
    geometryFor(size().width, size().height, zoom(), scrollX(), scrollY());

  const noteCount = () => app.pattern()?.notes.length ?? 0;

  // --- drawing -------------------------------------------------------------

  const paint = () => {
    const pattern = app.pattern();
    if (!renderer || !pattern) return;
    renderer.draw({
      pattern,
      geometry: geometry(),
      selected: selected(),
      playheadTick: playheadTick(),
    });
  };

  onMount(() => {
    if (!canvas || !host) return;
    renderer = new PianoRollRenderer(canvas, readRollColors(host));

    const applySize = () => {
      if (!host || !renderer) return;
      const rect = host.getBoundingClientRect();
      const dpr = window.devicePixelRatio || 1;
      renderer.resize(rect.width, rect.height, dpr);
      setSize({ width: rect.width, height: rect.height });

      // The first time the panel has a size, fit the pattern to the width and
      // put middle C in the middle. Done here rather than at mount because at
      // mount the element has no size, and centring on nothing scrolls to the
      // top of a roll nobody plays in. Once only: after that the zoom and the
      // scroll are the user's.
      const pattern = app.pattern();
      if (!settled && rect.height > 0 && rect.width > 0 && pattern) {
        settled = true;
        setZoom(fitZoom(pattern.steps, rect.width - KEY_COLUMN_WIDTH));
      }
      const geo = geometryFor(rect.width, rect.height, zoom(), scrollX(), scrollY());
      setScrollY((y) =>
        settled && y === 0
          ? scrollToCentre(DEFAULT_CENTRE_KEY, geo)
          : clampScroll(y, gridHeight(geo), contentHeight()),
      );
      paint();
    };
    applySize();

    const observer = new ResizeObserver(applySize);
    observer.observe(host);
    onCleanup(() => observer.disconnect());
  });

  // Redraw whenever the picture's inputs change. Cheap: the whole canvas is a
  // few hundred rectangles, and this only runs on an actual edit or scroll.
  createEffect(() => {
    // Touch every dependency, then paint.
    app.pattern();
    geometry();
    selected();
    playheadTick();
    paint();
  });

  // The playhead, and only the playhead, needs an animation frame -- so it only
  // gets one while the transport is running.
  createEffect(() => {
    if (!app.playing()) {
      setPlayheadTick(null);
      return;
    }
    let raf = 0;
    const tick = () => {
      const pattern = app.pattern();
      const rate = app.playhead.getSampleRate() || FALLBACK_SAMPLE_RATE;
      setPlayheadTick(
        pattern
          ? currentTick(app.playhead.positionSamples(), rate, app.bpm(), pattern.steps)
          : null,
      );
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    onCleanup(() => cancelAnimationFrame(raf));
  });

  // --- editing -------------------------------------------------------------

  /** Snap `ticks` if snapping is on, and keep it inside the pattern. */
  const quantise = (ticks: number, pattern: PatternDto): number => {
    const snapped = snapTicks(ticks, snap());
    return Math.max(0, Math.min(snapped, patternTicks(pattern) - 1));
  };

  /** The length a freshly placed note gets: one snap unit, or a sixteenth. */
  const newNoteLength = () => (snap() > 0 ? snap() : FREE_NOTE_TICKS);

  const pointerPosition = (event: PointerEvent | MouseEvent) => {
    const rect = canvas?.getBoundingClientRect();
    if (!rect) return null;
    return { x: event.clientX - rect.left, y: event.clientY - rect.top };
  };

  const onPointerDown = (event: PointerEvent) => {
    const pattern = app.pattern();
    const position = pointerPosition(event);
    if (!pattern || !position) return;
    const geo = geometry();
    if (!inGrid(position.x, position.y, geo)) return;

    canvas?.focus();
    const rawTick = xToTick(position.x, geo);
    const key = yToKey(position.y, geo);
    if (key < 0 || key > TOP_KEY || rawTick < 0 || rawTick >= patternTicks(pattern)) return;

    const hit = noteAt(pattern, Math.floor(rawTick), key);

    // Right-click deletes, which is the one gesture every roll agrees on.
    if (event.button === 2) {
      event.preventDefault();
      if (hit >= 0) {
        app.editPattern((current) => withoutNote(current, hit));
        setSelected(-1);
      }
      return;
    }
    if (event.button !== 0) return;
    event.preventDefault();

    const grabbed = hit >= 0 ? pattern.notes[hit] : undefined;
    if (grabbed) {
      setSelected(hit);
      drag = {
        index: hit,
        grip: gripFor(grabbed, rawTick, geo),
        origin: grabbed,
        grabTick: rawTick,
        pointerId: event.pointerId,
      };
      canvas?.setPointerCapture(event.pointerId);
      return;
    }

    if (pattern.notes.length >= MAX_NOTES) return;

    const note: MelodicNoteDto = {
      key,
      channel: channel(),
      start: quantise(rawTick, pattern),
      length: newNoteLength(),
      velocity: STEP_VELOCITY,
    };
    const next = withNote(pattern, note);
    const index = next.notes.length - 1;
    const placed = next.notes[index];
    if (next === pattern || !placed) return;
    app.editPattern(() => next);
    setSelected(index);
    // Dragging on from the click sizes the note while placing it, which is how
    // a note of an unusual length gets written without a second gesture.
    drag = {
      index,
      grip: 'resize',
      origin: placed,
      grabTick: rawTick,
      pointerId: event.pointerId,
    };
    canvas?.setPointerCapture(event.pointerId);
  };

  const onPointerMove = (event: PointerEvent) => {
    const active = drag;
    const pattern = app.pattern();
    const position = pointerPosition(event);
    if (!active || !pattern || !position) return;
    if (event.pointerId !== active.pointerId) return;

    const geo = geometry();
    const rawTick = xToTick(position.x, geo);
    const loop = patternTicks(pattern);

    let edited: MelodicNoteDto;
    if (active.grip === 'resize') {
      // The right edge follows the pointer; the start stays put. Snapped to the
      // *end* of a grid cell rather than the start of one, so dragging a little
      // way right of a line gives you the note you can see, not a shorter one.
      const rawEnd = Math.max(active.origin.start + 1, rawTick);
      const snapped = snap() > 0 ? Math.ceil(rawEnd / snap()) * snap() : Math.round(rawEnd);
      edited = {
        ...active.origin,
        length: Math.max(1, Math.min(snapped, loop) - active.origin.start),
      };
    } else {
      const key = Math.max(0, Math.min(TOP_KEY, yToKey(position.y, geo)));
      const delta = rawTick - active.grabTick;
      const start = quantise(active.origin.start + delta, pattern);
      edited = {
        ...active.origin,
        key,
        start: Math.min(start, loop - active.origin.length),
      };
    }

    const current = pattern.notes[active.index];
    if (
      current &&
      current.key === edited.key &&
      current.start === edited.start &&
      current.length === edited.length
    ) {
      return;
    }
    app.editPattern((now) => withReplacedNote(now, active.index, edited));
  };

  const endDrag = (event: PointerEvent) => {
    if (drag && drag.pointerId === event.pointerId) {
      canvas?.releasePointerCapture(event.pointerId);
      drag = undefined;
    }
  };

  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key !== 'Delete' && event.key !== 'Backspace') return;
    const index = selected();
    if (index < 0) return;
    // Stopped as well as prevented: the computer-keyboard piano listens on the
    // window, and Backspace in a text field is the browser's Back button.
    event.preventDefault();
    event.stopPropagation();
    app.editPattern((pattern) => withoutNote(pattern, index));
    setSelected(-1);
  };

  /**
   * The wheel scrolls; shift-wheel scrolls sideways.
   *
   * Not zoom-on-wheel: a pinch-zoom trackpad sends the same event as a scroll
   * wheel, and a roll that zooms when you meant to scroll past middle C is the
   * single most irritating thing a piano roll can do.
   */
  const onWheel = (event: WheelEvent) => {
    const pattern = app.pattern();
    if (!pattern) return;
    event.preventDefault();
    const geo = geometry();
    if (event.shiftKey || Math.abs(event.deltaX) > Math.abs(event.deltaY)) {
      const delta = event.shiftKey && event.deltaX === 0 ? event.deltaY : event.deltaX;
      setScrollX((x) => clampScroll(x + delta, gridWidth(geo), contentWidth(pattern, geo)));
    } else {
      setScrollY((y) => clampScroll(y + event.deltaY, gridHeight(geo), contentHeight()));
    }
  };

  const changeZoom = (delta: number) => {
    const pattern = app.pattern();
    if (!pattern) return;
    const next = Math.max(0, Math.min(ZOOM_LEVELS.length - 1, zoom() + delta));
    if (next === zoom()) return;
    // Keep the left edge of the view where it was in *musical* time, so zooming
    // does not also scroll.
    const before = geometry();
    const anchorTick = before.scrollX / before.pixelsPerTick;
    setZoom(next);
    const after = geometryFor(size().width, size().height, next, 0, scrollY());
    setScrollX(
      clampScroll(
        Math.max(0, anchorTick) * after.pixelsPerTick,
        gridWidth(after),
        contentWidth(pattern, after),
      ),
    );
  };

  const setLength = (steps: number) => {
    app.editPattern((pattern) => withPatternLength(pattern, steps));
    setSelected(-1);
  };

  const selectedNote = (): MelodicNoteDto | undefined => {
    const pattern = app.pattern();
    const index = selected();
    return pattern && index >= 0 ? pattern.notes[index] : undefined;
  };

  return (
    <div class="roll">
      <Show
        when={app.pattern()}
        fallback={<p class="roll__empty">Waiting for the pattern…</p>}
      >
        {(pattern) => (
          <>
            <div class="roll__toolbar">
              <span class="roll__target" title="New notes play the selected track">
                → {trackName()}
              </span>

              <label class="roll__control">
                Bars
                <select
                  value={pattern().steps}
                  onChange={(event) => setLength(Number(event.currentTarget.value))}
                >
                  {PATTERN_LENGTHS.map((steps) => (
                    <option value={steps}>{steps / 16}</option>
                  ))}
                </select>
              </label>

              <label class="roll__control">
                Snap
                <select
                  value={snap()}
                  onChange={(event) => setSnap(Number(event.currentTarget.value))}
                >
                  {SNAP_OPTIONS.map((option) => (
                    <option value={option.ticks}>{option.label}</option>
                  ))}
                </select>
              </label>

              <div class="roll__zoom">
                <button
                  type="button"
                  title="Zoom out"
                  disabled={zoom() === 0}
                  onClick={() => changeZoom(-1)}
                >
                  −
                </button>
                <button
                  type="button"
                  title="Zoom in"
                  disabled={zoom() === ZOOM_LEVELS.length - 1}
                  onClick={() => changeZoom(1)}
                >
                  +
                </button>
              </div>

              <span class="roll__spacer" />

              <span class="roll__count">
                {noteCount()} {noteCount() === 1 ? 'note' : 'notes'}
              </span>
              <button
                type="button"
                class="roll__clear"
                disabled={noteCount() === 0}
                title="Delete every note. The drum grid is untouched."
                onClick={() => {
                  app.editPattern(withoutNotes);
                  setSelected(-1);
                }}
              >
                Clear
              </button>
            </div>

            <div class="roll__canvas-host" ref={host}>
              <canvas
                ref={canvas}
                class="roll__canvas"
                tabindex="0"
                role="application"
                aria-label={
                  `Piano roll for ${trackName()}, ${pattern().steps / 16} ` +
                  `${pattern().steps === 16 ? 'bar' : 'bars'}, ${noteCount()} notes. ` +
                  'Click to place a note, drag to move it, drag its right edge to ' +
                  'resize, right-click or press Delete to remove it.'
                }
                onPointerDown={onPointerDown}
                onPointerMove={onPointerMove}
                onPointerUp={endDrag}
                onPointerCancel={endDrag}
                onKeyDown={onKeyDown}
                onWheel={onWheel}
                onContextMenu={(event) => event.preventDefault()}
              />
            </div>

            <div class="roll__footer">
              <Show
                when={selectedNote()}
                fallback={
                  <span class="roll__hint">
                    Click to place · drag to move · drag the right edge to resize ·
                    right-click to delete
                  </span>
                }
              >
                {(note) => (
                  <span class="roll__hint">
                    {noteName(note().key)} · bar{' '}
                    {Math.floor(note().start / (TICKS_PER_STEP * 16)) + 1} ·{' '}
                    {note().length} ticks · velocity {note().velocity.toFixed(2)} · track{' '}
                    {note().channel}
                  </span>
                )}
              </Show>
            </div>
          </>
        )}
      </Show>
    </div>
  );
}
