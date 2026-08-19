/**
 * A row of mutually exclusive positions. Used for delay divisions, saturator
 * type, and anything else the engine declared as a switch that a knob would
 * hide.
 */

import { For } from 'solid-js';

import type { ParamDefDto } from '../api/types';
import { switchLabel, switchPositions } from '../lib/param';

export function Segmented(props: {
  def: ParamDefDto;
  typeId: string;
  value: number;
  onChange: (value: number) => void;
  /** Override the engine-derived labels when the row wants shorter ones. */
  labels?: readonly string[];
}): ReturnType<typeof For> {
  const count = () => switchPositions(props.def);
  const step = () => (props.def.step && props.def.step > 0 ? props.def.step : 1);
  const index = () => Math.round((props.value - props.def.min) / step());

  return (
    <div class="wv-seg" role="radiogroup" aria-label={props.def.name}>
      <For each={Array.from({ length: count() }, (_, i) => i)}>
        {(i) => (
          <button
            type="button"
            class="wv-seg__btn"
            classList={{ 'is-on': i === index() }}
            role="radio"
            aria-checked={i === index()}
            onClick={() => props.onChange(props.def.min + i * step())}
          >
            {props.labels?.[i] ?? switchLabel(props.typeId, props.def, props.def.min + i * step())}
          </button>
        )}
      </For>
    </div>
  );
}
