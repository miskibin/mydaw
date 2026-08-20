/**
 * A horizontal slider laid out from a `ParamDef`.
 *
 * Knobs are the right control when the parameter lives in a dense grid of
 * four. A sample window has room for a track, and a track is what people reach
 * for when they say "the pitch slider" — click to jump, drag to ride, wheel to
 * nudge, shift for fine, double-click to reset. The value is always the
 * parameter's own unit; normalised position is only a picture of it.
 *
 * Bipolar parameters (pan, pitch) fill from the centre. Unipolar ones (volume)
 * fill from the left. Both read the same `ParamDef`, so a second, slightly
 * different slider does not appear the next time a range wants a track.
 */

import { onCleanup, onMount, Show } from 'solid-js';

import type { ParamDefDto } from '../api/types';
import { formatParam, fromNormalised, toNormalised } from '../lib/param';
import { useAppState } from '../state';
import { bindWheel, debounce, FINE_FACTOR, WHEEL_COMMIT_MS, WHEEL_NORMALISED_STEP } from './drag';

export interface SliderProps {
  def: ParamDefDto;
  value: () => number;
  onInput: (value: number) => void;
  onCommit?: (value: number) => void;
  owner: string;
  color?: string;
  title?: string;
  /** Overrides `formatParam` for the readout. */
  format?: (value: number) => string;
  /** Overrides the def's name on the label. */
  label?: string;
  disabled?: boolean;
}

export function Slider(props: SliderProps): ReturnType<typeof Show> {
  const app = useAppState();
  const reading = () =>
    props.format ? props.format(props.value()) : formatParam('', props.def, props.value());
  const t = () => toNormalised(props.def, props.value());
  const bipolar = () =>
    Math.abs(props.def.defaultValue - (props.def.min + props.def.max) / 2) < 1e-9;

  function announce(value: number): void {
    const text = props.format ? props.format(value) : formatParam('', props.def, value);
    app.say(`${props.owner} · ${props.label ?? props.def.name}`, text);
  }

  function apply(value: number): void {
    props.onInput(value);
    announce(value);
  }

  const commitSoon = debounce(WHEEL_COMMIT_MS, () => props.onCommit?.(props.value()));
  onCleanup(() => commitSoon.cancel());

  function valueAt(clientX: number, track: HTMLElement): number {
    const rect = track.getBoundingClientRect();
    const x = rect.width > 0 ? (clientX - rect.left) / rect.width : 0;
    return fromNormalised(props.def, x);
  }

  function onPointerDown(event: PointerEvent): void {
    if (props.disabled || event.button !== 0) return;
    const track = event.currentTarget as HTMLElement;
    commitSoon.cancel();
    event.preventDefault();
    event.stopPropagation();
    apply(valueAt(event.clientX, track));
    document.body.style.setProperty('cursor', 'ew-resize');
    document.body.style.setProperty('user-select', 'none');
    const onMove = (move: PointerEvent) => apply(valueAt(move.clientX, track));
    const onUp = () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      window.removeEventListener('pointercancel', onUp);
      document.body.style.removeProperty('cursor');
      document.body.style.removeProperty('user-select');
      props.onCommit?.(props.value());
    };
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
    window.addEventListener('pointercancel', onUp);
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
    const scaled = event.shiftKey ? notches * FINE_FACTOR : notches;
    const next = fromNormalised(
      props.def,
      toNormalised(props.def, current) + scaled * WHEEL_NORMALISED_STEP,
    );
    if (next === current) return;
    apply(next);
    commitSoon();
  }

  let root: HTMLDivElement | undefined;
  let unbindWheel: (() => void) | undefined;
  onMount(() => {
    if (root) unbindWheel = bindWheel(root, onWheel);
  });
  onCleanup(() => unbindWheel?.());

  const fillStyle = () => {
    const pos = t();
    if (!bipolar()) {
      return { left: '0%', width: `${pos * 100}%` };
    }
    const mid = 0.5;
    const lo = Math.min(mid, pos);
    const hi = Math.max(mid, pos);
    return { left: `${lo * 100}%`, width: `${(hi - lo) * 100}%` };
  };

  return (
    <div
      ref={root}
      class="wv-slider"
      classList={{ 'wv-disabled': props.disabled, 'is-bipolar': bipolar() }}
      title={
        props.title ??
        `${props.def.name} — click or drag, scroll to nudge, shift for fine, double-click to reset`
      }
    >
      <span class="wv-slider__name">{props.label ?? props.def.name}</span>
      <div
        class="wv-slider__track"
        style={{ '--slider-acc': props.color ?? 'var(--acc)' } as Record<string, string>}
        onPointerDown={onPointerDown}
        onDblClick={onDoubleClick}
      >
        <span class="wv-slider__fill" style={fillStyle()} />
        <Show when={bipolar()}>
          <span class="wv-slider__center" />
        </Show>
        <span class="wv-slider__thumb" style={{ left: `${t() * 100}%` }} />
      </div>
      <span class="wv-slider__value">{reading()}</span>
    </div>
  );
}
