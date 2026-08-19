import { onCleanup } from 'solid-js';

/**
 * One animation-frame loop for the whole shell.
 *
 * Everything that moves on its own -- meter fills, the playhead, the clock, the
 * step highlight -- is driven from here by writing to the DOM directly rather
 * than through a signal. A 30-60 Hz meter stream routed through Solid would
 * re-run every dependent computation at that rate; this is the same reason
 * `MeterBus` is a plain mutable holder and not a store, and it is the rule the
 * old shell already worked to.
 *
 * One loop rather than one per component because twenty `requestAnimationFrame`
 * callbacks are twenty scheduler entries doing the work of one, and because a
 * single pass keeps every meter in a frame showing the *same* frame.
 *
 * The loop does not run when nothing is subscribed, so an idle window is idle.
 */

type Painter = () => void;

const painters = new Set<Painter>();
let handle = 0;

/**
 * Painters that have already thrown.
 *
 * A broken painter must not stop the clock for the other nineteen, and it must
 * not log sixty times a second either -- but swallowing it in silence is worse
 * than both: a canvas that never draws then looks like a layout bug and costs
 * an hour. So the first throw is reported, once, and the rest are dropped.
 */
const broken = new WeakSet<Painter & object>();

function tick(): void {
  handle = requestAnimationFrame(tick);
  for (const paint of painters) {
    try {
      paint();
    } catch (error) {
      if (!broken.has(paint)) {
        broken.add(paint);
        console.error('[mydaw] a frame painter threw and will be reported only once:', error);
      }
    }
  }
}

/**
 * Run `paint` once per animation frame until the owning component is disposed.
 *
 * Call inside a component (or any reactive owner): the cleanup is registered for
 * you, which is what keeps a panel that has been switched away from off the
 * frame budget.
 */
export function onFrame(paint: Painter): void {
  painters.add(paint);
  if (handle === 0) handle = requestAnimationFrame(tick);
  onCleanup(() => {
    painters.delete(paint);
    if (painters.size === 0 && handle !== 0) {
      cancelAnimationFrame(handle);
      handle = 0;
    }
  });
}
