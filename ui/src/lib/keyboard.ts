/**
 * The computer-keyboard piano.
 *
 * Pure functions and constants only -- no DOM, no backend -- so the mapping can
 * be reasoned about (and later tested) without a renderer. `state.tsx` owns the
 * listeners and the held-note bookkeeping.
 *
 * The layout is FL Studio's typing keyboard (Ctrl+T): two octaves, black keys
 * on the number row, Q = C of the upper octave.
 *
 * ```text
 *     2 3   5 6 7     9 0
 *    Q W E R T Y U I O P     Q = C4 when the base octave is 3
 *     S D   G H J
 *    Z X C V B N M           Z = C3
 * ```
 */

/** MIDI key number of C in octave 0, i.e. C-1 is key 0. */
const KEYS_PER_OCTAVE = 12;

/** Velocity every key sends. A computer keyboard has no velocity sensor. */
export const DEFAULT_VELOCITY = 0.8;

/**
 * Octave the *lower* row starts on. Octave 3 puts `Z` on C3 and `Q` on C4,
 * which is FL's default and what the toolbar button announces.
 */
export const DEFAULT_OCTAVE = 3;

/**
 * Lowest and highest octave the piano can shift to.
 *
 * Bounded so the top of the Q-row (`P` = E of the octave above Q) still lands
 * inside CLAP's `0..=127`.
 */
export const MIN_OCTAVE = -1;
export const MAX_OCTAVE = 8;

/**
 * Physical key (`KeyboardEvent.code`) to semitone above the lower row's C.
 *
 * `code`, not `key`: the mapping is about where a finger sits, so it must not
 * move when somebody uses a French or Dvorak layout. The Q-row is +12 so it
 * sits one octave above Z.
 */
export const KEY_TO_SEMITONE: Readonly<Record<string, number>> = {
  // Lower octave: Z = C
  KeyZ: 0,
  KeyS: 1,
  KeyX: 2,
  KeyD: 3,
  KeyC: 4,
  KeyV: 5,
  KeyG: 6,
  KeyB: 7,
  KeyH: 8,
  KeyN: 9,
  KeyJ: 10,
  KeyM: 11,
  // Upper octave: Q = C
  KeyQ: 12,
  Digit2: 13,
  KeyW: 14,
  Digit3: 15,
  KeyE: 16,
  KeyR: 17,
  Digit5: 18,
  KeyT: 19,
  Digit6: 20,
  KeyY: 21,
  Digit7: 22,
  KeyU: 23,
  KeyI: 24,
  Digit9: 25,
  KeyO: 26,
  Digit0: 27,
  KeyP: 28,
};

// Drum pads (C/V/B/N one-shots) were dropped deliberately: the FL two-octave
// piano layout above already owns those keys, so pads never got wired into
// the listener and carry no mapping here.

const NOTE_NAMES = ['C', 'C#', 'D', 'D#', 'E', 'F', 'F#', 'G', 'G#', 'A', 'A#', 'B'] as const;

/** Clamp an octave into the playable range. */
export function clampOctave(octave: number): number {
  return Math.min(MAX_OCTAVE, Math.max(MIN_OCTAVE, Math.trunc(octave)));
}

/**
 * MIDI key number for a physical key at a given base octave, or `null` when
 * that key is not part of the piano.
 *
 * Returns `null` rather than clamping for a key outside `0..=127`: silently
 * playing a different note than the one asked for is worse than playing none.
 */
export function midiKeyFor(code: string, octave: number): number | null {
  const semitone = KEY_TO_SEMITONE[code];
  if (semitone === undefined) return null;
  const key = (clampOctave(octave) + 1) * KEYS_PER_OCTAVE + semitone;
  return key >= 0 && key <= 127 ? key : null;
}

/** `60` -> `"C4"`. Scientific pitch notation, the one DAWs print. */
export function noteName(key: number): string {
  const name = NOTE_NAMES[((key % KEYS_PER_OCTAVE) + KEYS_PER_OCTAVE) % KEYS_PER_OCTAVE];
  const octave = Math.floor(key / KEYS_PER_OCTAVE) - 1;
  return `${name}${octave}`;
}

/**
 * Whether the computer-keyboard piano is listening.
 *
 * Off by default, like FL's "typing keyboard to piano" (Ctrl+T). The top-bar
 * button and Ctrl+T flip this. When it is on, letter keys are notes and the
 * piano roll's tool shortcuts (P/B/E/D/C) stand down.
 */
let typingPianoEnabled = false;

/** Pause or resume the typing piano. */
export function setTypingPianoEnabled(enabled: boolean): void {
  typingPianoEnabled = enabled;
}

/**
 * Whether a keyboard event should reach the piano at all.
 *
 * Three rejections: the toggle is off, anything typed into a field, and
 * anything held with a modifier (Ctrl+S must stay Ctrl+S, not a D).
 */
export function isPianoEvent(event: KeyboardEvent): boolean {
  if (!typingPianoEnabled) return false;
  if (event.ctrlKey || event.altKey || event.metaKey) return false;
  const target = event.target as HTMLElement | null;
  if (!target) return true;
  if (target.isContentEditable) return false;
  const tag = target.tagName;
  return tag !== 'INPUT' && tag !== 'TEXTAREA' && tag !== 'SELECT';
}
