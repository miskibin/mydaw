/**
 * Which insert just appeared on a chain — used to open its editor on add.
 */

import type { InsertDto, InsertTarget, MixerState } from '../api/types';

/** The new insert's id, or `undefined` if the chain did not grow. */
export function addedInsertId(
  beforeIds: ReadonlySet<number>,
  after: readonly { id: number }[],
): number | undefined {
  return after.find((insert) => !beforeIds.has(insert.id))?.id;
}

/** The insert chain on a mixer strip, by the same address `addInsert` uses. */
export function insertsOnStrip(
  mixer: MixerState | null | undefined,
  strip: InsertTarget,
): InsertDto[] {
  if (!mixer) return [];
  if (strip.kind === 'track') return mixer.tracks[strip.index]?.inserts ?? [];
  if (strip.kind === 'bus') return mixer.buses[strip.index]?.inserts ?? [];
  return mixer.master.inserts;
}
