import { describe, expect, it } from 'vitest';

import {
  DEFAULT_BROWSER_WIDTH,
  MAX_BROWSER_WIDTH,
  MIN_BROWSER_WIDTH,
  clampBrowserWidth,
  maxBrowserWidth,
} from './browserSize';

describe('browser width', () => {
  it('uses the full range on a wide window', () => {
    expect(maxBrowserWidth(1_600, true)).toBe(MAX_BROWSER_WIDTH);
    expect(clampBrowserWidth(DEFAULT_BROWSER_WIDTH, 1_600, true)).toBe(DEFAULT_BROWSER_WIDTH);
  });

  it('preserves editor space when the inspector is open', () => {
    expect(maxBrowserWidth(900, true)).toBe(274);
    expect(clampBrowserWidth(420, 900, true)).toBe(274);
  });

  it('allows more browser space when the right panel is collapsed', () => {
    expect(maxBrowserWidth(900, false)).toBe(MAX_BROWSER_WIDTH);
  });

  it('never shrinks below the usable browser minimum', () => {
    expect(maxBrowserWidth(600, true)).toBe(MIN_BROWSER_WIDTH);
    expect(clampBrowserWidth(20, 1_600, true)).toBe(MIN_BROWSER_WIDTH);
  });
});
