/**
 * Canvas geometry and drawing for the piano roll.
 *
 * Deliberately free of Solid and of anything to do with pointers: this module
 * answers "where is tick 960 on screen?" and "paint this pattern", and the panel
 * answers "the user just dragged something". The split is the same one
 * `panels/meters/draw.ts` makes, and for the same reason -- the drawing is the
 * part that has to stay measurable, and the geometry is the part both halves
 * need to agree on exactly.
 *
 * # Why a canvas and not the DOM
 * The step sequencer is 64 buttons and is quite rightly DOM: it wants focus,
 * labels and hit-testing, and it gets them for free. A four-bar piano roll is
 * 128 rows against 960 sixteenths of grid line -- tens of thousands of elements
 * before a single note is drawn, restyled on every scroll. That is not a
 * performance opinion, it is a different order of magnitude, and it is why every
 * DAW draws its roll.
 *
 * # The hot path
 * `draw` allocates nothing per frame: colours are resolved on construction and
 * on theme change, the layout is a plain struct the caller owns, and only the
 * notes actually inside the viewport are painted. The panel calls it on state
 * change and, while the transport runs, once per animation frame -- an idle roll
 * is a static canvas, not a busy loop.
 */
import type { MelodicNoteDto, PatternDto } from '../../api/types';
import {
  isBlackKey,
  noteName,
  patternTicks,
  STEPS_PER_BEAT,
  TICKS_PER_STEP,
} from '../../lib/pattern';

/** Highest MIDI key, drawn at the top of the roll. */
export const TOP_KEY = 127;

/** Rows in the full roll: every MIDI key, whether or not it is scrolled to. */
export const KEY_COUNT = 128;

/** Height of one semitone row, in CSS pixels. */
export const ROW_HEIGHT = 12;

/** Width of the piano-key column down the left-hand side. */
export const KEY_COLUMN_WIDTH = 54;

/** Height of the bar/beat ruler across the top. */
export const RULER_HEIGHT = 20;

/**
 * Horizontal zoom levels, in CSS pixels per sixteenth-note step.
 *
 * Three, not a continuous control: 24 shows a bar and a half in a typical
 * workspace, 12 shows all four bars of the longest pattern at once, and 48 is
 * close enough to place a note by eye at 60 ticks. A slider would offer a
 * thousand values of which those are the three anybody wants.
 */
export const ZOOM_LEVELS = [12, 24, 48] as const;

/** Index into [`ZOOM_LEVELS`] a fresh panel opens at before it has a width. */
export const DEFAULT_ZOOM = 1;

/**
 * The largest zoom at which `steps` still fit in `available` pixels.
 *
 * A roll that opens showing one bar in the left third of the window and dead
 * space in the other two is a roll whose first impression is "something is
 * broken". Falls back to the smallest level when even that does not fit -- four
 * bars in a narrow window has to scroll, and that is fine.
 */
export function fitZoom(steps: number, available: number): number {
  for (let index = ZOOM_LEVELS.length - 1; index >= 0; index -= 1) {
    if (steps * (ZOOM_LEVELS[index] ?? 0) <= available) return index;
  }
  return 0;
}

/** How close to a note's right edge a pointer must be to resize rather than move. */
export const RESIZE_HANDLE_PX = 6;

/** Where the roll scrolls to before the user touches it: C4 in the middle. */
export const DEFAULT_CENTRE_KEY = 60;

/** Everything the geometry needs. Owned by the panel, read by everything here. */
export interface RollGeometry {
  /** Canvas size in CSS pixels. */
  width: number;
  height: number;
  /** Horizontal scroll, in CSS pixels from tick 0. */
  scrollX: number;
  /** Vertical scroll, in CSS pixels from the top of key 127's row. */
  scrollY: number;
  /** CSS pixels per tick, i.e. `ZOOM_LEVELS[zoom] / TICKS_PER_STEP`. */
  pixelsPerTick: number;
}

/** A geometry at `zoom`, scrolled to `scrollX`/`scrollY`. */
export function geometryFor(
  width: number,
  height: number,
  zoom: number,
  scrollX: number,
  scrollY: number,
): RollGeometry {
  const level = ZOOM_LEVELS[Math.min(ZOOM_LEVELS.length - 1, Math.max(0, zoom))] ?? ZOOM_LEVELS[1];
  return {
    width,
    height,
    scrollX,
    scrollY,
    pixelsPerTick: level / TICKS_PER_STEP,
  };
}

