import { For, type JSX } from 'solid-js';
import { IconCaret } from './icons';

/** A direct, keyboard-accessible choice instead of a hidden click cycle. */
export function SnapSelect(props: {
  value: number;
  choices: readonly (readonly [number, string])[];
  onChange(value: number): void;
  label: string;
}): JSX.Element {
  return (
    <label class="wv-chip wv-chip--sunk wv-snap">
      <span class="wv-chip__tag" aria-hidden="true">
        SNAP
      </span>
      <select
        value={props.value}
        aria-label={props.label}
        onChange={(event) => props.onChange(Number(event.currentTarget.value))}
        onKeyDown={(event) => event.stopPropagation()}
      >
        <For each={props.choices}>{([ticks, label]) => <option value={ticks}>{label}</option>}</For>
      </select>
      <IconCaret />
    </label>
  );
}
