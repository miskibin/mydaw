/**
 * Saturator waveshapers, transcribed from `plugins/effects/src/dsp.rs`.
 *
 * The transfer curve is a drawing of these functions at the current drive.
 * The live dot is the peak sample of the insert scope hop, shaped by the same
 * function — a measurement of this block, not a 0 dBFS marker.
 */

export type SaturatorType = 'tape' | 'tube' | 'clip';

export function saturatorType(choice: number): SaturatorType {
  if (Math.round(choice) === 1) return 'tube';
  if (Math.round(choice) === 2) return 'clip';
  return 'tape';
}

/** Drive in dB → linear gain. */
export function driveGain(driveDb: number): number {
  return 10 ** (driveDb / 20);
}

export function softClip(x: number): number {
  const t = Math.min(3, Math.max(-3, x));
  const x2 = t * t;
  return (t * (27 + x2)) / (27 + 9 * x2);
}

export function asymmetricClip(x: number): number {
  return x >= 0 ? softClip(x * 1.6) * 0.75 : softClip(x * 0.8);
}

export function hardClip(x: number): number {
  const KNEE = 2 / 3;
  const a = Math.abs(x);
  if (a <= KNEE) return x;
  if (a >= 1) return Math.sign(x);
  const t = (a - KNEE) / (1 - KNEE);
  return Math.sign(x) * (KNEE + (1 - KNEE) * (t - t * t / 2) / 0.5 * 0.5);
}

export function shape(kind: SaturatorType, x: number): number {
  switch (kind) {
    case 'tape':
      return softClip(x);
    case 'tube':
      return asymmetricClip(x);
    case 'clip':
      return hardClip(x);
  }
}

/** Output for an input sample after drive. */
export function saturatorOut(kind: SaturatorType, x: number, driveDb: number): number {
  return shape(kind, x * driveGain(driveDb));
}

/** Peak sample of a scope hop, signed — the live input on the transfer curve. */
export function liveDriveSample(scope: readonly number[]): number {
  let peak = 0;
  for (const sample of scope) {
    if (Math.abs(sample) > Math.abs(peak)) peak = sample;
  }
  return peak;
}

/** 0..1 bar heights from log-spaced analyser bins. */
export function spectrumBarHeights(
  bins: readonly number[],
  count = 8,
  floorDb = -90,
): number[] {
  const bars = Math.max(1, Math.trunc(count));
  if (bins.length === 0) return Array.from({ length: bars }, () => 0);
  const span = 0 - floorDb || 1;
  return Array.from({ length: bars }, (_, index) => {
    const start = Math.floor((index / bars) * bins.length);
    const end = Math.max(start + 1, Math.floor(((index + 1) / bars) * bins.length));
    let peak = floorDb;
    for (let i = start; i < end; i += 1) peak = Math.max(peak, bins[i] ?? floorDb);
    return Math.min(1, Math.max(0, (peak - floorDb) / span));
  });
}
