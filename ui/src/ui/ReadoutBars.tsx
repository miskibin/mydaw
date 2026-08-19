/**
 * The meters an effect publishes, as bars.
 *
 * Split out of the inspector because the fullscreen editor draws the same thing
 * from the same poll: a compressor's gain reduction has to be visible wherever
 * its threshold is, and two implementations of one bar is two scales that can
 * disagree about what 6 dB looks like.
 *
 * Every value here comes from `get_chain_readouts`. Nothing is drawn that the
 * effect did not declare, and an effect that declares none renders nothing.
 */

import { Index, type JSX } from 'solid-js';

import type { ParamValueDto, ReadoutDefDto } from '../api/types';
import { formatValue } from '../lib/param';

import '../styles/side.css';

/**
 * How much of a bar a readout fills, by the unit the engine declared.
 *
 * The full-scale figures are the UI's: 24 dB is where a gain-reduction meter
 * stops being informative, 512 samples is more latency than any of these
 * effects can introduce. A unit nobody has met yet fills on `0..1`, which is
 * wrong-looking rather than silently misleading.
 */
export function readoutFraction(unit: string, value: number): number {
  const size = Math.abs(value);
  switch (unit) {
    case 'dB':
      return Math.min(1, size / 24);
    case '%':
      return Math.min(1, size / 100);
    case 'samples':
      return Math.min(1, size / 512);
    default:
      return Math.min(1, size);
  }
}

export function ReadoutBars(props: {
  defs: ReadoutDefDto[];
  /** The live values for this insert, as the poll last delivered them. */
  live: ParamValueDto[];
  /** Widens the bars and the labels for the fullscreen editor. */
  large?: boolean;
}): JSX.Element {
  return (
    <div class="wv-meters" classList={{ 'wv-meters--large': props.large }}>
      <Index each={props.defs}>
        {(def) => {
          const value = () => props.live.find((entry) => entry.id === def().id)?.value ?? 0;
          return (
            <div class="wv-meterbar" title={`${def().name}, live from the engine`}>
              <span class="wv-meterbar__name">{def().name}</span>
              <span class="wv-meterbar__track">
                <span
                  class="wv-meterbar__fill"
                  style={{ width: `${readoutFraction(def().unit, value()) * 100}%` }}
                />
              </span>
              <span class="wv-meterbar__value">{formatValue(def().unit, value())}</span>
            </div>
          );
        }}
      </Index>
    </div>
  );
}
