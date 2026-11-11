import { describe, expect, it } from 'vitest';
import { sketchProject, mockCatalog } from '../api/mockData';
import type { Notebook, Project } from '../api/types';
import { applyAll } from './applyOps';
import {
  barOfBeat, beatOfBar, isHeader, lastWord, lineBars, lineOfBar, movePage, notebookHash, pageBarRange, rhymeGroups, rhymeKey, syllables,
  withPage, wordSyllables,
} from './notebook';

describe('syllables (Polish)', () => {
  const words: [string, number][] = [
    ['nie', 1], ['się', 1], ['piosenka', 3], ['miasto', 2], ['kiedy', 2], ['Polska', 2], ['poezja', 3], ['mówię', 2],
    ['dzień', 1], ['ciemność', 2], ['wszystko', 2], ['rzeczywistość', 4], ['chrząszcz', 1], ['pięćdziesiąt', 3],
    ['ulica', 3], ['auto', 2], ['Europa', 3], ['nauka', 3], ['zrozumieć', 3], ['pszczoła', 2], ['szaleństwo', 3],
    ['i', 1], ['w', 0], ['z', 0], ['bitów', 2], ['Wrocław', 2],
  ];
  for (const [w, n] of words) it(`${w} = ${n}`, () => expect(wordSyllables(w)).toBe(n));

  it('counts whole lines, ignoring punctuation and digits', () => {
    expect(syllables('Idę przez miasto nocą,')).toBe(7);
    expect(syllables('nie ma — nic!')).toBe(3);
    expect(syllables('  ')).toBe(0);
    expect(syllables('[Refren]')).toBe(0);
    expect(syllables('to jest 100% prawdy')).toBe(4);
  });
});

describe('line → bar mapping', () => {
  const text = '[Zwrotka]\nidę przez miasto\n\nświatła gasną\n[Refren]\nla la';
  it('skips headers, counts empty lines, offsets by startBar', () => {
    expect(lineBars(text, 1)).toEqual([null, 1, 2, 3, null, 4]);
    expect(lineBars(text, 17)).toEqual([null, 17, 18, 19, null, 20]);
  });
  it('finds the line of a bar', () => {
    expect(lineOfBar(text, 17, 19)).toBe(3);
    expect(lineOfBar(text, 17, 20)).toBe(5);
    expect(lineOfBar(text, 17, 16)).toBe(-1);
    expect(lineOfBar(text, 17, 21)).toBe(-1);
  });
  it('page range', () => {
    expect(pageBarRange({ text, startBar: 9 })).toEqual([9, 12]);
    expect(pageBarRange({ text: '[Intro]', startBar: 5 })).toEqual([5, 4]);
    expect(pageBarRange({ text: '', startBar: 5 })).toEqual([5, 5]); // one empty line = one bar of rest
  });
  it('headers need both brackets', () => {
    expect(isHeader('  [Refren x2] ')).toBe(true);
    expect(isHeader('[Refren')).toBe(false);
    expect(isHeader('[]')).toBe(true);
    expect(isHeader('a [b]')).toBe(false);
  });
  it('bars ↔ beats are 1-based, 4 beats per bar', () => {
    expect(barOfBeat(0)).toBe(1);
    expect(barOfBeat(3.99)).toBe(1);
    expect(barOfBeat(4)).toBe(2);
    expect(barOfBeat(-2)).toBeNull();
    expect(beatOfBar(1)).toBe(0);
    expect(beatOfBar(5)).toBe(16);
  });
});

describe('rhymes', () => {
  it('keys from the penultimate vowel (Polish stress)', () => {
    expect(rhymeKey('kochanie')).toBe('anie');
    expect(rhymeKey('miłości')).toBe('ości');
    expect(rhymeKey('noc')).toBe('oc');
    expect(rhymeKey('Wrocław')).toBe('ocłaf');
    expect(rhymeKey('brr')).toBeNull();
    // spelling variants of one sound rhyme
    expect(rhymeKey('góra')).toBe(rhymeKey('dziura'));
    expect(rhymeKey('chleb')).toBe(rhymeKey('sklep'));
    expect(rhymeKey('morze')).toBe(rhymeKey('może'));
  });
  it('takes the last word, ignoring trailing punctuation', () => {
    expect(lastWord('idę przez miasto nocą...')).toEqual({ word: 'nocą', from: 17, to: 21 });
    expect(lastWord('!!!')).toBeNull();
  });
  it('groups only endings shared by two or more bar lines', () => {
    const lines = [
      '[Zwrotka]', 'mówiłaś mi o miłości', 'a ja szukałem kochanie', 'w tej całej codzienności', '', 'na zawsze moje wyznanie', 'i nic więcej', '[ości]',
    ];
    const g = rhymeGroups(lines);
    expect(g.get(1)).toBe(0);
    expect(g.get(3)).toBe(0);
    expect(g.get(2)).toBe(1);
    expect(g.get(5)).toBe(1);
    expect(g.has(0)).toBe(false); // header
    expect(g.has(6)).toBe(false); // no partner
    expect(g.has(7)).toBe(false); // a header never rhymes
  });
});

describe('notebook op (mirror of ops.rs putNotebook)', () => {
  const cat = mockCatalog();
  const nb: Notebook = { pages: [{ id: 9000, name: 'Zwrotka 1', startBar: 1, text: '[Zwrotka]\nraz\ndwa' }, { id: 9001, name: 'Refren', startBar: 17, text: 'la' }] };
  it('round-trips through the model and JSON, bumping nextId', () => {
    const p = applyAll(sketchProject(cat), [{ op: 'putNotebook', notebook: nb }]);
    expect(p.notebook).toEqual(nb);
    expect(p.nextId).toBeGreaterThan(9001);
    const back = JSON.parse(JSON.stringify(p)) as Project;
    expect(back.notebook).toEqual(nb);
    expect(notebookHash(back.notebook)).toBe(notebookHash(nb));
  });
  it('rejects duplicate ids and bad bars, changing nothing', () => {
    const p0 = sketchProject(cat);
    expect(() => applyAll(p0, [{ op: 'putNotebook', notebook: { pages: [nb.pages[0], { ...nb.pages[1], id: 9000 }] } }])).toThrow();
    expect(() => applyAll(p0, [{ op: 'putNotebook', notebook: withPage(nb, 9000, { startBar: 0 }) }])).toThrow();
    expect(p0.notebook).toBeUndefined();
  });
  it('page helpers keep the other pages', () => {
    expect(movePage(nb, 9001, 0).pages.map(p => p.id)).toEqual([9001, 9000]);
    expect(movePage(nb, 9000, 99).pages.map(p => p.id)).toEqual([9001, 9000]);
    expect(withPage(nb, 9001, { name: 'Hook' }).pages.map(p => p.name)).toEqual(['Zwrotka 1', 'Hook']);
    expect(notebookHash(nb)).not.toBe(notebookHash(withPage(nb, 9001, { text: 'la!' })));
    expect(notebookHash(undefined)).toBe(notebookHash({ pages: [] }));
  });
});
