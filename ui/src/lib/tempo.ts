/**
 * Tempo as two independent axes: whole BPM and hundredths of a BPM.
 *
 * A single drag on `130.00` cannot be both coarse and fine, which is why the
 * top bar splits the readout. These helpers are the only place the two parts
 * are joined, so a whole-BPM nudge cannot rewrite `.47` and a hundredths drag
 * cannot jump from 130 to 131.
 */

export const MIN_BPM = 20;
export const MAX_BPM = 999;

export interface TempoParts {
  whole: number;
  /** Hundredths of a BPM, 0..99. */
  cents: number;
}

function clampBpm(bpm: number): number {
  return Math.min(MAX_BPM, Math.max(MIN_BPM, bpm));
}

/** Nearest hundredth, inside the engine's legal range. */
export function quantizeBpm(bpm: number): number {
  if (!Number.isFinite(bpm)) return MIN_BPM;
  return clampBpm(Math.round(bpm * 100) / 100);
}

export function tempoParts(bpm: number): TempoParts {
  const hundredths = Math.round(quantizeBpm(bpm) * 100);
  return {
    whole: Math.floor(hundredths / 100),
    cents: hundredths - Math.floor(hundredths / 100) * 100,
  };
}

/** Keep the current hundredths; replace the integer. */
export function tempoFromWhole(bpm: number, whole: number): number {
  const { cents } = tempoParts(bpm);
  const next = Math.round(Number.isFinite(whole) ? whole : tempoParts(bpm).whole);
  return quantizeBpm(next + cents / 100);
}

/** Keep the current integer; replace the hundredths (clamped to 0..99). */
export function tempoFromCents(bpm: number, cents: number): number {
  const { whole } = tempoParts(bpm);
  const next = Math.round(Number.isFinite(cents) ? cents : 0);
  const clamped = Math.min(99, Math.max(0, next));
  return quantizeBpm(whole + clamped / 100);
}
