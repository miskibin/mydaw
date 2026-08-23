/**
 * Limiter transfer curve, matching `LimiterEditor`'s input+gain brick wall.
 */

/** Output level after makeup gain, clamped to the ceiling. */
export function limiterOutputDb(inputDb: number, gainDb: number, ceilingDb: number): number {
  return Math.min(inputDb + gainDb, ceilingDb);
}

/**
 * Operating point from published GR. `input + gain - ceiling = gr` on this
 * brick wall, so a 0 dB readout sits on the last unlimited sample.
 */
export function limiterTransferPoint(
  gainDb: number,
  ceilingDb: number,
  grDb: number,
): { x: number; y: number } {
  const reduction = Math.max(0, grDb);
  const x = ceilingDb - gainDb + reduction;
  return { x, y: limiterOutputDb(x, gainDb, ceilingDb) };
}
