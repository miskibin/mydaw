import { createMemo, type JSX } from 'solid-js';

import type { EffectInfoDto } from '../../api/types';
import { EQ_MAX_HZ, EQ_MIN_HZ } from '../../lib/eq';
import { phaserCentreHz, phaserMagDb, phaserNotchHz } from '../../lib/modulation';
import { formatValue } from '../../lib/param';
import { Segmented } from '../../ui/Segmented';
import { ResponseCurve, sampleResponse } from '../../ui/ResponseCurve';
import type { InsertKnobs } from '../../ui/insertKnob';
import { Advanced, PrimaryRow } from './chrome';
import { createLfoPhase } from './lfoClock';

const POINTS = 320;

/** `phaser::STAGE_COUNTS`. */
const STAGE_COUNTS = [4, 6, 8] as const;

export function PhaserEditor(props: {
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
  const phase = createLfoPhase({
    rate: () => at('rate', 0.5),
    running: () => !props.muted,
  });

  const stagesDef = () => props.info.params.find((def) => def.id === 'stages');
  const stages = () => STAGE_COUNTS[Math.min(2, Math.max(0, Math.round(at('stages', 0))))] ?? 4;
  const centre = () => at('freq', 800);
  const depth = () => at('depth', 70);
  const feedback = () => at('feedback', 30) / 100;
  const mix = () => at('mix', 50) / 100;
  const centreNow = () => phaserCentreHz(centre(), depth(), phase());

  const response = (fc: number) =>
    sampleResponse(POINTS, (hz) =>
      phaserMagDb(hz, fc, stages(), feedback(), mix(), props.sampleRate),
    );
  const curve = createMemo(() => response(centreNow()));
  const band = createMemo(() => {
    const octaves = (depth() / 100) * 2;
    if (octaves < 0.01) return null;
    return {
      lower: response(centre() / 2 ** octaves),
      upper: response(centre() * 2 ** octaves),
    };
  });

  const notches = createMemo(() => {
    const marks: number[] = [];
    for (let index = 0; index < stages() / 2; index += 1) {
      const hz = phaserNotchHz(centreNow(), stages(), index, props.sampleRate);
      if (Number.isFinite(hz) && hz > EQ_MIN_HZ && hz < EQ_MAX_HZ) marks.push(hz);
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
          label="ALL-PASS NOTCHES"
          value={`${stages()} stages · ${notches().length} notches · ${formatValue('Hz', centreNow())}`}
          title={
            'Where the cascade has rotated the phase far enough to cancel against the dry ' +
            'path, drawn from the settings. The dashed lines are the notches; the shaded band ' +
            'is the whole travel of the sweep.'
          }
          marks={notches()}
        />
        <div class="wv-fx__satmeta">
          {stagesDef() && (
            <Segmented
              def={stagesDef()!}
              typeId={props.info.typeId}
              value={at('stages', 0)}
              onChange={(value) => {
                props.knobs.setParam('stages', value);
                props.knobs.commit();
              }}
            />
          )}
          <span class="wv-fx__topo">
            {stages()} all-pass stages · {notches().length} notches, unevenly spaced ·{' '}
            {formatValue('Hz', notches()[0] ?? centreNow())} lowest
          </span>
        </div>
      </div>
      <PrimaryRow params={props.primary} knobs={props.knobs} oversized="freq" />
      <Advanced params={props.advanced} knobs={props.knobs} />
    </>
  );
}
