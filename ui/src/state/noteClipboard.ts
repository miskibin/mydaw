// The note clipboard the Piano roll pastes from (Ctrl+V), shared across patterns. The Piano roll
// copies notes into it; the pitch editor copies a vocal's notes into it ("Copy notes").
import type { Note } from '../api/types';

export const noteClip: {
  notes: Note[];
  /** Step the copied block is measured from (absent: its first note), so a paste keeps the notes' place in the bar. */
  from: number | null;
  /** Where the next paste lands (absent: right after the copied block in the pattern it came from). */
  at: number | null;
} = { notes: [], from: null, at: null };

export function setNoteClip(notes: Note[], from: number | null = null, at: number | null = null) {
  noteClip.notes = notes; noteClip.from = from; noteClip.at = at;
}
