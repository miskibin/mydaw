// Shift+drag in the Piano roll clones notes (FL Studio): copies of the dragged notes are added
// at the end, the originals stay where they are, and the drag then moves the copies.
import type { Note } from '../api/types';

/** `notes` plus copies of `idx` (same order, appended), and the copies' indices. */
export function cloneNotes(notes: readonly Note[], idx: readonly number[]): { notes: Note[]; idx: number[] } {
  const ok = idx.filter(i => i >= 0 && i < notes.length);
  const base = notes.length;
  return { notes: [...notes, ...ok.map(i => ({ ...notes[i] }))], idx: ok.map((_, j) => base + j) };
}
