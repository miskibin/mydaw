// Pure builders for playlist edits. They return new clips / ops; nothing here touches the store.
import { TOTAL_BEATS, VOL_UNITY, type Clip, type Op, type Project, type Track } from '../api/types';
import { clamp } from './format';

export const TOTAL = TOTAL_BEATS;

/** Clamp a group move so every clip stays on the timeline and on an existing track. */
export function clampMove(orig: Clip[], d: number, dt: number, nTracks: number): [number, number] {
  if (!orig.length) return [0, 0];
  let minS = Infinity, maxE = -Infinity, minT = Infinity, maxT = -Infinity;
  for (const c of orig) {
    minS = Math.min(minS, c.start); maxE = Math.max(maxE, c.start + c.len);
    minT = Math.min(minT, c.track); maxT = Math.max(maxT, c.track);
  }
  return [clamp(d, -minS, TOTAL - maxE), clamp(dt, -minT, nTracks - 1 - maxT)];
}

export function moveClips(orig: Clip[], d: number, dt: number): Clip[] {
  return orig.map(c => ({ ...c, start: c.start + d, track: c.track + dt }));
}

/** Split `c` at beat `b`; null when `b` is not strictly inside. */
export function sliceClip(c: Clip, b: number, newId: number): [Clip, Clip] | null {
  if (b <= c.start + 1e-6 || b >= c.start + c.len - 1e-6) return null;
  const a = { ...c, len: b - c.start, fadeOut: 0 };
  const z = { ...c, id: newId, start: b, len: c.start + c.len - b, offset: c.offset + (b - c.start), fadeIn: 0 };
  return [a, z];
}

/** Span to shift a duplicate by: the selection length rounded up to the snap unit. */
export function duplicateSpan(cs: Clip[], unit: number): number {
  const a = Math.min(...cs.map(c => c.start)), b = Math.max(...cs.map(c => c.start + c.len));
  const u = unit || 1;
  return Math.ceil((b - a) / u - 1e-6) * u;
}

export function pasteClips(cb: Clip[], beat: number, track: number | null, nTracks: number, alloc: () => number): Clip[] {
  const a = Math.min(...cb.map(c => c.start)), t0 = Math.min(...cb.map(c => c.track));
  const dt = track == null ? 0 : track - t0;
  return cb.map(c => ({
    ...c, id: alloc(),
    start: clamp(c.start - a + beat, 0, TOTAL - c.len),
    track: clamp(c.track + dt, 0, nTracks - 1),
  }));
}

/** Resize from an edge. `srcLen` is the source length in beats (audio only; Infinity for patterns). */
export function resizeClip(o: Clip, edge: 'l' | 'r', target: number, minL: number, srcLen: number): Clip {
  const n = { ...o };
  if (edge === 'r') {
    let len = Math.max(minL, target - o.start);
    if (o.kind === 'audio' || o.kind === 'comp') len = Math.min(len, srcLen - o.offset);
    n.len = Math.min(len, TOTAL - o.start);
  } else {
    const lo = o.kind === 'audio' || o.kind === 'comp' ? o.start - o.offset : 0;
    const ns = clamp(target, Math.max(0, lo), o.start + o.len - minL);
    n.start = ns; n.offset = o.offset + (ns - o.start); n.len = o.len - (ns - o.start);
  }
  return n;
}

export function newTrack(id: number, n: number): Track {
  return { id, name: 'Track ' + (n + 1), color: '#8b93a1', muted: false, solo: false, arm: false, vol: VOL_UNITY, h: 56, insert: 0 };
}

/**
 * Insert a lane at index `i`. The op set has no track reorder, so the lane contents are
 * shifted down instead: every track from `i` on takes the data of the one above (ids
 * stay put), a new track is appended at the end, and clips on lanes ≥ i move down one.
 */
export function insertTrackOps(p: Project, i: number, newId: number): Op[] {
  const ts = p.tracks, n = ts.length, ops: Op[] = [];
  const contents = [...ts.slice(0, i), newTrack(0, n), ...ts.slice(i)];
  // existing ids keep their slot; the last slot is the new id
  for (let j = i; j < n; j++) {
    const src = contents[j];
    ops.push({ op: 'putTrack', track: { ...src, id: ts[j].id, name: j === i ? 'Track ' + (n + 1) : src.name } });
  }
  ops.push({ op: 'putTrack', track: { ...contents[n], id: newId } });
  const moved = p.clips.filter(c => c.track >= i).map(c => ({ ...c, track: c.track + 1 }));
  // putClips validates the track index against the *current* track count, so tracks go first.
  if (moved.length) ops.push({ op: 'putClips', clips: moved });
  return ops;
}

/** Ops that toggle mute for the given clips to `val`. */
export function muteOps(cs: Clip[], val: boolean): Op[] {
  const ch = cs.filter(c => c.muted !== val).map(c => ({ ...c, muted: val }));
  return ch.length ? [{ op: 'putClips', clips: ch }] : [];
}
