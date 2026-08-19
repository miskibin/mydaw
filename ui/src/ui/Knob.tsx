/**
 * One knob, laid out from a `ParamDef`.
 *
 * The engine sends a range, a curve, a step and a smoothing time rather than a
 * picture, so this is the *only* place that decides what a parameter looks like
 * and how it responds to a drag. Anything with a range gets one -- an effect
 * parameter, a send level, a pan -- by handing it a `ParamDefDto`-shaped spec,
 * which is why there is no second, subtly different knob for the mixer.
 *
 * Five behaviours, all of them expected of a DAW knob and none of them
 * negotiable:
 *
 * - **Vertical drag** moves the *normalised* position, so a logarithmic
 *   parameter moves by a constant musical interval per pixel.
 * - **The wheel** moves that same normalised position by a fixed fraction per
 *   notch, so it is curve-aware for free. A caller with a natural unit of its
 *   own says so with `wheelStep`.
 * - **Shift** is fine control, live: pressing or releasing it mid-drag changes
 *   the rate from that pixel on, and it is the same fifth of a step on the
 *   wheel.
 * - **Double-click** returns the parameter to its default.
 * - **Every change says what it did** in the status bar's readout, formatted in
 *   the parameter's own unit.
 *
 * A parameter the engine declared as a switch (`smoothingMs: 0` with a step)
 * is drawn as a switch, cycled by clicking and stepped by the wheel. It is
 * never animated: there is no value halfway between "peak" and "RMS".
 */

import { onCleanup, onMount, Show } from 'solid-js';

import type { ParamDefDto } from '../api/types';
import {
  formatParam,
  fromNormalised,
  isSwitch,
  knobAngle,
  snapToStep,
  switchPositions,
  toNormalised,
} from '../lib/param';
import { useAppState } from '../state';
import {
  bindWheel,
  debounce,
  FINE_FACTOR,
  KNOB_TRAVEL_PX,
  startDrag,
  WHEEL_COMMIT_MS,
} from './drag';

/**
 * How much of a parameter's range one notch of the wheel covers.
 *
 * A fiftieth, of the *normalised* position: a `log` frequency therefore moves
 * by a constant musical interval per notch and a linear one by a constant
 * amount, which is the same promise the drag makes. Fifty notches end to end is
 * a couple of comfortable flicks -- fewer and a filter sweep is a wrist
 * exercise, more and the wheel overshoots everything.
 */
const WHEEL_NORMALISED_STEP = 0.02;

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
  /**
   * Where one wheel notch lands, for a control whose own unit is not the
   * parameter's.
   *
   * `notches` arrives signed and already scaled by shift, so a caller only
   * states the size of a whole notch. Omitted -- which is every insert
   * parameter, because only the engine knows what its unit means -- the wheel
   * moves the normalised position by `WHEEL_NORMALISED_STEP` instead. The
   * result is clamped and snapped to the parameter's step grid either way.
   */
  wheelStep?: (value: number, notches: number) => number;
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

  /**
   * The wheel's commit, deferred.
   *
   * A drag commits on the release it obviously has; a wheel has no release, so
   * the end of the gesture is a pause. Callers batch a whole-mixer refetch onto
   * `onCommit`, and one of those per notch is a round trip between every click
   * of the wheel -- the value itself has already moved by then.
   */
  const commitSoon = debounce(WHEEL_COMMIT_MS, () => props.onCommit?.(props.value()));
  onCleanup(() => commitSoon.cancel());

  /** The position a switch is on, `0`-based. */
  const switchIndex = (): number => {
    const step = props.def.step && props.def.step > 0 ? props.def.step : 1;
    return Math.round((props.value() - props.def.min) / step);
  };

  const switchValue = (index: number): number =>
    props.def.min + index * (props.def.step && props.def.step > 0 ? props.def.step : 1);

  function onPointerDown(event: PointerEvent): void {
    if (props.disabled || event.button !== 0) return;
    if (isSwitch(props.def)) {
      // A switch has no travel: clicking advances it, wrapping at the end.
      const next = switchValue((switchIndex() + 1) % switchPositions(props.def));
      event.preventDefault();
      event.stopPropagation();
      apply(next);
      props.onCommit?.(next);
      return;
    }
    // A pointer gesture is the authority on where the value ends up, so it
    // cancels any notch still waiting to commit rather than letting that timer
    // fire mid-drag and pull the caller's local value out from under it.
    commitSoon.cancel();
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
    commitSoon.cancel();
    apply(props.def.defaultValue);
    props.onCommit?.(props.def.defaultValue);
  }

  function onWheel(notches: number, event: WheelEvent): void {
    if (props.disabled) return;
    const current = props.value();

    if (isSwitch(props.def)) {
      // A switch steps to the neighbouring position and stops at the ends. It
      // does not wrap the way the click does: a click has a "next" and nothing
      // else, a wheel has a direction, and wrapping a direction means rolling
      // gently downwards lands on the top position.
      const index = Math.min(
        switchPositions(props.def) - 1,
        Math.max(0, switchIndex() + (notches > 0 ? 1 : -1)),
      );
      const next = switchValue(index);
      if (next === current) return;
      apply(next);
      commitSoon();
      return;
    }

    const scaled = event.shiftKey ? notches * FINE_FACTOR : notches;
    const next = props.wheelStep
      ? snapToStep(props.def, props.wheelStep(current, scaled))
      : fromNormalised(props.def, toNormalised(props.def, current) + scaled * WHEEL_NORMALISED_STEP);
    if (next === current) return;
    apply(next);
    commitSoon();
  }

  // Bound on the wrapper rather than on the cap: a knob is 22-30 px across and
  // the value under it is part of the same control to everyone but the DOM.
  // Both shapes -- knob and switch -- are inside it, so there is one binding.
  let root: HTMLDivElement | undefined;
  let unbindWheel: (() => void) | undefined;
  onMount(() => {
    if (root) unbindWheel = bindWheel(root, onWheel);
  });
  onCleanup(() => unbindWheel?.());

  return (
    <div
      ref={root}
      class="wv-param"
      classList={{ 'wv-disabled': props.disabled }}
      title={
        props.title ??
        `${props.def.name} — drag or scroll, shift for fine, double-click to reset`
      }
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
