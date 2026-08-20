import type { EffectInfoDto } from '../../api/types';
import { SpectrumTerrain } from '../../ui/SpectrumTerrain';
import type { InsertKnobs } from '../../ui/insertKnob';
import { Advanced, PrimaryRow } from './chrome';
import type { JSX } from 'solid-js';

export function ReverbEditor(props: {
  info: EffectInfoDto;
  knobs: InsertKnobs;
  values: Map<string, number>;
  primary: EffectInfoDto['params'];
  advanced: EffectInfoDto['params'];
  muted: boolean;
  spectrum: readonly number[] | null;
}): JSX.Element {
  const decay = () => props.values.get('decay') ?? 1.6;
  const mix = () => props.values.get('mix') ?? 25;
  const size = () => props.values.get('size') ?? 80;
  const wall = () => 10 + (100 - size()) * 0.09;
  const tailOpacity = () => Math.min(0.8, 0.18 + decay() / 5);

  return (
    <>
      <div class="wv-fx__display">
        <div class="wv-fx__room" classList={{ 'is-off': props.muted }} title="Early reflections bounce between the virtual walls; longer decay leaves a brighter tail.">
          <SpectrumTerrain bins={props.spectrum} />
          <svg class="wv-fx__roomsvg" viewBox="0 0 100 58" preserveAspectRatio="none" aria-hidden="true">
            <rect class="wv-fx__roomwall" x={wall()} y="6" width={100 - wall() * 2} height="46" rx="2" />
            <path class="wv-fx__ray wv-fx__ray--dry" d="M18 37 L34 28 L48 34 L62 23 L82 29" />
            <path class="wv-fx__ray" style={{ opacity: tailOpacity() }} d="M20 37 L32 8 L54 50 L74 9 L86 28" />
            <path class="wv-fx__ray wv-fx__ray--late" style={{ opacity: tailOpacity() * 0.75 }} d="M22 38 L43 51 L66 12 L83 47" />
            <circle class="wv-fx__source" cx="18" cy="37" r="2.4" />
            <circle class="wv-fx__listener" cx="84" cy="29" r="2.7" />
          </svg>
        </div>
        <p class="wv-fx__topo" classList={{ 'is-off': props.muted }}>
          {size().toFixed(0)}% room · {decay().toFixed(1)} s decay · {mix().toFixed(0)}% wet
        </p>
      </div>
      <PrimaryRow params={props.primary} knobs={props.knobs} oversized="mix" />
      <Advanced params={props.advanced} knobs={props.knobs} />
    </>
  );
}
