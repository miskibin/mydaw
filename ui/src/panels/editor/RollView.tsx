import { createEffect, createSignal, onCleanup, onMount, Show, type JSX } from 'solid-js';
import { useAppState } from '../../state';
import { useTheme } from '../../theme/ThemeProvider';
import { mix, trackColor, type Palette } from '../../theme/tokens';
import { onFrame } from '../../ui/frameLoop';
import type { MelodicNoteDto, PatternDto } from '../../api/types';
import {
  currentTick,
  FALLBACK_SAMPLE_RATE,
  MAX_NOTES,
  noteAt,
  noteName,
  patternTicks,
  snapTicks,
  STEP_VELOCITY,
  TICKS_PER_STEP,
  withNote,
  withoutNote,
  withoutNotes,
  withReplacedNote,
} from '../../lib/pattern';
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
  scrollToCentre,
  TOP_KEY,
  xToTick,
  yToKey,
  ZOOM_LEVELS,
  type NoteGrip,
  type RollColors,
  type RollGeometry,
} from './draw';

/**
 * Snap options, in ticks. `0` is no snap at all -- a note goes exactly where it
 * is dropped, which is how a swung hat or a nudged bass note gets written.
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
  origin: MelodicNoteDto;
  grabTick: number;
  pointerId: number;
}

/**
 * The roll's palette, derived from the shell's tokens rather than read from CSS.
 *
 * The canvas cannot inherit custom properties, and a second set of `--roll-*`
 * variables to keep in step with the first is exactly the kind of duplication
 * that lets a theme swap leave one surface behind. This function is the single
 * mapping, and it takes the note colour as an argument because a note is drawn
 * in *its track's* colour -- which is what makes the roll and the mixer strip
 * agree at a glance about what is being edited.
 */
function rollColors(palette: Palette, note: string, dark: boolean): RollColors {
  return {
    background: palette.sunk,
    rowWhite: mix(palette.sunk, palette.panel2, 0.5),
    rowBlack: palette.sunk,
    gridStep: palette.grid1,
    gridBeat: palette.grid2,
    gridBar: palette.line2,
    note,
    noteAccent: mix(note, dark ? '#ffffff' : '#000000', 0.3),
    noteSelected: mix(note, dark ? '#ffffff' : '#000000', 0.55),
    noteEdge: palette.sunk,
    playhead: dark ? '#eef1f5' : '#1c2028',
    // A keyboard is a physical object: its keys are white and black in both
    // themes, or the column stops reading as a keyboard.
    keyWhite: dark ? '#c3c8d0' : '#f4f6f8',
    keyBlack: dark ? '#20262f' : '#2b313a',
    keyEdge: '#0b1220',
    label: palette.dim,
    outside: mix(palette.sunk, '#000000', dark ? 0.35 : 0.06),
  };
}

/**
 * The EDITOR tab's Notes mode: a canvas piano roll over the pattern's melodic
 * notes.
 *
 * The geometry, hit testing and painting are `panels/editor/draw.ts`,
 * unchanged from the previous shell -- it was already Solid-free, allocation-
 * free in its hot path and the one place "where is tick 960?" is answered. What
 * is new is where its colours come from and that the playhead now shares the
 * shell's animation frame instead of running one of its own.
 */
