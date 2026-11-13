import { describe, expect, it } from 'vitest';
import type { LibFile } from '../api/types';
import { FACTORY_FILES } from './factory';
import { groupFiles, groupOpen, GROUP_ROWS, LIST_ROWS, matches, OPEN_UP_TO, searchTerms, stepIndex } from './libraryView';
import { libFileGlyph, libGlyph, libGroupGlyph } from '../ui/menuIcons';
import { INST_ICONS } from '../ui/instIcons';

const f = (name: string, group = ''): LibFile => ({ name, path: `/lib/${group}/${name}`, group });
const std: LibFile[] = FACTORY_FILES.map(x => ({ name: x.name, path: x.file, group: x.group, root: x.root, pitched: x.pitched }));

describe('library search', () => {
  it('splits a query into lower-case words without accents', () => {
    expect(searchTerms('  808   Kick ')).toEqual(['808', 'kick']);
    expect(searchTerms('Pętla')).toEqual(['petla']);
    expect(searchTerms('   ')).toEqual([]);
  });
  it('needs every word, in the name or the group', () => {
    const kick = f('808 Kick Long', 'Kicks');
    expect(matches(kick, searchTerms('kick 808'))).toBe(true);
    expect(matches(kick, searchTerms('kicks long'))).toBe(true);
    expect(matches(kick, searchTerms('kick snare'))).toBe(false);
    expect(matches(kick, [])).toBe(true);
  });
  it('finds a group by the name it is shown under', () => {
    const snare = f('Rope Snare', 'Snares');
    expect(matches(snare, searchTerms('werble'), () => 'Werble')).toBe(true);
    expect(matches(snare, searchTerms('werble'))).toBe(false);
  });
  it('finds the orchestra hits and the 808 basses in the standard library', () => {
    const hits = groupFiles(std, searchTerms('orchestra hit'));
    expect(hits.map(g => g.name)).toEqual(['Orchestra']);
    expect(hits[0].files.map(x => x.name)).toEqual(['Orchestra Hit', 'Orchestra Hit Minor', 'Orchestra Hit Major']);
    expect(groupFiles(std, searchTerms('808 bass')).flatMap(g => g.files).length).toBeGreaterThanOrEqual(6);
    expect(groupFiles(std, searchTerms('zzz'))).toEqual([]);
  });
});

describe('library groups', () => {
  it('keeps the order of the list and counts every file', () => {
    const groups = groupFiles(std);
    expect(groups.map(g => g.name)).toEqual(['Kicks', 'Snares', 'Claps', 'Hats', 'Cymbals', 'Toms', 'Percussion', 'Tonal', 'Orchestra', 'FX']);
    expect(groups.reduce((n, g) => n + g.total, 0)).toBe(std.length);
  });
  it('draws a bounded number of rows of a huge folder and says how many there are', () => {
    const many = Array.from({ length: 5000 }, (_, i) => f(`hit ${i}`, 'One shots'));
    const [g] = groupFiles(many);
    expect(g.files.length).toBe(GROUP_ROWS);
    expect(g.total).toBe(5000);
    const spread = groupFiles(Array.from({ length: 5000 }, (_, i) => f(`hit ${i}`, `Folder ${i % 10}`)));
    expect(spread.reduce((n, x) => n + x.files.length, 0)).toBe(LIST_ROWS);
    expect(spread.reduce((n, x) => n + x.total, 0)).toBe(5000);
    // a search reaches the files past the cap
    expect(groupFiles(many, searchTerms('hit 4999'))[0].files.map(x => x.name)).toEqual(['hit 4999']);
  });
  it('starts a short list open and a long one closed; a search opens what it found', () => {
    expect(groupOpen(undefined, false, OPEN_UP_TO)).toBe(true);
    expect(groupOpen(undefined, false, std.length)).toBe(false);
    expect(groupOpen(true, false, std.length)).toBe(true);
    expect(groupOpen(false, false, 3)).toBe(false);
    expect(groupOpen(undefined, true, std.length)).toBe(true);
    expect(groupOpen(false, true, std.length)).toBe(false);
  });
  it('steps through rows and stops at the ends', () => {
    expect(stepIndex(3, -1, 1)).toBe(0);
    expect(stepIndex(3, -1, -1)).toBe(2);
    expect(stepIndex(3, 0, 1)).toBe(1);
    expect(stepIndex(3, 2, 1)).toBe(2);
    expect(stepIndex(3, 0, -1)).toBe(0);
    expect(stepIndex(0, -1, 1)).toBe(-1);
  });
});

describe('library icons', () => {
  it('has a mark for every standard-library group', () => {
    for (const g of new Set(FACTORY_FILES.map(x => x.group))) expect(libGlyph(g), g).not.toBe(INST_ICONS.sample);
  });
  it('reads the mark of a user file from its name, then from its folder', () => {
    expect(libFileGlyph(f('Trap Kick 03.wav', 'My pack'))).toBe(libGlyph('Kicks'));
    expect(libFileGlyph(f('808 Kick.wav', 'My pack'))).toBe(libGlyph('Kicks'));
    expect(libFileGlyph(f('808 long.wav', 'My pack'))).toBe(INST_ICONS.bass);
    expect(libFileGlyph(f('take 3.wav', 'Snares'))).toBe(libGlyph('Snares'));
    expect(libFileGlyph(f('take 3.wav', 'misc'))).toBe(INST_ICONS.sample);
    expect(libGroupGlyph('Hi-Hats')).toBe(libGlyph('Hats'));
  });
  it('lists which standard-library files play a note', () => {
    const pitched = std.filter(x => x.pitched).map(x => x.group);
    expect(new Set(pitched)).toEqual(new Set(['Tonal', 'Orchestra']));
    expect(std.find(x => x.name === 'Gong')?.pitched).toBe(false);
    expect(std.find(x => x.name === 'Orchestra Hit')?.pitched).toBe(true);
  });
});
