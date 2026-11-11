import { describe, expect, it } from 'vitest';
import type { Channel, Note } from '../api/types';
import {
  applyProps, fitRange, fmtStart, followPage, invertSelection, legato, MAX_STEP, notesOfKey, nudgeVelocity,
  parseKey, parseLen, parseStart, parseVel, quantizeStarts, readProps, wheelNotches,
} from './rollEdit';

const CH = { id: 1, root: 60 } as Channel;
const N: Note[] = [
  { ch: 1, step: 0.3, len: 2, p: 60 },
  { ch: 1, step: 3.6, len: 1, p: 64, v: 0.5 },
  { ch: 2, step: 5.2, len: 4, p: 67 },
  { ch: 1, step: 8, len: 3 },
];

describe('quantizeStarts', () => {
  it('rounds starts to the unit, keeps lengths, leaves other notes alone', () => {
    const r = quantizeStarts(N, [0, 1, 3], 1);
    expect(r.notes.map(n => n.step)).toEqual([0, 4, 5.2, 8]);
    expect(r.notes.map(n => n.len)).toEqual([2, 1, 4, 3]);
    expect(r.moved).toBe(2);
    expect(r.notes[3]).toBe(N[3]); // untouched notes are the same objects
  });
  it('uses coarser units and never goes below 0', () => {
    expect(quantizeStarts(N, [0, 1], 4).notes.slice(0, 2).map(n => n.step)).toEqual([0, 4]);
    expect(quantizeStarts([{ ch: 1, step: 0.2, len: 1 }], [0], 0.5).notes[0].step).toBe(0);
  });
  it('keeps a note that would end past the song inside it', () => {
    const r = quantizeStarts([{ ch: 1, step: MAX_STEP - 2.2, len: 2.2 }], [0], 4);
    expect(r.notes[0].step + r.notes[0].len).toBeLessThanOrEqual(MAX_STEP);
    expect(r.notes[0].step % 4).toBe(0);
  });
  it('reports nothing moved on the grid already', () => {
    expect(quantizeStarts(N, [3], 1).moved).toBe(0);
  });
});

describe('legato', () => {
  it('extends each note to the next later start; chords together; the last keep their length', () => {
    const ns: Note[] = [
      { ch: 1, step: 0, len: 1, p: 60 }, { ch: 1, step: 0, len: 2, p: 64 }, // chord
      { ch: 1, step: 6, len: 1, p: 62 },
      { ch: 1, step: 12, len: 1, p: 60 }, { ch: 1, step: 12.0005, len: 3, p: 67 }, // last chord (off by < EPS)
    ];
    const r = legato(ns, [0, 1, 2, 3, 4]);
    expect(r.notes.map(n => n.len)).toEqual([6, 6, 6, 1, 3]);
    expect(r.changed).toBe(3);
  });
  it('shortens overlapping notes and only looks at the given notes', () => {
    const ns: Note[] = [{ ch: 1, step: 0, len: 8 }, { ch: 1, step: 2, len: 1 }, { ch: 1, step: 4, len: 1 }];
    expect(legato(ns, [0, 2]).notes.map(n => n.len)).toEqual([4, 1, 1]);
  });
  it('a single note does not change', () => {
    expect(legato(N, [0]).changed).toBe(0);
  });
});

describe('selection helpers', () => {
  it('inverts within the channel', () => {
    expect([...invertSelection([0, 1, 3], new Set([1]))]).toEqual([0, 3]);
    expect([...invertSelection([0, 1], new Set([0, 1]))]).toEqual([]);
  });
  it('finds the notes of one key (the root for notes without p)', () => {
    expect(notesOfKey(N, 1, CH, 60)).toEqual([0, 3]);
    expect(notesOfKey(N, 1, CH, 67)).toEqual([]); // another channel's
  });
});

