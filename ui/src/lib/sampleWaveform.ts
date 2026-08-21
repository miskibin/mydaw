/**
 * Draw the waveform the channel will produce, not only the file stored on disk.
 *
 * The decoder publishes one min/max pair per bin.  Channel gain, equal-power
 * pan, pitch duration and fades are cheap view transforms, so both the sample
 * window and the inspector can give immediate feedback while the engine is
 * applying the same values to playback.
 */

export interface WaveformView {
  gainDb: number;
  pan: number;
  pitchCents: number;
  fadeInMs: number;
  fadeOutMs: number;
  sampleRate: number;
  totalFrames: number;
  startFrames: number;
  endFrames: number;
}

export interface StereoWaveformPaths {
  left: string;
  right: string;
  /** Width of the pitched sample relative to its root-speed duration. */
  widthPercent: number;
}

const clamp = (value: number, min: number, max: number): number =>
  Math.min(max, Math.max(min, Number.isFinite(value) ? value : min));

function channelPath(
  peaks: readonly (readonly [number, number])[],
  centre: number,
  channelGain: number,
  view: WaveformView,
): string {
  if (peaks.length === 0) return '';

  const total = Math.max(1, Math.round(view.totalFrames));
  const start = clamp(Math.round(view.startFrames), 0, total - 1);
  const storedEnd = view.endFrames <= 0 ? total : Math.round(view.endFrames);
  const end = clamp(storedEnd, start + 1, total);
  const first = Math.floor((start / total) * peaks.length);
  const last = Math.max(first + 1, Math.ceil((end / total) * peaks.length));
  const region = peaks.slice(first, Math.min(peaks.length, last));
  if (region.length === 0) return '';

  const regionFrames = end - start;
  const fadeInFrames = Math.max(0, view.fadeInMs) * Math.max(0, view.sampleRate) / 1_000;
  const fadeOutFrames = Math.max(0, view.fadeOutMs) * Math.max(0, view.sampleRate) / 1_000;
  const halfHeight = 21;
  const width = 100 / 2 ** (clamp(view.pitchCents, -1_200, 1_200) / 1_200);
  const points = region.map((pair, index) => {
    const t = region.length === 1 ? 0 : index / (region.length - 1);
    const frame = t * regionFrames;
    const fadeIn = fadeInFrames > 0 ? Math.min(1, frame / fadeInFrames) : 1;
    const fadeOut = fadeOutFrames > 0 ? Math.min(1, (regionFrames - frame) / fadeOutFrames) : 1;
    const envelope = Math.max(0, Math.min(fadeIn, fadeOut));
    const scale = channelGain * envelope * halfHeight;
    return {
      x: t * width,
      top: centre - clamp(pair[1] * scale, -halfHeight, halfHeight),
      bottom: centre - clamp(pair[0] * scale, -halfHeight, halfHeight),
    };
  });
  const top = points.map((point) => `${point.x},${point.top}`);
  const bottom = points.slice().reverse().map((point) => `${point.x},${point.bottom}`);
  return `M${top.join(' L')} L${bottom.join(' L')} Z`;
}

/** Two stacked paths: upper is left, lower is right. */
export function stereoWaveformPaths(
  peaks: readonly (readonly [number, number])[],
  view: WaveformView,
): StereoWaveformPaths {
  const pan = clamp(view.pan, -1, 1);
  const theta = ((pan + 1) * Math.PI) / 4;
  const gain = 10 ** (clamp(view.gainDb, -60, 12) / 20);
  const leftGain = Math.cos(theta) * Math.SQRT2 * gain;
  const rightGain = Math.sin(theta) * Math.SQRT2 * gain;
  return {
    left: channelPath(peaks, 25, leftGain, view),
    right: channelPath(peaks, 75, rightGain, view),
    widthPercent: 100 / 2 ** (clamp(view.pitchCents, -1_200, 1_200) / 1_200),
  };
}

/**
 * One centred min/max envelope for a playlist clip (`viewBox="0 0 100 100"`).
 *
 * The sample window stacks left and right. A clip is a few dozen pixels tall,
 * so this is the outer bound of both channels as a single fill. Each min stays
 * on the same x as its max — reversing the *points* (not the x of a forward
 * scan) is what closes the shape without mirroring the decay.
 */
export function overviewWaveformPath(
  peaks: readonly (readonly [number, number])[],
): string {
  if (peaks.length === 0) return '';
  const half = 46;
  const points = peaks.map((pair, index) => {
    const t = peaks.length === 1 ? 0 : index / (peaks.length - 1);
    return {
      x: t * 100,
      top: 50 - clamp(pair[1] * half, -half, half),
      bottom: 50 - clamp(pair[0] * half, -half, half),
    };
  });
  if (points.length === 1) {
    const point = points[0]!;
    return `M0,${point.top} L100,${point.top} L100,${point.bottom} L0,${point.bottom} Z`;
  }
  const top = points.map((point) => `${point.x},${point.top}`);
  const bottom = points.slice().reverse().map((point) => `${point.x},${point.bottom}`);
  return `M${top.join(' L')} L${bottom.join(' L')} Z`;
}
