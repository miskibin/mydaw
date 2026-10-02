/**
 * Pointer and wheel gestures for the continuous controls: faders, knobs and the
 * tempo.
 *
 * All three are the same gesture -- press, move, release, with the pointer
 * captured so the value keeps following even when the cursor leaves the little
 * rectangle it started in -- and all three are *relative*: the value moves by how
 * far the pointer moved, from where the value already was.
 *
 * Relative rather than absolute (pointer position -> value) on purpose. An
 * absolute fader slams the gain to whatever the click landed on, which on a 90 px
 * strip means a click three pixels too high is +6 dB into somebody's monitors.
 * Every DAW fader worth using works the way this does.
 *
 * Solid-free: these take an event and some callbacks and know nothing about
 * where the value is stored.
 */

/** How the drag turns pointer travel into a value. */
export type DragAxis = 'vertical' | 'horizontal' | 'both';

export interface DragOptions {
  /** The value when the gesture started. */
  from: number;
  /**
   * Value change per pixel of travel. Up and right are positive, which is what
   * "louder is up" and "more is right" both need.
   */
  perPixel: number;
  /** Bounds the result is held inside. */
  min: number;
  max: number;
  /** Called on every move with the new value. May be called with the same value. */
  onChange(value: number): void;
  /** Called once when the pointer is released, with the final value. */
  onCommit?: ((value: number) => void) | undefined;
  /**
   * Which axis the pointer travel is read from.
   *
   * `vertical` is a fader or a knob; `both` sums horizontal and vertical, which
   * is what a numeric field wants -- dragging a tempo readout sideways is as
   * natural as dragging it up, and demanding one or the other is a control that
   * feels stuck.
   */
  axis?: DragAxis;
  /**
   * Multiplier applied while a fine-adjust modifier is held. `1` disables it.
   * Shift is the modifier, as it is in every plugin editor.
   */
  fineFactor?: number;
}

/**
 * Start a drag from `event`. Returns immediately; the gesture runs on the
 * window until the pointer comes up.
 *
 * Listens on `window` rather than using `setPointerCapture` on the target: the
 * fader's handle is a 12 px sliver that Solid may re-render mid-drag when the
 * value changes, and capture on an element that gets replaced is a drag that
 * silently stops halfway.
 */
export function startDrag(event: PointerEvent, options: DragOptions): void {
  if (event.button !== 0) return;
  event.preventDefault();
  event.stopPropagation();

  const axis = options.axis ?? 'vertical';
  const fineFactor = options.fineFactor ?? FINE_FACTOR;
  let lastX = event.clientX;
  let lastY = event.clientY;
  let value = options.from;

  const clamp = (n: number) => Math.min(options.max, Math.max(options.min, n));

  // Accumulated per move rather than recomputed from the whole travel, because
  // shift has to be *live*: `from + totalTravel * fine` would rescale the pixels
  // already walked, so pressing shift after a 100 px drag would yank the value
  // most of the way back to where the gesture started. Adding each move's own
  // pixels at each move's own rate is the only reading of "fine from here on".
  const move = (moveEvent: PointerEvent) => {
    const dy = lastY - moveEvent.clientY;
    const dx = moveEvent.clientX - lastX;
    lastX = moveEvent.clientX;
    lastY = moveEvent.clientY;
    const travel = dragTravel(axis, dx, dy);
    const scale = moveEvent.shiftKey ? fineFactor : 1;
    value = clamp(value + travel * options.perPixel * scale);
    options.onChange(value);
  };

  const up = () => {
    window.removeEventListener('pointermove', move);
    window.removeEventListener('pointerup', up);
    window.removeEventListener('pointercancel', up);
    document.body.style.removeProperty('cursor');
    document.body.style.removeProperty('user-select');
    options.onCommit?.(value);
  };

  // A drag that lets the caret select the strip's labels on the way past looks
  // broken, and the cursor should say what is happening for the whole gesture
  // rather than only while over the control.
  document.body.style.setProperty('cursor', axis === 'vertical' ? 'ns-resize' : 'ew-resize');
  document.body.style.setProperty('user-select', 'none');

  window.addEventListener('pointermove', move);
  window.addEventListener('pointerup', up);
  window.addEventListener('pointercancel', up);
}

/** Convert pointer deltas to the positive-direction travel for one control. */
export function dragTravel(axis: DragAxis, dx: number, dy: number): number {
  return axis === 'vertical' ? dy : axis === 'horizontal' ? dx : dy + dx;
}