/** Width of the note area, i.e. everything right of the key column. */
export function gridWidth(geo: RollGeometry): number {
  return Math.max(0, geo.width - KEY_COLUMN_WIDTH);
}

/** Height of the note area, i.e. everything below the ruler. */
export function gridHeight(geo: RollGeometry): number {
  return Math.max(0, geo.height - RULER_HEIGHT);
}

/** Total scrollable width of one time round the loop, in CSS pixels. */
export function contentWidth(pattern: PatternDto, geo: RollGeometry): number {
  return patternTicks(pattern) * geo.pixelsPerTick;
}

/** Total scrollable height of all 128 rows, in CSS pixels. */
export function contentHeight(): number {
  return KEY_COUNT * ROW_HEIGHT;
}

/** Screen x of `tick`. May be outside the canvas; the caller clips. */
export function tickToX(tick: number, geo: RollGeometry): number {
  return KEY_COLUMN_WIDTH + tick * geo.pixelsPerTick - geo.scrollX;
}

/** The tick at screen x. Fractional, and not clamped to the pattern. */
export function xToTick(x: number, geo: RollGeometry): number {
  return (x - KEY_COLUMN_WIDTH + geo.scrollX) / geo.pixelsPerTick;
}

/** Screen y of the **top** of `key`'s row. */
export function keyToY(key: number, geo: RollGeometry): number {
  return RULER_HEIGHT + (TOP_KEY - key) * ROW_HEIGHT - geo.scrollY;
}

/** The key whose row contains screen y. Not clamped to `0..127`. */
export function yToKey(y: number, geo: RollGeometry): number {
  return TOP_KEY - Math.floor((y - RULER_HEIGHT + geo.scrollY) / ROW_HEIGHT);
}

/** `scroll` brought inside `0..max`, so a resize cannot strand the view. */
export function clampScroll(scroll: number, viewport: number, content: number): number {
  return Math.max(0, Math.min(scroll, Math.max(0, content - viewport)));
}

/** The scroll that puts `key` in the middle of the note area. */
export function scrollToCentre(key: number, geo: RollGeometry): number {
  const centre = (TOP_KEY - key) * ROW_HEIGHT + ROW_HEIGHT / 2;
  return clampScroll(centre - gridHeight(geo) / 2, gridHeight(geo), contentHeight());
}

/** Whether `(x, y)` is inside the note area rather than the key column or ruler. */
export function inGrid(x: number, y: number, geo: RollGeometry): boolean {
  return x >= KEY_COLUMN_WIDTH && y >= RULER_HEIGHT && x <= geo.width && y <= geo.height;
}

/** What a pointer press on a note would start. */
export type NoteGrip = 'move' | 'resize';

/** Whether a press at `tick` on `note` grabs its right edge or its body. */
export function gripFor(note: MelodicNoteDto, tick: number, geo: RollGeometry): NoteGrip {
  const fromEnd = (note.start + note.length - tick) * geo.pixelsPerTick;
  // Never a resize handle on a note too narrow to have one: on a 60-tick note
  // at the coarsest zoom the whole note is three pixels, and a six-pixel handle
  // would make it impossible to move.
  const width = note.length * geo.pixelsPerTick;
  return fromEnd <= RESIZE_HANDLE_PX && width > RESIZE_HANDLE_PX * 2 ? 'resize' : 'move';
}

/** The colours the roll draws with, read from the CSS custom properties. */
export interface RollColors {
  background: string;
  rowWhite: string;
  rowBlack: string;
  gridStep: string;
  gridBeat: string;
  gridBar: string;
  note: string;
  noteAccent: string;
  noteSelected: string;
  noteEdge: string;
  playhead: string;
  keyWhite: string;
  keyBlack: string;
  keyEdge: string;
  label: string;
  outside: string;
}

export const DEFAULT_ROLL_COLORS: RollColors = {
  background: '#0d1014',
  rowWhite: '#151a21',
  rowBlack: '#101419',
  gridStep: '#1b212a',
  gridBeat: '#28313d',
  gridBar: '#3c4756',
  note: '#1f7a63',
  noteAccent: '#33c9a3',
  noteSelected: '#7ee6c4',
  noteEdge: '#0b0d10',
  playhead: '#e0b341',
  keyWhite: '#c8cedb',
  keyBlack: '#20262f',
  keyEdge: '#0b0d10',
  label: '#6f7887',
  outside: '#080a0d',
};

