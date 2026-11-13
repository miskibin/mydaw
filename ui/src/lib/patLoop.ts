// The pattern loop preview's range and position, as the engine plays it (rt.rs `loop_span` /
// `schedule_pattern`): the mock follows it and the Piano roll's ruler draws it.

/** A part of a pattern, in beats from its start. */
export interface BeatRange { a: number; b: number }

/** Shortest range that loops on its own (beats); a shorter one loops the whole pattern. */
export const MIN_LOOP = 1 / 64;

/** The part of a pattern `len` beats long its loop plays: `r` kept inside it, else all of it. */
export function loopSpan(r: BeatRange | null | undefined, len: number): BeatRange {
  if (!r || !Number.isFinite(r.a) || !Number.isFinite(r.b)) return { a: 0, b: len };
  const a = Math.min(Math.max(Math.min(r.a, r.b), 0), len), b = Math.min(Math.max(Math.max(r.a, r.b), 0), len);
  return b - a >= MIN_LOOP ? { a, b } : { a: 0, b: len };
}

/** Where the loop is `elapsed` beats after it was at `from`: a position outside the range goes to its start, then it wraps. */
export function loopPos(from: number, elapsed: number, r: BeatRange | null | undefined, len: number): number {
  if (len <= 0) return 0;
  const { a, b } = loopSpan(r, len), span = b - a;
  const p = from >= a && from < b ? from : a;
  return a + (((p - a + elapsed) % span) + span) % span;
}

/** What `clipOfPattern` needs of a playlist clip. */
interface PatClip { kind: string; ref: number; start: number; len: number; offset: number; muted: boolean }

/** The playlist clip of pattern `pid` a song position goes to: the one playing at `at` (song beats), else the first after it, else the first. Muted clips count (they show where the pattern is). */
export function clipOfPattern<C extends PatClip>(clips: readonly C[], pid: number, at: number): C | null {
  let after: C | null = null, first: C | null = null;
  for (const c of clips) {
    if (c.kind !== 'pat' || c.ref !== pid) continue;
    if (at >= c.start && at < c.start + c.len) return c;
    if (!first || c.start < first.start) first = c;
    if (c.start >= at && (!after || c.start < after.start)) after = c;
  }
  return after ?? first;
}

/** Song beat where clip `c` plays pattern beat `pb` (the pattern is `len` beats; a long clip repeats it), in the pass nearest `at`; null when the clip never plays that beat. */
export function songBeatIn(c: PatClip, len: number, pb: number, at: number): number | null {
  if (len <= 0 || c.len <= 0) return null;
  const end = c.start + c.len, ref = Math.min(Math.max(at, c.start), end - 1e-9);
  let s = c.start - c.offset + Math.floor((ref - c.start + c.offset) / len) * len + pb;
  // the nearest pass to `at` that the clip really plays
  if (s - at > len / 2 && s - len >= c.start) s -= len;
  else if (at - s > len / 2 && s + len < end) s += len;
  if (s < c.start) s += len;
  if (s >= end) s -= len;
  return s >= c.start - 1e-9 && s < end ? Math.max(c.start, s) : null;
}

/** Pattern beat clip `c` plays at song beat `s` (null outside the clip). */
export function patBeatAt(c: PatClip, len: number, s: number): number | null {
  if (len <= 0 || s < c.start || s >= c.start + c.len) return null;
  return (((s - c.start + c.offset) % len) + len) % len;
}
