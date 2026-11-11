import { describe, expect, it } from 'vitest';
import type { Channel, Note, Project } from '../api/types';
import { applyAll } from './applyOps';
import {
  builtinChannel, cellsBetween, contentLen, duplicateRight, fillEvery, isRollRow, isStepOn, migrateProject, moveNotes, noteAt, resizeNotes,
  rotate, setKeyAt, setStep, setVelAt, snapStep, swingOffset, transpose,
} from './pattern';

const KICK = 1000, BASS = 1003;
const bass: Channel = { ...builtinChannel(BASS, 'bass') };
const steps = (notes: Note[], ch: number) => notes.filter(n => n.ch === ch).map(n => n.step).sort((a, b) => a - b);

describe('step sequencer', () => {
  it('toggles steps without duplicates', () => {
    let n: Note[] = [];
    n = setStep(n, KICK, 4, true);
    n = setStep(n, KICK, 4, true);
    expect(n).toEqual([{ ch: KICK, step: 4, len: 1 }]);
    expect(isStepOn(n, KICK, 4)).toBe(true);
    n = setStep(n, KICK, 4, false);
    expect(n).toEqual([]);
  });
  it('paint-drag visits every cell between pointer samples, in order', () => {
    expect(cellsBetween(0, 2, 0, 6)).toEqual([[0, 2], [0, 3], [0, 4], [0, 5], [0, 6]]);
    expect(cellsBetween(0, 6, 0, 3)).toEqual([[0, 6], [0, 5], [0, 4], [0, 3]]);
    expect(cellsBetween(1, 1, 1, 1)).toEqual([[1, 1]]);
    // diagonal across rows: no cell skipped on the longer axis
    const d = cellsBetween(0, 0, 2, 8);
    expect(d.length).toBe(9);
    expect(d[0]).toEqual([0, 0]); expect(d[8]).toEqual([2, 8]);
    // a drag painting "on" from the first cell's opposite state
    let n: Note[] = [{ ch: KICK, step: 3, len: 1 }];
    for (const [, s] of cellsBetween(0, 0, 0, 5)) n = setStep(n, KICK, s, true);
    expect(steps(n, KICK)).toEqual([0, 1, 2, 3, 4, 5]);
  });
  it('fills every N steps, replacing the channel and keeping the others', () => {
    const n: Note[] = [{ ch: KICK, step: 3, len: 1 }, { ch: BASS, step: 0, len: 4, p: 36 }, { ch: KICK, step: 20, len: 1 }];
    const f = fillEvery(n, KICK, 16, 4);
    expect(steps(f, KICK)).toEqual([0, 4, 8, 12]);
    expect(f.filter(x => x.ch === BASS)).toHaveLength(1);
    expect(steps(fillEvery([], KICK, 32, 8), KICK)).toEqual([0, 8, 16, 24]);
  });
  it('rotates with wrap-around', () => {
    const n: Note[] = [{ ch: KICK, step: 0, len: 1 }, { ch: KICK, step: 15, len: 1 }, { ch: BASS, step: 15, len: 1 }];
    expect(steps(rotate(n, KICK, 16, 1), KICK)).toEqual([0, 1]);
    expect(steps(rotate(n, KICK, 16, -1), KICK)).toEqual([14, 15]);
    expect(rotate(n, KICK, 16, 1).find(x => x.ch === BASS)!.step).toBe(15);
  });
  it('sets velocity per cell and tells plain steps from melodic rows', () => {
    const n = setVelAt([{ ch: KICK, step: 2, len: 1 }, { ch: KICK, step: 3, len: 1 }], KICK, 2, 0.456);
    expect(n.map(x => x.v)).toEqual([0.46, undefined]);
    expect(isRollRow([{ ch: BASS, step: 0, len: 1 }], bass)).toBe(false);
    expect(isRollRow([{ ch: BASS, step: 0, len: 1, p: 36 }], bass)).toBe(false);
    expect(isRollRow([{ ch: BASS, step: 0, len: 3, p: 36 }], bass)).toBe(true);
    // a step with its own key (graph editor) is still a step; a chord or an off-grid note is not
    expect(isRollRow([{ ch: BASS, step: 0, len: 1, p: 40 }], bass)).toBe(false);
    expect(isRollRow([{ ch: BASS, step: 0, len: 1, p: 40 }, { ch: BASS, step: 0, len: 1, p: 43 }], bass)).toBe(true);
    expect(isRollRow([{ ch: BASS, step: 0.5, len: 1 }], bass)).toBe(true);
  });
  it('sets a step key from the graph editor (the root clears it)', () => {
    const n: Note[] = [{ ch: BASS, step: 2, len: 1 }, { ch: BASS, step: 3, len: 1, p: 40 }];
    expect(setKeyAt(n, bass, 2, 43)[0]).toEqual({ ch: BASS, step: 2, len: 1, p: 43 });
    expect(setKeyAt(n, bass, 3, bass.root)[1]).toEqual({ ch: BASS, step: 3, len: 1 });
    expect(setKeyAt(n, bass, 3, 200)[1].p).toBe(127);
  });
  it('a pattern is as long as its notes: the last end rounded up to a bar, at least one', () => {
    expect(contentLen([])).toBe(4);
    expect(contentLen([{ ch: KICK, step: 15, len: 1 }])).toBe(4);
    expect(contentLen([{ ch: KICK, step: 15, len: 1.5 }])).toBe(8);
    expect(contentLen([{ ch: KICK, step: 40, len: 4 }])).toBe(12);
  });
  it('swing moves only odd 16ths, half a step at 100 %', () => {
    expect([0, 1, 2, 3, 4, 5].map(s => swingOffset(s, 1))).toEqual([0, 0.5, 0, 0.5, 0, 0.5]);
    expect(swingOffset(3, 0.5)).toBe(0.25);
    expect(swingOffset(3.25, 1)).toBe(0);
  });
});

