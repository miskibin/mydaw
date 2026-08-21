import { describe, expect, it } from 'vitest';

import { dragTravel, FADER_HANDLE_PX, FADER_TRAVEL_PX, faderThrowPx } from './drag';

describe('dragTravel', () => {
  it('reads only horizontal travel for a pane separator', () => {
    expect(dragTravel('horizontal', 24, -80)).toBe(24);
    expect(dragTravel('horizontal', -12, 40)).toBe(-12);
  });
});

describe('faderThrowPx', () => {
  it('moves the handle one pixel for each pixel the pointer travels on that track', () => {
    const track = 400;
    const throwPx = faderThrowPx(track);
    const from = 0.25;
    const pointerTravel = 80;
    const next = from + pointerTravel / throwPx;
    const handleFrom = (1 - from) * (track - FADER_HANDLE_PX);
    const handleNext = (1 - next) * (track - FADER_HANDLE_PX);
    expect(handleFrom - handleNext).toBeCloseTo(pointerTravel);
  });

  it('does not scale a tall strip against the 180 px fallback', () => {
    const track = 400;
    expect(faderThrowPx(track)).toBe(track - FADER_HANDLE_PX);
    expect(faderThrowPx(track)).not.toBe(FADER_TRAVEL_PX);
  });

  it('falls back when the track has not laid out', () => {
    expect(faderThrowPx(0)).toBe(FADER_TRAVEL_PX);
    expect(faderThrowPx(FADER_HANDLE_PX)).toBe(FADER_TRAVEL_PX);
  });
});
