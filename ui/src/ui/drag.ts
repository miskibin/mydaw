/**
 * Pointer dragging for the continuous controls: faders, knobs and the tempo.
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
  axis?: 'vertical' | 'both';
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
  event.preventDefault();
  event.stopPropagation();

  const axis = options.axis ?? 'vertical';
  const fineFactor = options.fineFactor ?? 0.2;
  const startX = event.clientX;
  const startY = event.clientY;
  let value = options.from;

  const clamp = (n: number) => Math.min(options.max, Math.max(options.min, n));

  const move = (moveEvent: PointerEvent) => {
    const dy = startY - moveEvent.clientY;
    const dx = moveEvent.clientX - startX;
    const travel = axis === 'vertical' ? dy : dy + dx;
    const scale = moveEvent.shiftKey ? fineFactor : 1;
    value = clamp(options.from + travel * options.perPixel * scale);
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

/**
 * Pixels of travel for the whole of a fader's throw.
 *
 * Roughly a strip's height, so a fader crosses its range in one comfortable
 * gesture rather than three. The strip is ~150 px of fader; 180 gives a little
 * more resolution than one-to-one without ever feeling slow.
 */
export const FADER_TRAVEL_PX = 180;

/** Pixels of travel from hard left to hard right on a pan knob. */
export const KNOB_TRAVEL_PX = 140;
