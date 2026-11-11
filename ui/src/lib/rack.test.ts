import { describe, expect, it } from 'vitest';
import { samplerDefault, type Channel, type Note, type Project } from '../api/types';
import { applyAll } from './applyOps';
import { copyChannelNotes, gridW, pasteChannelNotes } from './pattern';
import { fitOnScreen, moveIndex, RACK_FIT_STEPS, rackFitWidth } from './rack';

const geo = { left: 212, cw: 22, pad: 8, min: 480, room: 5000 };
const ch = (id: number, name: string): Channel => ({ id, name, color: '#4f8ef7', kind: 'sample', source: 1, sampler: samplerDefault(), vol: 0.7937, pan: 0, mute: false, solo: false, insert: null, root: 60 });
const project = (notes: Note[]): Project => ({
  version: 2, bpm: 120, nextId: 100, loop: null, tracks: [], inserts: [{ id: 0, name: 'Master', color: '#fff', vol: 0.7937, pan: 0, muted: false, solo: false, fx: [] }],
  channels: [ch(10, 'Kick'), ch(11, 'Clap'), ch(12, 'Hat')],
  patterns: [{ id: 1, name: 'A', color: '#fff', len: 4, minLen: 4, notes }],
  sources: [{ id: 1, name: 's', color: '#fff', path: 'x.wav', dur: 1, sr: 44100, ch: 1 }], clips: [],
} as unknown as Project);

describe('Channel Rack window width', () => {
  it('shows the pattern and the greyed bar at the comfortable step width', () => {
    expect(rackFitWidth(16, geo)).toBe(212 + gridW(32, 22) + 8);
    expect(rackFitWidth(32, geo)).toBe(212 + gridW(48, 22) + 8);
  });
  it('follows the pattern both ways', () => {
    expect(rackFitWidth(16, geo)).toBeLessThan(rackFitWidth(32, geo));
    expect(rackFitWidth(32, geo)).toBeGreaterThan(rackFitWidth(16, geo));
  });
  it('grows with the pattern up to 16 bars, then to the room it has; never below the minimum', () => {
    const wide = { ...geo, room: 50000 };
    expect(rackFitWidth(64, wide)).toBe(212 + gridW(80, 22) + 8);
    expect(rackFitWidth(240, wide)).toBe(212 + gridW(RACK_FIT_STEPS, 22) + 8);
    expect(rackFitWidth(512, wide)).toBe(rackFitWidth(240, wide));
    expect(rackFitWidth(32, { ...geo, room: 700 })).toBe(700);
    expect(rackFitWidth(1, { ...geo, cw: 1 })).toBe(480);
    expect(rackFitWidth(32, { ...geo, room: 300 })).toBe(480);
  });
});

describe('window kept on screen while it grows', () => {
  it('stays put while it fits, slides left when it would pass the right edge, never wider than the screen', () => {
    expect(fitOnScreen(300, 600, 1920)).toEqual({ x: 300, w: 600 });
    expect(fitOnScreen(900, 1400, 1920)).toEqual({ x: 510, w: 1400 });
    expect(fitOnScreen(900, 4000, 1920)).toEqual({ x: 10, w: 1900 });
  });
});

describe('Channel Rack: move a channel', () => {
  it('moves up and down, not past the ends', () => {
    expect(moveIndex(1, 3, -1)).toBe(0);
    expect(moveIndex(1, 3, 1)).toBe(2);
    expect(moveIndex(0, 3, -1)).toBeNull();
    expect(moveIndex(2, 3, 1)).toBeNull();
    expect(moveIndex(-1, 3, 1)).toBeNull();
  });
  it('is one putChannel op with an index (the engine reorders, nothing else changes)', () => {
    const p = project([]);
    const out = applyAll(p, [{ op: 'putChannel', channel: p.channels![2], index: 1 }]);
    expect(out.channels!.map(c => c.name)).toEqual(['Kick', 'Hat', 'Clap']);
    const back = applyAll(out, [{ op: 'putChannel', channel: out.channels![1], index: 2 }]);
    expect(back.channels!.map(c => c.name)).toEqual(['Kick', 'Clap', 'Hat']);
  });
});

describe('Channel Rack clipboard', () => {
  const notes: Note[] = [
    { ch: 10, step: 4, len: 1 }, { ch: 10, step: 0, len: 1, v: 0.5 }, { ch: 11, step: 4, len: 1, p: 62 },
  ];
  it('copies one channel’s notes without the channel, in order', () => {
    expect(copyChannelNotes(notes, 10)).toEqual([{ step: 0, len: 1, v: 0.5 }, { step: 4, len: 1 }]);
    expect(copyChannelNotes(notes, 12)).toEqual([]);
  });
  it('the copy does not change when the pattern is edited later', () => {
    const src = notes.map(n => ({ ...n }));
    const clip = copyChannelNotes(src, 10);
    src[1].step = 8;
    expect(clip[0].step).toBe(0);
  });
  it('paste replaces the target channel’s notes and keeps the others', () => {
    const out = pasteChannelNotes(notes, 11, copyChannelNotes(notes, 10));
    expect(out.filter(n => n.ch === 11)).toEqual([{ ch: 11, step: 0, len: 1, v: 0.5 }, { ch: 11, step: 4, len: 1 }]);
    expect(out.filter(n => n.ch === 10)).toEqual(notes.filter(n => n.ch === 10));
  });
  it('an empty clipboard clears the channel; notes past the song are dropped', () => {
    expect(pasteChannelNotes(notes, 10, []).some(n => n.ch === 10)).toBe(false);
    expect(pasteChannelNotes([], 10, [{ step: 1e9, len: 1 }])).toEqual([]);
  });
  it('a longer copy makes the target pattern grow (one putPattern)', () => {
    const p = project([{ ch: 11, step: 0, len: 1 }]);
    const long = copyChannelNotes([{ ch: 10, step: 0, len: 1 }, { ch: 10, step: 40, len: 1 }], 10);
    const pt = p.patterns[0];
    const out = applyAll(p, [{ op: 'putPattern', pattern: { ...pt, notes: pasteChannelNotes(pt.notes, 12, long) } }]);
    expect(out.patterns[0].len).toBe(12);
    expect(out.patterns[0].notes.filter(n => n.ch === 12).map(n => n.step)).toEqual([0, 40]);
    expect(out.patterns[0].notes.filter(n => n.ch === 11).length).toBe(1);
  });
});
