/**
 * Where a gesture should take the musician, as a pure function of what they
 * clicked and what that object contains.
 *
 * The shell's views are exclusive (playlist / rack / mixer / roll), so the
 * cheap way to make them feel connected is this map: click selects or paints,
 * double-click opens the editor that object belongs in. Nothing here talks to
 * the engine or Solid; `App` and the views apply the answer.
 */

/** Hit and note counts from a `PatternSummaryDto`. */
export interface PatternCounts {
  hitCount: number;
  noteCount: number;
}

/**
 * Where the pattern was double-clicked from.
 *
 * A rack tab is already the rack, so that surface always opens the roll.
 * Playlist clips, the picker and the top-bar chip follow Automatic: roll when
 * there are notes, rack when there are only steps (or nothing), Alt for rack.
 */
export type PatternNavSurface = 'clip' | 'picker' | 'tab' | 'menu';

export type PatternEditor = 'rack' | 'roll';

export function patternEditorTarget(
  counts: PatternCounts,
  opts: { altKey: boolean; surface: PatternNavSurface },
): PatternEditor {
  if (opts.surface === 'tab') return 'roll';
  if (opts.altKey) return 'rack';
  return counts.noteCount > 0 ? 'roll' : 'rack';
}

/** Whether a picker gesture is allowed to leave the current main view. */
export function pickerChangesView(gesture: 'click' | 'dblclick'): boolean {
  return gesture === 'dblclick';
}

/** Tab's playlist ↔ rack toggle. Mixer and settings fall back to the playlist. */
export function tabCounterpart(
  current: 'playlist' | 'rack' | 'mixer' | 'settings',
): 'playlist' | 'rack' {
  return current === 'playlist' ? 'rack' : 'playlist';
}

/** The pattern F7 should activate when a playlist clip is selected, else none. */
export function f7PatternId(
  view: string,
  selected: ReadonlyArray<{ kind: string; patternId?: number }>,
): number | null {
  if (view !== 'playlist') return null;
  for (const clip of selected) {
    if (clip.kind === 'pattern' && clip.patternId !== undefined) return clip.patternId;
  }
  return null;
}

/** The rack channel the piano roll should open on. */
export function rollChannelFromStrip(
  strip: { kind: string; index?: number },
  mixerSelected: number,
): number {
  return strip.kind === 'track' && strip.index !== undefined ? strip.index : mixerSelected;
}

export function mixerStripDestination(kind: 'track' | 'bus' | 'master'): 'rack' | null {
  return kind === 'track' ? 'rack' : null;
}

export function mixerSlotOpensEditor(kind: 'fx' | 'add' | 'blank' | 'more'): boolean {
  return kind === 'fx';
}