/** Reads the theme tokens off an element so the canvas matches the CSS palette. */
export function readRollColors(el: Element): RollColors {
  const style = getComputedStyle(el);
  const pick = (name: string, fallback: string): string => {
    const value = style.getPropertyValue(name).trim();
    return value.length > 0 ? value : fallback;
  };
  return {
    background: pick('--roll-bg', DEFAULT_ROLL_COLORS.background),
    rowWhite: pick('--roll-row-white', DEFAULT_ROLL_COLORS.rowWhite),
    rowBlack: pick('--roll-row-black', DEFAULT_ROLL_COLORS.rowBlack),
    gridStep: pick('--roll-grid-step', DEFAULT_ROLL_COLORS.gridStep),
    gridBeat: pick('--roll-grid-beat', DEFAULT_ROLL_COLORS.gridBeat),
    gridBar: pick('--roll-grid-bar', DEFAULT_ROLL_COLORS.gridBar),
    note: pick('--roll-note', DEFAULT_ROLL_COLORS.note),
    noteAccent: pick('--roll-note-accent', DEFAULT_ROLL_COLORS.noteAccent),
    noteSelected: pick('--roll-note-selected', DEFAULT_ROLL_COLORS.noteSelected),
    noteEdge: pick('--roll-note-edge', DEFAULT_ROLL_COLORS.noteEdge),
    playhead: pick('--roll-playhead', DEFAULT_ROLL_COLORS.playhead),
    keyWhite: pick('--roll-key-white', DEFAULT_ROLL_COLORS.keyWhite),
    keyBlack: pick('--roll-key-black', DEFAULT_ROLL_COLORS.keyBlack),
    keyEdge: pick('--roll-key-edge', DEFAULT_ROLL_COLORS.keyEdge),
    label: pick('--roll-label', DEFAULT_ROLL_COLORS.label),
    outside: pick('--roll-outside', DEFAULT_ROLL_COLORS.outside),
  };
}

/** Everything one frame of the roll depends on. */
export interface RollView {
  pattern: PatternDto;
  geometry: RollGeometry;
  /** Index into `pattern.notes`, or `-1`. */
  selected: number;
  /** Position within the loop in ticks, or `null` when the transport is stopped. */
  playheadTick: number | null;
}

const LABEL_FONT = '9px ui-monospace, Consolas, monospace';
const KEY_FONT = '9px "Segoe UI", system-ui, sans-serif';

/**
 * Stateful renderer: owns the 2D context and the device-pixel backing store.
 * One instance per canvas.
 */
export class PianoRollRenderer {
  private readonly ctx: CanvasRenderingContext2D;
  private colors: RollColors;
  private cssWidth = 0;
  private cssHeight = 0;
  private dpr = 1;

  constructor(canvas: HTMLCanvasElement, colors: RollColors = DEFAULT_ROLL_COLORS) {
    const ctx = canvas.getContext('2d', { alpha: false });
    if (!ctx) throw new Error('2D canvas context unavailable');
    this.ctx = ctx;
    this.colors = colors;
  }

  setColors(colors: RollColors): void {
    this.colors = colors;
  }

  /**
   * Sizes the backing store for the current device pixel ratio.
   *
   * Cheap enough for a ResizeObserver callback, and necessary on every one:
   * a canvas whose backing store is not `cssSize * dpr` is a blurry canvas on
   * every laptop sold this decade.
   */
  resize(cssWidth: number, cssHeight: number, dpr: number): void {
    const canvas = this.ctx.canvas;
    this.cssWidth = cssWidth;
    this.cssHeight = cssHeight;
    this.dpr = dpr;
    canvas.width = Math.max(1, Math.round(cssWidth * dpr));
    canvas.height = Math.max(1, Math.round(cssHeight * dpr));
    canvas.style.width = `${cssWidth}px`;
    canvas.style.height = `${cssHeight}px`;
  }

  /** Draws one frame. */
  draw(view: RollView): void {
    const ctx = this.ctx;
    const w = this.cssWidth;
    const h = this.cssHeight;
    if (w <= 0 || h <= 0) return;

    ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    ctx.fillStyle = this.colors.background;
    ctx.fillRect(0, 0, w, h);

    const geo = { ...view.geometry, width: w, height: h };
    this.drawRows(geo);
    this.drawVerticals(view.pattern, geo);
    this.drawBeyondTheLoop(view.pattern, geo);
    this.drawNotes(view, geo);
    this.drawPlayhead(view, geo);
    this.drawRuler(view.pattern, geo);
    this.drawKeys(geo);
  }

