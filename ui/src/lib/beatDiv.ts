// Musical times in beats (a beat = a quarter note), for tempo-synced controls such as the
// Delay's Time: the knob snaps to these, and values read as beats ("1/2 beat", "1 1/2 beats")
// with the note value alongside ("1/8", "1/4.", "1/8T").
import { t } from './i18n';

/** Note values a synced time snaps to, shortest first: `b` beats, `note` its name (T = triplet, . = dotted). */
export const BEAT_DIVS: readonly { b: number; note: string }[] = [
  { b: 1 / 48, note: '1/128T' }, { b: 1 / 32, note: '1/128' }, { b: 1 / 24, note: '1/64T' }, { b: 1 / 16, note: '1/64' },
  { b: 1 / 12, note: '1/32T' }, { b: 3 / 32, note: '1/64.' }, { b: 1 / 8, note: '1/32' }, { b: 1 / 6, note: '1/16T' },
  { b: 3 / 16, note: '1/32.' }, { b: 1 / 4, note: '1/16' }, { b: 1 / 3, note: '1/8T' }, { b: 3 / 8, note: '1/16.' },
  { b: 1 / 2, note: '1/8' }, { b: 2 / 3, note: '1/4T' }, { b: 3 / 4, note: '1/8.' }, { b: 1, note: '1/4' },
  { b: 4 / 3, note: '1/2T' }, { b: 3 / 2, note: '1/4.' }, { b: 2, note: '1/2' }, { b: 3, note: '1/2.' }, { b: 4, note: '1/1' },
];

/** The note value nearest to `b` (in ratio), for knob snapping. */
export function snapBeats(b: number): number {
  let best = BEAT_DIVS[0].b, err = Infinity;
  for (const d of BEAT_DIVS) {
    const e = Math.abs(Math.log(b / d.b));
    if (e < err) { err = e; best = d.b; }
  }
  return best;
}

/** Name of the note value `b` is (within 0.1 %), else null. */
export function noteName(b: number): string | null {
  const d = BEAT_DIVS.find(x => Math.abs(x.b - b) <= 1e-3 * x.b);
  return d ? d.note : null;
}

const DENS = [2, 3, 4, 6, 8, 12, 16, 24, 32, 48];
/** `b` as a mixed fraction ("1/2", "1 1/3", "2"), or a decimal when no small denominator fits. */
export function beatFraction(b: number): string {
  const whole = Math.floor(b + 1e-6), rest = b - whole;
  if (rest < 1e-4) return String(whole);
  for (const d of DENS) {
    const n = Math.round(rest * d);
    if (n > 0 && n < d && Math.abs(n / d - rest) < 1e-4) {
      // reduce (DENS is ordered, so the first hit is already lowest terms except for even pairs)
      const g = gcd(n, d);
      const f = `${n / g}/${d / g}`;
      return whole ? `${whole} ${f}` : f;
    }
  }
  return b.toFixed(b < 1 ? 3 : 2);
}
const gcd = (a: number, b: number): number => (b ? gcd(b, a % b) : a);

/** "1/2 beat", "3 beats" (translated). */
export function beatsLabel(b: number): string {
  const n = beatFraction(b);
  return b <= 1 + 1e-6 ? t('b.pe.beat1', { n }) : t('b.pe.beatN', { n });
}

/** Milliseconds of `b` beats at `bpm`. */
export const beatsToMs = (b: number, bpm: number) => b * 60000 / Math.max(1, bpm);

/** The next note value above (`dir` > 0) or below `b` — one wheel notch on a synced time. */
export function stepBeats(b: number, dir: number): number {
  const i = BEAT_DIVS.findIndex(d => d.b >= b * (1 - 1e-3));
  const at = i < 0 ? BEAT_DIVS.length - 1 : i;
  const exact = i >= 0 && Math.abs(BEAT_DIVS[i].b - b) <= 1e-3 * b;
  const j = dir > 0 ? (exact ? at + 1 : at) : at - 1;
  return BEAT_DIVS[Math.max(0, Math.min(BEAT_DIVS.length - 1, j))].b;
}
