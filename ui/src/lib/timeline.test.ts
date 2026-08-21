import { describe, expect, it } from 'vitest';

import {
  clampScroll,
  foldToLoop,
  followScroll,
  timelinePxAt,
  wheelIntent,
  wheelPixels,
  zoomAnchoredScroll,
} from './timeline';

const HEAD = 200;

describe('clampScroll', () => {
  it('holds a scroll inside its content and survives a NaN', () => {
    expect(clampScroll(-40, 900)).toBe(0);
    expect(clampScroll(1200, 900)).toBe(900);
    expect(clampScroll(Number.NaN, 900)).toBe(0);
    expect(clampScroll(120, -10)).toBe(0);
  });
});

describe('zoomAnchoredScroll', () => {
  it('keeps the tick under the pointer under the pointer', () => {
    const scrollLeft = 400;
    const pointerOffset = 500;
    const pxPerTick = 0.0125; // 48 px per bar
    const tick = timelinePxAt(pointerOffset, scrollLeft, HEAD) / pxPerTick;

    const next = zoomAnchoredScroll({
      scrollLeft,
      pointerOffset,
      headerPx: HEAD,
      pxPerTick,
      nextPxPerTick: pxPerTick * 2,
      maxScroll: 100_000,
    });

    expect(timelinePxAt(pointerOffset, next, HEAD) / (pxPerTick * 2)).toBeCloseTo(tick, 6);
  });

  it('anchors on the timeline when the pointer is over a lane header', () => {
    const next = zoomAnchoredScroll({
      scrollLeft: 600,
      pointerOffset: 40, // inside the header
      headerPx: HEAD,
      pxPerTick: 0.0125,
      nextPxPerTick: 0.025,
      maxScroll: 100_000,
    });
    // The leftmost visible tick (600 px in) is the one held still.
    expect(next).toBeCloseTo(1200, 6);
  });

  it('never scrolls past the ends', () => {
    expect(
      zoomAnchoredScroll({
        scrollLeft: 0,
        pointerOffset: 200,
        headerPx: HEAD,
        pxPerTick: 0.025,
        nextPxPerTick: 0.0125,
        maxScroll: 500,
      }),
    ).toBe(0);
  });
});

describe('followScroll', () => {
  const base = { viewportPx: 1000, headerPx: HEAD, maxScroll: 10_000 };

  it('stays put while the playhead is comfortably on screen', () => {
    expect(followScroll({ ...base, playheadPx: 400, scrollLeft: 300 })).toBeNull();
  });

  it('pages forward once the playhead reaches the right margin', () => {
    const next = followScroll({ ...base, playheadPx: 1300, scrollLeft: 300 });
    expect(next).not.toBeNull();
    expect(next!).toBeGreaterThan(300);
    // The playhead lands a margin in from the left, not glued to the edge.
    expect(1300 - next!).toBeCloseTo(Math.min((1000 - HEAD) * 0.12, 120), 6);
  });

  it('scrolls back when a seek puts the playhead behind the viewport', () => {
    expect(followScroll({ ...base, playheadPx: 0, scrollLeft: 900 })).toBe(0);
  });

  it('does not fight a viewport narrower than its headers', () => {
    expect(followScroll({ ...base, viewportPx: 100, playheadPx: 900, scrollLeft: 0 })).toBeNull();
  });
});

describe('wheelIntent', () => {
  const plain = { ctrlKey: false, altKey: false, metaKey: false, shiftKey: false, deltaX: 0 };

  it('spends ctrl and alt on zoom, and shift on sideways travel', () => {
    expect(wheelIntent({ ...plain, ctrlKey: true })).toBe('zoom');
    expect(wheelIntent({ ...plain, altKey: true })).toBe('zoom');
    expect(wheelIntent({ ...plain, shiftKey: true })).toBe('scrollX');
    expect(wheelIntent({ ...plain, deltaX: -12 })).toBe('scrollX');
    expect(wheelIntent(plain)).toBe('scrollY');
  });

  it('reads a modifier before an axis, so ctrl+trackpad still zooms', () => {
    expect(wheelIntent({ ...plain, ctrlKey: true, deltaX: 30 })).toBe('zoom');
  });
});

describe('wheelPixels', () => {
  it('turns lines and pages into pixels and keeps the sign', () => {
    expect(wheelPixels({ deltaX: 0, deltaY: 100, deltaMode: 0 })).toBe(100);
    expect(wheelPixels({ deltaX: 0, deltaY: -3, deltaMode: 1 })).toBe(-48);
    expect(wheelPixels({ deltaX: 0, deltaY: 1, deltaMode: 2 })).toBe(320);
    expect(wheelPixels({ deltaX: -20, deltaY: 100, deltaMode: 0 })).toBe(-20);
    expect(wheelPixels({ deltaX: 0, deltaY: 0, deltaMode: 0 })).toBe(0);
  });
});

describe('foldToLoop', () => {
  it('folds a rising position back into the loop', () => {
    expect(foldToLoop(0, 3840)).toBe(0);
    expect(foldToLoop(3840, 3840)).toBe(0);
    expect(foldToLoop(3841, 3840)).toBe(1);
    expect(foldToLoop(3840 * 7 + 100, 3840)).toBe(100);
  });

  it('leaves the position alone when there is no loop to fold into', () => {
    expect(foldToLoop(9_000, 0)).toBe(9_000);
    expect(foldToLoop(9_000, -1)).toBe(9_000);
    expect(foldToLoop(Number.POSITIVE_INFINITY, 100)).toBe(Number.POSITIVE_INFINITY);
  });
});
