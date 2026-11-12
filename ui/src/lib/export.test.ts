import { describe, expect, it } from 'vitest';
import { mockCatalog, sketchProject } from '../api/mockData';
import { safeFileStem, stemInserts, uniqueFileStems } from './export';

const sketch = () => sketchProject(mockCatalog());
const names = (p = sketch()) => stemInserts(p).map(m => m.name);

describe('stem export: which inserts get a file', () => {
  it('lists the inserts with clips routed to them, not the master and not empty inserts', () => {
    expect(names()).toEqual(['Drums', 'Hats', 'Bass']);
  });
  it('leaves out a muted insert and a muted track, like the engine', () => {
    const p = sketch();
    p.inserts.find(m => m.name === 'Hats')!.muted = true;
    p.tracks.find(t => t.insert === p.inserts.find(m => m.name === 'Bass')!.id)!.muted = true;
    expect(names(p)).toEqual(['Drums']);
    p.tracks.forEach(t => { t.muted = true; });
    expect(names(p)).toEqual([]);
  });
  it('counts a channel sent to an insert even without a clip on its track', () => {
    const p = sketch();
    const vocal = p.inserts.find(m => m.name === 'Vocal')!;
    expect(names(p)).not.toContain('Vocal');
    p.channels![0].insert = vocal.id;
    expect(names(p)).toContain('Vocal');
    p.channels![0].mute = true;
    expect(names(p)).not.toContain('Vocal');
  });
});

// the same cases as the Rust tests of `safe_file_stem` / `unique_file_stems` (app/src/actions.rs)
describe('stem export: file names', () => {
  it('makes names safe on every file system', () => {
    expect(safeFileStem('Kick / Snare: 2')).toBe('Kick _ Snare_ 2');
    expect(safeFileStem('  Lead...  ')).toBe('Lead');
    expect(safeFileStem('')).toBe('Insert');
    expect(safeFileStem('???')).toBe('___');
    expect(safeFileStem('con')).toBe('_con');
    expect(safeFileStem('NUL.txt')).toBe('_NUL.txt');
    expect(safeFileStem('COM1')).toBe('_COM1');
    expect(safeFileStem('COM0')).toBe('COM0');
    expect(safeFileStem('Wokal główny')).toBe('Wokal główny');
    expect(safeFileStem('x'.repeat(300))).toHaveLength(100);
  });
  it('makes them unique without regard to case', () => {
    expect(uniqueFileStems(['Bass', 'bass', 'BASS', 'Bass (2)'])).toEqual(['Bass', 'bass (2)', 'BASS (3)', 'Bass (2) (2)']);
    expect(uniqueFileStems(['Master', 'Master'])).toEqual(['Master', 'Master (2)']);
  });
});
