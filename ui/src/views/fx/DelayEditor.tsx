import { For, Show, type JSX } from 'solid-js';

import type { EffectInfoDto } from '../../api/types';
import { DIVISION_LABELS, MODIFIER_LABELS, delayFilterMagDb, delayTimeMs } from '../../lib/delayTime';
import { eqCurvePath, EQ_MAX_HZ, EQ_MIN_HZ } from '../../lib/eq';
import { formatValue } from '../../lib/param';
import { Segmented } from '../../ui/Segmented';
import { SpectrumTerrain } from '../../ui/SpectrumTerrain';
import type { InsertKnobs } from '../../ui/insertKnob';
import { Advanced, PrimaryRow } from './chrome';

export function DelayEditor(props: {
  info: EffectInfoDto;
  knobs: InsertKnobs;
  values: Map<string, number>;
  primary: EffectInfoDto['params'];
  advanced: EffectInfoDto['params'];
  muted: boolean;
  bpm: number;
  sampleRate: number;
  spectrum: readonly number[] | null;
}): JSX.Element {
  const at = (id: string, fallback: number) => props.values.get(id) ?? fallback;
  const sync = () => at('sync', 1) >= 0.5;
  const division = () => props.info.params.find((d) => d.id === 'division')!;
  const modifier = () => props.info.params.find((d) => d.id === 'modifier')!;
  const timeMs = () =>
    delayTimeMs(sync(), at('division', 3), at('modifier', 0), at('time_ms', 375), props.bpm);
  const feedback = () => Math.max(0, Math.min(95, at('feedback', 35)));
  const tapOpacity = (tap: number) => Math.max(0.12, (feedback() / 100) ** (tap - 1));

  const filterPath = () => {
    const points = 120;
    const ratio = EQ_MAX_HZ / EQ_MIN_HZ;
    const curve = Array.from({ length: points }, (_, i) => {
      const hz = EQ_MIN_HZ * ratio ** (i / (points - 1));
      return delayFilterMagDb(hz, at('damp_hp', 150), at('damp_lp', 6500), props.sampleRate);
    });
    return eqCurvePath(curve, 1000, 100, 18);
  };

  return (
    <>
      <div class="wv-fx__display">
        <div class="wv-fx__delayhead">
          <Segmented
            def={division()}
            typeId={props.info.typeId}
            value={at('division', 3)}
            labels={DIVISION_LABELS}
            onChange={(value) => {
              props.knobs.setParam('division', value);
              props.knobs.commit();
            }}
          />
          <Segmented
            def={modifier()}
            typeId={props.info.typeId}
            value={at('modifier', 0)}
            labels={MODIFIER_LABELS}
            onChange={(value) => {
              props.knobs.setParam('modifier', value);
              props.knobs.commit();
            }}
          />
          <span class="wv-fx__time" title="Resolved delay time from the same mapping the engine uses.">
            {formatValue('ms', timeMs())}
          </span>
        </div>
        <div class="wv-fx__stage" style={{ 'min-height': '200px' }}>
          <SpectrumTerrain bins={props.spectrum} />
          <div class="wv-fx__delaytaps" title="Echo taps get quieter with feedback; the filter curve shows each repeat being darkened or thinned.">
            <For each={[1, 2, 3, 4, 5]}>
              {(tap) => <i style={{ left: `${12 + tap * 14}%`, height: `${64 * tapOpacity(tap)}%`, opacity: tapOpacity(tap) }} />}
            </For>
          </div>
          <div class="wv-eqc wv-eqc--detailed wv-eqc--overlay" title="In-loop HP and LP. Each repeat is filtered again.">
            <svg class="wv-eqc__svg" viewBox="0 0 1000 100" preserveAspectRatio="none">
              <path class="wv-eqc__zero" d="M0 50 H1000" vector-effect="non-scaling-stroke" />
              <path class="wv-eqc__line" d={filterPath()} vector-effect="non-scaling-stroke" />
            </svg>
          </div>
        </div>
        <p class="wv-fx__topo">in → delay → HP → LP → feedback ↺ → mix</p>
      </div>
      <PrimaryRow params={props.primary} knobs={props.knobs} />
      <Show when={!sync()}>
        <PrimaryRow
          params={props.info.params.filter((d) => d.id === 'time_ms')}
          knobs={props.knobs}
        />
      </Show>
      <Advanced
        params={props.advanced.filter((d) => d.id !== 'division' && d.id !== 'modifier' && d.id !== 'time_ms')}
        knobs={props.knobs}
      />
    </>
  );
}
