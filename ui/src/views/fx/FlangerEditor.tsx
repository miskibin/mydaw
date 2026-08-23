import { createMemo, type JSX } from 'solid-js';

import type { EffectInfoDto } from '../../api/types';
import { EQ_MAX_HZ } from '../../lib/eq';
import { flangerDelayMs, flangerMagDb } from '../../lib/modulation';
import { formatValue } from '../../lib/param';
import { ResponseCurve, sampleResponse } from '../../ui/ResponseCurve';
import type { InsertKnobs } from '../../ui/insertKnob';
import { Advanced, PrimaryRow } from './chrome';
import { createLfoPhase } from './lfoClock';

/**
 * Points along the curve.
 *
 * A 10 ms comb puts a notch every 50 Hz, which is thirty of them in the top
 * octave alone; nine hundred samples keeps the ones a musician is listening to
 * -- the low, wide ones -- clean, and lets the crowded top of the axis read as
 * the dense haze it audibly is.
 */
const POINTS = 900;

export function FlangerEditor(props: {
  info: EffectInfoDto;
  knobs: InsertKnobs;
  values: Map<string, number>;
  primary: EffectInfoDto['params'];
  advanced: EffectInfoDto['params'];
  muted: boolean;
  spectrum: readonly number[] | null;
}): JSX.Element {
  const at = (id: string, fallback: number) => props.values.get(id) ?? fallback;
  const phase = createLfoPhase({
    rate: () => at('rate', 0.25),
    running: () => !props.muted,
  });

  const manual = () => at('delay', 1);
  const depth = () => at('depth', 3);
  const feedback = () => at('feedback', 40) / 100;
  const mix = () => at('mix', 50) / 100;
  const delayNow = () => flangerDelayMs(manual(), depth(), phase());

  const response = (ms: number) =>
    sampleResponse(POINTS, (hz) => flangerMagDb(hz, ms, feedback(), mix()));
  const curve = createMemo(() => response(delayNow()));
  const band = createMemo(() =>
    depth() > 0.01 ? { lower: response(manual()), upper: response(manual() + depth()) } : null,
  );

  /** Where the comb cancels right now: `(2k+1)/2d`, as far as the axis goes. */
  const notches = createMemo(() => {
    const spacing = 1000 / (2 * Math.max(0.05, delayNow()));
    const marks: number[] = [];
    for (let k = 0; k < 40; k += 1) {
      const hz = (2 * k + 1) * spacing;
      if (hz > EQ_MAX_HZ) break;
      marks.push(hz);
    }
    return marks;
  });

  return (
    <>
      <div class="wv-fx__display">
        <ResponseCurve
          curve={curve()}
          band={band()}
          rangeDb={18}
          spectrum={props.spectrum}
          muted={props.muted}
          label="COMB RESPONSE"
          value={`${formatValue('ms', delayNow())} · ${notches().length} notches`}
          title={
            'The comb the flanger is making right now, drawn from its settings — dry plus a ' +
            'delayed copy, resonating through the feedback loop. The shaded band is the whole ' +
            'travel of the sweep; the moving curve is where in that travel the LFO is.'
          }
          marks={notches().slice(0, 12)}
        />
        <p class="wv-fx__topo">
          notches every {formatValue('Hz', 1000 / Math.max(0.05, delayNow()))} · sweep{' '}
          {formatValue('ms', manual())} → {formatValue('ms', manual() + depth())} at{' '}
          {at('rate', 0.25).toFixed(2)} Hz · feedback {at('feedback', 40).toFixed(0)} %
          {at('feedback', 40) < 0 ? ' (inverted)' : ''}
        </p>
      </div>
      <PrimaryRow params={props.primary} knobs={props.knobs} oversized="delay" />
      <Advanced params={props.advanced} knobs={props.knobs} />
    </>
  );
}
