import type { EffectInfoDto } from '../../api/types';
import { analyserLine, analyserPath } from '../../lib/eq';
import type { InsertKnobs } from '../../ui/insertKnob';
import { Advanced, PrimaryRow } from './chrome';
import { Show, type JSX } from 'solid-js';

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
  const wall = () => 10 + (100 - size()) * 0.09;
  const tailOpacity = () => Math.min(0.8, 0.18 + decay() / 5);
  const predelay = () => props.values.get('predelay') ?? props.values.get('pre_delay') ?? 0;
  const decayPath = () => {
    const start = Math.min(24, 5 + predelay() / 10);
    const end = Math.min(98, start + 25 + decay() * 13);
    return `M${start} 12 C${start + 8} 20 ${Math.min(end, start + 25)} 52 ${end} 76 L${start} 76 Z`;
  };

  return (
    <>
      <div class="wv-fx__display">
        <div class="wv-fx__room" classList={{ 'is-off': props.muted }} title="Early reflections bounce between the virtual walls; longer decay leaves a brighter tail.">
          <div class="wv-fx__vizhead"><span>VIRTUAL ROOM</span><strong>EARLY REFLECTIONS</strong></div>
          <svg class="wv-fx__roomsvg" viewBox="0 0 100 58" preserveAspectRatio="none" aria-hidden="true">
            <rect class="wv-fx__roomwall" x={wall()} y="6" width={100 - wall() * 2} height="46" rx="2" />
            <path class="wv-fx__ray wv-fx__ray--dry" d="M18 37 L34 28 L48 34 L62 23 L82 29" />
            <path class="wv-fx__ray" style={{ opacity: tailOpacity() }} d="M20 37 L32 8 L54 50 L74 9 L86 28" />
            <path class="wv-fx__ray wv-fx__ray--late" style={{ opacity: tailOpacity() * 0.75 }} d="M22 38 L43 51 L66 12 L83 47" />
            <circle class="wv-fx__source" cx="18" cy="37" r="2.4" />
            <circle class="wv-fx__listener" cx="84" cy="29" r="2.7" />
            {[0, 1, 2, 3, 4].map((particle) => <circle class="wv-fx__particle" cx={20 + particle * 13} cy={37 - (particle % 2) * 16} r="1" style={{ 'animation-delay': `${particle * -0.42}s`, opacity: tailOpacity() }} />)}
          </svg>
        </div>
        <div class="wv-fx__decay" classList={{ 'is-off': props.muted }} role="img" aria-label="Reverb energy decay over time">
          <div class="wv-fx__vizhead"><span>ENERGY DECAY</span><strong>RT60 {decay().toFixed(1)} s</strong></div>
          <svg viewBox="0 0 100 82" preserveAspectRatio="none" aria-hidden="true">
            <path class="wv-fx__vgrid" d="M0 20H100 M0 48H100 M25 0V82 M50 0V82 M75 0V82" />
            <Show when={props.spectrum && props.spectrum.length > 1}>
              <path class="wv-eqc__spec" d={analyserPath(props.spectrum ?? [], 100, 82)} />
              <path class="wv-eqc__specline" d={analyserLine(props.spectrum ?? [], 100, 82)} />
            </Show>
            <path class="wv-fx__decayarea" d={decayPath()} />
            <path class="wv-fx__predelay" d={`M${Math.min(24, 5 + predelay() / 10)} 5V78`} />
          </svg>
          <div class="wv-fx__vizlegend"><span>direct</span><span>early reflections</span><span>late tail</span></div>
        </div>
        <p class="wv-fx__topo" classList={{ 'is-off': props.muted }}>
          {size().toFixed(0)}% room · {decay().toFixed(1)} s decay · {mix().toFixed(0)}% wet
        </p>
      </div>
      <PrimaryRow params={props.primary} knobs={props.knobs} oversized="mix" />
      <Advanced params={props.advanced} knobs={props.knobs} />
    </>
  );
}
