/**
 * Pattern arithmetic, shared by the rack, the piano roll and the playlist.
 *
 * Solid-free on purpose: everything here is a pure function over the wire
 * shapes, so it can be reasoned about (and one day tested) without a renderer.
 *
 * **Every `with*` helper returns the *same object reference* when the edit is
 * illegal or a no-op.** `state.tsx` relies on `after === before` to skip both
 * the redraw and the round trip to the engine.
 */

import type { ChannelClipDto, MelodicNoteDto, PatternDto, StepDto, StepRowDto } from '../api/types';

/** Ticks per quarter note. Mirrors `PPQ` in the sequencer. */
export const PPQ = 960;
/** Ticks in one step of the grid (a sixteenth). Mirrors `TICKS_PER_STEP`. */
export const TICKS_PER_STEP = 240;
/** Ticks in one bar of 4/4. Mirrors `TICKS_PER_BAR`. */
export const TICKS_PER_BAR = 3840;
/** Steps in one beat. */
export const STEPS_PER_BEAT = 4;
/** The lengths a pattern may be. Mirrors `PATTERN_LENGTHS`. */
export const PATTERN_LENGTHS = [16, 32, 64] as const;
/** Velocity a freshly switched-on step gets. Mirrors `DEFAULT_STEP_VELOCITY`. */
export const STEP_VELOCITY = 0.78;
/** Velocity a shift-clicked step gets. */
export const ACCENT_VELOCITY = 1;
/** At or above this, a step or note draws as an accent. */
export const ACCENT_THRESHOLD = 0.95;
/** Most notes one channel may hold. Mirrors `MAX_NOTES`. */
export const MAX_NOTES = 1024;
/** Most channels one pattern may speak for. Mirrors `MAX_PATTERN_CHANNELS`. */
export const MAX_PATTERN_CHANNELS = 16;
/** What the UI assumes before a device is open, only for laying out a grid. */
export const FALLBACK_SAMPLE_RATE = 48_000;

const NOTE_NAMES = ['C', 'C#', 'D', 'D#', 'E', 'F', 'F#', 'G', 'G#', 'A', 'A#', 'B'];
const BLACK_KEYS = new Set([1, 3, 6, 8, 10]);

/** `60` -> `C4`, the convention every DAW's piano roll labels keys with. */
export function noteName(key: number): string {
  const name = NOTE_NAMES[((key % 12) + 12) % 12] ?? 'C';
  return `${name}${Math.floor(key / 12) - 1}`;
}

/** Whether this key is drawn as a black key. */
export function isBlackKey(key: number): boolean {
  return BLACK_KEYS.has(((key % 12) + 12) % 12);
}

/** Ticks in one time round `pattern`'s loop. */
export function patternTicks(pattern: PatternDto): number {
  return pattern.steps * TICKS_PER_STEP;
}

/** Whether a step or note is loud enough to draw as an accent. */
export function isAccent(velocity: number): boolean {
  return velocity >= ACCENT_THRESHOLD;
}

// ---------------------------------------------------------------------------
// Channels
// ---------------------------------------------------------------------------

/** What `pattern` says to `channel`, or `undefined` when it says nothing. */
export function channelClip(pattern: PatternDto, channel: number): ChannelClipDto | undefined {
  return pattern.channels.find((clip) => clip.channel === channel);
}

/** The notes `pattern` puts on `channel`. Never `undefined`, so callers can map. */
export function notesOf(pattern: PatternDto, channel: number): MelodicNoteDto[] {
  return channelClip(pattern, channel)?.notes ?? [];
}

/** The step grid `pattern` puts on `channel`, when it has one. */
export function stepsOf(pattern: PatternDto, channel: number): StepRowDto | null {
  return channelClip(pattern, channel)?.steps ?? null;
}

/** How a channel draws: notes win, because a grid cannot show a note length. */
export function displayMode(clip: ChannelClipDto | undefined): 'steps' | 'roll' {
  return clip && clip.notes.length > 0 ? 'roll' : 'steps';
}

/** An empty grid on `channel`, triggering `key`. */
export function emptyClip(channel: number, key: number, steps: number): ChannelClipDto {
  return {
    channel,
    mode: 'steps',
    steps: {
      key,
      label: '',
      steps: Array.from({ length: steps }, () => ({ on: false, velocity: STEP_VELOCITY })),
    },
    notes: [],
  };
}

