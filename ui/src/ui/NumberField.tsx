import { createSignal, Show, type JSX } from 'solid-js';
import { startDrag } from './drag';

/**
 * A numeric readout that is also a control: drag it to change the value,
 * double-click it to type one.
 *
 * Both gestures, because they answer different questions. Dragging is how you
 * find a tempo by ear -- the value moves while you listen, and there is no
 * moment where the field is half-typed and meaningless. Typing is how you set
 * one you already know, and it is the only way to reach 128 from 130 without a
 * steady hand. A field with only the drag is a field people fight; one with only
 * the box is one nobody nudges.
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
  /** How the value is rendered. Defaults to two decimal places. */
  format?: (value: number) => string;
  class?: string;
  title?: string;
  'aria-label'?: string;
}

export function NumberField(props: NumberFieldProps): JSX.Element {
  const [editing, setEditing] = createSignal(false);
  let input: HTMLInputElement | undefined;

  const format = () => (props.format ?? ((n: number) => n.toFixed(2)))(props.value());

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
          type="button"
          class={`${props.class ?? ''} wv-meta__value--drag`}
          title={props.title ?? 'Drag to change, double-click to type'}
          aria-label={props['aria-label']}
          onPointerDown={(event) =>
            startDrag(event, {
              from: props.value(),
              perPixel: props.step,
              min: props.min,
              max: props.max,
              axis: 'both',
              onChange: props.onInput,
              onCommit: props.onCommit,
            })
          }
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
