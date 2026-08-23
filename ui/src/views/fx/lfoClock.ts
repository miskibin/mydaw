/**
 * A phase that runs at the rate an effect's LFO is set to.
 *
 * **This is the shell's own clock, not the engine's.** The audio thread's
 * oscillator is not sampled here: the readout channel polls at 15 Hz, which is
 * a slideshow, and an animation that stuttered at a sixth of the frame rate
 * would be worse than an honest one that runs smoothly at the same *rate*. So a
 * moving display driven by this says what the effect is set to do -- which
 * notch is sweeping where, how fast, how far -- and is not a phase-locked scope
 * of the audio. Anything that cannot be derived from the settings (the
 * auto-filter's envelope, and the cutoff it lands on) comes from the engine's
 * readouts instead.
 *
 * The clock stops when the effect is bypassed, when the document is hidden, and
 * when the viewer has asked for reduced motion -- in which case the phase sits
 * at a quarter turn, the top of the sweep, so the picture still shows the
 * modulation's extent rather than a curve parked in the middle.
 */

import { createSignal, onCleanup, type Accessor } from 'solid-js';

/** Where a stopped clock rests: the top of the sine. */
export const RESTING_PHASE = 0.25;

export function createLfoPhase(options: {
  /** Cycles per second. Read every frame, so a knob drag is followed live. */
  rate: () => number;
  /** Whether the effect is doing anything at all. */
  running?: () => boolean;
}): Accessor<number> {
  const reduced =
    typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;
  const [phase, setPhase] = createSignal(reduced ? RESTING_PHASE : 0);
  if (reduced) return phase;

  let last = performance.now();
  let frame = requestAnimationFrame(function tick(now) {
    frame = requestAnimationFrame(tick);
    // Clamped: a tab that was in the background for a minute must not fast
    // forward the sweep, and a hidden document should not advance at all.
    const elapsed = Math.min(0.1, Math.max(0, (now - last) / 1000));
    last = now;
    if (document.hidden || options.running?.() === false) return;
    const rate = options.rate();
    if (!Number.isFinite(rate) || rate <= 0) return;
    setPhase((previous) => (previous + elapsed * rate) % 1);
  });
  onCleanup(() => cancelAnimationFrame(frame));
  return phase;
}
