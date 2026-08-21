/**
 * Compact MIDI keyboard for the sample editor. Pointer-captured, gated notes.
 */

import { For, createEffect, createMemo, createSignal, onCleanup, onMount, type JSX } from 'solid-js';

import { isBlackKey, noteName } from '../lib/pattern';
import { createSampleKeyboardController } from '../lib/sampleKeyboard';
import { visibleKeyboardRange } from '../lib/samplePitch';

export function SampleKeyboard(props: {
  trackToken: string;
  center: number;
  heldNotes: number[];
  rootKey: number;
  disabled?: boolean;
  onCenter(next: number): void;
  noteOn(key: number, velocity: number): void;
  noteOff(key: number): void;
}): JSX.Element {
  const keys = createSampleKeyboardController({
    noteOn: (key, velocity) => props.noteOn(key, velocity),
    noteOff: (key) => props.noteOff(key),
  });

  createEffect(() => {
    keys.setTrack(props.trackToken);
  });

  onMount(() => {
    const blur = () => keys.cleanup();
    window.addEventListener('blur', blur);
    onCleanup(() => {
      window.removeEventListener('blur', blur);
      keys.cleanup();
    });
  });

  const range = createMemo(() => visibleKeyboardRange(props.center, 2));
  const whites = createMemo(() => {
    const [start, end] = range();
    const list: number[] = [];
    for (let key = start; key < end; key++) {
      if (!isBlackKey(key)) list.push(key);
    }
    return list;
  });
  const blacks = createMemo(() => {
    const white = whites();
    const list: { key: number; left: number }[] = [];
    white.forEach((key, index) => {
      const sharp = key + 1;
      if (isBlackKey(sharp) && sharp < range()[1] && index < white.length - 1) {
        list.push({ key: sharp, left: ((index + 1) / white.length) * 100 });
      }
    });
    return list;
  });

  const [localHeld, setLocalHeld] = createSignal<number | null>(null);

  const lit = (key: number): boolean =>
    localHeld() === key || props.heldNotes.includes(key);

  function syncHeld(): void {
    setLocalHeld(keys.heldKey());
  }

  function pointerDown(event: PointerEvent, key: number): void {
    if (props.disabled || event.button !== 0) return;
    event.preventDefault();
    const row = (event.currentTarget as HTMLElement).closest('.wv-sample__kb-keys');
    (row as HTMLElement | null)?.setPointerCapture(event.pointerId);
    const box = (event.currentTarget as HTMLElement).getBoundingClientRect();
    const velocity = Math.min(1, Math.max(0.15, 1 - (event.clientY - box.top) / Math.max(1, box.height)));
    keys.down(key, event.pointerId, velocity);
    syncHeld();
  }

  function keyFromPoint(clientX: number, clientY: number): number | null {
    const hit = document.elementFromPoint(clientX, clientY);
    const btn = hit?.closest('[data-midi-key]') as HTMLElement | null;
    if (!btn) return null;
    const key = Number(btn.dataset.midiKey);
    return Number.isFinite(key) ? key : null;
  }

  function pointerMove(event: PointerEvent): void {
    if (props.disabled || !event.buttons) return;
    const key = keyFromPoint(event.clientX, event.clientY);
    if (key === null) return;
    keys.move(key, event.pointerId);
    syncHeld();
  }

  function pointerUp(event: PointerEvent): void {
    keys.up(event.pointerId);
    syncHeld();
  }

  function pointerCancel(event: PointerEvent): void {
    keys.cancel(event.pointerId);
    syncHeld();
  }

  function lostCapture(event: PointerEvent): void {
    keys.lostCapture(event.pointerId);
    syncHeld();
  }

  return (
    <div class="wv-sample__kb" classList={{ 'is-off': props.disabled }}>
      <button
        type="button"
        class="wv-sample__kb-oct"
        title="Lower octave"
        disabled={props.disabled || range()[0] <= 0}
        onClick={() => props.onCenter(Math.max(0, props.center - 12))}
      >
        –
      </button>
      <div
        class="wv-sample__kb-keys"
        role="group"
        aria-label="Sample pitch keyboard"
        onPointerMove={pointerMove}
        onPointerUp={pointerUp}
        onPointerCancel={pointerCancel}
        onLostPointerCapture={lostCapture}
      >
        <div class="wv-sample__kb-whites">
          <For each={whites()}>
            {(key) => (
              <button
                type="button"
                class="wv-sample__kb-w"
                classList={{
                  'is-on': lit(key),
                  'is-root': key === props.rootKey,
                }}
                data-midi-key={key}
                aria-label={noteName(key)}
                disabled={props.disabled}
                onPointerDown={(event) => pointerDown(event, key)}
              />
            )}
          </For>
        </div>
        <div class="wv-sample__kb-blacks">
          <For each={blacks()}>
            {(entry) => (
              <button
                type="button"
                class="wv-sample__kb-b"
                classList={{
                  'is-on': lit(entry.key),
                  'is-root': entry.key === props.rootKey,
                }}
                data-midi-key={entry.key}
                style={{ left: `${entry.left}%` }}
                aria-label={noteName(entry.key)}
                disabled={props.disabled}
                onPointerDown={(event) => pointerDown(event, entry.key)}
              />
            )}
          </For>
        </div>
      </div>
      <button
        type="button"
        class="wv-sample__kb-oct"
        title="Higher octave"
        disabled={props.disabled || range()[1] >= 128}
        onClick={() => props.onCenter(Math.min(127, props.center + 12))}
      >
        +
      </button>
    </div>
  );
}
