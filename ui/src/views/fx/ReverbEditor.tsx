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

  return (
    <>
      <div class="wv-fx__display">
        <SpectrumTerrain bins={props.spectrum} />
        <p class="wv-fx__topo" classList={{ 'is-off': props.muted }}>
          {size().toFixed(0)}% room · {decay().toFixed(1)} s decay · {mix().toFixed(0)}% wet
        </p>
      </div>
      <PrimaryRow params={props.primary} knobs={props.knobs} oversized="mix" />
      <Advanced params={props.advanced} knobs={props.knobs} />
    </>
  );
}
