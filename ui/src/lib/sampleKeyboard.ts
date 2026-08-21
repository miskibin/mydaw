/**
 * Pointer lifecycle for the sample editor's compact MIDI keyboard.
 *
 * Framework-free so Node Vitest can prove every path sends a matching note-off:
 * down, glide, up, cancel, lost capture, track change, and cleanup.
 */

export type SampleKeyboardSink = {
  noteOn(key: number, velocity: number): void;
  noteOff(key: number): void;
};

export type SampleKeyboardController = {
  down(key: number, pointerId: number, velocity?: number): void;
  move(key: number, pointerId: number): void;
  up(pointerId: number): void;
  cancel(pointerId?: number): void;
  lostCapture(pointerId: number): void;
  setTrack(token: string): void;
  cleanup(): void;
  heldKey(): number | null;
};

const DEFAULT_VELOCITY = 0.9;

export function createSampleKeyboardController(sink: SampleKeyboardSink): SampleKeyboardController {
  let pointerId: number | null = null;
  let held: number | null = null;
  let trackToken = '';

  function release(): void {
    if (held === null) return;
    const key = held;
    held = null;
    sink.noteOff(key);
  }

  return {
    down(key, id, velocity = DEFAULT_VELOCITY) {
      if (key < 0 || key > 127) return;
      if (pointerId !== null && pointerId !== id) {
        release();
      }
      pointerId = id;
      if (held === key) return;
      release();
      held = key;
      sink.noteOn(key, Math.min(1, Math.max(0.05, velocity)));
    },
    move(key, id) {
      if (pointerId !== id || held === null) return;
      if (key < 0 || key > 127 || key === held) return;
      release();
      held = key;
      sink.noteOn(key, DEFAULT_VELOCITY);
    },
    up(id) {
      if (pointerId !== id) return;
      pointerId = null;
      release();
    },
    cancel(id) {
      if (id !== undefined && pointerId !== id) return;
      pointerId = null;
      release();
    },
    lostCapture(id) {
      if (pointerId !== id) return;
      pointerId = null;
      release();
    },
    setTrack(token) {
      if (token === trackToken) return;
      trackToken = token;
      pointerId = null;
      release();
    },
    cleanup() {
      pointerId = null;
      release();
    },
    heldKey() {
      return held;
    },
  };
}
