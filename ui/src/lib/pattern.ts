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
/** Ticks in the longest pattern the engine allows: four bars of 4/4. */
export const MAX_PATTERN_TICKS = PATTERN_LENGTHS[PATTERN_LENGTHS.length - 1]! * TICKS_PER_STEP;
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

/**
 * How a pattern miniature is laid out inside a playlist clip.
 *
 * Marks are fractions of **one pattern**, not of the clip. The art is then
 * drawn at `cyclePx` wide — the pattern's own duration on the timeline — and
 * tiled `cycles` times. A shorter clip crops via overflow; a longer clip shows
 * the loop instead of stretching one copy across the block.
 */
export function clipArtLayout(
  clipLengthTicks: number,
  patternLengthTicks: number,
  pxPerTick: number,
): { cyclePx: number; cycles: number } {
  const cycle = Math.max(1, patternLengthTicks);
  return {
    cyclePx: cycle * pxPerTick,
    cycles: Math.max(1, Math.ceil(Math.max(0, clipLengthTicks) / cycle)),
  };
}

/**
 * How an audio clip's waveform is laid out inside the playlist clip.
 *
 * The wave is drawn at the **file's** duration on the timeline, not the clip's.
 * Shortening the clip crops via overflow; lengthening past the remaining audio
 * leaves empty space (the engine does not loop or stretch audio). An offset
 * shifts the wave left so the audible start sits at the clip's left edge.
 */
