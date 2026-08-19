/**
 * The fullscreen piano roll.
 *
 * The canvas in `roll/draw.ts` paints; this file is the interaction model.
 * Everything it does ends in one `editPattern` call, so a drag of forty notes
 * and a single click go through the same path and both land on the undo stack
 * as one entry.
 *
 * A note is addressed by its **index** in the channel's note array, which is
 * why `withNotes` deliberately does not re-order: a selection has to survive
 * the pointer moves that are editing it.
 */

import { For, Show, createEffect, createMemo, createSignal, onCleanup, onMount } from 'solid-js';

import type { MelodicNoteDto } from '../api/types';
import {
  chord,
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
  ticksToSamples,
  TICKS_PER_STEP,
  withNotes,
  barsBeats,
} from '../lib/pattern';
import { useAppState } from '../state';
import { ROLL_SNAPS, clampZoom, nextSnap, snapLabel, useView, type RollTool } from '../shell/view';
import { useTheme } from '../theme/ThemeProvider';
import { trackColor } from '../theme/tokens';
import { onFrame } from '../ui/frameLoop';
import {
  IconBack,
  IconCaret,
  IconDrawTool,
  IconEraseTool,
  IconPaintTool,
  IconSelectTool,
  IconSliceTool,
} from '../ui/icons';
import {
  RollRenderer,
  clampScroll,
  contentHeight,
  contentWidth,
  geometryFor,
  gridHeight,
  gridWidth,
  gripFor,
  inGrid,
  inKeys,
  inRuler,
  inVelocity,
  noteAtPoint,
  scrollToCentre,
  xToTick,
  yToKey,
  type RollGeometry,
} from './roll/draw';

const TOOLS: readonly [RollTool, string, () => ReturnType<typeof IconDrawTool>][] = [
  ['draw', 'Draw — click to add a note, drag its length', IconDrawTool],
  ['paint', 'Paint — drag to lay a run of notes', IconPaintTool],
  ['select', 'Select — drag a marquee', IconSelectTool],
  ['delete', 'Delete — click or drag over notes', IconEraseTool],
  ['slice', 'Slice — drag across notes to split them', IconSliceTool],
];

/** The velocity lane's property tabs. Only the first is on the wire. */
const LANE_PROPS: readonly [string, boolean, string][] = [
  ['Velocity', true, ''],
  ['Pan', false, 'A note carries a key, a start, a length and a velocity — the engine has no per-note pan'],
  ['Mod X', false, 'The engine has no per-note modulation lane yet'],
];