  /** The horizontal bands: one per semitone, black keys darker. */
  private drawRows(geo: RollGeometry): void {
    const ctx = this.ctx;
    const top = RULER_HEIGHT;
    const bottom = geo.height;
    const first = Math.max(0, Math.min(TOP_KEY, yToKey(top, geo)));
    const last = Math.max(0, Math.min(TOP_KEY, yToKey(bottom, geo)));

    for (let key = first; key >= last; key -= 1) {
      const y = keyToY(key, geo);
      ctx.fillStyle = isBlackKey(key) ? this.colors.rowBlack : this.colors.rowWhite;
      ctx.fillRect(KEY_COLUMN_WIDTH, Math.max(top, y), gridWidth(geo), ROW_HEIGHT);
      // A hairline under every B/C boundary, so an octave is countable without
      // reading the key names.
      if (key % 12 === 0) {
        ctx.fillStyle = this.colors.gridBeat;
        ctx.fillRect(KEY_COLUMN_WIDTH, Math.round(y + ROW_HEIGHT) - 1, gridWidth(geo), 1);
      }
    }
  }

  /**
   * The time grid: a line per sixteenth, heavier per beat, heaviest per bar.
   *
   * Three weights rather than one, because a roll with a uniform grid is a roll
   * you cannot count bars in -- and counting bars is most of what the eye does
   * here.
   */
  private drawVerticals(pattern: PatternDto, geo: RollGeometry): void {
    const ctx = this.ctx;
    const top = RULER_HEIGHT;
    const height = gridHeight(geo);
    const totalSteps = pattern.steps;

    // At the coarsest zoom a line per sixteenth is a line every 12 px, which is
    // still legible; below that it would be a grey wash, so the step lines drop
    // out and the beats carry the grid.
    const stepPixels = TICKS_PER_STEP * geo.pixelsPerTick;
    const drawSteps = stepPixels >= 8;

    for (let step = 0; step <= totalSteps; step += 1) {
      const x = Math.round(tickToX(step * TICKS_PER_STEP, geo));
      if (x < KEY_COLUMN_WIDTH - 1 || x > geo.width) continue;
      const isBar = step % (STEPS_PER_BEAT * 4) === 0;
      const isBeat = step % STEPS_PER_BEAT === 0;
      if (!isBeat && !drawSteps) continue;
      ctx.fillStyle = isBar ? this.colors.gridBar : isBeat ? this.colors.gridBeat : this.colors.gridStep;
      ctx.fillRect(x, top, 1, height);
    }
  }

  /**
   * Everything past the loop point, greyed out.
   *
   * The pattern repeats there rather than continuing, and a roll that drew the
   * area past the last bar the same as the area before it would be inviting the
   * user to place a note where none can go.
   */
  private drawBeyondTheLoop(pattern: PatternDto, geo: RollGeometry): void {
    const end = tickToX(patternTicks(pattern), geo);
    if (end >= geo.width) return;
    const ctx = this.ctx;
    ctx.fillStyle = this.colors.outside;
    ctx.fillRect(Math.max(KEY_COLUMN_WIDTH, end), RULER_HEIGHT, geo.width - end, gridHeight(geo));
  }

  /** The notes. Only the ones actually on screen are painted. */
  private drawNotes(view: RollView, geo: RollGeometry): void {
    const ctx = this.ctx;
    const top = RULER_HEIGHT;
    for (let index = 0; index < view.pattern.notes.length; index += 1) {
      const note = view.pattern.notes[index];
      if (!note) continue;
      const x = tickToX(note.start, geo);
      const width = Math.max(2, note.length * geo.pixelsPerTick);
      const y = keyToY(note.key, geo);
      if (x + width < KEY_COLUMN_WIDTH || x > geo.width) continue;
      if (y + ROW_HEIGHT < top || y > geo.height) continue;

      // Clipped to the grid rather than allowed to run under the key column,
      // which would look like a note starting before the pattern does.
      const left = Math.max(KEY_COLUMN_WIDTH, x);
      const right = Math.min(geo.width, x + width);
      if (right <= left) continue;

      const selected = index === view.selected;
      ctx.fillStyle = selected
        ? this.colors.noteSelected
        : note.velocity >= 0.95
          ? this.colors.noteAccent
          : this.colors.note;
      ctx.fillRect(left, y + 1, right - left, ROW_HEIGHT - 2);
      ctx.fillStyle = this.colors.noteEdge;
      ctx.fillRect(left, y + 1, 1, ROW_HEIGHT - 2);
      if (right - left > 3) ctx.fillRect(right - 1, y + 1, 1, ROW_HEIGHT - 2);
    }
  }

