/**
 * The fullscreen piano roll.
 *
 * The canvas in `roll/draw.ts` paints; this file is the interaction model.
 * Pointer-down is dispatched by `roll/gestures.ts` so the FL matrix
 * (tool × button × modifier × hit) lives in one place, and a drag of forty
 * notes still ends in one `editPattern` call — one undo entry.
 *
 * A note is addressed by its **index** in the channel's note array, which is
 * why `withNotes` deliberately does not re-order: a selection has to survive
 * the pointer moves that are editing it.
 */

import { For, Show, createEffect, createMemo, createSignal, onCleanup, onMount } from 'solid-js';

import type { MelodicNoteDto } from '../api/types';
import {
  MAX_PATTERN_TICKS,
  chord,
  glueNotes,
  humanize,
  legato,
  lengthLabel,
  noteName,
  notesOf,
  patternTick,
  patternTicks,
  quantize,
  scaleLength,
  snapTicks,
  splitNoteAt,
  ticksToSamples,
  TICKS_PER_STEP,
  withNotes,
  barsBeats,
  STEP_VELOCITY,
} from '../lib/pattern';
import { channelColor } from '../lib/channel';
import { useAppState } from '../state';
import { ROLL_SNAPS, clampZoom, nextSnap, snapLabel, useView, type RollTool } from '../shell/view';
import { useTheme } from '../theme/ThemeProvider';
import { onFrame } from '../ui/frameLoop';
import { wheelNotches } from '../ui/drag';
import {
  IconBack,
  IconCaret,
  IconDrawTool,
  IconEraseTool,
  IconFitNotes,
  IconPaintTool,
  IconSelectTool,
  IconSliceTool,
} from '../ui/icons';
import {
  KEY_W,
  ROW_H,
  RULER_H,
  STEP_W,
  TOP_KEY,
  RollRenderer,
  clampScroll,
  contentHeight,
  contentWidth,
  cutTickForNote,
  geometryFor,
  gridHeight,
  gridWidth,
  gripFor,
  hScrollBar,
  hitHScroll,
  hitVScroll,
  inGrid,
  noteAtPoint,
  scrollToCentre,
  vScrollBar,
  xToTick,
  yToKey,
  type RollGeometry,
  type ScrollPart,
} from './roll/draw';
import { fitNotes } from './roll/fit';
import {
  ROLL_TOOL_BY_CODE,
  hoverCursor,
  regionAt,
  resolveGesture,
  type RollHit,
} from './roll/gestures';

const TOOLS: readonly [RollTool, string, () => ReturnType<typeof IconDrawTool>][] = [
  ['draw', 'Draw (P) — click to add, drag to move, Shift-drag to set length', IconDrawTool],
  ['paint', 'Paint (B) — drag to lay a run of notes, Shift for monophonic', IconPaintTool],
  ['select', 'Select (E) — drag a marquee, Shift to add', IconSelectTool],
  ['delete', 'Delete (D) — click or drag over notes', IconEraseTool],
  ['slice', 'Slice (C) — drag a cut line, Shift-click to cut', IconSliceTool],
];

/** The velocity lane's property tabs. Only the first is on the wire. */
const LANE_PROPS: readonly [string, boolean, string][] = [
  ['Velocity', true, ''],
  ['Pan', false, 'A note carries a key, a start, a length and a velocity — the engine has no per-note pan'],
  ['Mod X', false, 'The engine has no per-note modulation lane yet'],
];

/** In-memory note clipboard: survives closing the roll, not a restart. */
let noteClipboard: MelodicNoteDto[] = [];

