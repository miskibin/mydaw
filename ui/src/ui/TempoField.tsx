import { createSignal, onCleanup, Show, type JSX } from 'solid-js';

import {
  MAX_BPM,
  MIN_BPM,
  quantizeBpm,
  tempoFromCents,
  tempoFromWhole,
  tempoParts,
} from '../lib/tempo';
import { bindWheel, debounce, startDrag, WHEEL_COMMIT_MS } from './drag';

/**
 * Tempo as two sliders: whole BPM, and hundredths of a BPM.
 *
 * Dragging or scrolling the integer never rewrites `.47`. Dragging the
 * hundredths never jumps a whole beat. Double-click either side to type the
 * full tempo.
 */
export interface TempoFieldProps {
  value: () => number;
  onInput: (value: number) => void;
  onCommit?: (value: number) => void;
}

export function TempoField(props: TempoFieldProps): JSX.Element {
  const [editing, setEditing] = createSignal(false);
  let input: HTMLInputElement | undefined;
  let unbindWhole: (() => void) | undefined;
  let unbindCents: (() => void) | undefined;

  const parts = () => tempoParts(props.value());
  const display = () => quantizeBpm(props.value()).toFixed(2);

  const commitSoon = debounce(WHEEL_COMMIT_MS, () => props.onCommit?.(props.value()));

  function setBpm(next: number, commit: boolean): void {
    const value = quantizeBpm(next);
    if (value === quantizeBpm(props.value())) return;
    props.onInput(value);
    if (commit) props.onCommit?.(value);
  }

  function onWholeWheel(notches: number): void {
    setBpm(tempoFromWhole(props.value(), parts().whole + notches), false);
    commitSoon();
  }

  function onCentsWheel(notches: number): void {
    setBpm(tempoFromCents(props.value(), parts().cents + notches), false);
    commitSoon();
  }

  const bindWhole = (el: HTMLButtonElement): void => {
    unbindWhole?.();
    unbindWhole = bindWheel(el, onWholeWheel);
  };
  const bindCents = (el: HTMLButtonElement): void => {
    unbindCents?.();
    unbindCents = bindWheel(el, onCentsWheel);
  };
  onCleanup(() => {
    unbindWhole?.();
    unbindCents?.();
    commitSoon.cancel();
  });

  function beginEdit(): void {
    setEditing(true);
    queueMicrotask(() => {
      input?.focus();
      input?.select();
    });
  }

  function commitTyped(text: string): void {
    setEditing(false);
    const parsed = Number.parseFloat(text);
    if (!Number.isFinite(parsed)) return;
    setBpm(parsed, true);
  }

  function dragWhole(event: PointerEvent): void {
    commitSoon.cancel();
    startDrag(event, {
      from: parts().whole,
      perPixel: 1,
      min: MIN_BPM,
      max: MAX_BPM,
      axis: 'vertical',
      onChange: (whole) => setBpm(tempoFromWhole(props.value(), Math.round(whole)), false),
      onCommit: () => props.onCommit?.(props.value()),
    });
  }

  function dragCents(event: PointerEvent): void {
    commitSoon.cancel();
    startDrag(event, {
      from: parts().cents,
      perPixel: 0.5,
      min: 0,
      max: 99,
      axis: 'vertical',
      onChange: (cents) => setBpm(tempoFromCents(props.value(), Math.round(cents)), false),
      onCommit: () => props.onCommit?.(props.value()),
    });
  }

  return (
    <Show
      when={editing()}
      fallback={
        <div class="wv-tempo">
          <button
            ref={bindWhole}
            type="button"
            class="wv-tempo__slide"
            title="Whole BPM — drag or scroll by 1. Double-click to type."
            aria-label="Tempo, whole beats per minute"
            onPointerDown={dragWhole}
            onDblClick={beginEdit}
          >
            <span class="wv-tempo__value">{parts().whole}</span>
            <span class="wv-tempo__bar" aria-hidden="true">
              <span
                class="wv-tempo__fill"
                style={{ width: `${((parts().whole - MIN_BPM) / (MAX_BPM - MIN_BPM)) * 100}%` }}
              />
            </span>
          </button>
          <button
            ref={bindCents}
            type="button"
            class="wv-tempo__slide wv-tempo__slide--fine"
            title="Hundredths of a BPM — drag or scroll by 0.01. Double-click to type."
            aria-label="Tempo, hundredths of a beat per minute"
            onPointerDown={dragCents}
            onDblClick={beginEdit}
          >
            <span class="wv-tempo__value">.{String(parts().cents).padStart(2, '0')}</span>
            <span class="wv-tempo__bar" aria-hidden="true">
              <span class="wv-tempo__fill" style={{ width: `${(parts().cents / 99) * 100}%` }} />
            </span>
          </button>
        </div>
      }
    >
      <input
        ref={input}
        class="wv-meta__input"
        type="text"
        inputmode="decimal"
        value={display()}
        aria-label="Tempo in beats per minute"
        onBlur={(event) => commitTyped(event.currentTarget.value)}
        onKeyDown={(event) => {
          if (event.key === 'Enter') {
            event.preventDefault();
            commitTyped(event.currentTarget.value);
          } else if (event.key === 'Escape') {
            event.preventDefault();
            setEditing(false);
          }
          event.stopPropagation();
        }}
      />
    </Show>
  );
}