/**
 * Replace what `pattern` says to `channel`.
 *
 * `edit` is handed the clip that is there -- or a fresh empty one, so a caller
 * never has to decide whether the channel already existed -- and returns the
 * clip that should be there. Returning the clip it was given, or `null` for a
 * channel that was already silent, leaves `pattern` untouched *by reference*.
 */
export function withChannel(
  pattern: PatternDto,
  channel: number,
  fallbackKey: number,
  edit: (clip: ChannelClipDto) => ChannelClipDto | null,
): PatternDto {
  const current = channelClip(pattern, channel);
  const before = current ?? emptyClip(channel, fallbackKey, pattern.steps);
  const after = edit(before);
  if (after === before) return pattern;
  if (after === null) {
    if (!current) return pattern;
    return { ...pattern, channels: pattern.channels.filter((clip) => clip.channel !== channel) };
  }
  const channels = current
    ? pattern.channels.map((clip) => (clip.channel === channel ? after : clip))
    : [...pattern.channels, after].sort((a, b) => a.channel - b.channel);
  return { ...pattern, channels };
}

// ---------------------------------------------------------------------------
// Steps
// ---------------------------------------------------------------------------

/**
 * Toggle one cell.
 *
 * A shift-click *promotes* rather than toggles: off -> accent -> off, so the
 * accent is one gesture away from an empty cell and a plain click never
 * silently downgrades one somebody set.
 */
export function withToggledStep(
  pattern: PatternDto,
  channel: number,
  index: number,
  accent: boolean,
  fallbackKey: number,
): PatternDto {
  return withChannel(pattern, channel, fallbackKey, (clip) => {
    const row = clip.steps;
    if (!row || index < 0 || index >= row.steps.length) return clip;
    const cell = row.steps[index]!;
    const next: StepDto = accent
      ? cell.on && isAccent(cell.velocity)
        ? { on: false, velocity: cell.velocity }
        : { on: true, velocity: ACCENT_VELOCITY }
      : { on: !cell.on, velocity: cell.on ? cell.velocity : Math.max(cell.velocity, STEP_VELOCITY) };
    const steps = row.steps.slice();
    steps[index] = next;
    return { ...clip, steps: { ...row, steps } };
  });
}

/** Empty every cell and drop every note on `channel`. */
export function withClearedChannel(
  pattern: PatternDto,
  channel: number,
  fallbackKey: number,
): PatternDto {
  return withChannel(pattern, channel, fallbackKey, (clip) => {
    const emptyRow = clip.steps
      ? { ...clip.steps, steps: clip.steps.steps.map((cell) => ({ ...cell, on: false })) }
      : clip.steps;
    if (clip.notes.length === 0 && emptyRow === clip.steps) return clip;
    return { ...clip, steps: emptyRow, notes: [] };
  });
}

/** Switched-on cells plus notes, across every channel: the picker's "events". */
export function eventCount(pattern: PatternDto): number {
  return pattern.channels.reduce(
    (total, clip) =>
      total + clip.notes.length + (clip.steps?.steps.filter((cell) => cell.on).length ?? 0),
    0,
  );
}

// ---------------------------------------------------------------------------
// Steps <-> roll
// ---------------------------------------------------------------------------
//
// The engine's compiler plays a channel's grid *and* its notes -- `mode` is
// derived from whether there are notes, but both are sounded. So switching a
// channel between the two has to be a real conversion rather than a view
// toggle: leaving the cells on underneath a converted melody would double every
// hit. Both directions carry as much as the other representation can hold, and
// the caller says so in the parameter readout.

/** One note per switched-on cell, a sixteenth long, at the cell's velocity. */
export function stepsToNotes(row: StepRowDto, length: number): MelodicNoteDto[] {
  const notes: MelodicNoteDto[] = [];
  row.steps.slice(0, length).forEach((cell, index) => {
    if (!cell.on) return;
    notes.push({
      key: row.key,
      start: index * TICKS_PER_STEP,
      length: TICKS_PER_STEP,
      velocity: cell.velocity,
    });
  });
  return notes;
}

/**
 * Fold notes back onto the grid: each lands on its nearest step, at its own
 * velocity, and the row's key becomes the one most of them used.
 *
 * Lossy, and honestly so -- a grid has one key and no note lengths. The caller
 * says what happened in the readout rather than pretending it was free.
 */
