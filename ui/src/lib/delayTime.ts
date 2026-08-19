/**
 * Delay time, as the engine resolves it.
 *
 * `sync_delay_samples` in `plugins/effects/src/delay.rs` is the source. The
 * engine does not publish the resolved milliseconds, so the shell uses the
 * same public mapping rather than inventing a second one. Free mode reads the
 * `time_ms` parameter directly.
 */

export const DIVISION_BEATS = [4, 2, 1, 0.5, 0.25, 0.125, 0.0625] as const;
export const DIVISION_LABELS = ['1/1', '1/2', '1/4', '1/8', '1/16', '1/32', '1/64'] as const;
export const MODIFIER_FACTORS = [1, 1.5, 2 / 3] as const;
export const MODIFIER_LABELS = ['Straight', 'Dotted', 'Triplet'] as const;

export function syncDelayMs(division: number, modifier: number, bpm: number): number {
  const beats =
    (DIVISION_BEATS[Math.min(DIVISION_BEATS.length - 1, Math.max(0, Math.round(division)))] ?? 1) *
    (MODIFIER_FACTORS[Math.min(MODIFIER_FACTORS.length - 1, Math.max(0, Math.round(modifier)))] ??
      1);
  if (!Number.isFinite(bpm) || bpm <= 0) return 0;
  return (beats * 60_000) / bpm;
}

/** The engine's line is allocated for eight seconds — `MAX_DELAY_SECONDS`. */
export const MAX_DELAY_MS = 8_000;

/**
 * Mirror of the engine's `fit_to_capacity`: a delay longer than the line is
 * *halved* until it fits, staying on the musical grid (a dotted 1/1 at 20 BPM
 * is eighteen seconds; what plays is 4.5). The readout must halve the same
 * way or print a time that is not the one sounding.
 */
function fitToCapacityMs(ms: number): number {
  let out = ms;
  while (out > MAX_DELAY_MS && out > 0.1) out *= 0.5;
  return Math.min(out, MAX_DELAY_MS);
}

export function delayTimeMs(
  sync: boolean,
  division: number,
  modifier: number,
  timeMs: number,
  bpm: number,
): number {
  return fitToCapacityMs(sync ? syncDelayMs(division, modifier, bpm) : timeMs);
}

/**
 * Magnitude of the in-loop HP→LP pair, analog one-pole with bilinear prewarp.
 *
 * Close enough to draw the damping curve; the engine's TPT one-poles are the
 * same prototype.
 */
export function delayFilterMagDb(
  hz: number,
  hpHz: number,
  lpHz: number,
  sampleRate: number,
): number {
  const rate = sampleRate > 1 ? sampleRate : 48_000;
  const nyquist = rate * 0.5;
  const f = Math.min(nyquist * 0.99, Math.max(1, hz));
  const warp = (cut: number) => Math.tan((Math.PI * Math.min(nyquist * 0.99, Math.max(1, cut))) / rate);
  const w = Math.tan((Math.PI * f) / rate);
  const gHp = warp(hpHz);
  const gLp = warp(lpHz);
  const hp = w / Math.hypot(w, gHp);
  const lp = gLp / Math.hypot(w, gLp);
  return 20 * Math.log10(Math.max(1e-12, hp * lp));
}