/**
 * Fallback pixels of travel for a fader that has not laid out yet.
 *
 * The live mixer measures the rendered track instead — see {@link faderThrowPx}.
 * A constant here used to be the *only* throw, which multiplied handle motion
 * on any strip taller than 180 px (the mixer fills the window).
 */
export const FADER_TRAVEL_PX = 180;

/** Height of `.wv-fader__handle`. The throw is the track minus this. */
export const FADER_HANDLE_PX = 12;

/**
 * Pixels of pointer travel that cover a fader's whole throw.
 *
 * Must match how the handle is positioned: `(1 - fraction) * (track - handle)`.
 * Any other distance moves the handle by a constant multiple of the pointer.
 */
export function faderThrowPx(trackPx: number, handlePx = FADER_HANDLE_PX): number {
  const throwPx = trackPx - handlePx;
  return throwPx > 0 ? throwPx : FADER_TRAVEL_PX;
}

/** Pixels of travel from hard left to hard right on a pan knob. */
export const KNOB_TRAVEL_PX = 140;

// ---------------------------------------------------------------------------
// The wheel
// ---------------------------------------------------------------------------

/**
 * How much of a step shift leaves. Shared with `startDrag`'s `fineFactor` on
 * purpose: shift means one thing in this shell, and a wheel that disagreed with
 * a drag about how fine "fine" is would be a second opinion about a modifier.
 */
export const FINE_FACTOR = 0.2;

/**
 * How many notches one wheel event is worth. `+1` is up, i.e. *more*.
 *
 * **The magnitude of a wheel event is not a measurement of anything.** The same
 * physical flick is 100 in `deltaY` on a Windows mouse, 3 lines on a Linux one,
 * and a stream of 1.5s on a trackpad; multiplying a fader's dB by that number is
 * how one trackpad gesture ends up at -60 dB. So only the sign is read -- which
 * also disposes of `deltaMode`, since a line, a page and a pixel all have the
 * same sign and it is the only part of the event every device agrees on.
 *
 * `deltaX` is the fallback because shift is this shell's fine-adjust modifier
 * and Chrome reports a shift-held wheel as *horizontal*: reading `deltaY` alone
 * would make fine adjustment do nothing at all.
 */
export function wheelNotches(event: WheelEvent): number {
  const delta = event.deltaY !== 0 ? event.deltaY : event.deltaX;
  if (delta === 0) return 0;
  return delta < 0 ? 1 : -1;
}

/**
 * Wire `el` up so the wheel adjusts it instead of scrolling the page past it.
 *
 * Registered by hand with `{ passive: false }` rather than as a JSX `onWheel`.
 * A passive listener's `preventDefault()` is a silent no-op -- the console says
 * nothing, `defaultPrevented` stays false and the page scrolls anyway -- and
 * whether a framework's handler ends up passive is a decision made by the
 * browser, per element, per version. This is not a thing to leave to chance on
 * a fader.
 *
 * A handler may **return `false` to decline** the notch, which leaves the event
 * untouched and the page free to do whatever it would have done. That is for
 * the container that only wants the wheel *sometimes* -- the mixer's strip row
 * takes it to scroll sideways, but only while there is anything to scroll.
 *
 * Returns the undo, for `onCleanup`.
 */
export function bindWheel(
  el: HTMLElement,
  handler: (notches: number, event: WheelEvent) => void | boolean,
): () => void {
  const onWheel = (event: WheelEvent) => {
    const notches = wheelNotches(event);
    if (notches === 0) return;
    if (handler(notches, event) === false) return;
    event.preventDefault();
    // Cancelling the default keeps the page still; stopping the event keeps any
    // handler further out -- the strip row's own sideways scroll, the roll's
    // zoom -- from also acting on a gesture that was aimed at one control.
    event.stopPropagation();
  };
  el.addEventListener('wheel', onWheel, { passive: false });
  return () => el.removeEventListener('wheel', onWheel);
}

/** A call that only happens once the caller has stopped calling it. */
export interface Debounced {
  (): void;
  cancel(): void;
}

/**
 * Defer `run` until `ms` of quiet.
 *
 * The wheel needs this and the pointer does not: a drag has a release to commit
 * on, a wheel has no such edge, and committing per notch would put a whole
 * `get_tracks` round trip between a musician's finger and the next click of the
 * wheel. The *value* still moves on every notch -- only the refetch waits.
 */
export function debounce(ms: number, run: () => void): Debounced {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const schedule = () => {
    clearTimeout(timer);
    timer = setTimeout(run, ms);
  };
  schedule.cancel = () => clearTimeout(timer);
  return schedule;
}

/** How long a wheel gesture may pause before it counts as finished. */
export const WHEEL_COMMIT_MS = 180;
