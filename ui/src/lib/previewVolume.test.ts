import { describe, expect, it } from 'vitest';

import {
  clampPreviewVolumePercent,
  DEFAULT_PREVIEW_VOLUME_PERCENT,
  previewVolumeGain,
} from './previewVolume';

describe('preview volume percent', () => {
  it('defaults non-finite values to 80 rather than muting', () => {
    expect(clampPreviewVolumePercent(Number.NaN)).toBe(DEFAULT_PREVIEW_VOLUME_PERCENT);
    expect(clampPreviewVolumePercent(Number.POSITIVE_INFINITY)).toBe(
      DEFAULT_PREVIEW_VOLUME_PERCENT,
    );
  });

  it('clamps out of range percents and converts 80 to 0.8 gain', () => {
    expect(clampPreviewVolumePercent(-12)).toBe(0);
    expect(clampPreviewVolumePercent(150)).toBe(100);
    expect(previewVolumeGain(80)).toBeCloseTo(0.8);
    expect(previewVolumeGain(0)).toBe(0);
    expect(previewVolumeGain(100)).toBe(1);
  });
});