describe('velocity wheel', () => {
  it('nudges and clamps', () => {
    const r = nudgeVelocity(N, [0, 1], 0.05);
    expect(r[0].v).toBe(1);
    expect(r[1].v).toBe(0.55);
    expect(nudgeVelocity(N, [1], -0.6)[1].v).toBe(0);
    expect(nudgeVelocity(N, [1], -0.01)[1].v).toBe(0.49);
  });
  it('counts mouse notches per event and adds up touchpad deltas', () => {
    expect(wheelNotches(0, -100, false).n).toBe(1);
    expect(wheelNotches(0, 120, false).n).toBe(-1);
    expect(wheelNotches(0, 3, true).n).toBe(-1);
    let a = 0, n = 0;
    for (let k = 0; k < 10; k++) { const r = wheelNotches(a, -15, false); a = r.acc; n += r.n; }
    expect(n).toBe(1);
  });
});

describe('view navigation', () => {
  const lim = { total: 128, minLen: 4, maxLen: 64 };
  it('fits a range, centred when the limits keep it wider', () => {
    expect(fitRange(16, 48, lim)).toEqual({ pos: 16, len: 32 });
    expect(fitRange(10, 11, lim)).toEqual({ pos: 8.5, len: 4 });
    expect(fitRange(0, 128, lim)).toEqual({ pos: 32, len: 64 });
    expect(fitRange(120, 127, lim).pos + fitRange(120, 127, lim).len).toBeLessThanOrEqual(128);
  });
  it('follows page-wise', () => {
    expect(followPage(10, { pos: 0, len: 32 }, 2)).toBeNull();
    expect(followPage(32, { pos: 0, len: 32 }, 2)).toBe(30);
    expect(followPage(1, { pos: 30, len: 32 }, 2)).toBe(0); // the loop came round
  });
});

describe('note properties', () => {
  it('formats and parses starts as bar:beat:16th', () => {
    expect(fmtStart(0)).toBe('1:1:1');
    expect(fmtStart(21)).toBe('2:2:2');
    expect(fmtStart(4.5)).toBe('1:2:1.5');
    expect(parseStart('2:2:2')).toBe(21);
    expect(parseStart('1:2:1.5')).toBe(4.5);
    expect(parseStart('3')).toBe(32);
    expect(parseStart('3:2')).toBe(36);
    for (const bad of ['', '0:1:1', '1:5:1', '1:1:5', 'x', '1:1:1:1', '1.5:1']) expect(parseStart(bad)).toBeNull();
  });
  it('parses lengths, keys and velocities', () => {
    expect(parseLen('2')).toBe(2);
    expect(parseLen('1/8')).toBe(2);
    expect(parseLen('3/16')).toBe(3);
    expect(parseLen('.5')).toBe(0.5);
    for (const bad of ['0', '-1', 'a', '1/0']) expect(parseLen(bad)).toBeNull();
    expect(parseKey('C4')).toBe(60);
    expect(parseKey('c#4')).toBe(61);
    expect(parseKey('Db4')).toBe(61);
    expect(parseKey('A-1')).toBe(9);
    expect(parseKey('64')).toBe(64);
    for (const bad of ['H4', '128', 'G9x', 'C10']) expect(parseKey(bad)).toBeNull();
    expect(parseVel('80 %')).toBe(80);
    expect(parseVel('101')).toBeNull();
  });
  it('reads one note and a mixed selection', () => {
    expect(readProps(N, [1], CH)).toEqual({ vel: 50, len: 1, start: 3.6, key: 64 });
    expect(readProps(N, [0, 1], CH)).toEqual({ vel: null, len: null, start: 0.3, key: 60 });
  });
  it('sets velocity and length on all, moves start and key as a group', () => {
    const r = applyProps(N, [0, 1], CH, { vel: 70, len: 2, start: 4.3, key: 62 })!;
    expect(r[0]).toEqual({ ch: 1, step: 4.3, len: 2, p: 62, v: 0.7 });
    expect(r[1]).toEqual({ ch: 1, step: 7.6, len: 2, p: 66, v: 0.7 });
    expect(r[2]).toBe(N[2]);
    // a note without its own key keeps it when the key is not changed
    expect(applyProps(N, [3], CH, { vel: 20 })![3]).toEqual({ ch: 1, step: 8, len: 3, v: 0.2 });
  });
  it('refuses to leave the song or the keys', () => {
    expect(applyProps(N, [0, 1], CH, { key: 125 })).toBeNull();
    expect(applyProps(N, [3], CH, { start: MAX_STEP - 1 })).toBeNull();
  });
});
