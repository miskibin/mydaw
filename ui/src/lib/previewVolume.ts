/** Default browser / Sounds audition level, as a percent of master volume. */
export const DEFAULT_PREVIEW_VOLUME_PERCENT = 80;
export const MIN_PREVIEW_VOLUME_PERCENT = 0;
export const MAX_PREVIEW_VOLUME_PERCENT = 100;

/**
 * Clamp a Settings slider value into `0..=100`. Non-finite numbers become the
 * default rather than silence, matching the engine's `preview.json` rules.
 */
export function clampPreviewVolumePercent(percent: number): number {
  if (!Number.isFinite(percent)) return DEFAULT_PREVIEW_VOLUME_PERCENT;
  return Math.min(MAX_PREVIEW_VOLUME_PERCENT, Math.max(MIN_PREVIEW_VOLUME_PERCENT, percent));
}

/** Linear gain the preview voice multiplies by, `0..=1`. */
export function previewVolumeGain(percent: number): number {
  return clampPreviewVolumePercent(percent) / 100;
}