export function notesToSteps(
  notes: readonly MelodicNoteDto[],
  row: StepRowDto,
  length: number,
): StepRowDto {
  const tally = new Map<number, number>();
  for (const note of notes) tally.set(note.key, (tally.get(note.key) ?? 0) + 1);
  let key = row.key;
  let best = 0;
  for (const [candidate, count] of tally) {
    if (count > best) {
      best = count;
      key = candidate;
    }
  }
  const cells: StepDto[] = Array.from({ length: row.steps.length }, (_, index) => ({
    on: false,
    velocity: row.steps[index]?.velocity ?? STEP_VELOCITY,
  }));
  for (const note of notes) {
    const index = Math.round(note.start / TICKS_PER_STEP);
    if (index < 0 || index >= length || index >= cells.length) continue;
    cells[index] = { on: true, velocity: note.velocity };
  }
  return { ...row, key, steps: cells };
}

// ---------------------------------------------------------------------------
// Notes
// ---------------------------------------------------------------------------

/** Clamp a note into the pattern, or reject it outright. */
export function clampNote(note: MelodicNoteDto, pattern: PatternDto): MelodicNoteDto | null {
  const limit = patternTicks(pattern);
  const key = Math.round(note.key);
  if (key < 0 || key > 127) return null;
  const start = Math.max(0, Math.round(note.start));
  if (start >= limit) return null;
  const length = Math.max(1, Math.min(Math.round(note.length), limit - start));
  const velocity = Math.min(1, Math.max(0.01, note.velocity));
  return { key, start, length, velocity };
}

/**
 * Put `notes` on `channel`, clamped and capped.
 *
 * **Deliberately not sorted.** The roll addresses the note it is dragging by
 * its index in this array, so re-ordering on every edit would make a selection
 * point at different notes halfway through a drag. The engine does not care --
 * it compiles the notes into a schedule of its own -- so the order is the UI's
 * to keep stable, and keeping it stable is what makes a marquee of forty notes
 * draggable at all.
 */
export function withNotes(
  pattern: PatternDto,
  channel: number,
  notes: readonly MelodicNoteDto[],
  fallbackKey: number,
): PatternDto {
  const clean: MelodicNoteDto[] = [];
  for (const note of notes) {
    const fitted = clampNote(note, pattern);
    if (fitted) clean.push(fitted);
    if (clean.length >= MAX_NOTES) break;
  }
  return withChannel(pattern, channel, fallbackKey, (clip) => ({ ...clip, notes: clean }));
}

/**
 * Which note in `notes` is under `(tick, key)`, or `-1`.
 *
 * Reverse scan, so the note drawn last -- the one on top -- is the one the
 * pointer gets.
 */
export function noteAt(notes: readonly MelodicNoteDto[], tick: number, key: number): number {
  for (let index = notes.length - 1; index >= 0; index -= 1) {
    const note = notes[index]!;
    if (note.key === key && tick >= note.start && tick < note.start + note.length) return index;
  }
  return -1;
}

/** Snap a tick to a grid. `snap` of `0` means no snapping. */
export function snapTicks(ticks: number, snap: number, floor = false): number {
  if (snap <= 0) return Math.max(0, Math.round(ticks));
  const grid = floor ? Math.floor(ticks / snap) : Math.round(ticks / snap);
  return Math.max(0, grid * snap);
}

// ---------------------------------------------------------------------------
// Note operations
// ---------------------------------------------------------------------------
//
// Every one of these is a pure array -> array transform over the notes the user
// picked, so the roll can apply it, push the result and hand the previous state
// to the undo stack without any of them knowing about either.

/** Move each note's start onto the snap grid. */
export function quantize(notes: readonly MelodicNoteDto[], snap: number): MelodicNoteDto[] {
  const grid = snap > 0 ? snap : TICKS_PER_STEP;
  return notes.map((note) => ({ ...note, start: snapTicks(note.start, grid) }));
}

/**
 * Extend each note to where the next one starts.
 *
 * `all` is the whole channel, not just the selection: a selected note's
 * neighbour may not be selected, and stopping short of it would be a legato
 * that leaves a gap.
 */
