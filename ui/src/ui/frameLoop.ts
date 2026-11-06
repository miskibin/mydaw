// The one requestAnimationFrame loop. Everything that animates (playhead, clock, meters,
// effect visualisations) registers a task here and writes straight to canvas/DOM.
import { theme } from './theme';

export type FrameTask = (now: number) => void;

const tasks = new Set<FrameTask>();
let raf = 0;

// Work time per frame, for the performance check (window.__wavePerf).
const N = 600;
const work = new Float32Array(N);
const gaps = new Float32Array(N);
let wi = 0, wn = 0, lastStart = 0;

function frame() {
  raf = requestAnimationFrame(frame);
  // Tasks get performance.now(), not the rAF timestamp: telemetry arrival times are taken with
  // performance.now() too, and some webviews (WebKitGTK) use a different base for rAF stamps.
  const t0 = performance.now();
  for (const t of tasks) {
    try { t(t0); } catch (e) { console.error(e); }
  }
  const t1 = performance.now();
  work[wi] = t1 - t0;
  gaps[wi] = lastStart ? t0 - lastStart : 16.7;
  lastStart = t0;
  wi = (wi + 1) % N; wn = Math.min(N, wn + 1);
}

export function addFrameTask(t: FrameTask): () => void {
  tasks.add(t);
  if (!raf) raf = requestAnimationFrame(frame);
  return () => { tasks.delete(t); };
}

function stats(a: Float32Array) {
  const v = Array.from(a.subarray(0, wn)).sort((x, y) => x - y);
  if (!v.length) return { avg: 0, p95: 0, max: 0 };
  return { avg: v.reduce((s, x) => s + x, 0) / v.length, p95: v[Math.floor(v.length * 0.95)], max: v[v.length - 1] };
}

(window as unknown as { __wavePerf: unknown }).__wavePerf = {
  reset() { wi = 0; wn = 0; lastStart = 0; },
  get work() { return stats(work); },
  get gaps() { return stats(gaps); },
  get frames() { return wn; },
};

/** Size a canvas backing store to its CSS box × devicePixelRatio. Returns true if it changed. */
export function fitCanvas(cv: HTMLCanvasElement, W: number, H: number): boolean {
  const dpr = window.devicePixelRatio || 1;
  const w = Math.max(1, Math.round(W * dpr)), h = Math.max(1, Math.round(H * dpr));
  if (cv.width === w && cv.height === h) return false;
  cv.width = w; cv.height = h;
  return true;
}

/** Bumped when web fonts finish loading, so canvases drawn with fallback glyphs redraw. */
export const fonts = { epoch: 0 };
if (typeof document !== 'undefined' && document.fonts) {
  document.fonts.ready.then(() => { fonts.epoch++; });
  document.fonts.addEventListener('loadingdone', () => { fonts.epoch++; });
}

/** Offscreen layer cache: re-rendered only when `key` changes. */
export class Layer {
  cv: HTMLCanvasElement | OffscreenCanvas | null = null;
  key = '';
  get(W: number, H: number, key: string, draw: (g: CanvasRenderingContext2D, W: number, H: number) => void) {
    const dpr = window.devicePixelRatio || 1;
    const fullKey = key + '|' + W + 'x' + H + '@' + dpr + '#' + fonts.epoch + 't' + theme.epoch;
    if (this.cv && this.key === fullKey) return this.cv;
    if (!this.cv) this.cv = document.createElement('canvas');
    const cv = this.cv as HTMLCanvasElement;
    const w = Math.max(1, Math.round(W * dpr)), h = Math.max(1, Math.round(H * dpr));
    if (cv.width !== w || cv.height !== h) { cv.width = w; cv.height = h; }
    const g = cv.getContext('2d')!;
    g.setTransform(1, 0, 0, 1, 0, 0);
    g.clearRect(0, 0, w, h);
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    draw(g, W, H);
    this.key = fullKey;
    return cv;
  }
}
