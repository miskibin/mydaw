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
  const reduction = () => Math.min(12, gr());
  const transferPath = () => {
    const points = Array.from({ length: 25 }, (_, index) => {
      const input = -24 + index * 1.25;
      const output = Math.min(input + drive(), ceiling());
      const x = 12 + ((input + 24) / 30) * 176;
      const y = 108 - ((output + 24) / 30) * 88;
      return `${index === 0 ? 'M' : 'L'}${x.toFixed(1)} ${y.toFixed(1)}`;
    });
    return points.join(' ');
  };

  return (
    <>
      <div class="wv-fx__display wv-fx__display--comp">
        <SpectrumTerrain bins={props.spectrum} />
        <div class="wv-fx__compbody">
          <div class="wv-fx__limitread" title="Transfer curve: input level on the horizontal axis, output level on the vertical axis. The ceiling clips peaks above the limit.">
            <div class="wv-fx__limithead">
              <span>LIMITER RESPONSE</span>
              <strong>{reduction() < 0.05 ? 'NO REDUCTION' : `−${reduction().toFixed(1)} dB GAIN REDUCTION`}</strong>
            </div>
            <svg class="wv-fx__limitsvg" viewBox="0 0 200 126" role="img" aria-label="Limiter input to output transfer curve">
              <path class="wv-fx__limitgrid" d="M12 20H188 M12 64H188 M12 108H188 M12 20V108 M71 20V108 M130 20V108 M188 20V108" />
              <path class="wv-fx__limitunity" d="M12 108L188 20" />
              <path class="wv-fx__limitceil" d={`M12 ${(108 - ((ceiling() + 24) / 30) * 88).toFixed(1)}H188`} />
              <path class="wv-fx__limitcurve" d={transferPath()} />
              <circle class="wv-fx__limitpoint" cx="176" cy="31" r="3" />
              <text x="12" y="121">−24</text><text x="179" y="121">+6 dB IN</text>
              <text x="14" y="15">OUT</text>
            </svg>
            <div class="wv-fx__limitlegend">
              <span><i class="wv-fx__legendline wv-fx__legendline--unity" /> 1:1 signal</span>
              <span><i class="wv-fx__legendline wv-fx__legendline--curve" /> limited output</span>
              <span><i class="wv-fx__legendline wv-fx__legendline--ceiling" /> ceiling {ceiling().toFixed(1)} dB</span>
            </div>
          </div>
          <div class="wv-fx__gr" title="Peak gain reduction this block.">
            <span class="wv-fx__grlabel">GAIN REDUCTION</span>
            <div class="wv-fx__grtrack"><div class="wv-fx__grfill" style={{ width: `${(reduction() / 12) * 100}%` }} /></div>
            <span class="wv-fx__grnum">{gr() < 0.05 ? '0.0' : `−${gr().toFixed(1)}`} dB</span>
          </div>
        </div>
      </div>
      <PrimaryRow params={props.primary} knobs={props.knobs} oversized="gain" />
      <Advanced params={props.advanced} knobs={props.knobs} />
    </>
  );
}
