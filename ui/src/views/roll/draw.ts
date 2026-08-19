/**
 * The fullscreen piano roll, painted on one canvas.
 *
 * One canvas rather than a div per note: a pattern can hold a thousand notes,
 * and a thousand absolutely-positioned elements is a layout pass per pointer
 * move. It also puts the key column, the ruler, the grid, the notes, the ghosts
 * and the velocity lane in one coordinate system, so they cannot drift apart by
 * a rounding.
 *
 * The key column and the ruler are drawn **last**, over the scrolled content,
 * which is what makes them sticky without a second canvas or a clip region.
 *
 * Solid-free and DOM-free on purpose: everything here is arithmetic and
 * `CanvasRenderingContext2D` calls, so the geometry can be reasoned about (and
 * hit-tested) without a renderer.
 */

import type { MelodicNoteDto } from '../../api/types';
import { isBlackKey, noteName, PPQ, TICKS_PER_BAR, TICKS_PER_STEP } from '../../lib/pattern';
import type { Palette } from '../../theme/tokens';

/** Width of the key column, in CSS pixels. */
export const KEY_W = 74;
/** Height of the bar ruler. */
export const RULER_H = 26;
/** Height of the velocity lane, including its own label column. */
export const VELO_H = 92;
/** The highest key drawn. The roll shows the whole MIDI range. */
export const TOP_KEY = 127;
/** How tall one semitone is at zoom 1. */
export const ROW_H = 15;
/** How wide one sixteenth is at zoom 1. */
export const STEP_W = 22;
/** Pointer distance from a note's right edge that means "resize". */
export const GRIP_PX = 6;
/** Thickness of the FL-style zoom scrollbars. */
export const SCROLL_W = 12;
/** How much of each thumb end is a zoom handle. */
export const SCROLL_HANDLE = 7;
/** Smallest thumb that still has two grabable ends. */
const MIN_THUMB = 22;

/** Where everything is, for one frame. */
export interface RollGeometry {
  /** The canvas, in CSS pixels. */
  width: number;
  height: number;
  /** Horizontal scroll of the grid, in pixels. */
  scrollX: number;
  /** Vertical scroll of the grid, in pixels. */
  scrollY: number;
  pxPerTick: number;
  rowH: number;
}

export function geometryFor(
  width: number,
  height: number,
  zoomX: number,
  zoomY: number,
  scrollX: number,
  scrollY: number,
): RollGeometry {
  return {
    width,
    height,
    scrollX,
    scrollY,
    pxPerTick: (STEP_W * zoomX) / TICKS_PER_STEP,
    rowH: ROW_H * zoomY,
  };
}

/** The grid's viewport, excluding the key column, ruler, velocity lane and scrollbars. */
export function gridWidth(geo: RollGeometry): number {
  return Math.max(0, geo.width - KEY_W - SCROLL_W);
}

export function gridHeight(geo: RollGeometry): number {
  return Math.max(0, geo.height - RULER_H - VELO_H - SCROLL_W);
}

/** How wide and tall the scrollable content is. */
export function contentWidth(patternTicks: number, geo: RollGeometry): number {
  return patternTicks * geo.pxPerTick;
}

export function contentHeight(geo: RollGeometry): number {
  return (TOP_KEY + 1) * geo.rowH;
}

export function tickToX(tick: number, geo: RollGeometry): number {
  return KEY_W + tick * geo.pxPerTick - geo.scrollX;
}

export function xToTick(x: number, geo: RollGeometry): number {
  return (x - KEY_W + geo.scrollX) / geo.pxPerTick;
}

export function keyToY(key: number, geo: RollGeometry): number {
  return RULER_H + (TOP_KEY - key) * geo.rowH - geo.scrollY;
}

export function yToKey(y: number, geo: RollGeometry): number {
  return TOP_KEY - Math.floor((y - RULER_H + geo.scrollY) / geo.rowH);
}

