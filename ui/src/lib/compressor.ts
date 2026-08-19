/**
 * The compressor's static curve, transcribed from
 * `plugins/effects/src/compressor.rs::reduction_db`.
 *
 * The display draws this, not a measurement: threshold, ratio and knee are
 * parameters, and the live operating point is recovered from the published
 * gain-reduction readout rather than from a second, invented input meter.
 */

/** Positive reduction in dB for a detector level in dB. */
export function reductionDb(
  levelDb: number,
  thresholdDb: number,
  ratio: number,
  kneeDb: number,
): number {
  const slope = 1 - 1 / Math.max(1, ratio);
  const over = levelDb - thresholdDb;
  if (kneeDb > 0 && 2 * Math.abs(over) <= kneeDb) {
    const x = over + kneeDb * 0.5;
    return (slope * x * x) / (2 * kneeDb);
  }
  return over > 0 ? slope * over : 0;
}

/** Output level in dB for an input level, including makeup. */
export function compressorOutputDb(
  inputDb: number,
  thresholdDb: number,
  ratio: number,
  kneeDb: number,
  makeupDb: number,
): number {
  return inputDb - reductionDb(inputDb, thresholdDb, ratio, kneeDb) + makeupDb;
}

/**
 * The input level that would produce `grDb` of reduction on this curve.
 *
 * Unique because reduction is monotonic in input. Used to place the live
 * operating point from `gr_max` without a separate input-level readout.
 */
export function inputDbFromGr(
  grDb: number,
  thresholdDb: number,
  ratio: number,
  kneeDb: number,
): number {
  const target = Math.max(0, grDb);
  if (target < 1e-4) return thresholdDb - kneeDb * 0.5;
  let lo = thresholdDb - Math.max(kneeDb, 0) - 1;
  let hi = 0;
  for (let i = 0; i < 24; i++) {
    const mid = (lo + hi) * 0.5;
    if (reductionDb(mid, thresholdDb, ratio, kneeDb) < target) lo = mid;
    else hi = mid;
  }
  return (lo + hi) * 0.5;
}
