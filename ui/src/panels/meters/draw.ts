/**
 * Canvas drawing for the stereo meter. Deliberately free of Solid and of any
 * IPC types beyond `MeterFrame`: this is the code the Phase 1 IPC perf spike
 * will instrument, so it must stay measurable in isolation.
 *
 * The hot path (`MeterRenderer.draw`) allocates nothing -- colors are resolved
 * on construction/resize and peak-hold state lives in preallocated fields.
 */
import type { MeterFrame } from '../../api/types';

/** Bottom of the displayed scale, in dBFS. */
const MIN_DB = -60;
/** Top of the displayed scale, in dBFS (a little headroom above 0). */
const MAX_DB = 6;
/** Peak-hold fall rate, in dB per second. */
const PEAK_FALL_DB_PER_SECOND = 24;
/** How long a peak marker sits before it starts falling, in ms. */
const PEAK_HOLD_MS = 900;

const GRID_DB = [0, -6, -12, -24, -36, -48];

const LABEL_FONT = '9px ui-monospace, Consolas, monospace';

export interface MeterColors {
  background: string;
  trough: string;
  rms: string;
  peak: string;
  hot: string;
  clip: string;
  grid: string;
  label: string;
}

export const DEFAULT_METER_COLORS: MeterColors = {
  background: '#101216',
  trough: '#1b1f26',
  rms: '#2f9e6b',
  peak: '#4fd6a0',
  hot: '#e0b341',
  clip: '#e05263',
  grid: '#272c35',
  label: '#7c8494',
};

/** Reads theme tokens off an element so the canvas matches the CSS palette. */
export function readMeterColors(el: Element): MeterColors {
  const cs = getComputedStyle(el);
  const pick = (name: string, fallback: string): string => {
    const v = cs.getPropertyValue(name).trim();
    return v.length > 0 ? v : fallback;
  };
  return {
    background: pick('--meter-bg', DEFAULT_METER_COLORS.background),
    trough: pick('--meter-trough', DEFAULT_METER_COLORS.trough),
    rms: pick('--meter-rms', DEFAULT_METER_COLORS.rms),
    peak: pick('--meter-peak', DEFAULT_METER_COLORS.peak),
    hot: pick('--meter-hot', DEFAULT_METER_COLORS.hot),
    clip: pick('--meter-clip', DEFAULT_METER_COLORS.clip),
    grid: pick('--meter-grid', DEFAULT_METER_COLORS.grid),
    label: pick('--meter-label', DEFAULT_METER_COLORS.label),
  };
}

/** Linear amplitude to dBFS, floored at MIN_DB. */
export function amplitudeToDb(amplitude: number): number {
  if (amplitude <= 0) return MIN_DB;
  return Math.max(MIN_DB, 20 * Math.log10(amplitude));
}

/** dBFS to a 0..1 fraction of the meter scale. */
export function dbToFraction(db: number): number {
  const clamped = Math.min(MAX_DB, Math.max(MIN_DB, db));
  return (clamped - MIN_DB) / (MAX_DB - MIN_DB);
}

const EMPTY_FRAME: MeterFrame = {
  peakL: 0,
  peakR: 0,
  rmsL: 0,
  rmsR: 0,
  samplePos: 0,
  playing: false,
  // This panel is the *master* meter; per-track levels are the mixer's, and it
  // reads them from the bus itself.
  tracks: [],
};

/**
 * Stateful renderer: owns the canvas 2D context, the device-pixel-ratio backing
 * store, and peak-hold decay. One instance per canvas.
 */
export class MeterRenderer {
  private readonly ctx: CanvasRenderingContext2D;
  private colors: MeterColors;
  private cssWidth = 0;
  private cssHeight = 0;
  private dpr = 1;

  /** Held peak per channel, in dB, with the time it was set. */
  private readonly holdDb: [number, number] = [MIN_DB, MIN_DB];
  private readonly holdAtMs: [number, number] = [0, 0];
  private lastDrawMs = 0;

  constructor(canvas: HTMLCanvasElement, colors: MeterColors = DEFAULT_METER_COLORS) {
    const ctx = canvas.getContext('2d', { alpha: false });
    if (!ctx) throw new Error('2D canvas context unavailable');
    this.ctx = ctx;
    this.colors = colors;
  }

  setColors(colors: MeterColors): void {
    this.colors = colors;
  }

