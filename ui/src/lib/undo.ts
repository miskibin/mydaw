/** Native sessions mirror the shared Rust history. The browser mock replays local inverse commands. */

import type { Backend } from '../api/backend';
import type {
  ClipDto,
  MixerState,
  PatternDto,
  PlaylistDto,
  PlaylistTrackDto,
  SampleEditDto,
} from '../api/types';

/**
 * The shell's mixer funnel: run a command that answers with the whole mixer and
 * adopt what came back. `AppState.mixerCommand` is the only implementation.
 */
export type MixerRunner = (run: (backend: Backend) => Promise<MixerState>) => Promise<void>;

/** How many gestures the stack remembers. */
export const UNDO_DEPTH = 50;

/** One reversible gesture. */
export interface UndoEntry {
  /** What the status bar calls it: "Undo Draw notes". */
  readonly label: string;
  undo(): Promise<void>;
  redo(): Promise<void>;
}

/**
 * A bounded undo/redo stack.
 *
 * Deliberately dumb: it knows nothing about patterns or clips, only about
 * pairs of closures, so what is undoable is decided at each edit site rather
 * than by a switch here that has to be kept in step with the views.
 */
export class UndoStack {
  private shared?: { undoLabel: string | null; redoLabel: string | null };
  setShared(history: { undoLabel: string | null; redoLabel: string | null }): void {
    this.shared = history;
    this.version += 1;
  }
  private readonly past: UndoEntry[] = [];
  private readonly future: UndoEntry[] = [];
  /** Bumped on every mutation so a Solid signal can track the pair of labels. */
  private version = 0;

  /** Record a gesture that has **already been applied**. Drops the redo branch. */
  push(entry: UndoEntry): void {
    if (this.shared) return;
    this.past.push(entry);
    if (this.past.length > UNDO_DEPTH) this.past.shift();
    this.future.length = 0;
    this.version += 1;
  }

  /** Throw the history away, e.g. after an edit no inverse could survive. */
  clear(): void {
    this.past.length = 0;
    this.future.length = 0;
    this.version += 1;
  }

  get revision(): number {
    return this.version;
  }

  undoLabel(): string | null {
    if (this.shared) return this.shared.undoLabel;
    return this.past.at(-1)?.label ?? null;
  }

  redoLabel(): string | null {
    if (this.shared) return this.shared.redoLabel;
    return this.future.at(-1)?.label ?? null;
  }

  /**
   * Undo the last gesture, resolving with its label.
   *
   * A failed inverse is **not** put back on the stack and clears the history:
   * the engine is then in a state no entry below describes, and offering
   * another undo would compound the disagreement.
   */
  async undo(): Promise<string | null> {
    const entry = this.past.pop();
    this.version += 1;
    if (!entry) return null;
    try {
      await entry.undo();
    } catch (error) {
      this.clear();
      throw error;
    }
    this.future.push(entry);
    this.version += 1;
    return entry.label;
  }

  async redo(): Promise<string | null> {
    const entry = this.future.pop();
    this.version += 1;
    if (!entry) return null;
    try {
      await entry.redo();
    } catch (error) {
      this.clear();
      throw error;
    }
    this.past.push(entry);
    this.version += 1;
    return entry.label;
  }
}

// ---------------------------------------------------------------------------
// Patterns
// ---------------------------------------------------------------------------

/**
 * An entry that restores one pattern's whole content.
 *
 * A pattern is a few kilobytes of JSON and `set_pattern` is a whole-pattern
 * replacement, so the inverse of *any* content edit -- one step, a marquee of
 * forty notes, a Humanize -- is one command. There is no diff to get wrong.
 */
export function patternEntry(
  backend: Backend,
  label: string,
  before: PatternDto,
  after: PatternDto,
): UndoEntry {
  return {
    label,
    undo: async () => {
      await backend.setPattern(before);
    },
    redo: async () => {
      await backend.setPattern(after);
    },
  };
}

// ---------------------------------------------------------------------------
// Destructive sample edits
// ---------------------------------------------------------------------------

/**
 * An entry that restores the take a destructive edit replaced.
 *
 * The one place the table at the top of this file gains a row. Reverse,
 * Normalize, Trim and Gain are **copy on write** in the engine: they write a
 * new file, load it as a new source and repoint the channel at it, leaving the
 * original loaded. So the inverse is not "decode the old file again", it is one
 * `setTrackAudioSource` back to the pointer and region the channel had — the
 * same shape as every other entry here, and cheap enough that Ctrl+Z after a
 * ten-minute take is instant.
 *
 * The clip shift is negated on the way back: a Trim moved this lane's clips
 * `-start` frames into the shortened file, and undoing it moves them back.
 *
 * `run` is the shell's `mixerCommand` funnel rather than the raw backend: the
 * other entries here replay *sequencer* commands and `replay` refetches the
 * pattern for them, but nothing refetches the mixer, so an entry that swapped a
 * channel's file behind the UI's back would undo the engine and leave the
 * window drawing the edited waveform.
 */
