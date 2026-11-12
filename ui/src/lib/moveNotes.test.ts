import { describe, expect, it } from 'vitest';
import type { Clip, Note, Op, Pattern, Project, Track } from '../api/types';
import { newTrack } from './clipOps';
import { planMoveNotes } from './moveNotes';
import { fitLen } from './pattern';

const note = (step: number, key: number, ch = 1): Note => ({ step, key, len: 1, ch, v: 0.8 } as Note);
const pat = (id: number, notes: Note[], len = 8, minLen = 4): Pattern => ({ id, name: 'Beat', color: '#fff', len, minLen, notes } as Pattern);
const pclip = (id: number, ref: number, track: number, start: number, len: number): Clip =>
  ({ id, kind: 'pat', ref, track, start, len, offset: 0, muted: false, fadeIn: 0, fadeOut: 0, gain: 1 } as unknown as Clip);
const project = (tracks: number, patterns: Pattern[], clips: Clip[]): Project =>
  ({ tracks: Array.from({ length: tracks }, (_, i) => newTrack(100 + i, i)) as Track[], patterns, clips } as unknown as Project);
const ids = (from = 1000) => { let n = from; return () => n++; };
const putPats = (ops: Op[]) => ops.filter(o => o.op === 'putPattern').map(o => (o as Extract<Op, { op: 'putPattern' }>).pattern);
const putClips = (ops: Op[]) => ops.flatMap(o => (o.op === 'putClips' ? o.clips : []));

describe('moving notes to a new pattern', () => {
  const src = pat(1, [note(0, 36), note(4, 38), note(8, 36), note(28, 42)]);

  it('splits the notes, keeping their steps, and keeps both patterns as long as the source', () => {
    const plan = planMoveNotes(project(2, [src], []), src, [1, 3], 'Beat – Snare', '#f00', ids())!;
    const [kept, made] = putPats(plan.ops);
    expect(kept.id).toBe(1);
    expect(kept.notes.map(n => n.step)).toEqual([0, 8]);
    expect(made.notes.map(n => n.step)).toEqual([4, 28]);
    expect(made).toMatchObject({ id: 1000, name: 'Beat – Snare', color: '#f00', len: 8 });
    // the engine refits lengths from minLen and content: neither may shrink
    expect(fitLen(structuredClone(kept)).len).toBe(8);
    expect(fitLen(structuredClone(made)).len).toBe(8);
    expect(plan.placed).toBe(0);
  });

  it('puts a clip of the new pattern beside every clip of the source, on a free lane below', () => {
    const p = project(4, [src], [
      pclip(10, 1, 0, 0, 8), pclip(11, 1, 0, 8, 8),
      pclip(20, 9, 1, 10, 4), // lane 1 is busy over the second clip only
    ]);
    const plan = planMoveNotes(p, src, [0], 'x', '#0f0', ids())!;
    const cs = putClips(plan.ops);
    expect(cs.map(c => [c.track, c.start, c.len, c.ref])).toEqual([[1, 0, 8, 1000], [2, 8, 8, 1000]]);
    expect(plan.addedTracks).toBe(0);
    // the source clips are untouched
    expect(plan.ops.some(o => o.op === 'removeClips')).toBe(false);
  });

  it('adds a lane when none below is free, and never stacks two new clips', () => {
    const p = project(1, [src], [pclip(10, 1, 0, 0, 8), pclip(11, 1, 0, 4, 8)]);
    const plan = planMoveNotes(p, src, [0], 'x', '#0f0', ids())!;
    const cs = putClips(plan.ops);
    expect(plan.addedTracks).toBe(2);
    expect(cs.map(c => c.track)).toEqual([1, 2]);
    expect(plan.ops.filter(o => o.op === 'putTrack')).toHaveLength(2);
  });

  it('keeps each clip\'s offset, mute and gain', () => {
    const c = { ...pclip(10, 1, 0, 2, 6), offset: 2, muted: true } as Clip;
    const plan = planMoveNotes(project(2, [src], [c]), src, [0], 'x', '#0f0', ids())!;
    expect(putClips(plan.ops)[0]).toMatchObject({ start: 2, len: 6, offset: 2, muted: true, track: 1 });
  });

  it('does nothing without a valid selection', () => {
    expect(planMoveNotes(project(1, [src], []), src, [], 'x', '#0', ids())).toBeNull();
    expect(planMoveNotes(project(1, [src], []), src, [99], 'x', '#0', ids())).toBeNull();
  });
});
