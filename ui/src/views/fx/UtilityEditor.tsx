import type { JSX } from 'solid-js';

import type { EffectInfoDto, ParamValueDto } from '../../api/types';
import { correlation } from '../../lib/goniometer';
import { formatValue } from '../../lib/param';
import { Goniometer } from '../../ui/Goniometer';
import type { InsertKnobs } from '../../ui/insertKnob';
import { Advanced, PrimaryRow } from './chrome';

export function UtilityEditor(props: {
  info: EffectInfoDto;
  knobs: InsertKnobs;
  values: Map<string, number>;
  live: ParamValueDto[];
  primary: EffectInfoDto['params'];
  advanced: EffectInfoDto['params'];
  muted: boolean;
  scopeL: readonly number[];
  scopeR: readonly number[];
}): JSX.Element {
  const corr = () => {
    const published = props.live.find((r) => r.id === 'correlation')?.value;
    if (published !== undefined) return published;
    return correlation(props.scopeL, props.scopeR);
  };

  return (
    <>
      <div class="wv-fx__display wv-fx__display--util">
        <Goniometer
          left={() => props.scopeL}
          right={() => props.scopeR}
          pan={props.values.get('pan') ?? 0}
          widthPct={props.values.get('width') ?? 100}
          muted={props.muted}
          onDrag={(next) => {
            props.knobs.setParam('pan', next.pan);
            props.knobs.setParam('width', next.widthPct);
          }}
          onCommit={() => props.knobs.commit()}
        />
        <span class="wv-fx__corr" title="Stereo correlation. 1 is mono, 0 uncorrelated, −1 inverted.">
          corr {corr().toFixed(2)}
          {' · '}
          width {formatValue('%', props.values.get('width') ?? 100)}
        </span>
      </div>
      <PrimaryRow params={props.primary} knobs={props.knobs} oversized="gain" />
      <Advanced params={props.advanced} knobs={props.knobs} />
    </>
  );
}