export function RollView(): JSX.Element {
  const app = useAppState();
  const theme = useTheme();

  const [zoom, setZoom] = createSignal(DEFAULT_ZOOM);
  const [snap, setSnap] = createSignal<number>(TICKS_PER_STEP);
  const [selected, setSelected] = createSignal(-1);
  const [scrollX, setScrollX] = createSignal(0);
  const [scrollY, setScrollY] = createSignal(0);
  const [size, setSize] = createSignal({ width: 0, height: 0 });

  let canvas: HTMLCanvasElement | undefined;
  let host: HTMLDivElement | undefined;
  let renderer: PianoRollRenderer | undefined;
  let drag: Drag | undefined;
  let settled = false;
  /** The tick the playhead is at, or `null`. Not a signal: the frame loop owns it. */
  let playheadTick: number | null = null;

  /** The track a new note plays: the selected one, whose index is its channel. */
  const channel = () => {
    const selection = app.selection();
    return selection.kind === 'track' ? selection.index : (app.mixer()?.selected ?? 0);
  };

  const track = () => app.mixer()?.tracks[channel()];
  const noteColor = () => {
    const current = track();
    return current ? trackColor(current.id) : theme.accentDrawn();
  };

  const geometry = (): RollGeometry =>
    geometryFor(size().width, size().height, zoom(), scrollX(), scrollY());

  const noteCount = () => app.pattern()?.notes.length ?? 0;

  const paint = () => {
    const pattern = app.pattern();
    if (!renderer || !pattern) return;
    renderer.draw({
      pattern,
      geometry: geometry(),
      selected: selected(),
      playheadTick,
    });
  };

  onMount(() => {
    if (!canvas || !host) return;
    renderer = new PianoRollRenderer(
      canvas,
      rollColors(theme.palette(), noteColor(), theme.isDark()),
    );

    const applySize = () => {
      if (!host || !renderer) return;
      const rect = host.getBoundingClientRect();
      const dpr = window.devicePixelRatio || 1;
      renderer.resize(rect.width, rect.height, dpr);
      setSize({ width: rect.width, height: rect.height });

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

  // Colours follow the theme and the selected track's colour.
  createEffect(() => {
    renderer?.setColors(rollColors(theme.palette(), noteColor(), theme.isDark()));
    paint();
  });

  // Redraw on any change to what the picture is of.
  createEffect(() => {
    app.pattern();
    geometry();
    selected();
    paint();
  });

  // The playhead is the only thing that moves on its own, and it rides the
  // shell's shared frame rather than starting a loop of its own.
  onFrame(() => {
    const pattern = app.pattern();
    const next =
      pattern && app.playing()
        ? currentTick(
            app.playhead.positionSamples(),
            app.playhead.getSampleRate() || FALLBACK_SAMPLE_RATE,
            app.bpm(),
            pattern.steps,
          )
        : null;
    if (next === playheadTick) return;
    playheadTick = next;
    paint();
  });

  // --- editing -------------------------------------------------------------

  const quantise = (ticks: number, pattern: PatternDto): number => {
    const snapped = snapTicks(ticks, snap());
    return Math.max(0, Math.min(snapped, patternTicks(pattern) - 1));
  };

  const newNoteLength = () => (snap() > 0 ? snap() : FREE_NOTE_TICKS);

  const pointerPosition = (event: PointerEvent) => {
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
    // Dragging on from the click sizes the note while placing it, which is how a
    // note of an unusual length gets written without a second gesture.
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

  const selectedNote = (): MelodicNoteDto | undefined => {
    const pattern = app.pattern();
    const index = selected();
    return pattern && index >= 0 ? pattern.notes[index] : undefined;
  };

  return (
    <div class="wv-roll">
      <Show
        when={app.pattern()}
        fallback={<span class="wv-note">Waiting for the pattern…</span>}
      >
        {(pattern) => (
          <>
            <div class="wv-roll__bar">
              <span>→ {track()?.name ?? 'no track'}</span>
              <label>
                snap{' '}
                <select
                  value={snap()}
                  aria-label="Snap"
                  onChange={(event) => setSnap(Number(event.currentTarget.value))}
                >
                  {SNAP_OPTIONS.map((option) => (
                    <option value={option.ticks}>{option.label}</option>
                  ))}
                </select>
              </label>
              <button
                type="button"
                class="wv-f7"
                disabled={zoom() === 0}
                onClick={() => changeZoom(-1)}
                aria-label="Zoom out"
              >
                −
              </button>
              <button
                type="button"
                class="wv-f7"
                disabled={zoom() === ZOOM_LEVELS.length - 1}
                onClick={() => changeZoom(1)}
                aria-label="Zoom in"
              >
                +
              </button>
              <span class="wv-spacer" />
              <Show
                when={selectedNote()}
                fallback={
                  <span>click to place · drag to move · right-click to delete</span>
                }
              >
                {(note) => (
                  <span>
                    {noteName(note().key)} · {note().length} ticks · track {note().channel}
                  </span>
                )}
              </Show>
              <span>
                {noteCount()} {noteCount() === 1 ? 'note' : 'notes'}
              </span>
              <button
                type="button"
                class="wv-f7"
                disabled={noteCount() === 0}
                title="Delete every note. The drum grid is untouched."
                onClick={() => {
                  app.editPattern(withoutNotes);
                  setSelected(-1);
                }}
              >
                clear
              </button>
            </div>

            <div class="wv-roll__canvas-host" ref={host}>
              <canvas
                ref={canvas}
                class="wv-roll__canvas"
                tabindex="0"
                role="application"
                aria-label={
                  `Piano roll for ${track()?.name ?? 'no track'}, ${pattern().steps / 16} ` +
                  `${pattern().steps === 16 ? 'bar' : 'bars'}, ${noteCount()} notes. ` +
                  'Click to place a note, drag to move it, drag its right edge to resize, ' +
                  'right-click or press Delete to remove it.'
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
          </>
        )}
      </Show>
    </div>
  );
}