/** Clamp a scroll offset so the content cannot be dragged off screen. */
export function clampScroll(scroll: number, viewport: number, content: number): number {
  return Math.max(0, Math.min(scroll, Math.max(0, content - viewport)));
}

/** The scroll that puts `key` in the middle of the grid. */
export function scrollToCentre(key: number, geo: RollGeometry): number {
  return clampScroll(
    (TOP_KEY - key) * geo.rowH - gridHeight(geo) / 2,
    gridHeight(geo),
    contentHeight(geo),
  );
}

/** Whether a canvas point is inside the note grid. */
export function inGrid(x: number, y: number, geo: RollGeometry): boolean {
  return x >= KEY_W && y >= RULER_H && y < RULER_H + gridHeight(geo);
}

/** Whether a canvas point is inside the velocity lane's bars. */
export function inVelocity(x: number, y: number, geo: RollGeometry): boolean {
  return x >= KEY_W && y >= geo.height - VELO_H + VELO_LABEL_H;
}

/** Whether a canvas point is on the key column. */
export function inKeys(x: number, y: number, geo: RollGeometry): boolean {
  return x < KEY_W && y >= RULER_H && y < RULER_H + gridHeight(geo);
}

/** Whether a canvas point is on the ruler. */
export function inRuler(x: number, y: number, geo: RollGeometry): boolean {
  return x >= KEY_W && x < geo.width - SCROLL_W && y < RULER_H;
}

export type ScrollPart = 'track' | 'body' | 'start' | 'end';

export interface ScrollBar {
  track: { x: number; y: number; w: number; h: number };
  thumb: { x: number; y: number; w: number; h: number };
}

/** Map a scroll offset onto a track: the thumb is the visible window. */
function thumbSpan(
  scroll: number,
  viewport: number,
  content: number,
  track: number,
): { start: number; size: number } {
  if (track <= 0) return { start: 0, size: 0 };
  if (content <= viewport) return { start: 0, size: track };
  const size = Math.min(track, Math.max(MIN_THUMB, (viewport / content) * track));
  const maxScroll = content - viewport;
  const start = maxScroll <= 0 ? 0 : (scroll / maxScroll) * (track - size);
  return { start, size };
}

/** Horizontal scrollbar, sitting between the grid and the velocity lane. */
export function hScrollBar(geo: RollGeometry, patternTicks: number): ScrollBar {
  const track = {
    x: KEY_W,
    y: RULER_H + gridHeight(geo),
    w: gridWidth(geo),
    h: SCROLL_W,
  };
  const { start, size } = thumbSpan(
    geo.scrollX,
    gridWidth(geo),
    contentWidth(patternTicks, geo),
    track.w,
  );
  return { track, thumb: { x: track.x + start, y: track.y + 2, w: size, h: track.h - 4 } };
}

/** Vertical scrollbar, on the right of the grid. */
export function vScrollBar(geo: RollGeometry): ScrollBar {
  const track = {
    x: geo.width - SCROLL_W,
    y: RULER_H,
    w: SCROLL_W,
    h: gridHeight(geo),
  };
  const { start, size } = thumbSpan(geo.scrollY, gridHeight(geo), contentHeight(geo), track.h);
  return { track, thumb: { x: track.x + 2, y: track.y + start, w: track.w - 4, h: size } };
}

function hitThumb(pos: number, thumbStart: number, thumbSize: number): ScrollPart {
  if (pos < thumbStart || pos > thumbStart + thumbSize) return 'track';
  const handle = Math.min(SCROLL_HANDLE, thumbSize / 3);
  if (pos <= thumbStart + handle) return 'start';
  if (pos >= thumbStart + thumbSize - handle) return 'end';
  return 'body';
}

/** Which part of the horizontal zoom-scrollbar `(x, y)` is on, or `null`. */
export function hitHScroll(
  x: number,
  y: number,
  geo: RollGeometry,
  patternTicks: number,
): ScrollPart | null {
  const bar = hScrollBar(geo, patternTicks);
  if (
    x < bar.track.x ||
    x > bar.track.x + bar.track.w ||
    y < bar.track.y ||
    y > bar.track.y + bar.track.h
  ) {
    return null;
  }
  return hitThumb(x, bar.thumb.x, bar.thumb.w);
}