describe('piano roll math', () => {
  const line: Note[] = [{ ch: BASS, step: 0, len: 3, p: 33 }, { ch: BASS, step: 4, len: 2, p: 40 }, { ch: KICK, step: 0, len: 1 }];
  it('snaps to the grid (floor for new notes, free with unit 0)', () => {
    expect(snapStep(5.4, 1)).toBe(5);
    expect(snapStep(5.6, 2)).toBe(6);
    expect(snapStep(7.9, 4, true)).toBe(4);
    expect(snapStep(3.3, 0)).toBeCloseTo(3.3125);
  });
  it('moves a selection keeping relative positions, snapped and clamped', () => {
    const m = moveNotes(line, [0, 1], 2.4, 3, 1, bass);
    expect(m[0]).toMatchObject({ step: 2, p: 36 }); expect(m[1]).toMatchObject({ step: 6, p: 43 });
    expect(m[2]).toBe(line[2]);
    expect(moveNotes(line, [1], -10, 0, 1, bass)[1].step).toBe(0);
    expect(moveNotes(line, [0], 0, 200, 1, bass)[0].p).toBe(127);
    // free (Alt): no rounding
    expect(moveNotes(line, [1], 0.3, 0, 0, bass)[1].step).toBeCloseTo(4.3125);
  });
  it('resizes with a snapped end and a minimum of one grid unit', () => {
    expect(resizeNotes(line, [0], 1.6, 1)[0].len).toBe(5);
    expect(resizeNotes(line, [0], -10, 2)[0].len).toBe(2);
    expect(resizeNotes(line, [0], -10, 0)[0].len).toBe(0.25);
    expect(resizeNotes(line, [0, 1], 2, 2).slice(0, 2).map(n => n.len)).toEqual([6, 4]);
  });
  it('duplicates the selection right after it, rounded up to beats', () => {
    const d = duplicateRight(line, [0, 1]);
    expect(d.idx).toEqual([3, 4]);
    expect(d.notes.slice(3).map(n => n.step)).toEqual([8, 12]);
    const one = duplicateRight(line, [0]);
    expect(one.notes[3].step).toBe(4);
    expect(duplicateRight(line, []).notes).toBe(line);
  });
  it('transposes as a group within 0..127 and finds notes under the pointer', () => {
    expect(transpose(line, [0, 1], 12, bass).slice(0, 2).map(n => n.p)).toEqual([45, 52]);
    expect(transpose(line, [0, 1], 100, bass).slice(0, 2).map(n => n.p)).toEqual([120, 127]);
    expect(noteAt(line, BASS, bass, 1.5, 33)).toBe(0);
    expect(noteAt(line, BASS, bass, 3.5, 33)).toBe(-1);
    expect(noteAt(line, BASS, bass, 5, 40)).toBe(1);
  });
});

