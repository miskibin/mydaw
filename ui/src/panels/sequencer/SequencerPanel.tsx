import { createSignal, Index, onCleanup, onMount, Show, type JSX } from 'solid-js';
import { useAppState } from '../../state';
import {
  currentStep,
  FALLBACK_SAMPLE_RATE,
  hitCount,
  isAccent,
  STEPS_PER_BEAT,
} from '../../lib/pattern';
import type { PanelProps } from '../types';

/**
 * The step sequencer: an FL-style grid of rows against sixteenth notes.
 *
 * # Why DOM buttons and not a canvas
 * The meters are a canvas because they redraw at display rate from a 30-60 Hz
 * stream. This is 64 cells that change when a human clicks them. A canvas here
 * would cost keyboard focus, screen-reader labels, hover states and the
 * browser's own hit-testing, and buy nothing.
 *
 * `<Index>` rather than `<For>`, for both the rows and the steps: an edit
 * produces a *new* row and step object (the pattern is immutable, so the push to
 * the backend can be a plain send), and `<For>` keys by reference -- it would
 * tear down and rebuild sixteen buttons on every click. `<Index>` keys by
 * position and hands each cell an accessor, so a click updates one cell's
 * attributes and touches nothing else.
 *
 * # The moving highlight
 * The current step is the one thing here that moves on its own, so it is the one
 * thing driven by an animation frame rather than by a signal from the backend.
 * The position comes from `Playhead`, which interpolates locally between meter
 * frames; the step number is then the same `floor(position / step_length)`
 * modulo the pattern length that the Rust scheduler computes. Same arithmetic,
 * same answer -- which is why the highlight sits on the step you can hear rather
 * than near it.
 *
 * Only the derived *step index* becomes a signal, so the grid re-renders eight
 * times a second at 130 BPM, not sixty.
 */
export function SequencerPanel(_props: PanelProps): JSX.Element {
  const app = useAppState();
  const [step, setStep] = createSignal<number | null>(null);

  onMount(() => {
    let raf = 0;
    const tick = () => {
      const pattern = app.pattern();
      const rate = app.playhead.getSampleRate() || FALLBACK_SAMPLE_RATE;
      setStep(
        pattern && app.playing()
          ? currentStep(app.playhead.positionSamples(), rate, app.bpm(), pattern.steps)
          : null,
      );
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    onCleanup(() => cancelAnimationFrame(raf));
  });

  const hits = () => {
    const pattern = app.pattern();
    return pattern ? hitCount(pattern) : 0;
  };

  return (
    <div class="sequencer">
      <Show
        when={app.pattern()}
        fallback={<p class="sequencer__empty">Waiting for the pattern…</p>}
      >
        {(pattern) => (
          <>
            <div class="sequencer__beats" aria-hidden="true">
              <span class="sequencer__row-label" />
              <Index each={Array.from({ length: pattern().steps })}>
                {(_, index) => (
                  <span
                    class="sequencer__beat"
                    classList={{
                      'is-downbeat': index % STEPS_PER_BEAT === 0,
                      'is-group-start': index % STEPS_PER_BEAT === 0 && index > 0,
                    }}
                  >
                    {index % STEPS_PER_BEAT === 0 ? index / STEPS_PER_BEAT + 1 : ''}
                  </span>
                )}
              </Index>
            </div>

            <Index each={pattern().rows}>
              {(row, rowIndex) => (
                <div class="sequencer__row">
                  <span class="sequencer__row-label" title={`MIDI key ${row().key}`}>
                    {row().label}
                  </span>
                  <Index each={row().steps}>
                    {(cell, stepIndex) => (
                      <button
                        type="button"
                        class="sequencer__step"
                        classList={{
                          'is-on': cell().on,
                          'is-accent': isAccent(cell()),
                          'is-downbeat': stepIndex % STEPS_PER_BEAT === 0,
                          'is-group-start':
                            stepIndex % STEPS_PER_BEAT === 0 && stepIndex > 0,
                          'is-playing': step() === stepIndex,
                        }}
                        aria-pressed={cell().on}
                        aria-label={`${row().label}, step ${stepIndex + 1}`}
                        title={
                          cell().on
                            ? `${row().label} step ${stepIndex + 1} — velocity ${cell().velocity.toFixed(2)}`
                            : `${row().label} step ${stepIndex + 1} — click to place, shift-click to accent`
                        }
                        onClick={(event) => app.toggleStep(rowIndex, stepIndex, event.shiftKey)}
                      />
                    )}
                  </Index>
                </div>
              )}
            </Index>

            <div class="sequencer__footer">
              <span class="sequencer__hint">Click to place · Shift-click to accent</span>
              <span class="sequencer__count">
                {hits()} {hits() === 1 ? 'step' : 'steps'}
              </span>
              <button
                type="button"
                class="sequencer__clear"
                disabled={hits() === 0}
                title="Switch every step off"
                onClick={() => app.clearPattern()}
              >
                Clear
              </button>
            </div>
          </>
        )}
      </Show>
    </div>
  );
}
