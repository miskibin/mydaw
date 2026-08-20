import type { EffectInfoDto, ParamValueDto } from '../../api/types';
import { compressorOutputDb, inputDbFromGr, reductionDb } from '../../lib/compressor';
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
  const attack = () => at('attack', 10);
  const release = () => at('release', 100);
  const envelopePath = () => {
    const a = Math.min(34, 8 + Math.log10(attack() + 1) * 12);
    const r = Math.min(92, a + 22 + Math.log10(release() + 1) * 13);
    return `M4 72 L24 72 L28 18 L${(28 + a).toFixed(1)} 38 L${r.toFixed(1)} 66 L96 72`;
  };
  const reductionPath = () => {
    const depth = Math.min(38, 5 + gr() * 2.5);
    return `M4 78 L28 78 L${(36 + attack() / 10).toFixed(1)} ${78 - depth} L${Math.min(94, 52 + release() / 35).toFixed(1)} 78 Z`;
  };

  return (
    <>
      <div class="wv-fx__display wv-fx__display--comp">
        <div class="wv-fx__timeview" classList={{ 'is-off': props.muted }} role="img" aria-label="Compression over time: input transient and gain reduction">
          <div class="wv-fx__vizhead"><span>ENVELOPE</span><strong>TIME →</strong></div>
          <svg viewBox="0 0 100 86" preserveAspectRatio="none" aria-hidden="true">
            <path class="wv-fx__vgrid" d="M0 22H100 M0 50H100 M25 0V86 M50 0V86 M75 0V86" />
            <path class="wv-fx__threshold" d={`M0 ${Math.max(10, 76 + threshold()).toFixed(1)}H100`} />
            <path class="wv-fx__grarea" d={reductionPath()} />
            <path class="wv-fx__envelope" d={envelopePath()} />
          </svg>
          <div class="wv-fx__vizlegend"><span><i class="is-input" /> input envelope</span><span><i class="is-gr" /> gain reduction</span></div>
        </div>
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
