/**
 * The computer-keyboard piano.
 *
 * Pure functions and constants only -- no DOM, no backend -- so the mapping can
 * be reasoned about (and later tested) without a browser. `state.tsx` owns the
 * listeners and the held-note bookkeeping.
 *
 * The layout is the one FL Studio and Live both use, which is worth copying
 * exactly: anybody who has touched a DAW already knows it, and a novel layout
 * would be a novelty rather than an improvement.
 *
 * ```text
 *        W   E       T   Y   U
 *      A   S   D   F   G   H   J        A = C, J = B
 *        Z = octave down, X = octave up
 * ```
 */

/** MIDI key number of C in octave 0, i.e. C-1 is key 0. */
const KEYS_PER_OCTAVE = 12;

/** Velocity every key sends. A computer keyboard has no velocity sensor. */
export const DEFAULT_VELOCITY = 0.8;

/** Octave the piano starts on. Octave 4 puts `A` on middle C (MIDI 60). */
export const DEFAULT_OCTAVE = 4;

/**
 * Lowest and highest octave the piano can shift to.
 *
 * Bounded so every key of the top octave still lands inside CLAP's `0..=127`:
 * octave 8 gives keys 108..119, octave -1 gives 0..11.
 */
export const MIN_OCTAVE = -1;
export const MAX_OCTAVE = 8;

/**
 * Physical key (`KeyboardEvent.code`) to semitone above the octave's C.
 *
 * `code`, not `key`: the mapping is about where a finger sits, so it must not
 * move when somebody uses a French or Dvorak layout.
 */
export const KEY_TO_SEMITONE: Readonly<Record<string, number>> = {
  KeyA: 0, //  C
  KeyW: 1, //  C#
  KeyS: 2, //  D
  KeyE: 3, //  D#
  KeyD: 4, //  E
  KeyF: 5, //  F
  KeyT: 6, //  F#
  KeyG: 7, //  G
  KeyY: 8, //  G#
  KeyH: 9, //  A
  KeyU: 10, // A#
  KeyJ: 11, // B
};

/** Physical key that shifts the piano down an octave. */
export const OCTAVE_DOWN_CODE = 'KeyZ';
/** Physical key that shifts the piano up an octave. */
export const OCTAVE_UP_CODE = 'KeyX';

const NOTE_NAMES = ['C', 'C#', 'D', 'D#', 'E', 'F', 'F#', 'G', 'G#', 'A', 'A#', 'B'] as const;

/** Clamp an octave into the playable range. */
export function clampOctave(octave: number): number {
  return Math.min(MAX_OCTAVE, Math.max(MIN_OCTAVE, Math.trunc(octave)));
}

/**
 * MIDI key number for a physical key at a given octave, or `null` when that key
 * is not part of the piano.
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
 * Whether a keyboard event should reach the piano at all.
 *
 * Two rejections, both about not stealing keys that mean something else:
 * anything typed into a field, and anything held with a modifier (Ctrl+S must
 * stay Ctrl+S, not a D).
 */
export function isPianoEvent(event: KeyboardEvent): boolean {
  if (event.ctrlKey || event.altKey || event.metaKey) return false;
  const target = event.target as HTMLElement | null;
  if (!target) return true;
  if (target.isContentEditable) return false;
  const tag = target.tagName;
  return tag !== 'INPUT' && tag !== 'TEXTAREA' && tag !== 'SELECT';
}