/** Which part of the vertical zoom-scrollbar `(x, y)` is on, or `null`. */
export function hitVScroll(x: number, y: number, geo: RollGeometry): ScrollPart | null {
  const bar = vScrollBar(geo);
  if (
    x < bar.track.x ||
    x > bar.track.x + bar.track.w ||
    y < bar.track.y ||
    y > bar.track.y + bar.track.h
  ) {
    return null;
  }
  return hitThumb(y, bar.thumb.y, bar.thumb.h);
}

/** Room at the top of the velocity lane for its label row. */
const VELO_LABEL_H = 22;

/** Which grip a pointer at `tick` has on `note`. */
export type NoteGrip = 'move' | 'resize';

export function gripFor(note: MelodicNoteDto, tick: number, geo: RollGeometry): NoteGrip {
  const widthPx = note.length * geo.pxPerTick;
  if (widthPx < GRIP_PX * 2) return 'move';
  return (note.start + note.length - tick) * geo.pxPerTick <= GRIP_PX ? 'resize' : 'move';
}

/**
 * Tick where a cut line crosses `note`'s row, or `null` if it does not.
 *
 * FL slices at the intersection of the dragged line with each note, so a
 * diagonal cut lands at a different time on every pitch. A horizontal line
 * (no unique crossing) uses the overlap's midpoint, which is the only honest
 * answer a degenerate line has.
 */
export function cutTickForNote(
  note: MelodicNoteDto,
  x0: number,
  y0: number,
  x1: number,
  y1: number,
  geo: RollGeometry,
): number | null {
  const yTop = keyToY(note.key, geo);
  const yBot = yTop + geo.rowH;
  const nLeft = tickToX(note.start, geo);
  const nRight = tickToX(note.start + note.length, geo);
  let xCut: number;
  if (Math.abs(y1 - y0) < 0.5) {
    if (y0 < yTop || y0 > yBot) return null;
    const xLo = Math.max(Math.min(x0, x1), nLeft);
    const xHi = Math.min(Math.max(x0, x1), nRight);
    if (xHi <= xLo) return null;
    xCut = (xLo + xHi) / 2;
  } else {
    const yMid = yTop + geo.rowH / 2;
    const t = (yMid - y0) / (y1 - y0);
    if (t < 0 || t > 1) return null;
    xCut = x0 + t * (x1 - x0);
    if (xCut <= nLeft || xCut >= nRight) return null;
    const yLo = Math.min(y0, y1);
    const yHi = Math.max(y0, y1);
    if (yHi < yTop || yLo > yBot) return null;
  }
  return xToTick(xCut, geo);
}

/** Which note is under a canvas point, or `-1`. Reverse scan: topmost wins. */
export function noteAtPoint(
  notes: readonly MelodicNoteDto[],
  x: number,
  y: number,
  geo: RollGeometry,
): number {
  const tick = xToTick(x, geo);
  const key = yToKey(y, geo);
  for (let index = notes.length - 1; index >= 0; index -= 1) {
    const note = notes[index]!;
    if (note.key !== key) continue;
    if (tick < note.start) continue;
    // A very short note still has a few pixels of target: a note you cannot
    // click is a note you cannot delete.
    const end = note.start + Math.max(note.length, GRIP_PX / geo.pxPerTick);
    if (tick < end) return index;
  }
  return -1;
}

/** A faded note from another channel of the same pattern. */
export interface GhostLayer {
  notes: readonly MelodicNoteDto[];
  colour: string;
}

