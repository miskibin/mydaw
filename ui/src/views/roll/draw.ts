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
  zoom: number,
  scrollX: number,
  scrollY: number,
): RollGeometry {
  return {
    width,
    height,
    scrollX,
    scrollY,
    pxPerTick: (STEP_W * zoom) / TICKS_PER_STEP,
    rowH: ROW_H,
  };
}

/** The grid's viewport, excluding the key column, ruler and velocity lane. */
export function gridWidth(geo: RollGeometry): number {
  return Math.max(0, geo.width - KEY_W);
}

export function gridHeight(geo: RollGeometry): number {
  return Math.max(0, geo.height - RULER_H - VELO_H);
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
export function inRuler(x: number, y: number): boolean {
  return x >= KEY_W && y < RULER_H;
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
  /** Whether a note wide enough gets its name written on it. */
  labels: boolean;
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
    this.canvas.width = Math.max(1, Math.round(cssWidth * dpr));
    this.canvas.height = Math.max(1, Math.round(cssHeight * dpr));
    this.canvas.style.width = `${cssWidth}px`;
    this.canvas.style.height = `${cssHeight}px`;
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
    this.drawPlayhead(geo, scene);
    this.drawVelocity(geo, scene);
    // Last, so scrolled content cannot leak under them.
    this.drawRuler(geo, scene);
    this.drawKeys(geo, scene);
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
      if (isBlackKey(key)) {
        ctx.fillStyle = scene.dark ? 'rgba(255,255,255,.028)' : 'rgba(0,0,0,.045)';
        ctx.fillRect(KEY_W, Math.max(top, y), gridWidth(geo), geo.rowH);
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
          ? scene.dark
            ? '#ffffff'
            : '#14171c'
          : 'rgba(0,0,0,.45)';
      roundRect(ctx, x + 0.5, y + 0.5, w - 1, h - 1, 3);
      ctx.stroke();

      if (!ghost && scene.labels && w >= 34 && h >= 9) {
        ctx.fillStyle = scene.dark ? 'rgba(6,10,14,.85)' : 'rgba(255,255,255,.95)';
        ctx.font = `500 ${Math.min(9.5, h - 3)}px 'Geist Mono', ui-monospace, monospace`;
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
      ctx.fillStyle = selected ? (scene.dark ? '#ffffff' : '#14171c') : fade(scene.colour, 0.9);
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
        ctx.font = "500 10px 'Geist Mono', ui-monospace, monospace";
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

  /** The keyboard down the left, playable and always on top. */
  private drawKeys(geo: RollGeometry, scene: RollScene): void {
    const ctx = this.ctx;
    const top = RULER_H;
    const height = gridHeight(geo);
    ctx.save();
    ctx.beginPath();
    ctx.rect(0, top, KEY_W, height);
    ctx.clip();
    ctx.fillStyle = scene.palette.panel;
    ctx.fillRect(0, top, KEY_W, height);

    const first = Math.max(0, yToKey(top + height, geo));
    const last = Math.min(TOP_KEY, yToKey(top, geo));
    ctx.textBaseline = 'middle';
    ctx.textAlign = 'right';
    for (let key = first; key <= last; key += 1) {
      const y = keyToY(key, geo);
      const black = isBlackKey(key);
      ctx.fillStyle = black ? scene.palette.ink : scene.dark ? '#e9ecf1' : '#ffffff';
      ctx.fillRect(0, y, KEY_W, geo.rowH - 1);
      ctx.fillStyle = scene.dark ? 'rgba(0,0,0,.5)' : 'rgba(0,0,0,.08)';
      ctx.fillRect(0, y + geo.rowH - 1, KEY_W, 1);
      if (!black) {
        ctx.fillStyle = scene.dark ? '#3a4048' : '#6a7280';
        ctx.font = "400 8.5px 'Geist Mono', ui-monospace, monospace";
        ctx.fillText(noteName(key), KEY_W - 7, y + geo.rowH / 2);
      }
      if (key % 12 === 0) {
        ctx.fillStyle = fade(scene.accent, 0.5);
        ctx.fillRect(0, y + geo.rowH - 1, KEY_W, 1);
      }
    }
    ctx.textAlign = 'left';
    ctx.fillStyle = scene.palette.line2;
    ctx.fillRect(KEY_W - 1, top, 1, height);
    ctx.restore();
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
