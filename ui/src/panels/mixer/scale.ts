/**
 * The mixer's scales: fader travel to decibels, and amplitude to meter height.
 *
 * Kept free of Solid and of any IPC type, so the arithmetic can be reasoned
 * about (and one day tested) on its own.
 */

/** Bottom of the fader, in dB. Mirrors `SILENCE_DB` in `crates/rt-core`. */
export const MIN_DB = -60;
/** Top of the fader, in dB. Mirrors `MAX_GAIN_DB` in `crates/rt-core`. */
export const MAX_DB = 6;

/**
 * Curve exponent mapping fader travel to decibels:
 * `db = MIN + travel^CURVE * (MAX - MIN)`.
 *
 * A *linear* map spends half the throw between -60 and -30 dB, where nothing
 * musical happens, and squeezes the -12..+6 dB range everybody actually mixes
 * in into the top sixth. An exponent below 1 does the opposite: it gives the
 * loud end most of the travel, the way a console's markings do.
 *
 * 0.4 puts unity gain at about 79% of the throw, half travel at -10 dB and a
 * quarter at -22 dB -- close enough to a real fader that muscle memory works,
 * and it still reaches silence at the bottom.
 */
const FADER_CURVE = 0.4;

/** Fader travel (`0` bottom, `1` top) for a gain in dB. */
export function faderFraction(db: number): number {
  if (!Number.isFinite(db)) return 0;
  const clamped = Math.min(MAX_DB, Math.max(MIN_DB, db));
  const linear = (clamped - MIN_DB) / (MAX_DB - MIN_DB);
  return linear ** (1 / FADER_CURVE);
}

/** The dB a fader at `fraction` of its travel asks for. */
export function faderToDb(fraction: number): number {
  if (!Number.isFinite(fraction)) return 0;
  const clamped = Math.min(1, Math.max(0, fraction));
  const db = MIN_DB + clamped ** FADER_CURVE * (MAX_DB - MIN_DB);
  // Rounded to a tenth: the readout shows one decimal, and a fader that reports
  // -6.03 while displaying -6.0 makes a round trip look like a bug.
  return Math.round(db * 10) / 10;
}

/** A gain for a readout: `0.0` at unity, `-inf` at the bottom of the scale. */
export function formatDb(db: number): string {
  if (!Number.isFinite(db) || db <= MIN_DB) return '-∞';
  return `${db > 0 ? '+' : ''}${db.toFixed(1)}`;
}

/** A pan position for a readout: `C`, `L42`, `R100`. */
export function panLabel(pan: number): string {
  if (!Number.isFinite(pan)) return 'C';
  const amount = Math.round(Math.abs(pan) * 100);
  if (amount === 0) return 'C';
  return `${pan < 0 ? 'L' : 'R'}${amount}`;
}

/**
 * Meter height (`0`..`1`) for a linear amplitude.
 *
 * The same -60..+6 dBFS window the canvas meter uses, so a strip's meter and
 * the master meter panel agree about how loud something looks.
 */
export function peakFraction(amplitude: number): number {
  if (!Number.isFinite(amplitude) || amplitude <= 0) return 0;
  const db = 20 * Math.log10(amplitude);
  const clamped = Math.min(MAX_DB, Math.max(MIN_DB, db));
  return (clamped - MIN_DB) / (MAX_DB - MIN_DB);
}