export function sampleEditEntry(run: MixerRunner, edit: SampleEditDto): UndoEntry {
  const repoint = (sourceId: number, start: number, end: number, shift: number) => () =>
    run((backend) => backend.setTrackAudioSource(edit.track, sourceId, start, end, shift));
  return {
    label: edit.op,
    undo: repoint(
      edit.previousSourceId,
      edit.previousStartFrames,
      edit.previousEndFrames,
      -edit.clipOffsetShiftFrames,
    ),
    redo: repoint(
      edit.source.id,
      edit.startFrames,
      edit.endFrames,
      edit.clipOffsetShiftFrames,
    ),
  };
}

// ---------------------------------------------------------------------------
// The playlist
// ---------------------------------------------------------------------------

/**
 * What an arrangement looks like, with the ids left out of the clips.
 *
 * Ids are left out because restoring a clip **creates a new one**: the engine
 * allocates ids and never reuses them, so an inverse that addressed the old id
 * would work once and then point at nothing. Matching by value instead means
 * undo, redo, undo lands on the same arrangement every time even though the ids
 * underneath have all changed.
 */
export interface PlaylistSnapshot {
  tracks: readonly PlaylistTrackDto[];
  clips: readonly ClipDto[];
}

/** Take one. Cloned, so later edits cannot reach back into it. */
export function snapshotPlaylist(playlist: PlaylistDto): PlaylistSnapshot {
  return {
    tracks: playlist.tracks.map((track) => ({ ...track })),
    clips: playlist.clips.map((clip) => ({ ...clip })),
  };
}

/** Everything about a clip except which one it happens to be. */
function clipKey(clip: ClipDto): string {
  return [
    clip.kind,
    clip.track,
    clip.start,
    clip.length,
    clip.patternId ?? '-',
    clip.sourceId ?? '-',
    clip.offsetFrames ?? 0,
    clip.gainDb ?? 0,
  ].join('|');
}

/**
 * Drive the engine from wherever it is now to `target`.
 *
 * Four passes, in an order that cannot lose work: surplus clips go first (so a
 * lane about to be removed is already empty and the removal is not silently
 * taking clips with it), then the lane count, then lane names, mutes and solos, then
 * the clips that are missing. An edit that moved one clip therefore costs two
 * commands, not a rebuild.
 */
export async function restorePlaylist(
  backend: Backend,
  target: PlaylistSnapshot,
): Promise<void> {
  const live = (await backend.getPattern()).playlist;

  // Pass 1: every clip that is not in the target, by value.
  const wanted = new Map<string, number>();
  for (const clip of target.clips) wanted.set(clipKey(clip), (wanted.get(clipKey(clip)) ?? 0) + 1);
  const keep = new Set<number>();
  for (const clip of live.clips) {
    const key = clipKey(clip);
    const left = wanted.get(key) ?? 0;
    if (left > 0) {
      wanted.set(key, left - 1);
      keep.add(clip.id);
    }
  }
  for (const clip of live.clips) {
    if (!keep.has(clip.id)) await backend.removeClip(clip.id);
  }

  // Pass 2: the lane count. Removing from the end, because a lane's index is
  // its identity and taking one from the middle would shift every lane below.
  let lanes = live.tracks.length;
  while (lanes > target.tracks.length) {
    await backend.removePlaylistTrack(lanes - 1);
    lanes -= 1;
  }
  while (lanes < target.tracks.length) {
    await backend.addPlaylistTrack(target.tracks[lanes]?.name);
    lanes += 1;
  }

  // Pass 3: names, mutes and solos, which are cheap and idempotent.
  const after = (await backend.getPattern()).playlist;
  for (const [index, want] of target.tracks.entries()) {
    const have = after.tracks[index];
    if (!have) continue;
    if (have.name !== want.name) await backend.renamePlaylistTrack(index, want.name);
    if (have.muted !== want.muted) await backend.setPlaylistTrackMute(index, want.muted);
    if (have.soloed !== want.soloed) await backend.setPlaylistTrackSolo(index, want.soloed);
  }

  // Pass 4: whatever the target has that the engine now does not.
  const remaining = new Map<string, number>();
  for (const clip of target.clips) {
    const key = clipKey(clip);
    remaining.set(key, (remaining.get(key) ?? 0) + 1);
  }
  const current = (await backend.getPattern()).playlist;
  for (const clip of current.clips) {
    const key = clipKey(clip);
    const left = remaining.get(key) ?? 0;
    if (left > 0) remaining.set(key, left - 1);
  }
  for (const clip of target.clips) {
    const key = clipKey(clip);
    const left = remaining.get(key) ?? 0;
    if (left <= 0) continue;
    remaining.set(key, left - 1);
    if (clip.kind === 'pattern' && clip.patternId !== undefined) {
      await backend.addPatternClip(clip.patternId, clip.track, clip.start, clip.length);
    } else if (clip.sourceId !== undefined) {
      await backend.addAudioClip(
        clip.sourceId,
        clip.track,
        clip.start,
        clip.length,
        clip.offsetFrames ?? 0,
        clip.gainDb ?? 0,
      );
    }
  }
}

/** An entry that restores the whole arrangement either way. */
export function playlistEntry(
  backend: Backend,
  label: string,
  before: PlaylistSnapshot,
  after: PlaylistSnapshot,
): UndoEntry {
  return {
    label,
    undo: () => restorePlaylist(backend, before),
    redo: () => restorePlaylist(backend, after),
  };
}
