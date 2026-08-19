/**
 * The EQ's magnitude response, computed in the webview.
 *
 * **This is the UI's own model of the engine's filters, not a measurement.** The
 * engine has `eq::magnitude_db_at` and `eq::magnitude_curve` in
 * `plugins/effects/src/eq.rs` -- which evaluate the very coefficients the audio
 * path is running -- but neither is on the command surface, so nothing the
 * webview can ask for returns a curve. What is here is therefore a *second*
 * implementation of the same formula: the RBJ cookbook sections from
 * `plugins/effects/src/biquad.rs`, transcribed constant for constant, driven by
 * the parameter values the shell already holds.
 *
 * That makes the drawing honest in one specific sense and dishonest in another,
 * and the distinction is worth being exact about:
 *
 * - It is **a drawing of the same formula**, so it says what the EQ is set to do.
 * - It is **not evidence that the audio did it**. Anything that could make the
 *   two disagree -- a coefficient fix in Rust that is not mirrored here, a band
 *   the engine grows that {@link EQ_BANDS} does not know about -- would show up
 *   as a curve that is right about the settings and wrong about the sound.
 *
 * So every surface that draws this says so in its `title`, and the band table
 * below is deliberately explicit rather than derived: an unknown band is left
 * out of the curve rather than guessed at.
 *
 * No DOM, no backend, no Solid -- so the maths can be reasoned about, and later
 * tested, on its own.
 */

/** The five normalised coefficients of a second-order section, already over `a0`. */
export interface BiquadCoeffs {
  b0: number;
  b1: number;
  b2: number;
  a1: number;
  a2: number;
}

/** A section that passes its input through unchanged. */
export const BIQUAD_BYPASS: BiquadCoeffs = { b0: 1, b1: 0, b2: 0, a1: 0, a2: 0 };

/**
 * The two Qs a 24 dB/octave Butterworth response needs from two cascaded
 * 12 dB/octave sections: `1/(2 cos 22.5°)` and `1/(2 cos 67.5°)`.
 *
 * The same constant as `biquad::BUTTERWORTH_Q4`. Two sections both at `1/√2`
 * would put the corner 6 dB down instead of 3, which is the usual way this is
 * got wrong -- and the reason the number is copied rather than approximated.
 */
export const BUTTERWORTH_Q4: readonly [number, number] = [
  0.541_196_100_146_197, 1.306_562_964_876_377,
];

/** The band edges of the display, and of every band's frequency parameter. */
export const EQ_MIN_HZ = 20;
export const EQ_MAX_HZ = 20_000;

/** The rate the curve is drawn at when no device has opened one. */
const FALLBACK_SAMPLE_RATE = 48_000;

/** Divide through by `a0`, refusing a degenerate section. */
function normalise(
  b0: number,
  b1: number,
  b2: number,
  a0: number,
  a1: number,
  a2: number,
): BiquadCoeffs {
  if (!Number.isFinite(a0) || Math.abs(a0) < Number.EPSILON) return BIQUAD_BYPASS;
  const out = { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 };
  return Object.values(out).every((value) => Number.isFinite(value)) ? out : BIQUAD_BYPASS;
}

/**
 * `cos ω₀` and `α = sin ω₀ / 2Q`, with the same clamps the engine applies.
 *
 * The frequency is held just under Nyquist because a cutoff *at* Nyquist makes
 * `sin ω₀` zero and the section degenerate -- reachable here by dragging a
 * frequency knob to 20 kHz on a 44.1 kHz device.
 */
function rbj(hz: number, q: number, sampleRate: number): { cosW: number; alpha: number } {
  const rate =
    Number.isFinite(sampleRate) && sampleRate > 1 ? sampleRate : FALLBACK_SAMPLE_RATE;
  const nyquist = rate * 0.5;
  const wanted = Number.isFinite(hz) ? hz : nyquist * 0.5;
  const clamped = Math.min(nyquist * 0.999, Math.max(1, wanted));
  const safeQ = Number.isFinite(q) && q > 1.0e-4 ? q : 1.0e-4;
  const omega = (2 * Math.PI * clamped) / rate;
  return { cosW: Math.cos(omega), alpha: Math.sin(omega) / (2 * safeQ) };
}

