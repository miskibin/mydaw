import { beforeAll, describe, expect, it, vi } from 'vitest';
import { MockBackend } from '../api/mock';
import type { Channel, Clip, PitchAnalysis, Project } from '../api/types';
import { emptyProject } from './factory';
import { applyAll, OpError } from './applyOps';
import { collectMelody, melodyFit, melodyTarget, selectionSpan, vocalsUnder } from './melody';
import { correctPitch, CORRECT_PRESETS, DEFAULT_REF, editedCurve } from './pitch';

beforeAll(() => { vi.stubGlobal('window', {}); });

/** The engine's melody.rs `collect` test, mirrored: 120 BPM, a 1-bar pattern looped under a stretched vocal. */
function song(): { p: Project; vocal: Clip } {
  const p = emptyProject();
  p.bpm = 120;
  p.channels = [...(p.channels ?? []), { id: 7001, name: 'Piano', color: '#fff', kind: 'synth', vol: 0.8, pan: 0, mute: false, solo: false, insert: null, root: 60 } as unknown as Channel];
  p.patterns.push({ id: 7100, name: 'Melody', color: '#fff', len: 4, notes: [{ ch: 7001, step: 0, len: 4, p: 69 }, { ch: 7001, step: 8, len: 8 }] });
  p.clips.push({ id: 7200, track: 0, start: 4, len: 8, offset: 0, kind: 'pat', ref: 7100, muted: false, gain: 0, fadeIn: 0, fadeOut: 0, pan: 0, norm: false, rev: false } as Clip);
  const vocal = { id: 7300, track: 1, start: 5, len: 6, offset: 1, kind: 'audio', ref: 1, stretch: 2, muted: false, gain: 0, fadeIn: 0, fadeOut: 0, pan: 0, norm: false, rev: false } as Clip;
  return { p, vocal };
}
const src = (b: number) => (b - 5 + 1) * 0.5 / 2;

describe('melody reference (melody.rs mirror)', () => {
  it('places the notes the playlist plays over the vocal on its source time, like the engine', () => {
    const { p, vocal } = song();
    const ns = collectMelody(p, vocal, 7001);
    expect(ns.map(n => [n.start, n.end, n.key])).toEqual([[src(6), src(8), 60], [src(8), src(9), 69], [src(10), src(11), 60]]);
    expect(collectMelody(p, vocal, 7001, [src(8) + 0.01, src(9)])).toHaveLength(1);
    expect(collectMelody(p, vocal, 9999)).toEqual([]);
    // a muted pattern clip plays nothing
    p.clips.find(c => c.id === 7200)!.muted = true;
    expect(collectMelody(p, vocal, 7001)).toEqual([]);
  });
  it('a piano roll selection becomes the source seconds it covers on that vocal', () => {
    const { p, vocal } = song();
    p.clips.push(vocal);
    expect(vocalsUnder(p, 7100).map(c => c.id)).toEqual([7300]);
    // note 0 (A4) sounds over the vocal only in the second repeat (beats 8–9)
    expect(selectionSpan(p, vocal, 7001, 7100, [0])).toEqual([src(8), src(9)]);
    expect(selectionSpan(p, vocal, 7001, 7100, [0, 1])).toEqual([src(6), src(11)]);
    p.clips.find(c => c.id === 7200)!.start = 40; // the pattern moved away
    expect(vocalsUnder(p, 7100)).toEqual([]);
  });
  it('the target sits on the played notes in the octave nearest the voice; unplayed singing stays', () => {
    const rate = 200, n = 300;
    // sung: 57.4 for 0.6 s, 60.4 to 1.2 s, an ad-lib at 64.3 from 1.3 s
    const curve = Array.from({ length: n }, (_, f) => f < 120 ? 57.4 : f < 240 ? 60.4 : f < 260 ? null : 64.3);
    const take: PitchAnalysis = { rate, curve, levels: curve.map(() => 0.5), notes: [{ start: 0, end: 0.6, pitch: 57.4, level: 0.5 }, { start: 0.6, end: 1.2, pitch: 60.4, level: 0.5 }, { start: 1.3, end: 1.5, pitch: 64.3, level: 0.5 }] };
    const notes = [{ start: 0, end: 0.52, key: 69 }, { start: 0.52, end: 1.2, key: 72 }];
    const t = melodyTarget(take, take.notes, notes, { ...DEFAULT_REF, source: 0 });
    expect(t.octave).toBe(-12);
    expect(t.targetF0[50]).toBeCloseTo(57, 5);
    expect(t.targetF0[110]).toBeCloseTo(57, 5); // sung late: still the first note
    expect(t.targetF0[200]).toBeCloseTo(60, 5);
    expect(t.targetF0[280]).toBeCloseTo(64.3, 5);
    expect(t.reason).toBeNull();
    expect(melodyFit(take, [0, 1.5], notes)).toBeGreaterThan(0.7);
    expect(melodyTarget(take, take.notes, [], { ...DEFAULT_REF, source: 0 }).reason).toMatch(/no notes/);
  });
});