  /** The playhead: one vertical line, drawn over the notes and under the ruler. */
  private drawPlayhead(view: RollView, geo: RollGeometry): void {
    if (view.playheadTick === null) return;
    const x = Math.round(tickToX(view.playheadTick, geo));
    if (x < KEY_COLUMN_WIDTH || x > geo.width) return;
    const ctx = this.ctx;
    ctx.fillStyle = this.colors.playhead;
    ctx.fillRect(x, RULER_HEIGHT, 1, gridHeight(geo));
  }

  /** Bar numbers across the top, plus the beat ticks between them. */
  private drawRuler(pattern: PatternDto, geo: RollGeometry): void {
    const ctx = this.ctx;
    ctx.fillStyle = this.colors.background;
    ctx.fillRect(0, 0, geo.width, RULER_HEIGHT);
    ctx.fillStyle = this.colors.gridBar;
    ctx.fillRect(0, RULER_HEIGHT - 1, geo.width, 1);

    ctx.font = LABEL_FONT;
    ctx.textBaseline = 'middle';
    ctx.textAlign = 'left';

    const stepsPerBar = STEPS_PER_BEAT * 4;
    for (let step = 0; step <= pattern.steps; step += STEPS_PER_BEAT) {
      const x = Math.round(tickToX(step * TICKS_PER_STEP, geo));
      if (x < KEY_COLUMN_WIDTH - 1 || x > geo.width) continue;
      const isBar = step % stepsPerBar === 0;
      ctx.fillStyle = isBar ? this.colors.gridBar : this.colors.gridBeat;
      ctx.fillRect(x, isBar ? 4 : 12, 1, RULER_HEIGHT - (isBar ? 5 : 13));
      if (isBar && x + 24 < geo.width) {
        ctx.fillStyle = this.colors.label;
        ctx.fillText(String(step / stepsPerBar + 1), x + 4, RULER_HEIGHT / 2);
      }
    }

    // The ruler must not run under the key column, which is drawn last and
    // would otherwise have a bar number peeking out from behind it.
    ctx.fillStyle = this.colors.background;
    ctx.fillRect(0, 0, KEY_COLUMN_WIDTH, RULER_HEIGHT);
  }

  /**
   * The keyboard down the left: white and black keys, C's named.
   *
   * Drawn last so it sits over anything that scrolled under it, and so the
   * labels are never half-covered by a note.
   */
  private drawKeys(geo: RollGeometry): void {
    const ctx = this.ctx;
    const top = RULER_HEIGHT;
    ctx.fillStyle = this.colors.keyBlack;
    ctx.fillRect(0, top, KEY_COLUMN_WIDTH, gridHeight(geo));

    const first = Math.max(0, Math.min(TOP_KEY, yToKey(top, geo)));
    const last = Math.max(0, Math.min(TOP_KEY, yToKey(geo.height, geo)));

    ctx.font = KEY_FONT;
    ctx.textBaseline = 'middle';
    ctx.textAlign = 'right';

    for (let key = first; key >= last; key -= 1) {
      const y = keyToY(key, geo);
      if (y + ROW_HEIGHT < top) continue;
      const black = isBlackKey(key);
      ctx.fillStyle = black ? this.colors.keyBlack : this.colors.keyWhite;
      // A black key is drawn short, as it is on a keyboard -- which is what
      // makes the column read as a keyboard rather than as a striped list.
      const width = black ? KEY_COLUMN_WIDTH * 0.62 : KEY_COLUMN_WIDTH - 1;
      ctx.fillRect(0, Math.max(top, y), width, ROW_HEIGHT - 1);
      if (!black && key % 12 === 0) {
        ctx.fillStyle = this.colors.keyEdge;
        ctx.fillText(noteName(key), KEY_COLUMN_WIDTH - 4, y + ROW_HEIGHT / 2);
      }
    }

    ctx.fillStyle = this.colors.gridBar;
    ctx.fillRect(KEY_COLUMN_WIDTH - 1, top, 1, gridHeight(geo));
  }
}
