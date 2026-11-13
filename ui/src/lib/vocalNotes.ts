// The pitch editor's "Copy notes": a vocal's detected (and edited) notes as piano roll notes, so a
// melody that was sung can be played by an instrument (FL's NewTone "Send to piano roll", Melodyne's
// "Copy as MIDI"). Pure: the editor hands in what it draws, the piano roll pastes what comes out.
import type { Note } from '../api/types';

/** A pitch editor note: source seconds, the pitch it sounds at (MIDI, fractional), its level 0..1. */
export interface SungNote { start: number; end: number; pitch: number; level: number }

/** Where the clip plays its source: `start` beat on the song, from source second `t0`, `spbSrc` source seconds per beat. */
export interface ClipTiming { start: number; t0: number; t1: number; spbSrc: number }

const STEPS_PER_BEAT = 4, STEPS_PER_BAR = 16;

/**
 * The sung notes as pattern notes (channel `ch` 0: the paste puts them on its channel).
 * - Time: source seconds → song beats through the clip (offset, stretch, tempo), on the 1/16 grid;
 *   only what the clip plays is kept. A note shorter than a step gets one step.
 * - Steps count from the bar the first note is in, so the melody keeps its place in the bar.
 * - Key: the sounding pitch rounded to a semitone (pitch edits included).
 * - Velocity: the level relative to the loudest copied note (0.35..1), so accents survive.
 * - The voice is monophonic: a note that the grid makes overlap the next one ends where it starts;
 *   two that land on the same step keep the longer one.
 * `songStep` is where the first bar starts on the song, in steps (to paste in place).
 */
export function sungToPattern(sung: readonly SungNote[], clip: ClipTiming): { notes: Note[]; songStep: number } {
  const toStep = (sec: number) => (clip.start + (sec - clip.t0) / clip.spbSrc) * STEPS_PER_BEAT;
  const loud = Math.max(1e-6, ...sung.map(n => n.level));
  const raw: Note[] = [];
  for (const n of [...sung].sort((a, b) => a.start - b.start)) {
    const a = Math.max(n.start, clip.t0), b = Math.min(n.end, clip.t1);
    if (!(b > a) || !Number.isFinite(n.pitch)) continue;
    const s = Math.round(toStep(a)), e = Math.round(toStep(b));
    const p = Math.min(127, Math.max(0, Math.round(n.pitch)));
    const v = Math.round((0.35 + 0.65 * Math.min(1, Math.max(0, n.level / loud))) * 100) / 100;
    raw.push({ ch: 0, step: s, len: Math.max(1, e - s), p, v });
  }
  const out: Note[] = [];
  for (const n of raw) {
    const prev = out[out.length - 1];
    if (prev && n.step <= prev.step) { if (n.len > prev.len) out[out.length - 1] = n; continue; }
    if (prev && prev.step + prev.len > n.step) prev.len = n.step - prev.step;
    out.push(n);
  }
  if (!out.length) return { notes: [], songStep: 0 };
  const bar = Math.floor(out[0].step / STEPS_PER_BAR) * STEPS_PER_BAR;
  return { notes: out.map(n => ({ ...n, step: n.step - bar })), songStep: bar };
}
