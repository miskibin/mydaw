import { createSignal, Index, Show, type JSX } from 'solid-js';
import { useAppState } from '../../state';
import { onFrame } from '../../ui/frameLoop';
import { currentStep, FALLBACK_SAMPLE_RATE, isAccent } from '../../lib/pattern';

/**
 * The step grid: one lane per drum row, one cell per sixteenth.
 *
 * Click toggles, shift-click accents -- the rules are `toggleStep` in
 * `lib/pattern.ts` and they are not repeated here. Edits go through
 * `app.toggleStep`, which updates the local grid immediately and pushes the
 * whole pattern to Rust once the clicking stops, so dragging a row of hi-hats is
 * one message rather than sixteen.
 *
 * The moving highlight is computed from the interpolated transport position
 * with the same arithmetic the Rust scheduler uses, so the lit cell is the cell
 * being heard. It is read on the animation frame but only *published* when the
 * step number actually changes -- sixteen times a bar rather than sixty times a
 * second.
 */
export function StepGrid(): JSX.Element {
  const app = useAppState();
  const [step, setStep] = createSignal<number | null>(null);

  onFrame(() => {
    const pattern = app.pattern();
    if (!pattern || !app.playing()) {
      if (step() !== null) setStep(null);
      return;
    }
    const rate = app.playhead.getSampleRate() || FALLBACK_SAMPLE_RATE;
    const next = currentStep(app.playhead.positionSamples(), rate, app.bpm(), pattern.steps);
    if (next !== step()) setStep(next);
  });

  return (
    <div class="wv-steps">
      <Show
        when={app.pattern()}
        fallback={<span class="wv-note">Waiting for the pattern…</span>}
      >
        {/*
          `Index` on both axes. An edit replaces the touched row and cell with
          new objects, and a keyed `For` would rebuild all sixty-four buttons for
          one click -- losing the pressed element out from under the pointer
          mid-drag. A cell's identity here really is its position.
        */}
        {(pattern) => (
          <Index each={pattern().rows}>
            {(row, rowIndex) => (
              <div class="wv-steplane">
                <span class="wv-steplane__name">{row().label}</span>
                <div class="wv-steplane__steps">
                  <Index each={row().steps}>
                    {(cell, stepIndex) => (
                      <button
                        type="button"
                        class="wv-step"
                        classList={{
                          'is-downbeat': stepIndex % 4 === 0,
                          'is-on': cell().on && !isAccent(cell()),
                          'is-accent': isAccent(cell()),
                          'is-current': step() === stepIndex,
                        }}
                        aria-label={`${row().label} step ${stepIndex + 1}`}
                        aria-pressed={cell().on}
                        title="Click to toggle · shift-click to accent"
                        onClick={(event) => app.toggleStep(rowIndex, stepIndex, event.shiftKey)}
                      />
                    )}
                  </Index>
                </div>
              </div>
            )}
          </Index>
        )}
      </Show>
    </div>
  );
}
