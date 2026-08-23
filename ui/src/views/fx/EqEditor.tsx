import type { JSX } from 'solid-js';
import type { EffectInfoDto } from '../../api/types';
import { EqCurve } from '../../ui/EqCurve';
import type { InsertKnobs } from '../../ui/insertKnob';
import { bandGainRangeDb } from '../../lib/effectLayout';
import { Advanced, PrimaryRow } from './chrome';

export function EqEditor(props: {
  info: EffectInfoDto;
  knobs: InsertKnobs;
  values: Map<string, number>;
  primary: EffectInfoDto['params'];
  advanced: EffectInfoDto['params'];
  muted: boolean;
  spectrum: readonly number[] | null;
  onListen: (bandId: string | null) => void;
  onAnnounce: (name: string, value: string) => void;
}): JSX.Element {
  return (
    <>
      <div class="wv-fx__display wv-fx__display--eq">
        <div class="wv-fx__stage">
          <EqCurve
            values={props.values}
            rangeDb={bandGainRangeDb(props.info)}
            height={260}
            detailed
            muted={props.muted}
            spectrum={props.spectrum}
            onParam={(id, value) => props.knobs.setParam(id, value)}
            onCommit={() => props.knobs.commit()}
            onListen={props.onListen}
            onAnnounce={props.onAnnounce}
          />
        </div>
      </div>
      <PrimaryRow params={props.primary} knobs={props.knobs} />
      <Advanced params={props.advanced} knobs={props.knobs} />
    </>
  );
}