export function Roll(): ReturnType<typeof Show> {
  const app = useAppState();
  const view = useView();
  const theme = useTheme();

  const [scrollX, setScrollX] = createSignal(0);
  const [scrollY, setScrollY] = createSignal(0);
  const [size, setSize] = createSignal({ width: 800, height: 600 });
  const [marquee, setMarquee] = createSignal<{ x: number; y: number; w: number; h: number } | null>(null);
  const [channelMenu, setChannelMenu] = createSignal(false);
  const [lastLength, setLastLength] = createSignal(TICKS_PER_STEP);

  let canvas: HTMLCanvasElement | undefined;
  let host: HTMLDivElement | undefined;
  let renderer: RollRenderer | undefined;
  let dirty = true;

  const channel = () => view.rollChannel();
  const track = () => app.tracks()[channel()];
  const colour = () => trackColor(track()?.id ?? channel());
  const notes = (): MelodicNoteDto[] => {
    const pattern = app.pattern();
    return pattern ? notesOf(pattern, channel()) : [];
  };
  const loopTicks = () => {
    const pattern = app.pattern();
    return pattern ? patternTicks(pattern) : 3840;
  };

  const geometry = (): RollGeometry =>
    geometryFor(size().width, size().height, view.rollZoom(), scrollX(), scrollY());

  /** Ghost notes: every other channel of the same pattern, faded. */
  const ghosts = createMemo(() => {
    if (!view.ghosts()) return [];
    const pattern = app.pattern();
    if (!pattern) return [];
    return pattern.channels
      .filter((clip) => clip.channel !== channel() && clip.notes.length > 0)
      .map((clip) => ({
        notes: clip.notes,
        colour: trackColor(app.tracks()[clip.channel]?.id ?? clip.channel),
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

    // Open on the notes that are there, or on middle C when there are none.
    const existing = notes();
    const focus =
      existing.length > 0
        ? Math.round(existing.reduce((sum, note) => sum + note.key, 0) / existing.length)
        : 60;
    setScrollY(scrollToCentre(focus, geometry()));
    // Explicitly, because the effect below may have already run once with no
    // renderer to draw with, and centring can legitimately be a no-op.
    dirty = true;
    paint();

    onCleanup(() => observer.disconnect());
  });

  // A redraw is driven by *change*, not by the frame tick: an edit has to be on
  // screen the moment it happens, and a canvas that only repainted on the next
  // animation frame would be a canvas that never repaints in a window the
  // compositor has parked. The frame loop is only for the playhead, which is
  // the one thing that moves with no state change behind it.
  createEffect(() => {
    void notes();
    void ghosts();
    void view.selection();
    void view.rollZoom();
    void scrollX();
    void scrollY();
    void size();
    void marquee();
    void theme.name();
    void loopTicks();
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
      labels: true,
      palette: theme.palette(),
      dark: theme.isDark(),
    });
  }

  // --- editing -------------------------------------------------------------

  function commitNotes(next: readonly MelodicNoteDto[]): void {
    const key = app.channelKey(channel());
    app.editPattern((pattern) => withNotes(pattern, channel(), next, key));
  }

  /** Snap a tick, respecting the "off" setting. */
  const snap = (tick: number, floor = false) => snapTicks(tick, view.rollSnap(), floor);

  function pointerPos(event: PointerEvent): { x: number; y: number } {
    const rect = canvas!.getBoundingClientRect();
    return { x: event.clientX - rect.left, y: event.clientY - rect.top };
  }

  function onPointerDown(event: PointerEvent): void {
    if (!canvas) return;
    const { x, y } = pointerPos(event);
    const geo = geometry();

    if (inRuler(x, y)) {
      const tick = Math.max(0, xToTick(x, geo));
      app.seek(ticksToSamples(tick, app.playhead.getSampleRate(), app.bpm()));
      app.say('Transport · Seek', barsBeats(tick));
      return;
    }

    if (inKeys(x, y, geo)) {
      // The key column is playable, on the channel being edited.
      const key = Math.min(127, Math.max(0, yToKey(y, geo)));
      void app.backend.noteOn(key, 0.85).catch(app.report);
      app.say(`${track()?.name ?? 'Channel'} · Preview`, noteName(key));
      const release = () => {
        void app.backend.noteOff(key).catch(() => {});
        window.removeEventListener('pointerup', release);
      };
      window.addEventListener('pointerup', release);
      return;
    }

    if (inVelocity(x, y, geo)) {
      onVelocityDown(event, geo);
      return;
    }

    if (!inGrid(x, y, geo)) return;
    if (event.button === 2) {
      const index = noteAtPoint(notes(), x, y, geo);
      if (index >= 0) deleteNotes([index]);
      return;
    }
    if (event.button !== 0) return;

    const tool = view.rollTool();
    const index = noteAtPoint(notes(), x, y, geo);

    if (index >= 0 && tool === 'delete') {
      deleteNotes([index]);
      return;
    }
    if (index >= 0) {
      if (event.shiftKey) {
        view.setSelection((current) =>
          current.includes(index) ? current.filter((i) => i !== index) : [...current, index],
        );
        return;
      }
      onNoteDown(event, geo, index, gripFor(notes()[index]!, xToTick(x, geo), geo));
      return;
    }

    if (tool === 'select' || event.shiftKey) {
      onMarquee(event, geo, event.shiftKey);
      return;
    }
    if (tool === 'delete') {
      view.setSelection([]);
      return;
    }
    if (tool === 'slice') {
      onSlice(event, geo);
      return;
    }
    if (tool === 'paint') {
      onPaint(event, geo);
      return;
    }
    onDraw(event, geo, x, y);
  }

  /** Draw: add a note, and drag right to set its length. */
  function onDraw(event: PointerEvent, geo: RollGeometry, x: number, y: number): void {
    const key = Math.min(127, Math.max(0, yToKey(y, geo)));
    const start = snap(Math.max(0, xToTick(x, geo)), true);
    if (start >= loopTicks()) return;
    const length = Math.max(view.rollSnap() || TICKS_PER_STEP, lastLength());
    const index = notes().length;

    app.startPatternGesture('Draw note');
    commitNotes([...notes(), { key, start, length, velocity: 0.78 }]);
    view.setSelection([index]);
    void app.backend.noteOn(key, 0.85).catch(() => {});
    app.say(`${track()?.name ?? 'Channel'} · Note`, `${noteName(key)} · ${barsBeats(start)}`);

    const startX = event.clientX;
    const onMove = (moveEvent: PointerEvent) => {
      const delta = (moveEvent.clientX - startX) / geo.pxPerTick;
      const next = Math.max(view.rollSnap() || TICKS_PER_STEP / 2, snap(length + delta));
      setLastLength(next);
      const current = notes().slice();
      const note = current[index];
      if (!note) return;
      current[index] = { ...note, length: next };
      commitNotes(current);
      app.say(`${track()?.name ?? 'Channel'} · Length`, lengthLabel(next));
    };
    const onUp = () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      void app.backend.noteOff(key).catch(() => {});
      app.commitPatternGesture();
    };
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
  }

  /** Paint: one note per snap cell under the pointer, as it moves. */
  function onPaint(event: PointerEvent, geo: RollGeometry): void {
    app.startPatternGesture('Paint notes');
    const step = view.rollSnap() || TICKS_PER_STEP;
    const placed = new Set<string>();

    const put = (clientX: number, clientY: number) => {
      const rect = canvas!.getBoundingClientRect();
      const x = clientX - rect.left;
      const y = clientY - rect.top;
      if (!inGrid(x, y, geo)) return;
      const key = Math.min(127, Math.max(0, yToKey(y, geo)));
      const start = snapTicks(Math.max(0, xToTick(x, geo)), step, true);
      if (start >= loopTicks()) return;
      const id = `${key}:${start}`;
      if (placed.has(id)) return;
      placed.add(id);
      if (notes().some((note) => note.key === key && note.start === start)) return;
      commitNotes([...notes(), { key, start, length: step, velocity: 0.78 }]);
      app.say(`${track()?.name ?? 'Channel'} · Paint`, `${noteName(key)} · ${barsBeats(start)}`);
    };

    put(event.clientX, event.clientY);
    const onMove = (moveEvent: PointerEvent) => put(moveEvent.clientX, moveEvent.clientY);
    const onUp = () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      app.commitPatternGesture();
    };
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
  }

  /** Move or resize the selection. */
  function onNoteDown(
    event: PointerEvent,
    geo: RollGeometry,
    index: number,
    mode: 'move' | 'resize',
  ): void {
    const current = view.selection();
    const group = current.length > 1 && current.includes(index) ? [...current] : [index];
    view.setSelection(group);

    const origin = notes().map((note) => ({ ...note }));
    const startX = event.clientX;
    const startY = event.clientY;
    const limit = loopTicks();
    let moved = false;

    app.startPatternGesture(mode === 'move' ? 'Move notes' : 'Resize notes');

    const onMove = (moveEvent: PointerEvent) => {
      const dxTicks = (moveEvent.clientX - startX) / geo.pxPerTick;
      const dKeys = -Math.round((moveEvent.clientY - startY) / geo.rowH);
      if (!moved && Math.abs(moveEvent.clientX - startX) < 2 && Math.abs(moveEvent.clientY - startY) < 2) {
        return;
      }
      moved = true;
      const snapped = view.rollSnap() > 0 ? Math.round(dxTicks / view.rollSnap()) * view.rollSnap() : dxTicks;

      const next = origin.map((note, at) => {
        if (!group.includes(at)) return note;
        if (mode === 'resize') {
          const length = Math.max(view.rollSnap() || TICKS_PER_STEP / 4, snap(note.length + snapped));
          return { ...note, length: Math.min(length, limit - note.start) };
        }
        const start = Math.max(0, Math.min(limit - 1, note.start + snapped));
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
        app.say(
          `${track()?.name ?? 'Channel'} · Note`,
          `${noteName(lead.key)} · ${barsBeats(lead.start)}`,
        );
      }
    };

    const onUp = () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      app.commitPatternGesture();
    };
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
  }

  /** Rubber-band selection. Additive with shift. */
  function onMarquee(event: PointerEvent, geo: RollGeometry, additive: boolean): void {
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
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
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
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
  }

  /**
   * Slice: drag a line, and every note it crosses is split at the crossing.
   *
   * Applied on release rather than as the pointer moves -- a slice that
   * happened per pixel would shred a note into fifty.
   */
  function onSlice(event: PointerEvent, geo: RollGeometry): void {
    const rect = canvas!.getBoundingClientRect();
    const x0 = event.clientX - rect.left;
    const y0 = event.clientY - rect.top;
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
    const onUp = (upEvent: PointerEvent) => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      const box = marquee();
      setMarquee(null);
      if (!box) return;
      const x1 = upEvent.clientX - rect.left;
      const y1 = upEvent.clientY - rect.top;
      const tickAt = (x: number) => xToTick(x, geo);
      const keyAt = (y: number) => yToKey(y, geo);
      const lowKey = Math.min(keyAt(y0), keyAt(y1));
      const highKey = Math.max(keyAt(y0), keyAt(y1));
      const cutStart = Math.min(tickAt(x0), tickAt(x1));
      const cutEnd = Math.max(tickAt(x0), tickAt(x1));

      const out: MelodicNoteDto[] = [];
      let cuts = 0;
      for (const note of notes()) {
        const crossed =
          note.key >= lowKey &&
          note.key <= highKey &&
          note.start < cutEnd &&
          note.start + note.length > cutStart;
        if (!crossed) {
          out.push(note);
          continue;
        }
        // Cut where the line crosses that key's row: for a vertical-ish drag
        // that is the drag's own tick, snapped.
        const cut = snap((cutStart + cutEnd) / 2);
        if (cut <= note.start || cut >= note.start + note.length) {
          out.push(note);
          continue;
        }
        out.push({ ...note, length: cut - note.start });
        out.push({ ...note, start: cut, length: note.start + note.length - cut });
        cuts += 1;
      }
      if (cuts === 0) {
        app.say('Slice', 'nothing crossed');
        return;
      }
      app.startPatternGesture('Slice notes');
      commitNotes(out);
      app.commitPatternGesture();
      view.setSelection([]);
      app.say('Slice', `${cuts} note${cuts > 1 ? 's' : ''} split`);
    };
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
  }

  /** Drag a velocity bar, and keep dragging across others. */
  function onVelocityDown(event: PointerEvent, geo: RollGeometry): void {
    const rect = canvas!.getBoundingClientRect();
    const laneTop = geo.height - 92 + 22;
    const laneHeight = 92 - 22 - 6;
    app.startPatternGesture('Set velocity');

    const set = (clientX: number, clientY: number) => {
      const x = clientX - rect.left;
      const y = clientY - rect.top;
      const tick = xToTick(x, geo);
      // The bar nearest the pointer, within one step.
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
    const onMove = (moveEvent: PointerEvent) => set(moveEvent.clientX, moveEvent.clientY);
    const onUp = () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      app.commitPatternGesture();
    };
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
  }

  function deleteNotes(indices: readonly number[]): void {
    if (indices.length === 0) return;
    const drop = new Set(indices);
    app.startPatternGesture(indices.length > 1 ? 'Delete notes' : 'Delete note');
    commitNotes(notes().filter((_, index) => !drop.has(index)));
    app.commitPatternGesture();
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
    app.startPatternGesture(label);
    commitNotes(next);
    app.commitPatternGesture();
    app.say(label, `${indices.length} note${indices.length > 1 ? 's' : ''}`);
  }

  function addNotes(label: string, extra: MelodicNoteDto[]): void {
    if (extra.length === 0) return;
    app.startPatternGesture(label);
    commitNotes([...notes(), ...extra]);
    app.commitPatternGesture();
    app.say(label, `${extra.length} note${extra.length > 1 ? 's' : ''} added`);
  }

  const OPS: readonly [string, string, () => void][] = [
    [
      'Quantize',
      'Move each note onto the snap grid',
      () => applyToTargets('Quantize', (picked) => quantize(picked, view.rollSnap())),
    ],
    [
      'Legato',
      'Extend each note to where the next one starts',
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

  // --- keyboard ------------------------------------------------------------

  function onKeyDown(event: KeyboardEvent): void {
    const target = event.target as HTMLElement | null;
    if (target && (target.tagName === 'INPUT' || target.isContentEditable)) return;
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
    if (!(event.ctrlKey || event.metaKey)) return;
    const key = event.key.toLowerCase();
    if (key === 'a') {
      event.preventDefault();
      view.setSelection(notes().map((_, index) => index));
      app.say('Selection', `${notes().length} notes`);
      return;
    }
    if (key === 'd') {
      event.preventDefault();
      const picked = view.selection();
      if (picked.length === 0) return;
      const all = notes();
      const chosen = picked.map((index) => all[index]!);
      const span =
        Math.max(...chosen.map((note) => note.start + note.length)) -
        Math.min(...chosen.map((note) => note.start));
      const copies = chosen.map((note) => ({ ...note, start: note.start + span }));
      const first = all.length;
      addNotes('Duplicate notes', copies);
      view.setSelection(copies.map((_, at) => first + at));
    }
  }

  window.addEventListener('keydown', onKeyDown);
  onCleanup(() => window.removeEventListener('keydown', onKeyDown));

  // --- scrolling -----------------------------------------------------------

  function onWheel(event: WheelEvent): void {
    const geo = geometry();
    if (event.shiftKey) {
      event.preventDefault();
      view.setRollZoom((zoom) => clampZoom(zoom * (event.deltaY < 0 ? 1.1 : 1 / 1.1)));
      return;
    }
    event.preventDefault();
    if (event.deltaX !== 0 || event.ctrlKey) {
      setScrollX((x) =>
        clampScroll(x + (event.ctrlKey ? event.deltaY : event.deltaX), gridWidth(geo), contentWidth(loopTicks(), geo)),
      );
      return;
    }
    setScrollY((y) => clampScroll(y + event.deltaY, gridHeight(geo), contentHeight(geo)));
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
                        view.openRoll(candidate.index);
                        app.selectTrack(candidate.index);
                      }}
                    >
                      <span class="wv-swatch" style={{ background: trackColor(candidate.id) }} />
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
            title="Zoom out (shift-scroll)"
            onClick={() => view.setRollZoom((zoom) => clampZoom(zoom / 1.25))}
          >
            −
          </button>
          <button
            type="button"
            class="wv-zoom__btn"
            title="Zoom in (shift-scroll)"
            onClick={() => view.setRollZoom((zoom) => clampZoom(zoom * 1.25))}
          >
            +
          </button>
        </div>

        <button
          type="button"
          class="wv-chip"
          classList={{ 'is-active': view.ghosts() }}
          title="Show the other channels' notes of this pattern, faded"
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
          onWheel={onWheel}
          onContextMenu={(event) => event.preventDefault()}
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