export function audioWaveLayout(
  sourceDurationTicks: number,
  sourceFrames: number,
  offsetFrames: number,
  pxPerTick: number,
): { wavePx: number; offsetPx: number } {
  const wavePx = Math.max(0, sourceDurationTicks) * pxPerTick;
  const frames = Math.max(0, sourceFrames);
  const offset = Math.min(Math.max(0, offsetFrames), frames);
  return {
    wavePx,
    offsetPx: frames > 0 ? (offset / frames) * wavePx : 0,
  };
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

/** A cell nothing has been drawn into. */
export function emptyStep(): StepDto {
  return { on: false, velocity: STEP_VELOCITY };
}

/** An empty grid of `steps` cells, triggering `key`. */
export function emptyRow(key: number, steps: number): StepRowDto {
  return { key, label: '', steps: Array.from({ length: steps }, emptyStep) };
}

/**
 * The `length` cells a channel's lane draws, whatever state the clip is in.
 *
 * Three things can arrive on the wire and all three must come out as a full
 * lane, because **a rack row a user cannot click in is not a state anyone can
 * understand**:
 *
 * - no clip at all -- a channel added while a hosted plugin was placed used to
 *   arrive like this (`AppState::add_track` skipped its pattern sync on the two
 *   hosted branches);
 * - a clip with `steps: null` -- `ChannelClip::melodic` in the engine makes one,
 *   and `PatternSet::add_channel` will not retrofit a grid onto a clip that
 *   already exists;
 * - a clip whose row is shorter than the pattern, after a length change.
 *
 * The old rack read `clip.steps.steps` and fell back to `[]`, which rendered as
 * a row with the mode button and the delete cross and *no lane between them*.
 */
export function stepCells(clip: ChannelClipDto | undefined, length: number): StepDto[] {
  const row = clip?.steps?.steps;
  if (row && row.length === length) return row;
  return Array.from({ length }, (_, index) => row?.[index] ?? emptyStep());
}

/** How a channel draws: notes win, because a grid cannot show a note length. */
export function displayMode(clip: ChannelClipDto | undefined): 'steps' | 'roll' {
  return clip && clip.notes.length > 0 ? 'roll' : 'steps';
}

/** An empty grid on `channel`, triggering `key`. */
export function emptyClip(channel: number, key: number, steps: number): ChannelClipDto {
  return { channel, mode: 'steps', steps: emptyRow(key, steps), notes: [] };
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
    if (index < 0 || index >= pattern.steps) return clip;
    // The lane is drawn for every channel (see `stepCells`), so a click on one
    // has to be able to create the row it was drawn on -- otherwise the cell
    // the user just hit is a control that does nothing.
    const existing = clip.steps;
    const row =
      existing && existing.steps.length === pattern.steps
        ? existing
        : {
            ...(existing ?? emptyRow(fallbackKey, pattern.steps)),
            steps: stepCells(clip, pattern.steps),
          };
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

/**
 * Rewrite `channel`'s grid. Notes are dropped: the compiler plays both, so a
 * fill left under a melody would double every hit.
 *
 * Returning the same cell array, when the clip already had that row and no
 * notes, is a no-op by reference — same contract as the other `with*` helpers.
 */
function withMappedSteps(
  pattern: PatternDto,
  channel: number,
  fallbackKey: number,
  edit: (cells: readonly StepDto[]) => StepDto[],
): PatternDto {
  return withChannel(pattern, channel, fallbackKey, (clip) => {
    const row =
      clip.steps && clip.steps.steps.length === pattern.steps
        ? clip.steps
        : {
            ...(clip.steps ?? emptyRow(fallbackKey, pattern.steps)),
            steps: stepCells(clip, pattern.steps),
          };
    const steps = edit(row.steps);
    const sameGrid =
      steps === row.steps ||
      (steps.length === row.steps.length &&
        steps.every(
          (cell, index) =>
            cell.on === row.steps[index]!.on && cell.velocity === row.steps[index]!.velocity,
        ));
    if (sameGrid && clip.notes.length === 0 && clip.steps === row) return clip;
    return { ...clip, steps: sameGrid ? row : { ...row, steps }, notes: [] };
  });
}

/**
 * Turn on every `interval`th cell starting at 0, and turn the rest off.
 *
 * Interval 4 on a sixteenth grid is a hit on each beat; 2 is eighths; 1 is
 * every cell. Illegal intervals leave the pattern untouched.
 */
export function withFilledSteps(
  pattern: PatternDto,
  channel: number,
  interval: number,
  fallbackKey: number,
): PatternDto {
  if (!Number.isInteger(interval) || interval < 1) return pattern;
  return withMappedSteps(pattern, channel, fallbackKey, (cells) => {
    let changed = false;
    const steps = cells.map((cell, index) => {
      const on = index % interval === 0;
      if (cell.on === on) return cell;
      changed = true;
      return { ...cell, on, velocity: on ? Math.max(cell.velocity, STEP_VELOCITY) : cell.velocity };
    });
    return changed ? steps : (cells as StepDto[]);
  });
}

/** Flip every cell. Empty becomes a full row; a full row becomes empty. */
export function withInvertedSteps(
  pattern: PatternDto,
  channel: number,
  fallbackKey: number,
): PatternDto {
  return withMappedSteps(pattern, channel, fallbackKey, (cells) =>
    cells.map((cell) => ({ ...cell, on: !cell.on })),
  );
}

/**
 * Rotate the grid. Negative `delta` moves hits earlier (left on the rack);
 * positive moves them later.
 */
export function withShiftedSteps(
  pattern: PatternDto,
  channel: number,
  delta: number,
  fallbackKey: number,
): PatternDto {
  return withMappedSteps(pattern, channel, fallbackKey, (cells) => {
    const length = cells.length;
    if (length === 0) return cells as StepDto[];
    const shift = ((delta % length) + length) % length;
    if (shift === 0) return cells as StepDto[];
    return cells.map((_, index) => cells[(index - shift + length) % length]!);
  });
}

/** Copy one channel's first bar into every later bar of the pattern. */
export function withRepeatedFirstBar(
  pattern: PatternDto,
  channel: number,
  fallbackKey: number,
): PatternDto {
  if (pattern.steps <= 16) return pattern;
  return withChannel(pattern, channel, fallbackKey, (clip) => {
    const sourceCells = stepCells(clip, pattern.steps).slice(0, 16);
    const steps = clip.steps
      ? {
          ...clip.steps,
          steps: Array.from({ length: pattern.steps }, (_, at) => sourceCells[at % 16]!),
        }
      : clip.steps;
    const sourceNotes = clip.notes.filter((note) => note.start < TICKS_PER_BAR);
    const notes = Array.from({ length: pattern.steps / 16 }, (_, bar) =>
      sourceNotes.map((note) => ({ ...note, start: note.start + bar * TICKS_PER_BAR })),
    )
      .flat()
      .map((note) => clampNote(note, pattern))
      .filter((note): note is MelodicNoteDto => note !== null)
      .slice(0, MAX_NOTES);
    return { ...clip, steps, notes };
  });
}

/**
 * Independently switch each cell on or off. Seeded so the same press is a
 * repeatable undo entry, not a new roll every time the stack is replayed.
 */
export function withRandomSteps(
  pattern: PatternDto,
  channel: number,
  seed: number,
  fallbackKey: number,
): PatternDto {
  const random = mulberry32(seed);
  return withMappedSteps(pattern, channel, fallbackKey, (cells) =>
    cells.map((cell) => ({
      ...cell,
      on: random() < 0.4,
      velocity: Math.max(cell.velocity, STEP_VELOCITY),
    })),
  );
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

/**
 * The shortest allowed length that holds `ticks`, capped at the longest.
 *
 * A pattern may be one, two or four bars and nothing else (the engine's
 * `PATTERN_LENGTHS`), so "long enough" is a choice from three, not a number.
 */
export function lengthForTicks(ticks: number): number {
  for (const steps of PATTERN_LENGTHS) {
    if (steps * TICKS_PER_STEP >= ticks) return steps;
  }
  return PATTERN_LENGTHS[PATTERN_LENGTHS.length - 1]!;
}

/**
 * Grow `pattern` to `steps`, padding every step row to match.
 *
 * The engine **rejects** a pattern whose step rows are not exactly as long as
 * the pattern claims to be (`PatternError::WrongRowLength`), so a length change
 * that did not carry the rows with it would be refused at the wall and the edit
 * would silently roll back. Never shortens: this is the growth path, and
 * shortening is a deliberate command with its own non-destructive rules.
 */
export function withLength(pattern: PatternDto, steps: number): PatternDto {
  if (steps <= pattern.steps) return pattern;
  return {
    ...pattern,
    steps,
    channels: pattern.channels.map((clip) =>
      clip.steps ? { ...clip, steps: { ...clip.steps, steps: stepCells(clip, steps) } } : clip,
    ),
  };
}

/**
 * Grow `pattern` so that `ticks` fits inside it. Same reference when it does.
 *
 * This is what makes the piano roll's grid bigger than the pattern honest: you
 * can draw in the fourth bar of a one-bar pattern, and the pattern becomes four
 * bars long rather than the note being thrown away at the wall. Past four bars
 * there is nowhere to grow to, and the note is clamped as it always was.
 */
export function withGrownLength(pattern: PatternDto, ticks: number): PatternDto {
  if (ticks <= patternTicks(pattern)) return pattern;
  return withLength(pattern, lengthForTicks(ticks));
}

/**
 * Shrink a pattern to the shortest supported length that contains its events.
 *
 * The piano roll remains four bars wide; this only moves the loop/shaded edge.
 * Step-row array lengths follow the wire contract when the loop gets shorter.
 */
export function withContentLength(pattern: PatternDto): PatternDto {
  const needed = pattern.channels.reduce((end, clip) => {
    const noteEnd = clip.notes.reduce(
      (latest, note) => Math.max(latest, Math.round(note.start) + Math.round(note.length)),
      0,
    );
    const stepEnd = clip.steps?.steps.reduce(
      (latest, cell, index) => (cell.on ? Math.max(latest, (index + 1) * TICKS_PER_STEP) : latest),
      0,
    ) ?? 0;
    return Math.max(end, noteEnd, stepEnd);
  }, 0);
  const steps = lengthForTicks(needed);
  if (steps >= pattern.steps) return pattern;
  return {
    ...pattern,
    steps,
    channels: pattern.channels.map((clip) =>
      clip.steps
        ? { ...clip, steps: { ...clip.steps, steps: clip.steps.steps.slice(0, steps) } }
        : clip,
    ),
  };
}

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
  // The pattern grows to whatever was written before anything is clamped to
  // it: a note drawn in bar three of a one-bar pattern is a longer pattern,
  // not a note to throw away. Every roll gesture goes through here -- draw,
  // paint, drag, resize, paste, the note operations -- so all of them extend.
  const needed = notes.reduce(
    (end, note) => Math.max(end, Math.round(note.start) + Math.round(note.length)),
    0,
  );
  const grown = withGrownLength(pattern, needed);
  const clean: MelodicNoteDto[] = [];
  for (const note of notes) {
    const fitted = clampNote(note, grown);
    if (fitted) clean.push(fitted);
    if (clean.length >= MAX_NOTES) break;
  }
  const edited = withChannel(grown, channel, fallbackKey, (clip) => ({ ...clip, notes: clean }));
  return withContentLength(edited);
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

/**
 * Merge selected notes that share a pitch and touch or overlap in time.
 *
 * FL's Ctrl+G. Notes that do not touch, and notes on other keys, stay as they
 * are. Returns `all` by reference when nothing actually joins, so a gesture
 * that finds no seam is free.
 */
export function glueNotes(
  all: readonly MelodicNoteDto[],
  indices: readonly number[],
): MelodicNoteDto[] {
  if (indices.length < 2) return all as MelodicNoteDto[];
  const drop = new Set(indices);
  const picked = indices
    .map((index) => all[index])
    .filter((note): note is MelodicNoteDto => note !== undefined)
    .map((note) => ({ ...note }))
    .sort((a, b) => a.key - b.key || a.start - b.start);
  const merged: MelodicNoteDto[] = [];
  for (const note of picked) {
    const last = merged[merged.length - 1];
    if (last && last.key === note.key && note.start <= last.start + last.length) {
      const end = Math.max(last.start + last.length, note.start + note.length);
      last.length = end - last.start;
      last.velocity = Math.max(last.velocity, note.velocity);
    } else {
      merged.push(note);
    }
  }
  if (merged.length === picked.length) return all as MelodicNoteDto[];
  return [...all.filter((_, index) => !drop.has(index)), ...merged];
}

/** Split `note` at `tick`, or return it unchanged when the cut is not inside. */
export function splitNoteAt(note: MelodicNoteDto, tick: number): MelodicNoteDto[] {
  if (tick <= note.start || tick >= note.start + note.length) return [note];
  return [
    { ...note, length: tick - note.start },
    { ...note, start: tick, length: note.start + note.length - tick },
  ];
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
