import { describe, expect, it } from 'vitest';

import { TICKS_PER_STEP } from '../../lib/pattern';
import { ROW_H, STEP_W, TOP_KEY } from './draw';
import { fitNotes, notesBounds } from './fit';

describe('notesBounds', () => {
  it('returns null when there is nothing to frame', () => {
    expect(notesBounds([])).toBeNull();
  });

  it('covers every note in time and pitch', () => {
    expect(
      notesBounds([
        { start: 100, length: 50, key: 60 },
        { start: 400, length: 80, key: 72 },
      ]),
    ).toEqual({ start: 100, end: 480, lowKey: 60, highKey: 72 });
  });
});

describe('fitNotes', () => {
  const VIEW_W = 800;
  const VIEW_H = 400;
  const CONTENT_TICKS = 16_000;

  it('returns null when there is nothing to frame', () => {
    expect(fitNotes([], VIEW_W, VIEW_H, CONTENT_TICKS)).toBeNull();
  });

  it('keeps the notes inside the viewport after the zoom', () => {
    const notes = [
      { start: 0, length: TICKS_PER_STEP, key: 60 },
      { start: TICKS_PER_STEP * 8, length: TICKS_PER_STEP, key: 67 },
    ];
    const fit = fitNotes(notes, VIEW_W, VIEW_H, CONTENT_TICKS);
    expect(fit).not.toBeNull();
    const pxPerTick = (STEP_W * fit!.zoomX) / TICKS_PER_STEP;
    const rowH = ROW_H * fit!.zoomY;
    const left = 0 * pxPerTick - fit!.scrollX;
    const right = (TICKS_PER_STEP * 9) * pxPerTick - fit!.scrollX;
    expect(left).toBeGreaterThanOrEqual(-1);
    expect(right).toBeLessThanOrEqual(VIEW_W + 1);

    const top = (TOP_KEY - 67) * rowH - fit!.scrollY;
    const bottom = (TOP_KEY - 60 + 1) * rowH - fit!.scrollY;
    expect(top).toBeGreaterThanOrEqual(-1);
    expect(bottom).toBeLessThanOrEqual(VIEW_H + 1);
  });

  it('frames only the notes it was given, not neighbours', () => {
    const selected = [{ start: TICKS_PER_STEP * 16, length: TICKS_PER_STEP * 4, key: 72 }];
    const fit = fitNotes(selected, VIEW_W, VIEW_H, CONTENT_TICKS)!;
    const pxPerTick = (STEP_W * fit.zoomX) / TICKS_PER_STEP;
    const x = selected[0]!.start * pxPerTick - fit.scrollX;
    expect(x).toBeGreaterThanOrEqual(-1);
    expect(x).toBeLessThan(VIEW_W);
  });
});
