import type { JSX } from 'solid-js';
import { onFrame } from './frameLoop';
import { peakFraction } from '../panels/mixer/scale';

/**
 * One vertical peak meter.
 *
 * Paints itself from the shared animation frame by writing `height` straight
 * onto the fill element -- no signal, no re-render. `read` returns the current
 * linear amplitude; the -60..+6 dBFS mapping is the mixer's own `peakFraction`,
 * so a strip meter and any other meter in the app agree about how loud
 * something looks.
 *
 * A short decay is applied on the way down. Without it a 30 Hz frame stream on a
 * percussive source is a meter that strobes rather than moves; with it the rise
 * is still instant, which is the half that has to be honest.
 */
export function Meter(props: { read: () => number; class?: string }): JSX.Element {
  let fill: HTMLSpanElement | undefined;
  let shown = 0;

  onFrame(() => {
    if (!fill) return;
    const target = peakFraction(props.read());
    // Instant attack, ~12 dB/s-ish release.
    shown = target >= shown ? target : shown + (target - shown) * 0.18;
    if (shown < 0.001) shown = 0;
    fill.style.height = `${(shown * 100).toFixed(1)}%`;
  });

  return (
    <div class={`wv-meter ${props.class ?? ''}`}>
      <span class="wv-meter__fill" ref={fill} />
    </div>
  );
}
