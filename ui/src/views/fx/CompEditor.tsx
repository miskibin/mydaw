import type { EffectInfoDto, ParamValueDto } from '../../api/types';
import { compressorOutputDb, inputDbFromGr, reductionDb } from '../../lib/compressor';
import { SpectrumTerrain } from '../../ui/SpectrumTerrain';
import { TransferCurve } from '../../ui/TransferCurve';
import type { InsertKnobs } from '../../ui/insertKnob';
import { Advanced, PrimaryRow } from './chrome';
import type { JSX } from 'solid-js';

export function CompEditor(props: {
  info: EffectInfoDto;
  knobs: InsertKnobs;
  values: Map<string, number>;
  live: ParamValueDto[];
  primary: EffectInfoDto['params'];
  advanced: EffectInfoDto['params'];
  muted: boolean;
  spectrum: readonly number[] | null;
}): JSX.Element {
  const at = (id: string, fallback: number) => props.values.get(id) ?? fallback;
  const gr = () => Math.abs(props.live.find((r) => r.id === 'gr_max')?.value ?? 0);
  const threshold = () => at('threshold', -18);
  const ratio = () => at('ratio', 4);
  const knee = () => at('knee', 6);
  const makeup = () => at('makeup', 0) + (at('auto_makeup', 0) > 0.5 ? reductionDb(0, threshold(), ratio(), knee()) : 0);

  const live = () => {
    const reduction = gr();
    const x = inputDbFromGr(reduction, threshold(), ratio(), knee());
    return { x, y: compressorOutputDb(x, threshold(), ratio(), knee(), makeup()) };
  };

  return (
    <>
      <div class="wv-fx__display wv-fx__display--comp">
        <SpectrumTerrain bins={props.spectrum} />
        <div class="wv-fx__compbody">
          <TransferCurve
            min={-60}
            max={0}
            yAt={(input) => compressorOutputDb(input, threshold(), ratio(), knee(), makeup())}
            live={live()}
            marks={[threshold()]}
            muted={props.muted}
            title="Input dB → output dB. The dot is the current gain reduction on this curve."
          />
          <div class="wv-fx__gr" title="Peak gain reduction this block. Click the number to note it.">
            <div
              class="wv-fx__grfill"
              style={{ height: `${Math.min(100, (gr() / Math.max(12, gr())) * 100)}%` }}
            />
            <span class="wv-fx__grnum">{gr() < 0.05 ? '0.0' : `−${gr().toFixed(1)}`} dB</span>
          </div>
        </div>
      </div>
      <PrimaryRow params={props.primary} knobs={props.knobs} />
      <Advanced params={props.advanced} knobs={props.knobs} />
    </>
  );
}
