import { beforeAll, describe, expect, it, vi } from 'vitest';
import { MockBackend } from './mock';
import { emptyProject, FACTORY_FILES, FACTORY_KIT, libPath } from '../lib/factory';
import { migrateProject } from '../lib/pattern';
import type { ExportProgress, ExportStemsResult } from './types';

// the mock publishes itself on `window` for dev tools; node has none
beforeAll(() => { vi.stubGlobal('window', {}); });

describe('standard library in the mock', () => {
  it('lists the standard library first, with groups and root keys', async () => {
    const be = new MockBackend();
    const { kits } = await be.libraryList();
    expect(kits[0]).toMatchObject({ id: FACTORY_KIT, installed: true, count: FACTORY_FILES.length });
    const files = await be.libraryFiles({ kit: FACTORY_KIT });
    expect(files).toHaveLength(FACTORY_FILES.length);
    expect(files.find(f => f.name === 'Marimba Low')).toMatchObject({ group: 'Tonal', root: 36, path: libPath('marimba-low.flac') });
  });
  it('keeps a library file as one source however often it is imported', async () => {
    const be = new MockBackend();
    await be.projectNew();
    const before = (await be.getState()).project.sources.length;
    const a = await be.importAudio({ paths: [libPath('triangle.flac')] });
    const b = await be.importAudio({ paths: [libPath('triangle.flac')], track: 2, beat: 4 });
    expect(b.sourceIds).toEqual(a.sourceIds);
    const p = (await be.getState()).project;
    expect(p.sources).toHaveLength(before + 1);
    expect(p.sources.find(s => s.id === a.sourceIds[0])).toMatchObject({ name: 'Triangle', path: 'lib:Standard/triangle.flac' });
    expect(p.clips.filter(c => c.kind === 'audio' && c.ref === a.sourceIds[0])).toHaveLength(1);
    // the starter kit's own file is found too
    const kick = await be.importAudio({ paths: [libPath('kick-808-short.flac')] });
    expect((await be.getState()).project.sources).toHaveLength(before + 1);
    expect(p.channels!.some(c => c.kind === 'sample' && c.source === kick.sourceIds[0])).toBe(true);
    // an ordinary file is a new source every time, as in the engine
    await be.importAudio({ paths: ['C:\\\\loops\\\\beat.wav'] });
    await be.importAudio({ paths: ['C:\\\\loops\\\\beat.wav'] });
    expect((await be.getState()).project.sources).toHaveLength(before + 3);
  });
  it('starts a new project like the engine: empty, with the starter kit', async () => {
    const be = new MockBackend();
    const p = (await be.projectNew()).project;
    expect(p).toEqual(emptyProject());
    expect(p.channels!.map(c => c.name)).toEqual(['Kick', 'Clap', 'Hat', 'Snare']);
    // it is already what the engine would make of it
    const q = structuredClone(p); migrateProject(q); expect(q).toEqual(p);
  });
});

describe('stem export in the mock', () => {
  const opts = { useLoop: false, tail: 2, sampleRate: 48000, includeMaster: true };
  const done = (r: ExportStemsResult) => { if (r.cancelled) throw new Error('cancelled'); return r; };

  it('writes Master and one file per insert with sound, named after the insert, and reports progress', async () => {
    const be = new MockBackend();
    const seen: ExportProgress[] = [];
    await be.on('export-progress', p => seen.push(p));
    const r = done(await be.exportStems(opts));
    expect(r.files.map(f => f.file)).toEqual(['Master.wav', 'Drums.wav', 'Hats.wav', 'Bass.wav']);
    expect(r.files.map(f => f.master)).toEqual([true, false, false, false]);
    expect(new Set(r.files.map(f => f.stats.seconds)).size).toBe(1); // all the same length
    expect(r.format).toBe('wav');
    await new Promise(res => setTimeout(res, 5)); // the mock delivers events on a timer
    expect(seen.map(p => p.done)).toEqual([0, 1, 2, 3, 4]);
    expect(seen[0]).toMatchObject({ total: 4, name: 'Master' });
    expect(seen.at(-1)).toMatchObject({ done: 4, total: 4, name: '' });
  });
  it('uses MP3 for every file including the master', async () => {
    const be = new MockBackend();
    const r = done(await be.exportStems({ ...opts, format: 'mp3', mp3Kbps: 192 }));
    expect(r.format).toBe('mp3');
    expect(r.files.map(f => f.file)).toEqual(['Master.mp3', 'Drums.mp3', 'Hats.mp3', 'Bass.mp3']);
  });
  it('can leave the master out', async () => {
    const be = new MockBackend();
    const r = done(await be.exportStems({ ...opts, includeMaster: false }));
    expect(r.files.map(f => f.file)).toEqual(['Drums.wav', 'Hats.wav', 'Bass.wav']);
  });
  it('refuses when nothing has sound, instead of writing an empty folder', async () => {
    const be = new MockBackend();
    const tracks = (await be.getState()).project.tracks;
    await be.applyOps(tracks.map(t => ({ op: 'putTrack' as const, track: { ...t, muted: true } })));
    await expect(be.exportStems({ ...opts, includeMaster: false })).rejects.toThrow(/nothing to export/);
  });
  it('stops on cancel without a result', async () => {
    const be = new MockBackend();
    const run = be.exportStems(opts);
    await be.exportCancel();
    expect(await run).toEqual({ cancelled: true });
  });
});