/** A 12 dB/octave highpass at `hz` with resonance `q`. */
export function highpass(hz: number, q: number, sampleRate: number): BiquadCoeffs {
  const { cosW, alpha } = rbj(hz, q, sampleRate);
  const b0 = (1 + cosW) / 2;
  return normalise(b0, -(1 + cosW), b0, 1 + alpha, -2 * cosW, 1 - alpha);
}

/** A peaking (bell) section: `gainDb` at `hz`, width set by `q`. */
export function peaking(
  hz: number,
  q: number,
  gainDb: number,
  sampleRate: number,
): BiquadCoeffs {
  const { cosW, alpha } = rbj(hz, q, sampleRate);
  const a = 10 ** (gainDb / 40);
  return normalise(
    1 + alpha * a,
    -2 * cosW,
    1 - alpha * a,
    1 + alpha / a,
    -2 * cosW,
    1 - alpha / a,
  );
}

/** A low shelf: `gainDb` below `hz`, unity above. */
export function lowShelf(
  hz: number,
  q: number,
  gainDb: number,
  sampleRate: number,
): BiquadCoeffs {
  const { cosW, alpha } = rbj(hz, q, sampleRate);
  const a = 10 ** (gainDb / 40);
  const twoSqrtAAlpha = 2 * Math.sqrt(a) * alpha;
  return normalise(
    a * (a + 1 - (a - 1) * cosW + twoSqrtAAlpha),
    2 * a * (a - 1 - (a + 1) * cosW),
    a * (a + 1 - (a - 1) * cosW - twoSqrtAAlpha),
    a + 1 + (a - 1) * cosW + twoSqrtAAlpha,
    -2 * (a - 1 + (a + 1) * cosW),
    a + 1 + (a - 1) * cosW - twoSqrtAAlpha,
  );
}

/** A high shelf: `gainDb` above `hz`, unity below. */
export function highShelf(
  hz: number,
  q: number,
  gainDb: number,
  sampleRate: number,
): BiquadCoeffs {
  const { cosW, alpha } = rbj(hz, q, sampleRate);
  const a = 10 ** (gainDb / 40);
  const twoSqrtAAlpha = 2 * Math.sqrt(a) * alpha;
  return normalise(
    a * (a + 1 + (a - 1) * cosW + twoSqrtAAlpha),
    -2 * a * (a - 1 + (a + 1) * cosW),
    a * (a + 1 + (a - 1) * cosW - twoSqrtAAlpha),
    a + 1 - (a - 1) * cosW + twoSqrtAAlpha,
    2 * (a - 1 - (a + 1) * cosW),
    a + 1 - (a - 1) * cosW - twoSqrtAAlpha,
  );
}

/**
 * `|H(e^{jω})|` at `hz`, as a linear magnitude.
 *
 * The closed form, evaluated from the coefficients rather than by running an
 * impulse -- the same expression `BiquadCoeffs::magnitude_at` uses, so the two
 * can be compared term by term if they ever disagree.
 */
export function magnitudeAt(c: BiquadCoeffs, hz: number, sampleRate: number): number {
  if (!Number.isFinite(sampleRate) || sampleRate <= 0 || !Number.isFinite(hz)) return 1;
  const omega = (2 * Math.PI * hz) / sampleRate;
  const cos1 = Math.cos(omega);
  const cos2 = Math.cos(2 * omega);
  const num =
    c.b0 * c.b0 +
    c.b1 * c.b1 +
    c.b2 * c.b2 +
    2 * (c.b0 * c.b1 + c.b1 * c.b2) * cos1 +
    2 * c.b0 * c.b2 * cos2;
  const den =
    1 + c.a1 * c.a1 + c.a2 * c.a2 + 2 * (c.a1 + c.a1 * c.a2) * cos1 + 2 * c.a2 * cos2;
  if (den <= 0 || !Number.isFinite(den) || num < 0 || !Number.isFinite(num)) return 1;
  return Math.sqrt(num / den);
}

/** What shape a band takes, which is fixed by its position in the chain. */
export type EqBandKind = 'highpass' | 'lowShelf' | 'bell' | 'highShelf';

/**
 * The five bands, in signal order, keyed by the parameter-id prefix each one
 * owns.
 *
 * A transcription of `eq::BAND_IDS` and `eq::BAND_KINDS`. It has to be written
 * out because nothing on the wire says what *shape* a band is -- `low.freq` and
 * `bell1.freq` are the same `ParamDef` as far as the catalogue is concerned, and
 * a shelf and a bell draw nothing like each other. A band the engine grows that
 * is not in this table is simply left out of the curve.
 */
