import { For, Show, createMemo, type JSX } from 'solid-js';

import type { EffectInfoDto } from '../../api/types';
import {
  MAX_DRAWN_LEVELS,
  MAX_TONE_HZ,
  crushedCycle,
  holdRateHz,
  linePath,
  quantiseStep,
  stepPath,
} from '../../lib/bitcrusher';
import { analyserLine, analyserPath } from '../../lib/eq';
import { formatValue } from '../../lib/param';
import type { InsertKnobs } from '../../ui/insertKnob';
import { Advanced, PrimaryRow } from './chrome';

const VIEW_W = 400;
const VIEW_H = 160;

export function BitcrusherEditor(props: {
  info: EffectInfoDto;
  knobs: InsertKnobs;
  values: Map<string, number>;
  primary: EffectInfoDto['params'];
  advanced: EffectInfoDto['params'];
  muted: boolean;
  sampleRate: number;
  spectrum: readonly number[] | null;
}): JSX.Element {
  const at = (id: string, fallback: number) => props.values.get(id) ?? fallback;
  const bits = () => at('bits', 24);
  const downsample = () => at('downsample', 1);
  const tone = () => at('tone', MAX_TONE_HZ);

  /**
   * One cycle of a sine through the same quantiser and hold the engine runs.
   *
   * Drawn at four times the hold ratio so the treads are always visible: the
   * picture is of the *shape* the two knobs make, and a staircase sampled at
   * exactly its own step count would draw as a straight line.
   */
  const cycle = createMemo(() => crushedCycle(bits(), downsample(), 256));
  const levels = createMemo(() => {
    const step = quantiseStep(bits());
    if (step === null) return [];
    const count = Math.round(2 / step) + 1;
    if (count > MAX_DRAWN_LEVELS) return [];
    return Array.from({ length: count }, (_, index) => -1 + index * step);
  });

  const analyser = () => {
    const bins = props.spectrum;
    if (!bins || bins.length < 2) return null;
    return { fill: analyserPath(bins, VIEW_W, VIEW_H), ridge: analyserLine(bins, VIEW_W, VIEW_H) };
  };

  return (
    <>
      <div class="wv-fx__display wv-fx__display--crush">
        <div
          class="wv-fx__vizpanel"
          classList={{ 'is-off': props.muted }}
          title="One cycle of a sine through this bitcrusher: the ghost is the input, the staircase is what leaves. Horizontal lines are the levels the bit depth allows; each tread is one held sample."
        >
          <div class="wv-fx__vizhead">
            <span>QUANTISED WAVEFORM</span>
            <strong>
              {bits() >= 24 ? 'full depth' : `${2 ** Math.round(bits())} levels`} ·{' '}
              {downsample() <= 1 ? 'every sample' : `1 in ${Math.round(downsample())}`}
            </strong>
          </div>
          <div class="wv-crush__stage">
            <svg
              class="wv-crush__svg"
              viewBox={`0 0 ${VIEW_W} ${VIEW_H}`}
              preserveAspectRatio="none"
              aria-hidden="true"
            >
              <For each={levels()}>
                {(level) => (
                  <path
                    class="wv-crush__level"
                    d={`M0 ${((1 - level) * VIEW_H) / 2} H${VIEW_W}`}
                    vector-effect="non-scaling-stroke"
                  />
                )}
              </For>
              <path
                class="wv-resp__zero"
                d={`M0 ${VIEW_H / 2} H${VIEW_W}`}
                vector-effect="non-scaling-stroke"
              />
              <path
                class="wv-crush__clean"
                d={linePath(cycle().clean, VIEW_W, VIEW_H)}
                vector-effect="non-scaling-stroke"
              />
              <path
                class="wv-crush__step"
                d={stepPath(cycle().crushed, VIEW_W, VIEW_H)}
                vector-effect="non-scaling-stroke"
              />
            </svg>
          </div>
          <div class="wv-fx__vizlegend">
            <span>
              <i class="is-ghost" /> input
            </span>
            <span>
              <i /> crushed
            </span>
            <span>
              hold {formatValue('Hz', holdRateHz(props.sampleRate, downsample()))}
            </span>
          </div>
        </div>

        <div
          class="wv-fx__vizpanel"
          classList={{ 'is-off': props.muted }}
          title="The spectrum leaving this effect. A zero-order hold mirrors every partial around its hold rate, and nothing here removes those images — the ring you hear is these."
        >
          <div class="wv-fx__vizhead">
            <span>OUTPUT SPECTRUM</span>
            <strong>{tone() >= MAX_TONE_HZ ? 'filter off' : `lp ${formatValue('Hz', tone())}`}</strong>
          </div>
          <div class="wv-crush__stage">
            <svg
              class="wv-crush__svg"
              viewBox={`0 0 ${VIEW_W} ${VIEW_H}`}
              preserveAspectRatio="none"
              aria-hidden="true"
            >
              <Show when={analyser()} fallback={<path class="wv-resp__zero" d={`M0 ${VIEW_H - 1} H${VIEW_W}`} vector-effect="non-scaling-stroke" />}>
                {(spectrum) => (
                  <>
                    <path class="wv-eqc__spec" d={spectrum().fill} />
                    <path
                      class="wv-eqc__specline"
                      d={spectrum().ridge}
                      vector-effect="non-scaling-stroke"
                    />
                  </>
                )}
              </Show>
            </svg>
            <Show when={!analyser()}>
              <span class="wv-crush__idle">no signal through this insert yet</span>
            </Show>
          </div>
          <div class="wv-fx__vizlegend">
            <span>20 Hz</span>
            <span>images at multiples of the hold rate</span>
            <span>20 kHz</span>
          </div>
        </div>
      </div>
      <PrimaryRow params={props.primary} knobs={props.knobs} oversized="bits" />
      <Advanced params={props.advanced} knobs={props.knobs} />
    </>
  );
}
