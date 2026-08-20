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
  const drive = () => props.values.get('gain') ?? 0;
  const crush = () => Math.min(1, gr() / 12);

  return (
    <>
      <div class="wv-fx__display wv-fx__display--comp">
        <SpectrumTerrain bins={props.spectrum} />
        <div class="wv-fx__compbody">
          <div class="wv-fx__limitread" title="Ceiling is the loudest sample that may leave. GR is how hard it is catching.">
            <svg class="wv-fx__limitsvg" viewBox="0 0 120 82" aria-hidden="true">
              <path class="wv-fx__limitwave wv-fx__limitwave--in" d="M2 41 C12 4 24 4 34 41 S56 78 66 41 S88 4 98 41 S112 78 118 41" />
              <path class="wv-fx__limitceil" d="M0 18 H120 M0 64 H120" />
              <path class="wv-fx__limitwave" style={{ opacity: 0.45 + crush() * 0.5 }} d="M2 41 C12 18 24 18 34 41 S56 64 66 41 S88 18 98 41 S112 64 118 41" />
            </svg>
            <span>ceiling {ceiling().toFixed(1)} dB · input {drive().toFixed(1)} dB</span>
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