/** Everything one frame draws. */
export interface RollScene {
  notes: readonly MelodicNoteDto[];
  ghosts: readonly GhostLayer[];
  /** Indices into `notes`. */
  selection: ReadonlySet<number>;
  /** The edited channel's colour. */
  colour: string;
  accent: string;
  patternTicks: number;
  /** Where the transport is inside the loop, or `null` when it is not playing. */
  playheadTick: number | null;
  /** The marquee being dragged, in canvas pixels. */
  marquee: { x: number; y: number; w: number; h: number } | null;
  /** The slice tool's cut line, in canvas pixels, or `null`. */
  cutLine: { x0: number; y0: number; x1: number; y1: number } | null;
  /** Whether a note wide enough gets its name written on it. */
  labels: boolean;
  /** MIDI keys currently sounding from the typing keyboard or a click-preview. */
  heldKeys: ReadonlySet<number>;
  palette: Palette;
  dark: boolean;
}

/** rgba from a `#rrggbb`, so the renderer needs no theme helper import. */
function fade(hex: string, alpha: number): string {
  const parts = [1, 3, 5].map((offset) => Number.parseInt(hex.slice(offset, offset + 2), 16));
  return `rgba(${parts.join(',')},${alpha})`;
}

/**
 * The renderer. Owns its context and its device-pixel scaling; a caller resizes
 * it from a `ResizeObserver` and calls `draw` whenever anything changed.
 */
export class RollRenderer {
  private readonly ctx: CanvasRenderingContext2D;
  private dpr = 1;

  constructor(private readonly canvas: HTMLCanvasElement) {
    const ctx = canvas.getContext('2d');
    if (!ctx) throw new Error('this browser has no 2d canvas context');
    this.ctx = ctx;
  }

  resize(cssWidth: number, cssHeight: number, dpr: number): void {
    this.dpr = dpr;
    const width = Math.max(1, Math.round(cssWidth * dpr));
    const height = Math.max(1, Math.round(cssHeight * dpr));
    // Assigning width/height discards the bitmap even when the value is the
    // same, and this runs on every paint — only touch them on a real change.
    if (this.canvas.width !== width) this.canvas.width = width;
    if (this.canvas.height !== height) this.canvas.height = height;
    const styleWidth = `${cssWidth}px`;
    const styleHeight = `${cssHeight}px`;
    if (this.canvas.style.width !== styleWidth) this.canvas.style.width = styleWidth;
    if (this.canvas.style.height !== styleHeight) this.canvas.style.height = styleHeight;
  }

  draw(geo: RollGeometry, scene: RollScene): void {
    const ctx = this.ctx;
    ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    ctx.clearRect(0, 0, geo.width, geo.height);

    this.drawRows(geo, scene);
    this.drawVerticals(geo, scene);
    for (const layer of scene.ghosts) this.drawNotes(geo, scene, layer.notes, layer.colour, true);
    this.drawNotes(geo, scene, scene.notes, scene.colour, false);
    this.drawMarquee(geo, scene);
    this.drawCutLine(geo, scene);
    this.drawPlayhead(geo, scene);
    this.drawVelocity(geo, scene);
    // Last, so scrolled content cannot leak under them.
    this.drawRuler(geo, scene);
    this.drawKeys(geo, scene);
    this.drawScrollbars(geo, scene);
  }

  /** The horizontal key lanes, black keys shaded. */
  private drawRows(geo: RollGeometry, scene: RollScene): void {
    const ctx = this.ctx;
    const top = RULER_H;
    const bottom = top + gridHeight(geo);
    ctx.fillStyle = scene.palette.lane;
    ctx.fillRect(KEY_W, top, gridWidth(geo), gridHeight(geo));

    const first = Math.max(0, yToKey(bottom, geo));
    const last = Math.min(TOP_KEY, yToKey(top, geo));
    for (let key = first; key <= last; key += 1) {
      const y = keyToY(key, geo);
      if (y + geo.rowH < top || y > bottom) continue;
      const rowTop = Math.max(top, y);
      const rowH = Math.min(bottom, y + geo.rowH) - rowTop;
      if (isBlackKey(key)) {
        ctx.fillStyle = scene.palette.keyLane;
        ctx.fillRect(KEY_W, rowTop, gridWidth(geo), rowH);
      }
      if (scene.heldKeys.has(key)) {
        ctx.fillStyle = fade(scene.colour, scene.dark ? 0.22 : 0.18);
        ctx.fillRect(KEY_W, rowTop, gridWidth(geo), rowH);
      }
      ctx.fillStyle = key % 12 === 0 ? scene.palette.grid2 : scene.palette.grid1;
      const line = Math.round(y + geo.rowH) - 0.5;
      if (line > top && line < bottom) {
        ctx.fillRect(KEY_W, line, gridWidth(geo), 1);
      }
    }

    // Past the end of the loop is not part of the pattern: shade it, so a note
    // dragged there is visibly outside rather than silently refused.
    const end = tickToX(scene.patternTicks, geo);
    if (end < geo.width) {
      ctx.fillStyle = fade(scene.palette.sunk, 0.65);
      ctx.fillRect(Math.max(KEY_W, end), top, geo.width - Math.max(KEY_W, end), gridHeight(geo));
    }
  }

