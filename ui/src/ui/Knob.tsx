/**
 * One knob, laid out from a `ParamDef`.
 *
 * The engine sends a range, a curve, a step and a smoothing time rather than a
 * picture, so this is the *only* place that decides what a parameter looks like
 * and how it responds to a drag. Anything with a range gets one -- an effect
 * parameter, a send level, a pan -- by handing it a `ParamDefDto`-shaped spec,
 * which is why there is no second, subtly different knob for the mixer.
 *
 * Four behaviours, all of them expected of a DAW knob and none of them
 * negotiable:
 *
 * - **Vertical drag** moves the *normalised* position, so a logarithmic
 *   parameter moves by a constant musical interval per pixel.
 * - **Shift** is fine control, live: pressing or releasing it mid-drag changes
 *   the rate from that pixel on.
 * - **Double-click** returns the parameter to its default.
 * - **Every change says what it did** in the status bar's readout, formatted in
 *   the parameter's own unit.
 *
 * A parameter the engine declared as a switch (`smoothingMs: 0` with a step)
 * is drawn as a switch and cycled by clicking. It is never animated: there is
 * no value halfway between "peak" and "RMS".
 */

import { Show } from 'solid-js';

import type { ParamDefDto } from '../api/types';
import {
  formatParam,
  fromNormalised,
  isSwitch,
  knobAngle,
  switchPositions,
  toNormalised,
} from '../lib/param';
import { useAppState } from '../state';
import { KNOB_TRAVEL_PX, startDrag } from './drag';

export interface KnobProps {
  /** The parameter's metadata. Synthesise one for anything not an insert. */
  def: ParamDefDto;
  /** Which effect it belongs to, so a switch's positions can be named. */
  typeId?: string;
  value: () => number;
  /** Called for every step of a drag, and once for a click or a reset. */
  onInput: (value: number) => void;
  /** Called once when the gesture ends, for callers that batch. */
  onCommit?: (value: number) => void;
  /** What owns it: `Reverb Bus · Compressor`. Prefixes the readout. */
  owner: string;
  /** Overrides the parameter's own name on the cap. */
  label?: string;
  size?: number;
  /** Ring colour. Defaults to the accent. */
  color?: string;
  /** Greys it out and refuses every gesture. `title` says why. */
  disabled?: boolean;
  title?: string;
}

export function Knob(props: KnobProps): ReturnType<typeof Show> {
  const app = useAppState();
  const size = () => props.size ?? 30;
  const typeId = () => props.typeId ?? '';
  const reading = () => formatParam(typeId(), props.def, props.value());

  function announce(value: number): void {
    app.say(`${props.owner} · ${props.label ?? props.def.name}`, formatParam(typeId(), props.def, value));
  }

  function apply(value: number): void {
    props.onInput(value);
    announce(value);
  }

  function onPointerDown(event: PointerEvent): void {
    if (props.disabled || event.button !== 0) return;
    if (isSwitch(props.def)) {
      // A switch has no travel: clicking advances it, wrapping at the end.
      const step = props.def.step && props.def.step > 0 ? props.def.step : 1;
      const index = Math.round((props.value() - props.def.min) / step);
      const next = props.def.min + ((index + 1) % switchPositions(props.def)) * step;
      event.preventDefault();
      event.stopPropagation();
      apply(next);
      props.onCommit?.(next);
      return;
    }
    startDrag(event, {
      from: toNormalised(props.def, props.value()),
      perPixel: 1 / KNOB_TRAVEL_PX,
      min: 0,
      max: 1,
      onChange: (t) => apply(fromNormalised(props.def, t)),
      onCommit: (t) => props.onCommit?.(fromNormalised(props.def, t)),
    });
  }

  function onDoubleClick(event: MouseEvent): void {
    if (props.disabled) return;
    event.preventDefault();
    event.stopPropagation();
    apply(props.def.defaultValue);
    props.onCommit?.(props.def.defaultValue);
  }

  return (
    <div
      class="wv-param"
      classList={{ 'wv-disabled': props.disabled }}
      title={props.title ?? `${props.def.name} — drag, shift for fine, double-click to reset`}
    >
      <Show
        when={!isSwitch(props.def)}
        fallback={
          <button
            type="button"
            class="wv-param__switch"
            classList={{ 'is-on': props.value() > props.def.min }}
            disabled={props.disabled}
            onPointerDown={onPointerDown}
          >
            {reading()}
          </button>
        }
      >
        <span
          class="wv-knob"
          style={{ width: `${size()}px`, height: `${size()}px` }}
          onPointerDown={onPointerDown}
          onDblClick={onDoubleClick}
        >
          <span
            class="wv-knob__pointer"
            style={{
              height: `${Math.round(size() * 0.34)}px`,
              background: props.color ?? 'var(--acc)',
              transform: `translateX(-50%) rotate(${knobAngle(toNormalised(props.def, props.value()))}deg)`,
            }}
          />
        </span>
      </Show>
      <span class="wv-param__value">{reading()}</span>
      <span class="wv-param__name">{props.label ?? props.def.name}</span>
    </div>
  );
}

/**
 * A `ParamDef` for something that is not an insert parameter.
 *
 * The mixer's pan, a send's level and a clip's trim all want the same knob with
 * the same drag law, and the alternative -- a second knob component with its own
 * slightly different rules -- is how two controls end up disagreeing about what
 * shift does.
 */
export function spec(
  id: string,
  name: string,
  unit: string,
  min: number,
  max: number,
  defaultValue: number,
  curve: ParamDefDto['curve'] = 'linear',
): ParamDefDto {
  return { id, name, unit, min, max, defaultValue, curve, step: null, smoothingMs: 10 };
}
