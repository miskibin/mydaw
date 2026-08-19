import type { JSX } from 'solid-js';
import type { EffectInfoDto, ParamValueDto } from '../../api/types';
import { saturatorOut, saturatorType } from '../../lib/saturator';
import { formatValue } from '../../lib/param';
import { Segmented } from '../../ui/Segmented';
import { SpectrumTerrain } from '../../ui/SpectrumTerrain';
import { TransferCurve } from '../../ui/TransferCurve';
import type { InsertKnobs } from '../../ui/insertKnob';
import { Advanced, PrimaryRow } from './chrome';

export function SatEditor(props: {
  info: EffectInfoDto;
  knobs: InsertKnobs;
  values: Map<string, number>;
  live: ParamValueDto[];
  primary: EffectInfoDto['params'];
  advanced: EffectInfoDto['params'];
  muted: boolean;
  spectrum: readonly number[] | null;
}): JSX.Element {
  const typeDef = () => props.info.params.find((d) => d.id === 'type')!;
  const kind = () => saturatorType(props.values.get('type') ?? 0);
  const drive = () => props.values.get('drive') ?? 0;
  const auto = () => props.live.find((r) => r.id === 'auto_gain_db')?.value ?? 0;
  const knobsWithoutType = () => props.primary.filter((d) => d.id !== 'type');

  return (
    <>
      <div class="wv-fx__display">
        <SpectrumTerrain bins={props.spectrum} />
        <TransferCurve
          min={-1.5}
          max={1.5}
          yAt={(x) => saturatorOut(kind(), x, drive())}
          live={{ x: 1, y: saturatorOut(kind(), 1, drive()) }}
          marks={[0]}
          muted={props.muted}
          title="Input sample → shaped sample. The dot is 0 dBFS after drive."
        />
        <div class="wv-fx__satmeta">
          <Segmented
            def={typeDef()}
            typeId={props.info.typeId}
            value={props.values.get('type') ?? 0}
            onChange={(value) => {
              props.knobs.setParam('type', value);
              props.knobs.commit();
            }}
          />
          <span class="wv-fx__auto" title="Drive-compensated output, from the engine's table.">
            AUTO {formatValue('dB', auto())}
          </span>
        </div>
      </div>
      <PrimaryRow params={knobsWithoutType()} knobs={props.knobs} oversized="drive" />
      <Advanced params={props.advanced} knobs={props.knobs} />
    </>
  );
}