  /** Step, beat and bar lines. */
  private drawVerticals(geo: RollGeometry, scene: RollScene): void {
    const ctx = this.ctx;
    const top = RULER_H;
    const height = gridHeight(geo);
    const stepPx = TICKS_PER_STEP * geo.pxPerTick;
    const firstTick = Math.max(0, Math.floor(xToTick(KEY_W, geo) / TICKS_PER_STEP) * TICKS_PER_STEP);
    const lastTick = Math.min(scene.patternTicks, xToTick(geo.width, geo));
    const drawSteps = stepPx >= 8;

    for (let tick = firstTick; tick <= lastTick; tick += TICKS_PER_STEP) {
      const x = Math.round(tickToX(tick, geo)) - 0.5;
      if (x < KEY_W) continue;
      const isBar = tick % TICKS_PER_BAR === 0;
      const isBeat = tick % PPQ === 0;
      if (!isBeat && !drawSteps) continue;
      ctx.fillStyle = isBar ? scene.palette.line2 : isBeat ? scene.palette.grid2 : scene.palette.grid1;
      ctx.fillRect(x, top, 1, height);
    }
  }

  /** Notes, ghosted or live. */
  private drawNotes(
    geo: RollGeometry,
    scene: RollScene,
    notes: readonly MelodicNoteDto[],
    colour: string,
    ghost: boolean,
  ): void {
    const ctx = this.ctx;
    const top = RULER_H;
    const bottom = top + gridHeight(geo);
    ctx.save();
    ctx.beginPath();
    ctx.rect(KEY_W, top, gridWidth(geo), gridHeight(geo));
    ctx.clip();

    notes.forEach((note, index) => {
      const x = tickToX(note.start, geo);
      const w = Math.max(3, note.length * geo.pxPerTick - 1);
      const y = keyToY(note.key, geo) + 1;
      if (x + w < KEY_W || x > geo.width || y + geo.rowH < top || y > bottom) return;
      const h = geo.rowH - 2;
      const selected = !ghost && scene.selection.has(index);

      ctx.fillStyle = ghost
        ? fade(colour, 0.16)
        : fade(colour, 0.45 + note.velocity * 0.55);
      roundRect(ctx, x, y, w, h, 3);
      ctx.fill();

      ctx.lineWidth = selected ? 2 : 1;
      ctx.strokeStyle = ghost
        ? fade(colour, 0.3)
        : selected
          ? scene.palette.txt
          : scene.palette.noteStroke;
      roundRect(ctx, x + 0.5, y + 0.5, w - 1, h - 1, 3);
      ctx.stroke();

      if (!ghost && scene.labels && w >= 34 && h >= 9) {
        ctx.fillStyle = scene.palette.noteInk;
        ctx.font = `500 ${Math.min(9.5, h - 3)}px 'Geist Mono Variable', ui-monospace, monospace`;
        ctx.textBaseline = 'middle';
        ctx.fillText(noteName(note.key), x + 4, y + h / 2 + 0.5);
      }
    });
    ctx.restore();
  }