export function Roll(): ReturnType<typeof Show> {
  const app = useAppState();
  const view = useView();
  const theme = useTheme();

  const [scrollX, setScrollX] = createSignal(0);
  const [scrollY, setScrollY] = createSignal(0);
  const [size, setSize] = createSignal({ width: 800, height: 600 });
  const [marquee, setMarquee] = createSignal<{ x: number; y: number; w: number; h: number } | null>(
    null,
  );
  const [cutLine, setCutLine] = createSignal<{
    x0: number;
    y0: number;
    x1: number;
    y1: number;
  } | null>(null);
  const [channelMenu, setChannelMenu] = createSignal(false);
  const [lastLength, setLastLength] = createSignal(TICKS_PER_STEP);
  /** Key being previewed by clicking the keyboard column. */
  const [previewKey, setPreviewKey] = createSignal<number | null>(null);

  let canvas: HTMLCanvasElement | undefined;
  let host: HTMLDivElement | undefined;
  let renderer: RollRenderer | undefined;
  let dirty = true;
  let lastPointer = { x: 400, y: 300 };
  let lastClickTick = 0;
  let gestureActive = false;

  const channel = () => view.rollChannel();
  const track = () => app.tracks()[channel()];
  const colour = () => channelColor(track());
  const notes = (): MelodicNoteDto[] => {
    const pattern = app.pattern();
    return pattern ? notesOf(pattern, channel()) : [];
  };
  /** Where the pattern's loop ends: the shaded edge, and where PAT wraps. */
  const loopTicks = () => {
    const pattern = app.pattern();
    return pattern ? patternTicks(pattern) : 3840;
  };

  /**
   * How far you may write, which is the longest a pattern may *become*.
   *
   * The roll used to end where the pattern ended: the grid stopped, the
   * scrollbar stopped, and a note aimed past the last bar was dropped on the
   * floor. It is FL's behaviour that is wanted here -- the roll is four bars
   * wide whatever the pattern is, and writing in bar three makes the pattern
   * three bars long (rounded up to a length the engine allows). The growth
   * itself is in `withNotes`, so every gesture gets it.
   */
  const editTicks = () => MAX_PATTERN_TICKS;

  const geometry = (): RollGeometry =>
    geometryFor(
      size().width,
      size().height,
      view.rollZoom(),
      view.rollZoomY(),
      scrollX(),
      scrollY(),
    );

  /** Ghost notes: every other channel of the same pattern, faded. */
  const ghosts = createMemo(() => {
    if (!view.ghosts()) return [];
    const pattern = app.pattern();
    if (!pattern) return [];
    return pattern.channels
      .filter((clip) => clip.channel !== channel() && clip.notes.length > 0)
      .map((clip) => ({
        notes: clip.notes,
        colour: channelColor(app.tracks()[clip.channel]),
      }));
  });

  // --- painting ------------------------------------------------------------

  onMount(() => {
    if (!canvas || !host) return;
    renderer = new RollRenderer(canvas);
    const observer = new ResizeObserver(() => {
      const rect = host!.getBoundingClientRect();
      setSize({ width: Math.max(1, rect.width), height: Math.max(1, rect.height) });
      dirty = true;
    });
    observer.observe(host);
    const rect = host.getBoundingClientRect();
    setSize({ width: Math.max(1, rect.width), height: Math.max(1, rect.height) });
    lastPointer = { x: rect.width / 2, y: rect.height / 2 };

    // Open on the notes that are there, or on middle C when there are none.
    const existing = notes();
    const focus =
      existing.length > 0
        ? Math.round(existing.reduce((sum, note) => sum + note.key, 0) / existing.length)
        : 60;
    setScrollY(scrollToCentre(focus, geometry()));
    dirty = true;
    paint();

    // By hand with { passive: false }, per drag.ts's rule: a JSX onWheel may
    // end up passive, and a passive listener's preventDefault is a silent
    // no-op — the page would scroll past the roll.
    canvas.addEventListener('wheel', onWheel, { passive: false });

    onCleanup(() => {
      observer.disconnect();
      canvas?.removeEventListener('wheel', onWheel);
    });
  });

  createEffect(() => {
    void notes();
    void ghosts();
    void view.selection();
    void view.rollZoom();
    void view.rollZoomY();
    void scrollX();
    void scrollY();
    void size();
    void marquee();
    void cutLine();
    void theme.name();
    void loopTicks();
    void app.heldNotes();
    void previewKey();
    dirty = true;
    paint();
  });

  onFrame(() => {
    if (app.playing()) paint();
  });

  function paint(): void {
    if (!renderer) return;
    const playing = app.playing();
    if (!dirty && !playing) return;
    dirty = false;
    const geo = geometry();
    renderer.resize(geo.width, geo.height, window.devicePixelRatio || 1);
    const pattern = app.pattern();
    renderer.draw(geo, {
      notes: notes(),
      ghosts: ghosts(),
      selection: new Set(view.selection()),
      colour: colour(),
      accent: theme.accentDrawn(),
      patternTicks: loopTicks(),
      drawTicks: editTicks(),
      playheadTick:
        playing && pattern
          ? patternTick(
              app.playhead.positionSamples(),
              app.playhead.getSampleRate(),
              app.bpm(),
              pattern,
            )
          : null,
      marquee: marquee(),
      cutLine: cutLine(),
      labels: true,
      heldKeys: new Set(
        previewKey() === null ? app.heldNotes() : [...app.heldNotes(), previewKey()!],
      ),
      palette: theme.palette(),
      dark: theme.isDark(),
    });
  }

  // --- editing -------------------------------------------------------------

  function commitNotes(next: readonly MelodicNoteDto[]): void {
    const key = app.channelKey(channel());
    app.editPattern((pattern) => withNotes(pattern, channel(), next, key));
  }

  function commitOnce(label: string, next: readonly MelodicNoteDto[]): void {
    const key = app.channelKey(channel());
    app.editPatternOnce(label, (pattern) => withNotes(pattern, channel(), next, key));
  }

  /** Snap a tick. Alt (or `unsnap`) is FL's temporary snap-to-none. */
  const snap = (tick: number, floor = false, unsnap = false) =>
    snapTicks(tick, unsnap ? 0 : view.rollSnap(), floor);

  function pointerPos(event: PointerEvent | WheelEvent): { x: number; y: number } {
    const rect = canvas!.getBoundingClientRect();
    return { x: event.clientX - rect.left, y: event.clientY - rect.top };
  }

  function hitAt(x: number, y: number, geo: RollGeometry): RollHit {
    const index = noteAtPoint(notes(), x, y, geo);
    if (index < 0) return 'empty';
    return gripFor(notes()[index]!, xToTick(x, geo), geo) === 'resize' ? 'resize' : 'body';
  }

  function onPointerDown(event: PointerEvent): void {
    if (!canvas) return;
    if (event.button === 1) event.preventDefault();
    const { x, y } = pointerPos(event);
    lastPointer = { x, y };
    const geo = geometry();
    const hHit = hitHScroll(x, y, geo, editTicks());
    if (hHit && event.button !== 1) {
      onHScroll(event, geo, hHit);
      return;
    }
    const vHit = hitVScroll(x, y, geo);
    if (vHit && event.button !== 1) {
      onVScroll(event, geo, vHit);
      return;
    }
    const region = regionAt(x, y, geo);
    if (region === 'grid') lastClickTick = Math.max(0, xToTick(x, geo));

    const gesture = resolveGesture({
      tool: view.rollTool(),
      button: event.button,
      shift: event.shiftKey,
      ctrl: event.ctrlKey || event.metaKey,
      alt: event.altKey,
      region,
      hit: region === 'grid' ? hitAt(x, y, geo) : 'empty',
    });

    const index = region === 'grid' ? noteAtPoint(notes(), x, y, geo) : -1;
    if (index >= 0) setLastLength(notes()[index]!.length);

    switch (gesture.kind) {
      case 'ignore':
        return;
      case 'pan':
        onPan(event, geo);
        return;
      case 'seek':
        onSeek(event, geo);
        return;
      case 'select-time':
        onSelectTime(event, geo, event.shiftKey);
        return;
      case 'preview-key':
        onPreviewKey(event, geo);
        return;
      case 'select-pitch':
        onSelectPitch(event, geo, event.shiftKey);
        return;
      case 'velocity':
        onVelocityDown(event, geo);
        return;
      case 'erase':
        onErase(event, geo);
        return;
      case 'marquee':
        onMarquee(event, geo, gesture.additive);
        return;
      case 'toggle-select':
        if (index < 0) return;
        view.setSelection((current) =>
          current.includes(index) ? current.filter((i) => i !== index) : [...current, index],
        );
        return;
      case 'select-note':
        if (index < 0) return;
        view.setSelection([index]);
        return;
      case 'move-note':
        if (index < 0) return;
        onNoteDown(event, geo, index, 'move');
        return;
      case 'resize-note':
        if (index < 0) return;
        onNoteDown(event, geo, index, 'resize');
        return;
      case 'clone-move':
        if (index < 0) return;
        onNoteDown(event, geo, index, 'move', { clone: true });
        return;
      case 'draw-place':
        onDrawPlace(event, geo, x, y);
        return;
      case 'draw-stretch':
        onDrawStretch(event, geo, x, y);
        return;
      case 'paint':
        onPaint(event, geo, gesture.monophonic);
        return;
      case 'slice':
        onSlice(event, geo);
        return;
      case 'slice-click':
        onSliceClick(x, y, geo, event.altKey);
        return;
    }
  }

  /** Detach for the gesture in flight, so unmount can pull its listeners. */
  let detachGesture: (() => void) | undefined;

  /**
   * Window listeners for one grid gesture: move until release, where release
   * is pointerup *or* pointercancel — a pointer the OS takes away mid-drag
   * must still put the cursor and gesture state back.
   *
   * The detach is kept so `onCleanup` can remove the listeners *without*
   * running `onEnd`: an Escape that unmounts the roll mid-drag must not fire
   * `commitNotes` into a view that is gone.
   */
  function windowGesture(
    onMove: ((event: PointerEvent) => void) | null,
    onEnd: (event: PointerEvent) => void,
  ): void {
    const detach = () => {
      if (onMove) window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', end);
      window.removeEventListener('pointercancel', end);
      detachGesture = undefined;
      gestureActive = false;
    };
    const end = (event: PointerEvent) => {
      detach();
      onEnd(event);
    };
    if (onMove) window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', end);
    window.addEventListener('pointercancel', end);
    detachGesture = detach;
  }

  onCleanup(() => {
    detachGesture?.();
    const held = previewKey();
    if (held !== null) void app.backend.noteOff(held).catch(() => {});
  });

  function onPan(event: PointerEvent, geo: RollGeometry): void {
    gestureActive = true;
    if (canvas) canvas.style.cursor = 'grabbing';
    const originX = scrollX();
    const originY = scrollY();
    const startX = event.clientX;
    const startY = event.clientY;
    const onMove = (moveEvent: PointerEvent) => {
      setScrollX(
        clampScroll(
          originX - (moveEvent.clientX - startX),
          gridWidth(geo),
          contentWidth(editTicks(), geo),
        ),
      );
      setScrollY(
        clampScroll(originY - (moveEvent.clientY - startY), gridHeight(geo), contentHeight(geo)),
      );
    };
    windowGesture(onMove, () => {
      if (canvas) canvas.style.cursor = 'default';
    });
  }

  /**
   * Horizontal scrollbar: body pans time, ends zoom (FL: drag a handle to
   * change how much of the pattern is in view, keeping the other edge).
   */
  function onHScroll(event: PointerEvent, geo: RollGeometry, part: ScrollPart): void {
    if (event.button !== 0) return;
    const pattern = editTicks();
    const bar = hScrollBar(geo, pattern);
    const viewport = gridWidth(geo);

    if (part === 'track') {
      const { x } = pointerPos(event);
      const frac = bar.track.w <= 0 ? 0 : (x - bar.track.x) / bar.track.w;
      const content = contentWidth(pattern, geo);
      setScrollX(clampScroll(frac * content - viewport / 2, viewport, content));
      return;
    }

    gestureActive = true;
    if (canvas) canvas.style.cursor = part === 'body' ? 'grabbing' : 'ew-resize';

    if (part === 'body') {
      const origin = scrollX();
      const startX = event.clientX;
      const content = contentWidth(pattern, geo);
      const maxScroll = Math.max(0, content - viewport);
      const travel = Math.max(1, bar.track.w - bar.thumb.w);
      const onMove = (moveEvent: PointerEvent) => {
        setScrollX(
          clampScroll(origin + ((moveEvent.clientX - startX) / travel) * maxScroll, viewport, content),
        );
      };
      windowGesture(onMove, () => {});
      return;
    }

    const originLeft = geo.scrollX / geo.pxPerTick;
    const originRight = (geo.scrollX + viewport) / geo.pxPerTick;
    const rect = canvas!.getBoundingClientRect();
    const onMove = (moveEvent: PointerEvent) => {
      const x = moveEvent.clientX - rect.left;
      const frac = bar.track.w <= 0 ? 0 : Math.min(1, Math.max(0, (x - bar.track.x) / bar.track.w));
      const at = frac * pattern;
      const left = part === 'start' ? at : originLeft;
      const right = part === 'end' ? at : originRight;
      applyTimeWindow(left, right, part === 'start' ? 'right' : 'left');
    };
    windowGesture(onMove, () => {});
  }

  /**
   * Vertical scrollbar: body pans pitch, ends zoom key height (FL: drag a
   * handle to make the keys taller or shorter, keeping the other edge).
   */
  function onVScroll(event: PointerEvent, geo: RollGeometry, part: ScrollPart): void {
    if (event.button !== 0) return;
    const bar = vScrollBar(geo);
    const viewport = gridHeight(geo);
    const total = TOP_KEY + 1;

    if (part === 'track') {
      const { y } = pointerPos(event);
      const frac = bar.track.h <= 0 ? 0 : (y - bar.track.y) / bar.track.h;
      const content = contentHeight(geo);
      setScrollY(clampScroll(frac * content - viewport / 2, viewport, content));
      return;
    }

    gestureActive = true;
    if (canvas) canvas.style.cursor = part === 'body' ? 'grabbing' : 'ns-resize';

    if (part === 'body') {
      const origin = scrollY();
      const startY = event.clientY;
      const content = contentHeight(geo);
      const maxScroll = Math.max(0, content - viewport);
      const travel = Math.max(1, bar.track.h - bar.thumb.h);
      const onMove = (moveEvent: PointerEvent) => {
        setScrollY(
          clampScroll(origin + ((moveEvent.clientY - startY) / travel) * maxScroll, viewport, content),
        );
      };
      windowGesture(onMove, () => {});
      return;
    }

    const originTop = geo.scrollY / geo.rowH;
    const originBot = (geo.scrollY + viewport) / geo.rowH;
    const rect = canvas!.getBoundingClientRect();
    const onMove = (moveEvent: PointerEvent) => {
      const y = moveEvent.clientY - rect.top;
      const frac = bar.track.h <= 0 ? 0 : Math.min(1, Math.max(0, (y - bar.track.y) / bar.track.h));
      const at = frac * total;
      const top = part === 'start' ? at : originTop;
      const bot = part === 'end' ? at : originBot;
      applyKeyWindow(top, bot, part === 'start' ? 'bottom' : 'top');
    };
    windowGesture(onMove, () => {});
  }

  /** Fit `leftTick..rightTick` into the grid width, anchored on one edge. */
  function applyTimeWindow(leftTick: number, rightTick: number, anchor: 'left' | 'right'): void {
    const geo = geometry();
    const viewport = gridWidth(geo);
    const pattern = editTicks();
    const left = Math.max(0, Math.min(leftTick, rightTick));
    const right = Math.max(left + 1, Math.max(leftTick, rightTick));
    const zoom = clampZoom((viewport / (right - left)) * (TICKS_PER_STEP / STEP_W));
    const px = (STEP_W * zoom) / TICKS_PER_STEP;
    view.setRollZoom(zoom);
    const content = pattern * px;
    const scroll =
      anchor === 'left' ? left * px : right * px - viewport;
    setScrollX(clampScroll(scroll, viewport, content));
    app.say('Zoom', `${Math.round(zoom * 100)}% time`);
  }

  /** Fit a span of key-rows into the grid height, anchored on one edge. */
  function applyKeyWindow(topRow: number, botRow: number, anchor: 'top' | 'bottom'): void {
    const geo = geometry();
    const viewport = gridHeight(geo);
    const total = TOP_KEY + 1;
    const top = Math.max(0, Math.min(topRow, botRow));
    const bot = Math.max(top + 0.5, Math.max(topRow, botRow));
    const zoom = clampZoom(viewport / ((bot - top) * ROW_H));
    const row = ROW_H * zoom;
    view.setRollZoomY(zoom);
    const content = total * row;
    const scroll = anchor === 'top' ? top * row : bot * row - viewport;
    setScrollY(clampScroll(scroll, viewport, content));
    app.say('Zoom', `${Math.round(zoom * 100)}% keys`);
  }

  function zoomToNotes(): void {
    const indices = targets();
    const picked = indices.map((index) => notes()[index]!).filter(Boolean);
    const geo = geometry();
    const fit = fitNotes(picked, gridWidth(geo), gridHeight(geo), editTicks());
    if (!fit) {
      app.say('Zoom', 'no notes to frame');
      return;
    }
    view.setRollZoom(fit.zoomX);
    view.setRollZoomY(fit.zoomY);
    setScrollX(fit.scrollX);
    setScrollY(fit.scrollY);
    app.say(
      'Zoom',
      view.selection().length > 0
        ? `${view.selection().length} selected`
        : `${picked.length} notes`,
    );
  }

  function onSeek(event: PointerEvent, geo: RollGeometry): void {
    gestureActive = true;
    const seekTo = (clientX: number) => {
      const rect = canvas!.getBoundingClientRect();
      const tick = Math.max(0, xToTick(clientX - rect.left, geo));
      app.seek(ticksToSamples(tick, app.playhead.getSampleRate(), app.bpm()));
      app.say('Transport · Seek', barsBeats(tick));
    };
    seekTo(event.clientX);
    windowGesture((moveEvent) => seekTo(moveEvent.clientX), () => {});
  }

  function onPreviewKey(event: PointerEvent, geo: RollGeometry): void {
    const key = Math.min(127, Math.max(0, yToKey(pointerPos(event).y, geo)));
    setPreviewKey(key);
    void app.backend.noteOn(key, 0.85).catch(app.report);
    app.say(`${track()?.name ?? 'Channel'} · Preview`, noteName(key));
    windowGesture(null, () => {
      setPreviewKey(null);
      void app.backend.noteOff(key).catch(() => {});
    });
  }

  function onSelectTime(event: PointerEvent, geo: RollGeometry, additive: boolean): void {
    gestureActive = true;
    const rect = canvas!.getBoundingClientRect();
    const x0 = event.clientX - rect.left;
    const t0 = xToTick(x0, geo);
    const base = additive ? [...view.selection()] : [];
    const top = RULER_H;
    const height = gridHeight(geo);

    const apply = (x1: number) => {
      setMarquee({
        x: Math.min(x0, x1),
        y: top,
        w: Math.abs(x1 - x0),
        h: height,
      });
      const t1 = xToTick(x1, geo);
      const lo = Math.min(t0, t1);
      const hi = Math.max(t0, t1);
      const hit = notes().reduce<number[]>((found, note, index) => {
        if (note.start < hi && note.start + note.length > lo) found.push(index);
        return found;
      }, []);
      view.setSelection([...new Set([...base, ...hit])]);
    };

    apply(x0);
    windowGesture(
      (moveEvent) => apply(moveEvent.clientX - rect.left),
      () => {
        setMarquee(null);
        app.say('Selection', `${view.selection().length} notes`);
      },
    );
  }

  function onSelectPitch(event: PointerEvent, geo: RollGeometry, additive: boolean): void {
    gestureActive = true;
    const rect = canvas!.getBoundingClientRect();
    const y0 = event.clientY - rect.top;
    const key0 = yToKey(y0, geo);
    const base = additive ? [...view.selection()] : [];

    const apply = (y1: number) => {
      const key1 = yToKey(y1, geo);
      const lo = Math.min(key0, key1);
      const hi = Math.max(key0, key1);
      setMarquee({
        x: KEY_W,
        y: Math.min(y0, y1),
        w: gridWidth(geo),
        h: Math.abs(y1 - y0) || geo.rowH,
      });
      const hit = notes().reduce<number[]>((found, note, index) => {
        if (note.key >= lo && note.key <= hi) found.push(index);
        return found;
      }, []);
      view.setSelection([...new Set([...base, ...hit])]);
    };

    apply(y0);
    windowGesture(
      (moveEvent) => apply(moveEvent.clientY - rect.top),
      () => {
        setMarquee(null);
        app.say('Selection', `${view.selection().length} notes`);
      },
    );
  }

  /** Draw: place at lastLength, then reposition until release. */
  function onDrawPlace(event: PointerEvent, geo: RollGeometry, x: number, y: number): void {
    const key = Math.min(127, Math.max(0, yToKey(y, geo)));
    const start = snap(Math.max(0, xToTick(x, geo)), true, event.altKey);
    if (start >= editTicks()) return;
    const length = Math.max(1, Math.min(lastLength(), editTicks() - start));
    const index = notes().length;

    app.startPatternGesture('Draw note');
    commitNotes([...notes(), { key, start, length, velocity: STEP_VELOCITY }]);
    view.setSelection([index]);
    void app.backend.noteOn(key, 0.85).catch(() => {});
    app.say(`${track()?.name ?? 'Channel'} · Note`, `${noteName(key)} · ${barsBeats(start)}`);
    onNoteDown(event, geo, index, 'move', { open: true, preview: key });
  }

  /** Draw + Shift: place a note and drag its length. */
  function onDrawStretch(event: PointerEvent, geo: RollGeometry, x: number, y: number): void {
    const key = Math.min(127, Math.max(0, yToKey(y, geo)));
    const start = snap(Math.max(0, xToTick(x, geo)), true, event.altKey);
    if (start >= editTicks()) return;
    const length = Math.max(view.rollSnap() || TICKS_PER_STEP, lastLength());
    const index = notes().length;

    app.startPatternGesture('Draw note');
    commitNotes([...notes(), { key, start, length, velocity: STEP_VELOCITY }]);
    view.setSelection([index]);
    void app.backend.noteOn(key, 0.85).catch(() => {});
    app.say(`${track()?.name ?? 'Channel'} · Note`, `${noteName(key)} · ${barsBeats(start)}`);

    gestureActive = true;
    const startX = event.clientX;
    const onMove = (moveEvent: PointerEvent) => {
      const delta = (moveEvent.clientX - startX) / geo.pxPerTick;
      const next = Math.max(
        view.rollSnap() || TICKS_PER_STEP / 2,
        snap(length + delta, false, moveEvent.altKey),
      );
      setLastLength(next);
      const current = notes().slice();
      const note = current[index];
      if (!note) return;
      current[index] = { ...note, length: Math.min(next, editTicks() - note.start) };
      commitNotes(current);
      app.say(`${track()?.name ?? 'Channel'} · Length`, lengthLabel(next));
    };
    windowGesture(onMove, () => {
      void app.backend.noteOff(key).catch(() => {});
      app.commitPatternGesture();
    });
  }

  /** Paint: one note per snap cell under the pointer. Shift is monophonic. */
  function onPaint(event: PointerEvent, geo: RollGeometry, monophonicAtDown: boolean): void {
    app.startPatternGesture('Paint notes');
    gestureActive = true;
    const step = view.rollSnap() || TICKS_PER_STEP;
    const length = Math.max(1, lastLength());
    const placed = new Set<string>();
    const monoAt = new Map<number, number>();

    const put = (clientX: number, clientY: number, monophonic: boolean) => {
      const rect = canvas!.getBoundingClientRect();
      const x = clientX - rect.left;
      const y = clientY - rect.top;
      if (!inGrid(x, y, geo)) return;
      const key = Math.min(127, Math.max(0, yToKey(y, geo)));
      const start = snapTicks(Math.max(0, xToTick(x, geo)), step, true);
      if (start >= editTicks()) return;
      if (monophonic) {
        if (monoAt.get(start) === key) return;
        monoAt.set(start, key);
      } else {
        const id = `${key}:${start}`;
        if (placed.has(id)) return;
        placed.add(id);
      }
      const span = Math.min(length, editTicks() - start);
      if (monophonic) {
        const kept = notes().filter(
          (note) => note.start + note.length <= start || note.start >= start + step,
        );
        commitNotes([...kept, { key, start, length: span, velocity: STEP_VELOCITY }]);
      } else {
        if (notes().some((note) => note.key === key && note.start === start)) return;
        commitNotes([...notes(), { key, start, length: span, velocity: STEP_VELOCITY }]);
      }
      app.say(`${track()?.name ?? 'Channel'} · Paint`, `${noteName(key)} · ${barsBeats(start)}`);
    };

    put(event.clientX, event.clientY, monophonicAtDown || event.shiftKey);
    windowGesture(
      (moveEvent) => put(moveEvent.clientX, moveEvent.clientY, monophonicAtDown || moveEvent.shiftKey),
      () => app.commitPatternGesture(),
    );
  }

  /**
   * Move or resize the selection.
   *
   * `clone` is Shift-at-down: copies appear on the first real drag, so a
   * Shift-click without a move is just a selection. Shift/Ctrl *after* the
   * grab lock pitch/time, but the Shift that started a clone does not.
   */
  function onNoteDown(
    event: PointerEvent,
    geo: RollGeometry,
    index: number,
    mode: 'move' | 'resize',
    options?: { clone?: boolean; open?: boolean; preview?: number },
  ): void {
    const current = view.selection();
    let group = current.length > 1 && current.includes(index) ? [...current] : [index];
    view.setSelection(group);
    if (mode === 'resize' || !options?.clone) {
      const lead = notes()[index];
      if (lead) setLastLength(lead.length);
    }

    let origin = notes().map((note) => ({ ...note }));
    const startX = event.clientX;
    const startY = event.clientY;
    const limit = editTicks();
    let moved = false;
    let cloned = false;
    let sounding: number | undefined = options?.preview;
    const clone = options?.clone === true;

    if (!options?.open) {
      app.startPatternGesture(
        clone ? 'Clone notes' : mode === 'move' ? 'Move notes' : 'Resize notes',
      );
    }
    gestureActive = true;

    const onMove = (moveEvent: PointerEvent) => {
      const dxTicks = (moveEvent.clientX - startX) / geo.pxPerTick;
      let dKeys = -Math.round((moveEvent.clientY - startY) / geo.rowH);
      if (
        !moved &&
        Math.abs(moveEvent.clientX - startX) < 2 &&
        Math.abs(moveEvent.clientY - startY) < 2
      ) {
        return;
      }
      moved = true;

      if (clone && !cloned) {
        const copies = group.map((at) => ({ ...origin[at]! }));
        const startAt = origin.length;
        origin = [...origin, ...copies];
        const clickedOffset = group.indexOf(index);
        group = copies.map((_, at) => startAt + at);
        index = startAt + Math.max(0, clickedOffset);
        cloned = true;
        view.setSelection(group);
      }

      let snappedDx = dxTicks;
      const unsnap = moveEvent.altKey;
      const snapSize = unsnap ? 0 : view.rollSnap();
      if (snapSize > 0) snappedDx = Math.round(dxTicks / snapSize) * snapSize;

      // Click then Shift: lock pitch. The Shift that started a clone does not.
      if (!clone && moveEvent.shiftKey) dKeys = 0;
      if (moveEvent.ctrlKey || moveEvent.metaKey) snappedDx = 0;

      const next = origin.map((note, at) => {
        if (!group.includes(at)) return note;
        if (mode === 'resize') {
          const length = Math.max(
            view.rollSnap() || TICKS_PER_STEP / 4,
            snap(note.length + snappedDx, false, unsnap),
          );
          return { ...note, length: Math.min(length, limit - note.start) };
        }
        const start = Math.max(0, Math.min(limit - 1, note.start + snappedDx));
        const key = Math.min(127, Math.max(0, note.key + dKeys));
        return { ...note, start, key, length: Math.min(note.length, limit - start) };
      });
      commitNotes(next);

      const lead = next[index];
      if (!lead) return;
      if (mode === 'resize') {
        setLastLength(lead.length);
        app.say(`${track()?.name ?? 'Channel'} · Length`, lengthLabel(lead.length));
      } else {
        if (sounding !== undefined && sounding !== lead.key) {
          void app.backend.noteOff(sounding).catch(() => {});
          void app.backend.noteOn(lead.key, 0.85).catch(() => {});
          sounding = lead.key;
        }
        app.say(
          `${track()?.name ?? 'Channel'} · Note`,
          `${noteName(lead.key)} · ${barsBeats(lead.start)}`,
        );
      }
    };

    windowGesture(onMove, () => {
      if (sounding !== undefined) void app.backend.noteOff(sounding).catch(() => {});
      app.commitPatternGesture();
    });
  }

  /** Rubber-band selection. Additive with Shift (Select tool) or Ctrl+Shift. */
  function onMarquee(event: PointerEvent, geo: RollGeometry, additive: boolean): void {
    gestureActive = true;
    const rect = canvas!.getBoundingClientRect();
    const x0 = event.clientX - rect.left;
    const y0 = event.clientY - rect.top;
    const base = additive ? [...view.selection()] : [];
    setMarquee({ x: x0, y: y0, w: 0, h: 0 });

    const onMove = (moveEvent: PointerEvent) => {
      const x1 = moveEvent.clientX - rect.left;
      const y1 = moveEvent.clientY - rect.top;
      setMarquee({
        x: Math.min(x0, x1),
        y: Math.min(y0, y1),
        w: Math.abs(x1 - x0),
        h: Math.abs(y1 - y0),
      });
    };
    const onUp = () => {
      const box = marquee();
      setMarquee(null);
      if (!box || (box.w < 3 && box.h < 3)) {
        if (!additive) view.setSelection([]);
        return;
      }
      const t0 = xToTick(box.x, geo);
      const t1 = xToTick(box.x + box.w, geo);
      const k1 = yToKey(box.y, geo);
      const k0 = yToKey(box.y + box.h, geo);
      const hit = notes().reduce<number[]>((found, note, index) => {
        const overlaps =
          note.start < t1 && note.start + note.length > t0 && note.key >= k0 && note.key <= k1;
        if (overlaps) found.push(index);
        return found;
      }, []);
      view.setSelection([...new Set([...base, ...hit])]);
      app.say('Selection', `${new Set([...base, ...hit]).size} notes`);
    };
    windowGesture(onMove, onUp);
  }

  /**
   * Slice: drag a line; every note it crosses splits at the crossing.
   *
   * Applied on release rather than as the pointer moves — a slice that
   * happened per pixel would shred a note into fifty.
   */
  function onSlice(event: PointerEvent, geo: RollGeometry): void {
    gestureActive = true;
    if (canvas) canvas.style.cursor = 'crosshair';
    const rect = canvas!.getBoundingClientRect();
    const x0 = event.clientX - rect.left;
    const y0 = event.clientY - rect.top;
    setCutLine({ x0, y0, x1: x0, y1: y0 });

    const onMove = (moveEvent: PointerEvent) => {
      const x1 = moveEvent.clientX - rect.left;
      const y1 = moveEvent.clientY - rect.top;
      setCutLine({ x0, y0, x1, y1 });
    };
    windowGesture(onMove, (upEvent) => {
      setCutLine(null);
      const x1 = upEvent.clientX - rect.left;
      const y1 = upEvent.clientY - rect.top;
      applySliceLine(x0, y0, x1, y1, geo, upEvent.altKey);
    });
  }

  /** Shift+slice: cut notes on this key at this tick, no drag. */
  function onSliceClick(x: number, y: number, geo: RollGeometry, unsnap: boolean): void {
    const key = Math.min(127, Math.max(0, yToKey(y, geo)));
    const tick = snap(Math.max(0, xToTick(x, geo)), true, unsnap);
    const out: MelodicNoteDto[] = [];
    let cuts = 0;
    for (const note of notes()) {
      if (note.key !== key) {
        out.push(note);
        continue;
      }
      const parts = splitNoteAt(note, tick);
      if (parts.length > 1) cuts += 1;
      out.push(...parts);
    }
    if (cuts === 0) {
      app.say('Slice', 'nothing crossed');
      return;
    }
    commitOnce('Slice notes', out);
    view.setSelection([]);
    app.say('Slice', `${cuts} note${cuts > 1 ? 's' : ''} split`);
  }

  function applySliceLine(
    x0: number,
    y0: number,
    x1: number,
    y1: number,
    geo: RollGeometry,
    unsnap: boolean,
  ): void {
    const out: MelodicNoteDto[] = [];
    let cuts = 0;
    for (const note of notes()) {
      const raw = cutTickForNote(note, x0, y0, x1, y1, geo);
      if (raw === null) {
        out.push(note);
        continue;
      }
      const cut = snap(raw, false, unsnap);
      const parts = splitNoteAt(note, cut);
      if (parts.length > 1) cuts += 1;
      out.push(...parts);
    }
    if (cuts === 0) {
      app.say('Slice', 'nothing crossed');
      return;
    }
    commitOnce('Slice notes', out);
    view.setSelection([]);
    app.say('Slice', `${cuts} note${cuts > 1 ? 's' : ''} split`);
  }

  /** Right-drag / Delete tool: brush-erase. Empty click deselects. */
  function onErase(event: PointerEvent, geo: RollGeometry): void {
    gestureActive = true;
    let open = false;

    const wipe = (clientX: number, clientY: number) => {
      const rect = canvas!.getBoundingClientRect();
      const x = clientX - rect.left;
      const y = clientY - rect.top;
      const index = noteAtPoint(notes(), x, y, geo);
      if (index < 0) return;
      if (!open) {
        app.startPatternGesture('Delete notes');
        open = true;
      }
      commitNotes(notes().filter((_, at) => at !== index));
      view.setSelection([]);
      app.say('Notes', 'deleted');
    };

    wipe(event.clientX, event.clientY);
    windowGesture(
      (moveEvent) => wipe(moveEvent.clientX, moveEvent.clientY),
      () => {
        if (open) app.commitPatternGesture();
        else view.setSelection([]);
      },
    );
  }

  /** Drag a velocity bar, and keep dragging across others. */
  function onVelocityDown(event: PointerEvent, geo: RollGeometry): void {
    gestureActive = true;
    const rect = canvas!.getBoundingClientRect();
    const laneTop = geo.height - 92 + 22;
    const laneHeight = 92 - 22 - 6;
    app.startPatternGesture('Set velocity');

    const set = (clientX: number, clientY: number) => {
      const x = clientX - rect.left;
      const y = clientY - rect.top;
      const tick = xToTick(x, geo);
      let best = -1;
      let bestDistance = Number.POSITIVE_INFINITY;
      notes().forEach((note, index) => {
        const distance = Math.abs(note.start - tick);
        if (distance < bestDistance && distance < TICKS_PER_STEP) {
          bestDistance = distance;
          best = index;
        }
      });
      if (best < 0) return;
      const velocity = Math.min(1, Math.max(0.01, (laneTop + laneHeight - y) / laneHeight));
      const next = notes().slice();
      const note = next[best];
      if (!note) return;
      next[best] = { ...note, velocity };
      commitNotes(next);
      view.setSelection([best]);
      app.say(`${track()?.name ?? 'Channel'} · Velocity`, `${Math.round(velocity * 127)} / 127`);
    };

    set(event.clientX, event.clientY);
    windowGesture(
      (moveEvent) => set(moveEvent.clientX, moveEvent.clientY),
      () => app.commitPatternGesture(),
    );
  }

  function deleteNotes(indices: readonly number[]): void {
    if (indices.length === 0) return;
    const drop = new Set(indices);
    commitOnce(indices.length > 1 ? 'Delete notes' : 'Delete note', notes().filter((_, index) => !drop.has(index)));
    view.setSelection([]);
    app.say('Notes', `${indices.length} deleted`);
  }

  // --- note operations -----------------------------------------------------

  /** The notes an operation applies to: the selection, or everything. */
  function targets(): number[] {
    const selected = view.selection();
    return selected.length > 0 ? selected : notes().map((_, index) => index);
  }

  function applyToTargets(label: string, transform: (picked: MelodicNoteDto[]) => MelodicNoteDto[]): void {
    const indices = targets();
    if (indices.length === 0) return;
    const all = notes();
    const picked = indices.map((index) => all[index]!);
    const replaced = transform(picked);
    const next = all.slice();
    indices.forEach((index, at) => {
      const value = replaced[at];
      if (value) next[index] = value;
    });
    commitOnce(label, next);
    app.say(label, `${indices.length} note${indices.length > 1 ? 's' : ''}`);
  }

  function addNotes(label: string, extra: MelodicNoteDto[]): void {
    if (extra.length === 0) return;
    commitOnce(label, [...notes(), ...extra]);
    app.say(label, `${extra.length} note${extra.length > 1 ? 's' : ''} added`);
  }

  function duplicateNotes(): void {
    const all = notes();
    const picked = view.selection().length > 0 ? view.selection() : all.map((_, index) => index);
    if (picked.length === 0) return;
    const chosen = picked.map((index) => all[index]!);
    const span =
      Math.max(...chosen.map((note) => note.start + note.length)) -
      Math.min(...chosen.map((note) => note.start));
    const copies = chosen.map((note) => ({ ...note, start: note.start + span }));
    const first = all.length;
    addNotes('Duplicate notes', copies);
    view.setSelection(copies.map((_, at) => first + at));
  }

  function copyNotes(): void {
    const picked = view.selection();
    if (picked.length === 0) return;
    const all = notes();
    noteClipboard = picked.map((index) => ({ ...all[index]! }));
    app.say('Clipboard', `${noteClipboard.length} notes copied`);
  }

  function cutNotes(): void {
    copyNotes();
    if (view.selection().length === 0) return;
    deleteNotes(view.selection());
  }

  function pasteNotes(): void {
    if (noteClipboard.length === 0) return;
    const origin = Math.min(...noteClipboard.map((note) => note.start));
    const dest = snap(lastClickTick, true);
    const offset = dest - origin;
    const copies = noteClipboard.map((note) => ({ ...note, start: note.start + offset }));
    const first = notes().length;
    addNotes('Paste notes', copies);
    view.setSelection(copies.map((_, at) => first + at));
  }

  function glueSelection(): void {
    const indices = view.selection().length > 0 ? view.selection() : notes().map((_, index) => index);
    if (indices.length < 2) return;
    const before = notes();
    const next = glueNotes(before, indices);
    if (next === before) {
      app.say('Glue', 'nothing touching');
      return;
    }
    const kept = before.length - indices.length;
    const merged = next.length - kept;
    commitOnce('Glue notes', next);
    view.setSelection(Array.from({ length: merged }, (_, at) => next.length - merged + at));
    app.say('Glue', `${indices.length} → ${merged}`);
  }

  function nudge(dTicks: number, dKeys: number, label: string): void {
    const group = view.selection();
    if (group.length === 0) return;
    const limit = editTicks();
    const next = notes().map((note, at) => {
      if (!group.includes(at)) return note;
      const start = Math.max(0, Math.min(limit - 1, note.start + dTicks));
      const key = Math.min(127, Math.max(0, note.key + dKeys));
      return { ...note, start, key, length: Math.min(note.length, limit - start) };
    });
    commitOnce(label, next);
    const lead = next[group[0]!];
    if (lead) app.say(label, `${noteName(lead.key)} · ${barsBeats(lead.start)}`);
  }

  const OPS: readonly [string, string, () => void][] = [
    [
      'Quantize',
      'Move each note onto the snap grid (Ctrl+Q)',
      () => applyToTargets('Quantize', (picked) => quantize(picked, view.rollSnap())),
    ],
    [
      'Legato',
      'Extend each note to where the next one starts (Ctrl+L)',
      () => applyToTargets('Legato', (picked) => legato(picked, notes())),
    ],
    ['Len ×2', 'Double every length', () => applyToTargets('Len ×2', (p) => scaleLength(p, 2))],
    ['Len ÷2', 'Halve every length', () => applyToTargets('Len ÷2', (p) => scaleLength(p, 0.5))],
    [
      'Chord',
      'Add a major third and a fifth above each note',
      () => {
        const picked = targets().map((index) => notes()[index]!);
        addNotes('Chord', chord(picked));
      },
    ],
    [
      'Humanize',
      'Nudge timing and velocity, repeatably',
      () =>
        applyToTargets('Humanize', (picked) =>
          humanize(picked, picked.length * 2654435761 + notes().length),
        ),
    ],
  ];

  function zoomAt(factor: number, originX: number, originY: number): void {
    const geo = geometry();
    const tick = xToTick(originX, geo);
    const key = yToKey(originY, geo);
    view.setRollZoom((zoom) => clampZoom(zoom * factor));
    const next = geometry();
    setScrollX(
      clampScroll(
        tick * next.pxPerTick - (originX - KEY_W),
        gridWidth(next),
        contentWidth(editTicks(), next),
      ),
    );
    setScrollY(
      clampScroll(
        (TOP_KEY - key) * next.rowH - (originY - RULER_H),
        gridHeight(next),
        contentHeight(next),
      ),
    );
  }

  // --- keyboard ------------------------------------------------------------

  function onKeyDown(event: KeyboardEvent): void {
    const target = event.target as HTMLElement | null;
    if (target && (target.tagName === 'INPUT' || target.isContentEditable)) return;
    const ctrl = event.ctrlKey || event.metaKey;

    if (event.key === 'Escape') {
      event.preventDefault();
      view.closeRoll();
      return;
    }
    if (event.key === 'Delete' || event.key === 'Backspace') {
      if (view.selection().length === 0) return;
      event.preventDefault();
      deleteNotes(view.selection());
      return;
    }
    if (event.key === 'PageUp' || event.key === 'PageDown') {
      event.preventDefault();
      zoomAt(event.key === 'PageUp' ? 1.25 : 1 / 1.25, lastPointer.x, lastPointer.y);
      return;
    }

    if (event.altKey && !ctrl && event.key.toLowerCase() === 'v') {
      event.preventDefault();
      view.setGhosts((on) => !on);
      app.say('Ghost notes', view.ghosts() ? 'on' : 'off');
      return;
    }

    if (event.altKey && !ctrl) {
      const altNudge =
        event.key === 'ArrowLeft'
          ? [-1, 0, 'Nudge']
          : event.key === 'ArrowRight'
            ? [1, 0, 'Nudge']
            : event.key === 'ArrowUp'
              ? [0, 1, 'Nudge']
              : event.key === 'ArrowDown'
                ? [0, -1, 'Nudge']
                : null;
      if (altNudge) {
        event.preventDefault();
        nudge(altNudge[0] as number, altNudge[1] as number, altNudge[2] as string);
        return;
      }
    }

    if (event.shiftKey && !ctrl && !event.altKey) {
      const step = view.rollSnap() || TICKS_PER_STEP;
      const shiftNudge =
        event.key === 'ArrowLeft'
          ? [-step, 0, 'Nudge']
          : event.key === 'ArrowRight'
            ? [step, 0, 'Nudge']
            : event.key === 'ArrowUp'
              ? [0, 1, 'Nudge']
              : event.key === 'ArrowDown'
                ? [0, -1, 'Nudge']
                : null;
      if (shiftNudge) {
        event.preventDefault();
        nudge(shiftNudge[0] as number, shiftNudge[1] as number, shiftNudge[2] as string);
        return;
      }
    }

    if (ctrl && !event.altKey && (event.key === 'ArrowUp' || event.key === 'ArrowDown')) {
      event.preventDefault();
      nudge(0, event.key === 'ArrowUp' ? 12 : -12, 'Transpose octave');
      return;
    }

    if (!ctrl && !event.altKey && !event.shiftKey && event.key.toLowerCase() === 'f') {
      event.preventDefault();
      zoomToNotes();
      return;
    }

    if (!ctrl && !event.altKey && !event.shiftKey && !view.typingKeyboard()) {
      const tool = ROLL_TOOL_BY_CODE[event.code];
      if (tool) {
        event.preventDefault();
        view.setRollTool(tool);
        app.say('Tool', tool);
        return;
      }
    }

    if (!ctrl) return;
    const key = event.key.toLowerCase();
    if (key === 'a') {
      event.preventDefault();
      view.setSelection(notes().map((_, index) => index));
      app.say('Selection', `${notes().length} notes`);
      return;
    }
    if (key === 'd') {
      event.preventDefault();
      view.setSelection([]);
      app.say('Selection', 'none');
      return;
    }
    if (key === 'b') {
      event.preventDefault();
      duplicateNotes();
      return;
    }
    if (key === 'c') {
      event.preventDefault();
      copyNotes();
      return;
    }
    if (key === 'x') {
      event.preventDefault();
      cutNotes();
      return;
    }
    if (key === 'v') {
      event.preventDefault();
      pasteNotes();
      return;
    }
    if (key === 'g') {
      event.preventDefault();
      glueSelection();
      return;
    }
    if (key === 'l') {
      event.preventDefault();
      applyToTargets('Legato', (picked) => legato(picked, notes()));
      return;
    }
    if (key === 'q') {
      event.preventDefault();
      applyToTargets('Quantize', (picked) => quantize(picked, view.rollSnap()));
    }
  }

  window.addEventListener('keydown', onKeyDown);
  onCleanup(() => window.removeEventListener('keydown', onKeyDown));

  // --- hover cursor / wheel ------------------------------------------------

  function onHoverMove(event: PointerEvent): void {
    if (!canvas) return;
    const { x, y } = pointerPos(event);
    lastPointer = { x, y };
    if (gestureActive || event.buttons !== 0) return;
    const geo = geometry();
    const hHit = hitHScroll(x, y, geo, editTicks());
    if (hHit) {
      canvas.style.cursor =
        hHit === 'start' || hHit === 'end' ? 'ew-resize' : hHit === 'body' ? 'grab' : 'default';
      return;
    }
    const vHit = hitVScroll(x, y, geo);
    if (vHit) {
      canvas.style.cursor =
        vHit === 'start' || vHit === 'end' ? 'ns-resize' : vHit === 'body' ? 'grab' : 'default';
      return;
    }
    const region = regionAt(x, y, geo);
    canvas.style.cursor = hoverCursor(
      view.rollTool(),
      region,
      region === 'grid' ? hitAt(x, y, geo) : 'empty',
    );
  }

  function onWheel(event: WheelEvent): void {
    const geo = geometry();
    const { x, y } = pointerPos(event);
    lastPointer = { x, y };
    // Sign-only reads go through wheelNotches: Chromium reports a shift-held
    // wheel as *horizontal* (the value in deltaX, deltaY zero), so reading
    // `deltaY < 0` would make every shift-zoom notch zoom out.
    const notches = wheelNotches(event);

    if (hitVScroll(x, y, geo)) {
      event.preventDefault();
      if (notches === 0) return;
      const centerY = RULER_H + gridHeight(geo) / 2;
      const key = yToKey(centerY, geo);
      view.setRollZoomY((zoom) => clampZoom(zoom * (notches > 0 ? 1.1 : 1 / 1.1)));
      const next = geometry();
      setScrollY(
        clampScroll(
          (TOP_KEY - key) * next.rowH - (centerY - RULER_H),
          gridHeight(next),
          contentHeight(next),
        ),
      );
      return;
    }
    if (hitHScroll(x, y, geo, editTicks())) {
      event.preventDefault();
      if (notches === 0) return;
      zoomAt(notches > 0 ? 1.1 : 1 / 1.1, KEY_W + gridWidth(geo) / 2, y);
      return;
    }

    if (event.altKey) {
      event.preventDefault();
      if (notches === 0) return;
      const index = inGrid(x, y, geo) ? noteAtPoint(notes(), x, y, geo) : -1;
      if (index < 0) return;
      const delta = notches > 0 ? 0.04 : -0.04;
      const next = notes().slice();
      const note = next[index]!;
      const velocity = Math.min(1, Math.max(0.01, note.velocity + delta));
      next[index] = { ...note, velocity };
      commitOnce('Set velocity', next);
      view.setSelection([index]);
      app.say(`${track()?.name ?? 'Channel'} · Velocity`, `${Math.round(velocity * 127)} / 127`);
      return;
    }
    if (event.shiftKey) {
      event.preventDefault();
      if (notches === 0) return;
      zoomAt(notches > 0 ? 1.1 : 1 / 1.1, x, y);
      return;
    }
    event.preventDefault();
    if (event.deltaX !== 0 || event.ctrlKey) {
      setScrollX((sx) =>
        clampScroll(
          sx + (event.ctrlKey ? event.deltaY : event.deltaX),
          gridWidth(geo),
          contentWidth(editTicks(), geo),
        ),
      );
      return;
    }
    setScrollY((sy) => clampScroll(sy + event.deltaY, gridHeight(geo), contentHeight(geo)));
  }

  // --- the readouts --------------------------------------------------------

  const lead = () => {
    const picked = view.selection();
    return picked.length > 0 ? notes()[picked[0]!] : undefined;
  };

  return (
    <div class="wv-roll">
      <div class="wv-viewbar">
        <button type="button" class="wv-chip" title="Back (Escape)" onClick={() => view.closeRoll()}>
          <IconBack />
          {view.previousView() === 'playlist'
            ? 'Playlist'
            : view.previousView() === 'mixer'
              ? 'Mixer'
              : 'Rack'}
        </button>

        <div class="wv-roll__chan">
          <button
            type="button"
            class="wv-chip wv-chip--sunk"
            title="Which channel this roll is editing"
            onClick={() => setChannelMenu((open) => !open)}
          >
            <span class="wv-swatch" style={{ background: colour() }} />
            <span class="wv-roll__chname">{track()?.name ?? '—'}</span>
            <span class="wv-roll__chsrc">{track()?.sourceLabel ?? ''}</span>
            <IconCaret />
          </button>
          <Show when={channelMenu()}>
            <div class="wv-scrim" onClick={() => setChannelMenu(false)}>
              <div class="wv-menu wv-menu--channel" onClick={(event) => event.stopPropagation()}>
                <For each={app.tracks()}>
                  {(candidate) => (
                    <button
                      type="button"
                      class="wv-menu__row"
                      classList={{ 'is-active': candidate.index === channel() }}
                      onClick={() => {
                        setChannelMenu(false);
                        app.promoteStepsToRoll(candidate.index);
                        view.openRoll(candidate.index);
                        app.selectTrack(candidate.index);
                      }}
                    >
                      <span class="wv-swatch" style={{ background: channelColor(candidate) }} />
                      {candidate.name}
                      <span class="wv-spacer" />
                      <span class="wv-menu__meta">
                        {app.pattern()?.channels.find((clip) => clip.channel === candidate.index)
                          ?.notes.length || 0}{' '}
                        notes
                      </span>
                    </button>
                  )}
                </For>
              </div>
            </div>
          </Show>
        </div>

        <span class="wv-viewbar__meta">
          {app.patterns().find((p) => p.id === app.sequencer()?.patternSet.activeId)?.name} ·{' '}
          {(app.pattern()?.steps ?? 16) / 16} bar{(app.pattern()?.steps ?? 16) / 16 === 1 ? '' : 's'}
        </span>

        <div class="wv-seg wv-seg--plain">
          <For each={TOOLS}>
            {([id, title, Icon]) => (
              <button
                type="button"
                class="wv-tool"
                classList={{ 'is-active': view.rollTool() === id }}
                title={title}
                onClick={() => view.setRollTool(id)}
              >
                <Icon />
              </button>
            )}
          </For>
          <button
            type="button"
            class="wv-tool wv-tool--fit"
            title="Zoom to notes (F) — selected notes, or all if none are selected"
            onClick={() => zoomToNotes()}
          >
            <IconFitNotes />
          </button>
        </div>

        <button
          type="button"
          class="wv-chip wv-chip--sunk"
          title="What notes snap to"
          onClick={() => view.setRollSnap(nextSnap(ROLL_SNAPS, view.rollSnap()))}
        >
          <span class="wv-chip__tag">SNAP</span>
          {snapLabel(ROLL_SNAPS, view.rollSnap())}
        </button>

        <div class="wv-zoom">
          <button
            type="button"
            class="wv-zoom__btn"
            title="Zoom out (shift-scroll or Page Down)"
            onClick={() => view.setRollZoom((zoom) => clampZoom(zoom / 1.25))}
          >
            −
          </button>
          <button
            type="button"
            class="wv-zoom__btn"
            title="Zoom in (shift-scroll or Page Up)"
            onClick={() => view.setRollZoom((zoom) => clampZoom(zoom * 1.25))}
          >
            +
          </button>
        </div>

        <button
          type="button"
          class="wv-chip"
          classList={{ 'is-active': view.ghosts() }}
          title="Show the other channels' notes of this pattern, faded (Alt+V)"
          onClick={() => view.setGhosts((on) => !on)}
        >
          Ghost notes
        </button>

        <div class="wv-spacer" />
        <span class="wv-viewbar__meta">
          {notes().length} notes
          {view.selection().length > 0 ? ` · ${view.selection().length} selected` : ''}
        </span>
      </div>

      <div class="wv-roll__host" ref={host}>
        <canvas
          class="wv-roll__canvas"
          ref={canvas}
          onPointerDown={onPointerDown}
          onPointerMove={onHoverMove}
          onPointerLeave={() => {
            if (!gestureActive && canvas) canvas.style.cursor = 'default';
          }}
          onContextMenu={(event) => event.preventDefault()}
          onAuxClick={(event) => event.preventDefault()}
        />
        <div class="wv-roll__lanetabs">
          <For each={LANE_PROPS}>
            {([name, live, why]) => (
              <span
                class="wv-roll__lanetab"
                classList={{ 'is-active': live, 'wv-disabled': !live }}
                title={live ? 'Drag a bar to set a note’s velocity' : why}
              >
                {name}
              </span>
            )}
          </For>
        </div>
      </div>

      <div class="wv-roll__foot">
        <For
          each={[
            ['NOTE', lead() ? noteName(lead()!.key) : '—'],
            ['START', lead() ? barsBeats(lead()!.start) : '—'],
            ['LENGTH', lead() ? lengthLabel(lead()!.length) : '—'],
            ['VELOCITY', lead() ? String(Math.round(lead()!.velocity * 127)) : '—'],
          ]}
        >
          {([tag, value]) => (
            <div class="wv-roll__field">
              <span class="wv-roll__fieldtag">{tag}</span>
              <span class="wv-roll__fieldval" classList={{ 'is-empty': !lead() }}>
                {value}
              </span>
            </div>
          )}
        </For>
        <div class="wv-spacer" />
        <For each={OPS}>
          {([name, title, run]) => (
            <button type="button" class="wv-chip" title={title} onClick={() => run()}>
              {name}
            </button>
          )}
        </For>
      </div>
    </div>
  );
}