describe('retune speed (tune_render.rs mirror)', () => {
  const rate = 200;
  const take: PitchAnalysis = { rate, curve: Array.from({ length: 200 }, () => 57.3), levels: new Array(200).fill(0.5), notes: [{ start: 0, end: 1, pitch: 57.3, level: 0.5 }] };
  const tune = (speed: number | null) => ({ notes: [{ start: 0, end: 1, shift: 100, drift: 0, formant: 0, gain: 0, speed }], pencil: [] });
  it('no speed: the correction at once; a speed: 95 % of it after that many ms', () => {
    expect(editedCurve(take, tune(null))[1] - 57.3).toBeCloseTo(1, 5);
    const slow = editedCurve(take, tune(100));
    expect(slow[4] - 57.3).toBeLessThan(0.6);
    expect(slow[19] - 57.3).toBeGreaterThan(0.9);
    expect(slow[19] - 57.3).toBeLessThan(0.97);
  });
  it('Correct writes the preset speed onto the notes', () => {
    const robot = CORRECT_PRESETS.find(x => x.id === 'robot')!;
    const out = correctPitch({ notes: tune(null).notes, pencil: [] }, take, [0], robot.strength, robot.flatten, 0, 0xfff, robot.speed);
    expect(out.notes[0]).toMatchObject({ speed: 0, drift: 1, shift: -30 });
  });
});

describe('mock: melody reference and preview', () => {
  it('rejects a bad melody or speed, accepts a melody without a reference source', async () => {
    const be = new MockBackend();
    await be.importAudio({ paths: ['C:\\takes\\vocal.wav'], track: 3, beat: 0 });
    const p = (await be.getState()).project;
    const c = p.clips.find(x => x.kind === 'audio')!;
    const put = (reference: object) => applyAll(structuredClone(p), [{ op: 'putClips', clips: [{ ...c, tune: { notes: [], pencil: [], reference: { ...DEFAULT_REF, source: 0, ...reference } } }] }]);
    expect(() => put({ melody: { channel: 1, notes: [{ start: 1, end: 0.5, key: 60 }] } })).toThrow(OpError);
    expect(() => put({ melody: { channel: 1, notes: [] }, speed: -1 })).toThrow(OpError);
    expect(() => put({ melody: { channel: 1, notes: [{ start: 0, end: 0.5, key: 60 }] }, speed: 40 })).not.toThrow();
  });
  it('solo preview starts where asked, in source seconds, and refuses past the clip', async () => {
    const be = new MockBackend();
    await be.importAudio({ paths: ['C:\\takes\\vocal.wav'], track: 3, beat: 0 });
    const p = (await be.getState()).project;
    const c = p.clips.find(x => x.kind === 'audio')!;
    const spb = 60 / p.bpm, a = c.offset * spb / (c.stretch ?? 1), b = (c.offset + c.len) * spb / (c.stretch ?? 1);
    await be.previewClip(c.id, a + 0.5);
    await expect(be.previewClip(c.id, b + 1)).rejects.toThrow();
  });
});