  private drawMarquee(_geo: RollGeometry, scene: RollScene): void {
    if (!scene.marquee) return;
    const ctx = this.ctx;
    const { x, y, w, h } = scene.marquee;
    ctx.fillStyle = fade(scene.accent, 0.12);
    ctx.fillRect(x, y, w, h);
    ctx.strokeStyle = scene.accent;
    ctx.lineWidth = 1;
    ctx.strokeRect(x + 0.5, y + 0.5, w, h);
  }

  /** The slice tool's cut, clipped to the grid so it cannot run under the keys. */
  private drawCutLine(geo: RollGeometry, scene: RollScene): void {
    if (!scene.cutLine) return;
    const ctx = this.ctx;
    const { x0, y0, x1, y1 } = scene.cutLine;
    ctx.save();
    ctx.beginPath();
    ctx.rect(KEY_W, RULER_H, gridWidth(geo), gridHeight(geo));
    ctx.clip();
    ctx.beginPath();
    ctx.moveTo(x0, y0);
    ctx.lineTo(x1, y1);
    ctx.strokeStyle = scene.accent;
    ctx.lineWidth = 1.5;
    ctx.setLineDash([5, 4]);
    ctx.stroke();
    ctx.restore();
  }

  private drawPlayhead(geo: RollGeometry, scene: RollScene): void {
    if (scene.playheadTick === null) return;
    const x = Math.round(tickToX(scene.playheadTick, geo)) - 0.5;
    if (x < KEY_W || x > geo.width) return;
    const ctx = this.ctx;
    ctx.fillStyle = scene.accent;
    ctx.fillRect(x, RULER_H, 1, gridHeight(geo));
  }

  /**
   * The velocity lane: one bar per note, at the note's own start.
   *
   * Only velocity. Pan and modulation are drawn as disabled tabs by the view,
   * because a `MelodicNote` carries a key, a start, a length and a velocity --
   * and nothing else.
   */
  private drawVelocity(geo: RollGeometry, scene: RollScene): void {
    const ctx = this.ctx;
    const top = geo.height - VELO_H;
    ctx.fillStyle = scene.palette.panel;
    ctx.fillRect(0, top, geo.width, VELO_H);
    ctx.fillStyle = scene.palette.line2;
    ctx.fillRect(0, top, geo.width, 1);

    const laneTop = top + VELO_LABEL_H;
    const laneHeight = VELO_H - VELO_LABEL_H - 6;

    ctx.save();
    ctx.beginPath();
    ctx.rect(KEY_W, laneTop, gridWidth(geo), laneHeight + 6);
    ctx.clip();

    // Beat lines, so a velocity bar can be read against the bar it is in.
    const firstTick = Math.max(0, Math.floor(xToTick(KEY_W, geo) / PPQ) * PPQ);
    for (let tick = firstTick; tick <= scene.patternTicks; tick += PPQ) {
      const x = Math.round(tickToX(tick, geo)) - 0.5;
      if (x < KEY_W) continue;
      if (x > geo.width) break;
      ctx.fillStyle = tick % TICKS_PER_BAR === 0 ? scene.palette.line2 : scene.palette.grid2;
      ctx.fillRect(x, laneTop, 1, laneHeight);
    }

    const barWidth = Math.max(3, Math.min(10, TICKS_PER_STEP * geo.pxPerTick * 0.5));
    scene.notes.forEach((note, index) => {
      const x = tickToX(note.start, geo) + 1;
      if (x + barWidth < KEY_W || x > geo.width) return;
      const selected = scene.selection.has(index);
      const height = note.velocity * laneHeight;
      ctx.fillStyle = fade(scene.colour, selected ? 1 : 0.55);
      ctx.fillRect(x, laneTop + laneHeight - height, barWidth, height);
      ctx.fillStyle = selected ? scene.palette.txt : fade(scene.colour, 0.9);
      ctx.fillRect(x - 1, laneTop + laneHeight - height - 3, barWidth + 2, 3);
    });
    ctx.restore();
  }

