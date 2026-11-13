// The time ruler of the Playlist, the Piano roll and the Pitch editor: one look and one set of
// gestures. Click = the start position (playback jumps there), drag = a loop range, drag a
// loop edge = move it, Shift+drag inside = move the whole loop, double-click = no loop. Snaps
// to the grid; Alt = free. Each view says what its beats are and where loops go.
import { clamp } from '../lib/format';
import type { BeatRange } from '../lib/patLoop';
import { dragLoop, drawnLoop, loopEdgeAt, sameRange } from '../lib/rulerEdit';
import { snapRound } from '../lib/snap';
import { t } from '../lib/i18n';
import { dblCheck, track } from './drag';
import { rgba, T } from './theme';

/** Ruler height (px). */
export const RULER_H = 26;

/**
 * Draws the ruler `W` px wide: beat `b` at x = b × bw − offX. Bar numbers count from beat 0
 * (bar 1); `loop` is the green band, `start` the start marker (none: not drawn).
 */
export function drawRuler(g: CanvasRenderingContext2D, offX: number, W: number, bw: number, loop: BeatRange | null | undefined, start: number | null, H = RULER_H) {
  g.fillStyle = T.rulerBg; g.fillRect(0, 0, W, H);
  if (!(bw > 0)) return;
  g.save(); g.translate(-offX, 0);
  const x0 = offX, x1 = offX + W;
  if (loop) {
    g.fillStyle = rgba(T.green, 0.16); g.fillRect(loop.a * bw, 0, (loop.b - loop.a) * bw, H);
    g.fillStyle = T.green; g.fillRect(Math.round(loop.a * bw), 0, 1, H); g.fillRect(Math.round(loop.b * bw) - 1, 0, 1, H);
  }
  // beat ticks along the bottom (when they are not crammed together)
  g.fillStyle = T.fg6;
  if (bw >= 3) for (let b = Math.max(0, Math.floor(x0 / bw)); b * bw <= x1; b++) g.fillRect(Math.round(b * bw), H - 5, 1, 5);
  const barW = bw * 4, barStep = barW < 4 ? 32 : barW < 9 ? 16 : barW < 16 ? 8 : barW < 36 ? 4 : barW < 64 ? 2 : 1;
  g.font = '500 10px "Geist Mono Variable", "Geist Mono", monospace';
  for (let bar = Math.max(0, Math.floor(x0 / barW / barStep) * barStep); bar * barW <= x1; bar += barStep) {
    const x = Math.round(bar * barW);
    g.fillStyle = T.fg6; g.fillRect(x, 0, 1, H);
    g.fillStyle = T.rulerText; g.fillText(String(bar + 1), x + 4, H - 11);
  }
  if (start != null) {
    const sx = start * bw;
    g.fillStyle = T.green; g.beginPath(); g.moveTo(sx - 5, 0); g.lineTo(sx + 5, 0); g.lineTo(sx, 7); g.closePath(); g.fill();
  }
  g.restore();
  g.fillStyle = T.bg0; g.fillRect(0, H - 1, W, 1);
}

/** What a ruler measures and where its gestures go. */
export interface RulerHost {
  /** Beat under client x (unsnapped; read live: the view may scroll during a drag). */
  beatAt(clientX: number): number;
  /** Px per beat now. */
  bw(): number;
  /** Grid unit in beats (Alt: free = 0). */
  unit(alt: boolean): number;
  /** The last beat a position or a loop can reach. */
  max(): number;
  /** The loop range shown on this ruler. */
  loop(): BeatRange | null;
  /** A loop edit once the pointer really moves: `set` on each move, `end` on release (one undo step where loops are undoable). */
  edit(label: string): { set(r: BeatRange): void; end(): void };
  /** Double-click: the loop goes. */
  clear(): void;
  /** Click: the start position. */
  seek(beat: number): void;
  /** A loop was drawn (not an edge move): the view goes to its start. */
  drawn(r: BeatRange): void;
  /** Pointer tracking for drags (the Playlist's auto-scrolls at the edges). */
  track?: (move: (e: PointerEvent) => void, up: (e: PointerEvent) => void, cursor: string) => void;
  /** Double-click key: one per ruler. */
  key: string;
}

/** The cursor over a ruler at `e`: a loop edge resizes, Shift inside the loop grabs it. */
export function rulerCursor(h: RulerHost, e: PointerEvent | MouseEvent): string {
  const ed = loopEdgeAt(h.loop(), h.beatAt(e.clientX), h.bw(), e.shiftKey);
  return ed === 'a' || ed === 'b' ? 'ew-resize' : ed === 'body' ? 'grab' : 'pointer';
}

/** A left press on a ruler. */
export function rulerDown(h: RulerHost, e: PointerEvent) {
  if (e.button !== 0) return;
  e.preventDefault();
  if (dblCheck(h.key)) { if (h.loop()) h.clear(); return; }
  const tr = h.track ?? ((m, u, c) => track(m, u, c));
  const snapAt = (ev: PointerEvent) => clamp(snapRound(h.beatAt(ev.clientX), h.unit(ev.altKey)), 0, h.max());
  const b0 = snapAt(e);
  let g: { set(r: BeatRange): void; end(): void } | null = null;
  // an existing loop: grab its start or end edge (or with Shift its body) instead of drawing a new one
  const lp = h.loop(), edge = loopEdgeAt(lp, h.beatAt(e.clientX), h.bw(), e.shiftKey);
  if (lp && edge) {
    const raw0 = h.beatAt(e.clientX);
    let last = lp;
    tr(ev => {
      if (!g && Math.abs(ev.clientX - e.clientX) > 2) g = h.edit(edge === 'body' ? t('a.pl.moveLoop') : t('a.pl.loopRange'));
      if (!g) return;
      const r = dragLoop(lp, edge, h.beatAt(ev.clientX), raw0, h.unit(ev.altKey), h.max());
      if (sameRange(r, last)) return;
      last = r; g.set(r);
    }, () => { if (g) g.end(); else h.seek(b0); }, edge === 'body' ? 'grabbing' : 'ew-resize');
    return;
  }
  let last: BeatRange | null = lp;
  tr(ev => {
    if (!g && Math.abs(ev.clientX - e.clientX) > 4) g = h.edit(t('a.pl.loop'));
    const r = drawnLoop(b0, snapAt(ev));
    if (g && r && !sameRange(r, last)) { last = r; g.set(r); }
  }, () => {
    if (!g) { h.seek(b0); return; }
    g.end();
    const r = h.loop(); if (r) h.drawn(r);
  }, 'ew-resize');
}
