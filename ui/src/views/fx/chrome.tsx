/**
 * Shared bits of a display-first effect editor: the primary knob row and the
 * Advanced drawer that holds everything the display does not.
 */

import { For, Show, type JSX } from 'solid-js';

import type { ParamDefDto } from '../../api/types';
import type { InsertKnobs } from '../../ui/insertKnob';

export function PrimaryRow(props: {
  params: ParamDefDto[];
  knobs: InsertKnobs;
  /** Drive-sized knob for the saturator. */
  oversized?: string;
}): JSX.Element {
  return (
    <Show when={props.params.length > 0}>
      <div class="wv-fx__primary">
        <For each={props.params}>
          {(def) => (
            <props.knobs.Param
              def={def}
              size={def.id === props.oversized ? 72 : 48}
              label={def.name}
            />
          )}
        </For>
      </div>
    </Show>
  );
}

export function Advanced(props: { params: ParamDefDto[]; knobs: InsertKnobs }): JSX.Element {
  return (
    <Show when={props.params.length > 0}>
      <details class="wv-fx__adv">
        <summary>Advanced</summary>
        <div class="wv-fx__grid">
          <For each={props.params}>{(def) => <props.knobs.Param def={def} size={36} />}</For>
        </div>
      </details>
    </Show>
  );
}
