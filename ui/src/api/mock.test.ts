import { beforeAll, describe, expect, it, vi } from 'vitest';
import { MockBackend } from './mock';
import { emptyProject, FACTORY_FILES, FACTORY_KIT, libPath } from '../lib/factory';
import { migrateProject } from '../lib/pattern';

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
