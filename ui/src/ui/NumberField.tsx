import { createSignal, onCleanup, Show, type JSX } from 'solid-js';
import { bindWheel, debounce, FINE_FACTOR, startDrag, WHEEL_COMMIT_MS } from './drag';

/**
 * A numeric readout that is also a control: drag it to change the value,
 * scroll it to nudge it, double-click it to type one.
 *
 * Three gestures, because they answer different questions. Dragging is how you
 * find a tempo by ear -- the value moves while you listen, and there is no
 * moment where the field is half-typed and meaningless. Typing is how you set
 * one you already know, and it is the only way to reach 128 from 130 without a
 * steady hand. Scrolling is how you move a value you can already see by a known
 * amount, which is the gesture a tempo actually wants most of the time. A field
 * with only the drag is a field people fight; one with only the box is one
 * nobody nudges.
 *
 * The value is not held here. `value` is read from the caller's signal and
 * `onInput` is called for every step of a drag (so the change is audible while
 * dragging), with `onCommit` once at the end. The caller decides how much of
 * that reaches the backend.
 */
export interface NumberFieldProps {
  value: () => number;
  /** Called continuously while dragging, and once when a typed value is accepted. */
  onInput: (value: number) => void;
  /** Called once when the gesture ends. Defaults to `onInput`'s effect being enough. */
  onCommit?: (value: number) => void;
  min: number;
  max: number;
  /** Value change per pixel of drag. */
  step: number;
  /** Value change per notch of the wheel. Defaults to `step`. */
  wheelStep?: number;
  /** How the value is rendered. Defaults to two decimal places. */
  format?: (value: number) => string;
  class?: string;
  title?: string;
  'aria-label'?: string;
}

export function NumberField(props: NumberFieldProps): JSX.Element {
  const [editing, setEditing] = createSignal(false);
  let input: HTMLInputElement | undefined;
  let unbindWheel: (() => void) | undefined;

  const format = () => (props.format ?? ((n: number) => n.toFixed(2)))(props.value());

  // The value moves on every notch; only the commit waits, because a tempo
  // change is a whole engine command and a fast scroll is a lot of them.
  const commitSoon = debounce(WHEEL_COMMIT_MS, () => props.onCommit?.(props.value()));

  function onWheel(notches: number, event: WheelEvent): void {
    const step = props.wheelStep ?? props.step;
    const delta = notches * step * (event.shiftKey ? FINE_FACTOR : 1);
    const next = Math.min(props.max, Math.max(props.min, props.value() + delta));
    if (next === props.value()) return;
    props.onInput(next);
    commitSoon();
  }

  // Bound from the ref rather than from `onMount`, because the readout is torn
  // down and rebuilt every time the typing box opens and closes -- a listener
  // attached once at mount would be attached to an element long gone.
  const bindReadout = (el: HTMLButtonElement): void => {
    unbindWheel?.();
    unbindWheel = bindWheel(el, onWheel);
  };
  onCleanup(() => {
    unbindWheel?.();
    commitSoon.cancel();
  });

  const commitTyped = (text: string) => {
    setEditing(false);
    const parsed = Number.parseFloat(text);
    // A field left empty, or filled with something that is not a number, is a
    // cancelled edit rather than a reason to complain: the old value is still on
    // screen the instant the box goes away.
    if (!Number.isFinite(parsed)) return;
    const clamped = Math.min(props.max, Math.max(props.min, parsed));
    props.onInput(clamped);
    props.onCommit?.(clamped);
  };

  return (
    <Show
      when={editing()}
      fallback={
        <button
          ref={bindReadout}
          type="button"
          class={`${props.class ?? ''} wv-meta__value--drag`}
          title={props.title ?? 'Drag or scroll to change, double-click to type'}
          aria-label={props['aria-label']}
          onPointerDown={(event) => {
            commitSoon.cancel();
            startDrag(event, {
              from: props.value(),
              perPixel: props.step,
              min: props.min,
              max: props.max,
              axis: 'both',
              onChange: props.onInput,
              onCommit: props.onCommit,
            });
          }}
          onDblClick={() => {
            setEditing(true);
            // Focus after the input exists. `queueMicrotask` rather than a
            // timeout: Solid has rendered by then, and a timeout would let a
            // click land somewhere else first.
            queueMicrotask(() => {
              input?.focus();
              input?.select();
            });
          }}
        >
          {format()}
        </button>
      }
    >
      <input
        ref={input}
        class="wv-meta__input"
        type="text"
        inputmode="decimal"
        value={format()}
        aria-label={props['aria-label']}
        onBlur={(event) => commitTyped(event.currentTarget.value)}
        onKeyDown={(event) => {
          if (event.key === 'Enter') {
            event.preventDefault();
            commitTyped(event.currentTarget.value);
          } else if (event.key === 'Escape') {
            event.preventDefault();
            setEditing(false);
          }
          // Everything else stays in the box: the window-level piano listener
          // checks for an editable target, but stopping here is what makes that
          // guarantee local rather than remote.
          event.stopPropagation();
        }}
      />
    </Show>
  );
}