  private drawRuler(geo: RollGeometry, scene: RollScene): void {
    const ctx = this.ctx;
    ctx.fillStyle = scene.palette.panel;
    ctx.fillRect(0, 0, geo.width, RULER_H);
    ctx.fillStyle = scene.palette.line2;
    ctx.fillRect(0, RULER_H - 1, geo.width, 1);

    ctx.save();
    ctx.beginPath();
    ctx.rect(KEY_W, 0, gridWidth(geo), RULER_H);
    ctx.clip();
    const firstBeat = Math.max(0, Math.floor(xToTick(KEY_W, geo) / PPQ) * PPQ);
    ctx.textBaseline = 'middle';
    for (let tick = firstBeat; tick <= scene.patternTicks; tick += PPQ) {
      const x = Math.round(tickToX(tick, geo)) - 0.5;
      if (x > geo.width) break;
      const isBar = tick % TICKS_PER_BAR === 0;
      ctx.fillStyle = isBar ? scene.palette.line2 : scene.palette.line;
      ctx.fillRect(x, 0, 1, RULER_H);
      if (isBar) {
        ctx.fillStyle = scene.palette.txt2;
        ctx.font = "500 10px 'Geist Mono Variable', ui-monospace, monospace";
        ctx.fillText(String(tick / TICKS_PER_BAR + 1), x + 5, RULER_H / 2);
      }
    }
    ctx.restore();

    // The corner, over both.
    ctx.fillStyle = scene.palette.panel;
    ctx.fillRect(0, 0, KEY_W, RULER_H);
    ctx.fillStyle = scene.palette.line2;
    ctx.fillRect(KEY_W - 1, 0, 1, RULER_H);
  }

  /**
   * The keyboard down the left: equal-height chromatic rows (a piano roll, not
   * a geometric piano), but staggered like the real thing — black keys stop
   * short so the ivory fronts of the whites wrap around them.
   *
   * `palette.ink` is text-on-accent, white in the light theme, so it must not
   * be used as the black-key fill.
   */
  private drawKeys(geo: RollGeometry, scene: RollScene): void {
    const ctx = this.ctx;
    const top = RULER_H;
    const height = gridHeight(geo);
    const blackW = Math.round(KEY_W * 0.58);
    const ivory = scene.palette.ivory;
    const ebony = scene.palette.ebony;
    const ivoryEdge = scene.palette.ivoryEdge;

    ctx.save();
    ctx.beginPath();
    ctx.rect(0, top, KEY_W, height);
    ctx.clip();
    ctx.fillStyle = ivory;
    ctx.fillRect(0, top, KEY_W, height);

    const first = Math.max(0, yToKey(top + height, geo));
    const last = Math.min(TOP_KEY, yToKey(top, geo));

    for (let key = first; key <= last; key += 1) {
      if (isBlackKey(key)) continue;
      const y = keyToY(key, geo);
      const pressed = scene.heldKeys.has(key);
      ctx.fillStyle = pressed ? scene.colour : ivory;
      ctx.fillRect(0, y, KEY_W, geo.rowH);
      if (pressed) {
        ctx.fillStyle = scene.palette.sheen;
        ctx.fillRect(0, y, KEY_W, 1);
      }
      ctx.fillStyle = pressed ? fade(scene.colour, 0.65) : ivoryEdge;
      ctx.fillRect(0, Math.round(y + geo.rowH) - 1, KEY_W, 1);
      if (key % 12 === 0) {
        ctx.fillStyle = fade(scene.accent, pressed ? 0.95 : 0.7);
        ctx.fillRect(0, Math.round(y + geo.rowH) - 1, KEY_W, 1);
      }
    }

    for (let key = first; key <= last; key += 1) {
      if (!isBlackKey(key)) continue;
      const y = keyToY(key, geo);
      const pressed = scene.heldKeys.has(key);
      ctx.fillStyle = pressed ? scene.colour : ebony;
      ctx.fillRect(0, y, blackW, geo.rowH);
      if (!pressed) {
        ctx.fillStyle = scene.palette.sheen;
        ctx.fillRect(0, y, blackW, 1);
        ctx.fillStyle = scene.palette.noteStroke;
        ctx.fillRect(blackW - 1, y, 1, geo.rowH);
      } else {
        ctx.fillStyle = scene.palette.keyLane;
        ctx.fillRect(0, y, blackW, geo.rowH);
        ctx.fillStyle = scene.palette.sheen;
        ctx.fillRect(0, y, blackW, 1);
      }
    }

    ctx.textBaseline = 'middle';
    ctx.textAlign = 'right';
    for (let key = first; key <= last; key += 1) {
      if (isBlackKey(key)) continue;
      const isC = key % 12 === 0;
      if (geo.rowH < 11 && !isC) continue;
      const y = keyToY(key, geo);
      const pressed = scene.heldKeys.has(key);
      ctx.fillStyle = pressed
        ? scene.palette.ink
        : isC
          ? scene.palette.txt
          : scene.palette.dim2;
      ctx.font = `${isC ? 600 : 400} ${isC ? 9 : 8}px 'Geist Mono Variable', ui-monospace, monospace`;
      ctx.fillText(noteName(key), KEY_W - 6, y + geo.rowH / 2);
    }

    ctx.textAlign = 'left';
    ctx.fillStyle = scene.palette.line2;
    ctx.fillRect(KEY_W - 1, top, 1, height);
    ctx.restore();
  }