// the same cases as crates/engine/tests/make_unique.rs
describe('make unique (audio) in the mock', () => {
  async function setup() {
    const be = new MockBackend();
    await be.projectNew();
    const r = await be.importAudio({ paths: ['C:\\loops\\guitar.wav'], track: 0, beat: 0 });
    const a = (await be.getState()).project.clips.find(c => c.id === r.clipIds[0])!;
    await be.applyOps([{ op: 'putClips', clips: [{ ...a, id: 70001, start: 4 }, { ...a, id: 70002, track: 1 }] }]);
    return { be, src: r.sourceIds[0], a: a.id };
  }
  const clip = async (be: MockBackend, id: number) => (await be.getState()).project.clips.find(c => c.id === id)!;

  it('gives the clip its own source on a new track with the old routing, in one undo step', async () => {
    const { be, src, a } = await setup();
    const before = structuredClone((await be.getState()).project);
    const r = await be.makeUnique([a]);
    const p = (await be.getState()).project;
    expect(p.sources.find(s => s.id === r.sourceId)).toMatchObject({ name: 'guitar #2', path: p.sources.find(s => s.id === src)!.path });
    expect(r.track).toBe(1);
    expect(p.tracks).toHaveLength(before.tracks.length + 1);
    expect(p.inserts).toEqual(before.inserts);
    expect(p.tracks[1]).toMatchObject({ name: 'guitar #2', insert: 0 });
    expect(r.insertId).toBe(0);
    expect(await clip(be, a)).toMatchObject({ track: 1, ref: r.sourceId });
    expect(await clip(be, 70001)).toMatchObject({ track: 0, ref: src });
    expect(await clip(be, 70002)).toMatchObject({ track: 2, ref: src });
    await be.undo();
    expect((await be.getState()).project).toEqual(before);
  });
  it('moves several clips together and counts names up', async () => {
    const { be, a } = await setup();
    const r = await be.makeUnique([a, 70001]);
    const r2 = await be.makeUnique([70002]);
    const r3 = await be.makeUnique([a]);
    const name = async (id: number) => (await be.getState()).project.sources.find(s => s.id === id)!.name;
    expect([await name(r.sourceId), await name(r2.sourceId), await name(r3.sourceId)]).toEqual(['guitar #2', 'guitar #3', 'guitar #4']);
    expect([r.insertId, r2.insertId, r3.insertId]).toEqual([0, 0, 0]);
  });
  it('keeps a routing the user picked', async () => {
    const { be, a } = await setup();
    const p0 = (await be.getState()).project;
    await be.applyOps([{ op: 'putTrack', track: { ...p0.tracks[0], insert: p0.inserts[3].id } }]);
    const r = await be.makeUnique([a]);
    const p = (await be.getState()).project;
    expect(r.insertId).toBe(p0.inserts[3].id);
    expect(p.tracks[1].insert).toBe(p0.inserts[3].id);
    expect(p.inserts).toHaveLength(p0.inserts.length);
  });
  it('refuses what it cannot do and changes nothing', async () => {
    const { be, a } = await setup();
    const before = structuredClone((await be.getState()).project);
    await expect(be.makeUnique([])).rejects.toThrow();
    await expect(be.makeUnique([999999])).rejects.toThrow();
    await expect(be.makeUnique([a, 70002])).rejects.toThrow(/same track/);
    expect((await be.getState()).project).toEqual(before);
  });
});

describe('pattern loop in the mock (like the engine)', () => {
  it('starts where asked, loops the range, and keeps its place when the range changes', async () => {
    const be = new MockBackend();
    const p = (await be.getState()).project, pt = p.patterns.find(x => x.len >= 4)!, spb = 60 / p.bpm;
    let now = 1000;
    const clock = vi.spyOn(performance, 'now').mockImplementation(() => now);
    const pos = () => (be as unknown as { patPos(len: number, spb: number): number }).patPos(pt.len, spb);
    try {
      await be.patternPlay(pt.id, { from: 2, range: { a: 1, b: 3 } });
      expect(pos()).toBe(2);
      now += 1000 * spb * 1.5; // 1.5 beats: past the range's end, wrapped
      expect(pos()).toBeCloseTo(1.5);
      await be.patternLoop({ a: 0, b: 1 }); // outside the new range: its start
      expect(pos()).toBe(0);
      now += 1000 * spb * 0.25;
      await be.patternLoop(null); // inside the whole pattern: stays
      expect(pos()).toBeCloseTo(0.25);
      await expect(be.patternPlay(pt.id, { from: NaN })).rejects.toThrow();
      await expect(be.patternLoop({ a: 0, b: Infinity })).rejects.toThrow();
      await expect(be.patternPlay(424242)).rejects.toThrow();
    } finally { clock.mockRestore(); }
  });
});