export const EQ_BANDS: readonly { id: string; kind: EqBandKind }[] = [
  { id: 'hp', kind: 'highpass' },
  { id: 'low', kind: 'lowShelf' },
  { id: 'bell1', kind: 'bell' },
  { id: 'bell2', kind: 'bell' },
  { id: 'high', kind: 'highShelf' },
];

/** Every parameter value of one EQ, by id. */
export type EqValues = ReadonlyMap<string, number>;

/** The sections one band contributes, or none when it is switched off. */
function bandSections(
  band: { id: string; kind: EqBandKind },
  values: EqValues,
  sampleRate: number,
): BiquadCoeffs[] {
  if ((values.get(`${band.id}.enabled`) ?? 0) < 0.5) return [];
  const freq = values.get(`${band.id}.freq`);
  if (freq === undefined) return [];
  const gain = values.get(`${band.id}.gain`) ?? 0;
  const q = values.get(`${band.id}.q`) ?? Math.SQRT1_2;
  switch (band.kind) {
    case 'highpass': {
      // The slope switch is two staggered sections, never two at the same Q.
      const slope = Math.max(0, Math.round(values.get(`${band.id}.slope`) ?? 0));
      return slope === 0
        ? [highpass(freq, Math.SQRT1_2, sampleRate)]
        : [
            highpass(freq, BUTTERWORTH_Q4[0], sampleRate),
            highpass(freq, BUTTERWORTH_Q4[1], sampleRate),
          ];
    }
    case 'lowShelf':
      return [lowShelf(freq, q, gain, sampleRate)];
    case 'bell':
      return [peaking(freq, q, gain, sampleRate)];
    case 'highShelf':
      return [highShelf(freq, q, gain, sampleRate)];
  }
}

/** The whole EQ's response at `hz`, in dB, including the output trim. */
export function eqMagnitudeDbAt(values: EqValues, hz: number, sampleRate: number): number {
  let magnitude = 1;
  for (const band of EQ_BANDS) {
    for (const section of bandSections(band, values, sampleRate)) {
      magnitude *= magnitudeAt(section, hz, sampleRate);
    }
  }
  return 20 * Math.log10(Math.max(1e-12, magnitude)) + (values.get('output') ?? 0);
}

/**
 * The response sampled at `points` logarithmically spaced frequencies from
 * {@link EQ_MIN_HZ} to {@link EQ_MAX_HZ}, in dB.
 *
 * Log spacing because the x-axis is logarithmic: linear spacing would put four
 * fifths of the samples in the top two octaves and draw the bottom of the curve
 * as three straight lines.
 */
export function eqCurveDb(values: EqValues, sampleRate: number, points: number): number[] {
  const count = Math.max(2, Math.trunc(points));
  const ratio = EQ_MAX_HZ / EQ_MIN_HZ;
  return Array.from({ length: count }, (_, index) =>
    eqMagnitudeDbAt(values, EQ_MIN_HZ * ratio ** (index / (count - 1)), sampleRate),
  );
}

/** Where a frequency sits across a box of `width`, logarithmically. */
export function eqX(hz: number, width: number): number {
  const t = Math.log(hz / EQ_MIN_HZ) / Math.log(EQ_MAX_HZ / EQ_MIN_HZ);
  return Math.min(1, Math.max(0, t)) * width;
}

/**
 * The sampled response as an SVG `path` command.
 *
 * `rangeDb` is the EQ's own band-gain range, so the box is exactly as tall as
 * the parameters can reach; anything the highpass takes below that (a 24 dB/oct
 * slope leaves the box within an octave) is clamped to the floor rather than
 * rescaling the whole drawing around one corner.
 */
export function eqCurvePath(
  curve: readonly number[],
  width: number,
  height: number,
  rangeDb: number,
): string {
  const span = rangeDb > 0 ? rangeDb : 1;
  const step = width / Math.max(1, curve.length - 1);
  return curve
    .map((db, index) => {
      const y = height / 2 - (Math.min(span, Math.max(-span, db)) / span) * (height / 2);
      return `${index === 0 ? 'M' : 'L'}${(index * step).toFixed(2)} ${y.toFixed(2)}`;
    })
    .join(' ');
}