describe('mock engine ops (mirror of ops.rs)', () => {
  const base = (): Project => {
    const p: Project = { version: 2, bpm: 128, tracks: [{ id: 1, name: 'T', color: '#fff', muted: false, solo: false, arm: false, vol: 0.79, h: 56, insert: 1 }],
      inserts: [{ id: 0, name: 'M', color: '#fff', vol: 0.79, pan: 0, muted: false, solo: false, fx: [] }, { id: 1, name: 'I', color: '#fff', vol: 0.79, pan: 0, muted: false, solo: false, fx: [] }],
      patterns: [], sources: [], clips: [], loop: null, nextId: 1000 };
    migrateProject(p);
    return p;
  };
  it('migrates legacy inst notes and resolves them in putPattern', () => {
    const p = base();
    expect(p.channels!.map(c => c.name)).toEqual(['Kick', 'Clap', 'Hat', 'Bass']);
    const q = applyAll(p, [{ op: 'putPattern', pattern: { id: 5, name: 'x', color: '#fff', len: 4, notes: [{ inst: 'hat', step: 2, len: 1 } as Note] } }]);
    expect(q.patterns[0].notes[0]).toEqual({ ch: 1002, step: 2, len: 1 });
    expect(() => applyAll(p, [{ op: 'putPattern', pattern: { id: 5, name: 'x', color: '#fff', len: 4, notes: [{ ch: 77, step: 0, len: 1 }] } }])).toThrow();
  });
  it('length follows the notes and minLen; whole-pattern clips follow it; removing a channel removes its notes', () => {
    let p = base();
    p = applyAll(p, [{ op: 'putPattern', pattern: { id: 5, name: 'x', color: '#fff', len: 99, notes: [{ ch: 1000, step: 0, len: 1 }, { ch: 1001, step: 4, len: 1 }] } },
      { op: 'putClips', clips: [
        { id: 1, track: 0, start: 0, len: 4, offset: 0, kind: 'pat', ref: 5, muted: false, gain: 0, fadeIn: 0, fadeOut: 0, pan: 0, norm: false, rev: false },
        { id: 2, track: 0, start: 8, len: 16, offset: 0, kind: 'pat', ref: 5, muted: false, gain: 0, fadeIn: 0, fadeOut: 0, pan: 0, norm: false, rev: false }] }]);
    expect([p.patterns[0].len, p.patterns[0].minLen]).toEqual([4, 4]); // a sent len is ignored
    // a note in bar 2 grows it; the clip that shows one whole pass follows
    p = applyAll(p, [{ op: 'putPattern', pattern: { ...p.patterns[0], notes: [...p.patterns[0].notes, { ch: 1000, step: 20, len: 1 }] } }]);
    expect(p.patterns[0].len).toBe(8);
    expect(p.clips.map(c => c.len)).toEqual([8, 16]);
    // removed again: back to one bar — unless an empty bar is kept on purpose (minLen)
    const two = { ...p.patterns[0], notes: p.patterns[0].notes.slice(0, 2) };
    expect(applyAll(p, [{ op: 'putPattern', pattern: two }]).patterns[0].len).toBe(4);
    p = applyAll(p, [{ op: 'putPattern', pattern: { ...two, minLen: 8 } }]);
    expect([p.patterns[0].len, p.clips[0].len]).toEqual([8, 8]);
    expect(() => applyAll(p, [{ op: 'putPattern', pattern: { ...two, notes: [{ ch: 1000, step: 2047, len: 2 }] } }])).toThrow();
    // removePattern takes its clips with it
    expect(applyAll(p, [{ op: 'removePattern', id: 5 }]).clips).toEqual([]);
    p = applyAll(p, [{ op: 'removeChannel', id: 1000 }]);
    expect(p.patterns[0].notes).toEqual([{ ch: 1001, step: 4, len: 1 }]);
    expect(p.channels!.some(c => c.id === 1000)).toBe(false);
  });
});