export function legato(
  picked: readonly MelodicNoteDto[],
  all: readonly MelodicNoteDto[],
): MelodicNoteDto[] {
  const starts = [...new Set(all.map((note) => note.start))].sort((a, b) => a - b);
  return picked.map((note) => {
    const next = starts.find((start) => start > note.start);
    return next === undefined ? note : { ...note, length: Math.max(1, next - note.start) };
  });
}

/** Scale every length by `factor`, never below a 64th. */
export function scaleLength(notes: readonly MelodicNoteDto[], factor: number): MelodicNoteDto[] {
  return notes.map((note) => ({
    ...note,
    length: Math.max(TICKS_PER_STEP / 4, Math.round(note.length * factor)),
  }));
}

/**
 * The **added** notes of a major triad on each of `notes`: +4 and +7 semitones.
 *
 * Deliberately not diatonic. The engine has no key signature and inventing one
 * would put a guess in somebody's song; a major triad is the one answer that is
 * obviously a choice rather than a wrong analysis, and it is what the mock's
 * Chord button does.
 */
export function chord(notes: readonly MelodicNoteDto[]): MelodicNoteDto[] {
  const added: MelodicNoteDto[] = [];
  for (const note of notes) {
    for (const interval of [4, 7]) {
      if (note.key + interval <= 127) added.push({ ...note, key: note.key + interval });
    }
  }
  return added;
}

/**
 * Nudge start and velocity, deterministically.
 *
 * Seeded rather than `Math.random`, so pressing Humanize twice with the same
 * selection is a repeatable edit and undo/redo lands on the same notes.
 */
export function humanize(notes: readonly MelodicNoteDto[], seed: number): MelodicNoteDto[] {
  const random = mulberry32(seed);
  return notes.map((note) => ({
    ...note,
    start: Math.max(0, note.start + Math.round((random() - 0.5) * 20)),
    velocity: Math.min(1, Math.max(0.05, note.velocity + (random() - 0.5) * 0.125)),
  }));
}

/** A small, fast, seeded PRNG. */
function mulberry32(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = Math.imul(state ^ (state >>> 15), 1 | state);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

// ---------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------

/** Samples one tick covers at `bpm`. */
export function samplesPerTick(sampleRate: number, bpm: number): number {
  const rate = sampleRate > 0 ? sampleRate : FALLBACK_SAMPLE_RATE;
  return (rate * 60) / (bpm * PPQ);
}

/** Absolute song position, in fractional ticks. */
export function samplesToTicks(samplePos: number, sampleRate: number, bpm: number): number {
  return samplePos / samplesPerTick(sampleRate, bpm);
}

/** The inverse, for a click on a ruler: which sample to seek to. */
export function ticksToSamples(ticks: number, sampleRate: number, bpm: number): number {
  return ticks * samplesPerTick(sampleRate, bpm);
}

/**
 * Where the playhead is inside the looping pattern, in ticks, or `null` when
 * nothing is playing.
 */
export function patternTick(
  samplePos: number,
  sampleRate: number,
  bpm: number,
  pattern: PatternDto,
): number {
  const loop = patternTicks(pattern);
  const ticks = samplesToTicks(samplePos, sampleRate, bpm);
  return loop > 0 ? ((ticks % loop) + loop) % loop : 0;
}

/** `1.2.3` -- bar, beat, sixteenth, all one-based, as a transport reads them. */
export function barsBeats(ticks: number): string {
  const safe = Math.max(0, ticks);
  const bar = Math.floor(safe / TICKS_PER_BAR) + 1;
  const beat = Math.floor((safe % TICKS_PER_BAR) / PPQ) + 1;
  const sixteenth = Math.floor((safe % PPQ) / TICKS_PER_STEP) + 1;
  return `${String(bar).padStart(3, '0')}.${beat}.${sixteenth}`;
}

/** A note length as a musician would say it: `1/16`, `1/4`, `2 bars`. */
export function lengthLabel(ticks: number): string {
  if (ticks % TICKS_PER_BAR === 0 && ticks >= TICKS_PER_BAR) {
    const bars = ticks / TICKS_PER_BAR;
    return bars === 1 ? '1 bar' : `${bars} bars`;
  }
  const fraction = (4 * PPQ) / ticks;
  if (Number.isInteger(fraction)) return `1/${fraction}`;
  return `${(ticks / PPQ).toFixed(2)} beats`;
}
