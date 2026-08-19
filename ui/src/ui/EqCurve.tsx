/**
 * The EQ's magnitude response, drawn.
 *
 * A single `<path>` recomputed when the parameters change and at no other time.
 * It is deliberately **not** on the frame loop: an EQ curve is a picture of
 * settings, and settings only move when somebody moves them -- repainting it
 * sixty times a second would be sixty identical drawings a second.
 *
 * What it is honest about is in {@link EQ_CURVE_NOTE}, which every surface that
 * draws this puts in its `title`. See `lib/eq.ts` for why that sentence has to
 * exist.
 */

import { For, Show, createMemo, type JSX } from 'solid-js';

import { EQ_MAX_HZ, EQ_MIN_HZ, eqCurveDb, eqCurvePath, eqX, type EqValues } from '../lib/eq';
import { useAppState } from '../state';

/**
 * What the curve is, said plainly.
 *
 * The engine can evaluate this response against the coefficients it is actually
 * running, and does so in its own tests -- but it publishes no command that
 * returns one, so this drawing is the webview's own arithmetic. That is a real
 * distinction and the tooltip makes it rather than letting the picture imply a
 * measurement.
 */
export const EQ_CURVE_NOTE =
  'The shell computes this curve from the EQ’s declared filters (the same RBJ ' +
  'biquad formulas the engine runs) at the settings shown. It is a drawing of ' +
  'what the EQ is set to do — not a measurement of the audio, which the ' +
  'engine does not publish.';

/** The path's own coordinate space. The SVG is stretched to whatever box it is in. */
const VIEW_W = 1000;
const VIEW_H = 100;

/** Samples across the band. Enough that a Q of 10 is a curve and not a corner. */
const POINTS = 220;

/** The decades a frequency axis is read by. */
const DECADES = [
  { hz: 100, label: '100' },
  { hz: 1_000, label: '1k' },
  { hz: 10_000, label: '10k' },
];

export function EqCurve(props: {
  /** Every parameter of this EQ, by id, live during a drag. */
  values: EqValues;
  /** The EQ's own band-gain range, so the box is as tall as the knobs reach. */
  rangeDb: number;
  /** CSS height of the box. */
  height: number;
  /** Draw the decade grid and its labels. The compact card has no room. */
  detailed?: boolean;
  /** Drawn grey when the insert is bypassed: the settings still, the sound not. */
  muted?: boolean;
}): JSX.Element {
  const app = useAppState();

  /**
   * The rate the filters are evaluated at.
   *
   * A biquad's response depends on it -- a 20 kHz shelf on a 44.1 kHz device is
   * a different curve from the same shelf at 96 kHz -- so the drawing follows
   * the open device rather than assuming 48 kHz. Before a stream exists the
   * engine's own fallback applies, in `lib/eq.ts`.
   */
  const sampleRate = () => {
    const fromSequencer = app.sequencer()?.sampleRate ?? 0;
    return fromSequencer > 0 ? fromSequencer : (app.streamInfo()?.sampleRate ?? 0);
  };

  const curve = createMemo(() => eqCurveDb(props.values, sampleRate(), POINTS));
  const line = createMemo(() => eqCurvePath(curve(), VIEW_W, VIEW_H, props.rangeDb));
  /** The curve closed back along the 0 dB line, so boost and cut are shaded. */
  const area = () => `${line()} L${VIEW_W} ${VIEW_H / 2} L0 ${VIEW_H / 2} Z`;

  return (
    <div
      class="wv-eqc"
      classList={{ 'is-muted': props.muted, 'wv-eqc--detailed': props.detailed }}
      style={{ height: `${props.height}px` }}
      title={EQ_CURVE_NOTE}
    >
      <svg
        class="wv-eqc__svg"
        viewBox={`0 0 ${VIEW_W} ${VIEW_H}`}
        preserveAspectRatio="none"
        aria-hidden="true"
      >
        <Show when={props.detailed}>
          <For each={DECADES}>
            {(decade) => (
              <path
                class="wv-eqc__grid"
                d={`M${eqX(decade.hz, VIEW_W).toFixed(1)} 0 V${VIEW_H}`}
                vector-effect="non-scaling-stroke"
              />
            )}
          </For>
          <For each={[0.25, 0.75]}>
            {(t) => (
              <path
                class="wv-eqc__grid"
                d={`M0 ${VIEW_H * t} H${VIEW_W}`}
                vector-effect="non-scaling-stroke"
              />
            )}
          </For>
        </Show>
        <path
          class="wv-eqc__zero"
          d={`M0 ${VIEW_H / 2} H${VIEW_W}`}
          vector-effect="non-scaling-stroke"
        />
        <path class="wv-eqc__area" d={area()} />
        <path class="wv-eqc__line" d={line()} vector-effect="non-scaling-stroke" />
      </svg>

      <Show when={props.detailed}>
        <For each={DECADES}>
          {(decade) => (
            <span
              class="wv-eqc__hz"
              style={{ left: `${(eqX(decade.hz, VIEW_W) / VIEW_W) * 100}%` }}
            >
              {decade.label}
            </span>
          )}
        </For>
        <span class="wv-eqc__db wv-eqc__db--top">+{props.rangeDb.toFixed(0)}</span>
        <span class="wv-eqc__db wv-eqc__db--bottom">&minus;{props.rangeDb.toFixed(0)} dB</span>
        <span class="wv-eqc__hz wv-eqc__hz--end" style={{ left: '100%' }}>
          {EQ_MAX_HZ / 1000}k
        </span>
        <span class="wv-eqc__hz wv-eqc__hz--start" style={{ left: '0%' }}>
          {EQ_MIN_HZ}
        </span>
      </Show>
    </div>
  );
}
