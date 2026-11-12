// Clips that overlap on one playlist track. Overlapping is allowed (layered patterns, audio
// crossfades), but a clip lying exactly under another one is invisible: a duplicate pressed
// once too often doubles the part, and dragging the top clip away reveals it by surprise.
import type { Clip, Id } from '../api/types';

const EPS = 1e-6;

export interface LaneOverlaps {
  /** Clip id → how many clips it hides completely (drawn under it and within its span). */
  covers: Map<Id, number>;
  /** Spans where two or more clips of one track play at once: track index and beats. */
  spans: { track: number; a: number; b: number }[];
}

/**
 * Overlaps per track. Clips are drawn in array order, so a later clip is on top of an earlier
 * one (and is the one a click picks). A hidden clip counts for the topmost clip that covers it.
 * Sorted sweep per track: O(n log n) for an arrangement whose clips follow each other; a long clip
 * spanning many others adds a backward scan for each clip it may cover.
 */
export function laneOverlaps(clips: readonly Clip[]): LaneOverlaps {
  const covers = new Map<Id, number>();
  const spans: LaneOverlaps['spans'] = [];
  const byTrack = new Map<number, { c: Clip; z: number }[]>();
  clips.forEach((c, z) => {
    if (!(c.len > 0)) return;
    let l = byTrack.get(c.track); if (!l) byTrack.set(c.track, l = []);
    l.push({ c, z });
  });
  for (const [track, list] of byTrack) {
    if (list.length < 2) continue;
    // ---- spans where 2+ clips sound together (touching ends do not overlap)
    const ev: [number, number][] = [];
    for (const { c } of list) { ev.push([c.start, 1]); ev.push([c.start + c.len, -1]); }
    ev.sort((x, y) => x[0] - y[0] || x[1] - y[1]); // ends first at equal beats
    let depth = 0, from = 0;
    for (const [at, d] of ev) {
      const was = depth; depth += d;
      if (was < 2 && depth >= 2) from = at;
      else if (was >= 2 && depth < 2 && at - from > EPS) {
        const last = spans[spans.length - 1];
        if (last && last.track === track && Math.abs(last.b - from) <= EPS) last.b = at; else spans.push({ track, a: from, b: at });
      }
    }
    // ---- fully hidden clips: covered by a clip drawn later that starts no later and ends no earlier
    list.sort((x, y) => x.c.start - y.c.start || y.c.len - x.c.len);
    let reach = -Infinity; // the latest end among the clips before i: below hEnd nothing earlier covers i
    for (let i = 0; i < list.length; i++) {
      const h = list[i], hEnd = h.c.start + h.c.len;
      let top: { c: Clip; z: number } | null = null;
      const consider = (o: { c: Clip; z: number }) => {
        if (o.z > h.z && o.c.start + o.c.len >= hEnd - EPS && (!top || o.z > top.z)) top = o;
      };
      if (reach >= hEnd - EPS) for (let j = i - 1; j >= 0; j--) consider(list[j]);
      // equal starts sorted after h (no longer than h: only one of equal length covers it)
      for (let j = i + 1; j < list.length && list[j].c.start - h.c.start <= EPS; j++) consider(list[j]);
      reach = Math.max(reach, hEnd);
      const t = top as { c: Clip; z: number } | null;
      if (t) covers.set(t.c.id, (covers.get(t.c.id) ?? 0) + 1);
    }
  }
  return { covers, spans };
}