  /**
   * Sizes the backing store for the current device pixel ratio. Call on mount
   * and on resize; cheap enough for a ResizeObserver callback.
   */
  resize(cssWidth: number, cssHeight: number, dpr: number): void {
    const canvas = this.ctx.canvas;
    this.cssWidth = cssWidth;
    this.cssHeight = cssHeight;
    this.dpr = dpr;
    canvas.width = Math.max(1, Math.round(cssWidth * dpr));
    canvas.height = Math.max(1, Math.round(cssHeight * dpr));
    canvas.style.width = cssWidth + 'px';
    canvas.style.height = cssHeight + 'px';
  }

  /** Draws one frame. `frame` may be null before the first packet arrives. */
  draw(frame: MeterFrame | null, nowMs: number): void {
    const f = frame ?? EMPTY_FRAME;
    const dt = this.lastDrawMs === 0 ? 0 : (nowMs - this.lastDrawMs) / 1000;
    this.lastDrawMs = nowMs;

    const ctx = this.ctx;
    const w = this.cssWidth;
    const h = this.cssHeight;
    if (w <= 0 || h <= 0) return;

    ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    ctx.fillStyle = this.colors.background;
    ctx.fillRect(0, 0, w, h);

    // Layout: two vertical bars on the left, dB scale in a right-hand gutter.
    const gutter = 26;
    const padding = 6;
    const trackTop = padding;
    const trackHeight = Math.max(4, h - padding * 2);
    const trackWidth = Math.max(4, w - gutter - padding * 2);
    const barGap = 6;
    const barWidth = Math.max(3, (trackWidth - barGap) / 2);

    this.drawGrid(trackTop, trackHeight, padding, trackWidth);

    const peaks: readonly [number, number] = [f.peakL, f.peakR];
    const rmsValues: readonly [number, number] = [f.rmsL, f.rmsR];

    for (let ch = 0; ch < 2; ch += 1) {
      const x = padding + ch * (barWidth + barGap);
      const peakDb = amplitudeToDb(ch === 0 ? peaks[0] : peaks[1]);
      const rmsDb = amplitudeToDb(ch === 0 ? rmsValues[0] : rmsValues[1]);
      this.updateHold(ch, peakDb, nowMs, dt);

      ctx.fillStyle = this.colors.trough;
      ctx.fillRect(x, trackTop, barWidth, trackHeight);

      const peakPx = dbToFraction(peakDb) * trackHeight;
      if (peakDb > MIN_DB && peakPx > 0.5) {
        ctx.fillStyle =
          peakDb >= 0 ? this.colors.clip : peakDb >= -6 ? this.colors.hot : this.colors.peak;
        ctx.fillRect(x, trackTop + trackHeight - peakPx, barWidth, peakPx);
      }

      const rmsPx = dbToFraction(rmsDb) * trackHeight;
      if (rmsDb > MIN_DB && rmsPx > 0.5) {
        ctx.fillStyle = this.colors.rms;
        ctx.fillRect(x + 1, trackTop + trackHeight - rmsPx, Math.max(1, barWidth - 2), rmsPx);
      }

      const held = ch === 0 ? this.holdDb[0] : this.holdDb[1];
      if (held > MIN_DB) {
        const holdY = trackTop + trackHeight - dbToFraction(held) * trackHeight;
        ctx.fillStyle = held >= 0 ? this.colors.clip : this.colors.label;
        ctx.fillRect(x, Math.max(trackTop, holdY - 1), barWidth, 2);
      }
    }
  }

  private drawGrid(
    trackTop: number,
    trackHeight: number,
    padding: number,
    trackWidth: number,
  ): void {
    const ctx = this.ctx;
    ctx.font = LABEL_FONT;
    ctx.textBaseline = 'middle';
    ctx.textAlign = 'left';

    for (const db of GRID_DB) {
      const y = trackTop + trackHeight - dbToFraction(db) * trackHeight;
      ctx.fillStyle = this.colors.grid;
      ctx.fillRect(padding, Math.round(y), trackWidth, 1);
      ctx.fillStyle = this.colors.label;
      ctx.fillText(String(db), padding + trackWidth + 4, y);
    }
  }

  private updateHold(ch: number, peakDb: number, nowMs: number, dt: number): void {
    const current = ch === 0 ? this.holdDb[0] : this.holdDb[1];
    if (peakDb >= current) {
      this.holdDb[ch === 0 ? 0 : 1] = peakDb;
      this.holdAtMs[ch === 0 ? 0 : 1] = nowMs;
      return;
    }
    const heldAt = ch === 0 ? this.holdAtMs[0] : this.holdAtMs[1];
    if (nowMs - heldAt < PEAK_HOLD_MS) return;
    this.holdDb[ch === 0 ? 0 : 1] = Math.max(peakDb, current - PEAK_FALL_DB_PER_SECOND * dt);
  }
}
