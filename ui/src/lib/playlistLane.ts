import type { PlaylistTrackDto } from '../api/types';

/**
 * Whether playlist lane `index` is silent right now, solo included.
 *
 * Mirrors `Playlist::lane_silenced` in the sequencer: with nothing soloed a
 * lane is silent only if it is muted; with anything soloed a lane is silent
 * unless it is itself soloed. A soloed muted lane stays audible.
 */
export function laneSilenced(
  tracks: readonly PlaylistTrackDto[],
  index: number,
): boolean {
  const lane = tracks[index];
  if (!lane) return true;
  if (tracks.some((entry) => entry.soloed)) return !lane.soloed;
  return lane.muted;
}