  /**
   * FL-style scrollbars: the thumb is the visible window, and each end is a
   * zoom handle. Drawn last so they sit on top of the grid's right/bottom edge.
   */
  private drawScrollbars(geo: RollGeometry, scene: RollScene): void {
    const ctx = this.ctx;
    const h = hScrollBar(geo, scene.patternTicks);
    const v = vScrollBar(geo);
    ctx.fillStyle = scene.palette.sunk;
    ctx.fillRect(h.track.x, h.track.y, h.track.w, h.track.h);
    ctx.fillRect(v.track.x, v.track.y, v.track.w, v.track.h);
    ctx.fillStyle = scene.palette.panel;
    ctx.fillRect(0, h.track.y, KEY_W, SCROLL_W);
    ctx.fillRect(v.track.x, h.track.y, SCROLL_W, SCROLL_W);
    this.paintThumb(h.thumb, scene, 'h');
    this.paintThumb(v.thumb, scene, 'v');
  }

  private paintThumb(
    thumb: { x: number; y: number; w: number; h: number },
    scene: RollScene,
    axis: 'h' | 'v',
  ): void {
    const ctx = this.ctx;
    ctx.fillStyle = scene.palette.line2;
    roundRect(ctx, thumb.x, thumb.y, thumb.w, thumb.h, 3);
    ctx.fill();
    ctx.fillStyle = scene.palette.thumbMark;
    if (axis === 'h') {
      const inset = thumb.y + 2;
      const tall = Math.max(1, thumb.h - 4);
      ctx.fillRect(thumb.x + 2, inset, 2, tall);
      ctx.fillRect(thumb.x + thumb.w - 4, inset, 2, tall);
    } else {
      const inset = thumb.x + 2;
      const wide = Math.max(1, thumb.w - 4);
      ctx.fillRect(inset, thumb.y + 2, wide, 2);
      ctx.fillRect(inset, thumb.y + thumb.h - 4, wide, 2);
    }
  }
}

/** A rounded rectangle path, because `roundRect` is not everywhere yet. */
function roundRect(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  w: number,
  h: number,
  r: number,
): void {
  const radius = Math.min(r, w / 2, h / 2);
  ctx.beginPath();
  ctx.moveTo(x + radius, y);
  ctx.arcTo(x + w, y, x + w, y + h, radius);
  ctx.arcTo(x + w, y + h, x, y + h, radius);
  ctx.arcTo(x, y + h, x, y, radius);
  ctx.arcTo(x, y, x + w, y, radius);
  ctx.closePath();
}
