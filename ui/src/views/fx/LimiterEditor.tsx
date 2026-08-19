import type { EffectInfoDto, ParamValueDto } from '../../api/types';
import { SpectrumTerrain } from '../../ui/SpectrumTerrain';
import type { InsertKnobs } from '../../ui/insertKnob';
import { Advanced, PrimaryRow } from './chrome';
import type { JSX } from 'solid-js';

export function LimiterEditor(props: {
  info: EffectInfoDto;
  knobs: InsertKnobs;
  values: Map<string, number>;
  live: ParamValueDto[];
  primary: EffectInfoDto['params'];
  advanced: EffectInfoDto['params'];
  muted: boolean;
  spectrum: readonly number[] | null;
}): JSX.Element {
  const gr = () => Math.abs(props.live.find((r) => r.id === 'gr_max')?.value ?? 0);
  const ceiling = () => props.values.get('ceiling') ?? 0;

  return (
    <>
      <div class="wv-fx__display wv-fx__display--comp">
        <SpectrumTerrain bins={props.spectrum} />
        <div class="wv-fx__compbody">
          <div class="wv-fx__limitread" title="Ceiling is the loudest sample that may leave. GR is how hard it is catching.">
            <span>ceiling {ceiling().toFixed(1)} dB</span>
            <span>{gr() < 0.05 ? 'open' : `−${gr().toFixed(1)} dB GR`}</span>
          </div>
          <div class="wv-fx__gr" title="Peak gain reduction this block.">
            <div
              class="wv-fx__grfill"
              style={{ height: `${Math.min(100, (gr() / Math.max(12, gr())) * 100)}%` }}
            />
            <span class="wv-fx__grnum">{gr() < 0.05 ? '0.0' : `−${gr().toFixed(1)}`} dB</span>
          </div>
        </div>
      </div>
      <PrimaryRow params={props.primary} knobs={props.knobs} oversized="gain" />
      <Advanced params={props.advanced} knobs={props.knobs} />
    </>
  );
}
