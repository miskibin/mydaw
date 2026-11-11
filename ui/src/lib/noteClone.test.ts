import { describe, expect, it } from 'vitest';
import type { Note } from '../api/types';
import { cloneNotes } from './noteClone';

const N: Note[] = [
  { ch: 1, step: 0, len: 2, p: 60 },
  { ch: 1, step: 4, len: 1, p: 62, v: 0.5 },
  { ch: 2, step: 8, len: 4, p: 64 },
];

describe('cloneNotes', () => {
  it('appends copies in the given order and returns their indices', () => {
    const r = cloneNotes(N, [1, 0]);
    expect(r.notes.slice(0, 3)).toEqual(N);
    expect(r.notes.slice(3)).toEqual([N[1], N[0]]);
    expect(r.idx).toEqual([3, 4]);
  });
  it('copies are new objects (moving them leaves the originals alone)', () => {
    const r = cloneNotes(N, [0]);
    expect(r.notes[3]).not.toBe(N[0]);
  });
  it('ignores indices that do not exist', () => {
    const r = cloneNotes(N, [2, 7, -1]);
    expect(r.notes).toHaveLength(4);
    expect(r.idx).toEqual([3]);
  });
});
