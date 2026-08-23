import { describe, expect, it } from 'vitest';

import { badgeOf, filterInstruments, instrumentRows } from './instruments';
import type { ClapPluginInfoDto, SourceDto, Vst3PluginInfoDto } from '../api/types';

function source(id: string, label: string, extra: Partial<SourceDto> = {}): SourceDto {
  return {
    id,
    label,
    description: `${label}, described`,
    kind: 'internal',
    vendor: '',
    bundlePath: null,
    pluginId: null,
    selected: false,
    takesNotes: true,
    ...extra,
  };
}

function clap(
  id: string,
  name: string,
  extra: Partial<ClapPluginInfoDto> = {},
): ClapPluginInfoDto {
  return {
    id,
    name,
    vendor: 'Acme',
    version: '1.0',
    path: `C:\\plugins\\${name}.clap`,
    kind: 'instrument',
    features: [],
    ...extra,
  } as ClapPluginInfoDto;
}

function vst3(
  id: string,
  name: string,
  extra: Partial<Vst3PluginInfoDto> = {},
): Vst3PluginInfoDto {
  return {
    id,
    name,
    vendor: 'Acme',
    version: '1.0',
    path: `C:\\plugins\\${name}.vst3`,
    kind: 'instrument',
    subcategories: [],
    ...extra,
  } as Vst3PluginInfoDto;
}

describe('badgeOf', () => {
  it('names what makes the sound, not who shipped it', () => {
    expect(badgeOf(source('poly-synth', 'Poly Synth'))).toBe('SYNTH');
    expect(badgeOf(source('sampler:808-kit', '808 Kit'))).toBe('SAMPLER');
    expect(badgeOf(source('test-tone', 'Test Tone'))).toBe('TONE');
    expect(badgeOf(source('clap:x', 'X', { kind: 'clap' }))).toBe('CLAP');
    expect(badgeOf(source('vst3:x', 'X', { kind: 'vst3' }))).toBe('VST3');
  });
});

describe('instrumentRows', () => {
  it('puts built-ins first, then every plugin by name whatever its format', () => {
    const rows = instrumentRows(
      [
        source('test-tone', 'Test Tone'),
        source('sampler:808-kit', '808 Kit'),
        source('poly-synth', 'Poly Synth'),
      ],
      [clap('com.acme.zeta', 'Zeta'), clap('com.acme.alpha', 'Alpha')],
      [vst3('AABB', 'Mono')],
    );
    expect(rows.map((row) => row.label)).toEqual([
      '808 Kit',
      'Poly Synth',
      'Test Tone',
      // One alphabetical run, CLAP and VST3 together: the format is a badge on
      // the row, so sorting by it would be the old split wearing a new hat.
      'Alpha',
      'Mono',
      'Zeta',
    ]);
    expect(rows.filter((row) => row.builtIn)).toHaveLength(3);
  });

  it('takes hosted instruments from the scan, never from the source list', () => {
    // `get_sources` offers hosted sources too, but without the bundle path
    // placement needs -- and listing a plugin twice under two names is a
    // browser you cannot trust.
    const rows = instrumentRows(
      [source('poly-synth', 'Poly Synth'), source('clap:com.acme.diva', 'Diva', { kind: 'clap' })],
      [clap('com.acme.diva', 'Diva')],
      [],
    );
    expect(rows.map((row) => row.label)).toEqual(['Poly Synth', 'Diva']);
    expect(rows[1]!.place).toEqual({
      via: 'clap',
      path: 'C:\\plugins\\Diva.clap',
      pluginId: 'com.acme.diva',
    });
  });

  it('leaves effects out; they are added from the mixer', () => {
    const rows = instrumentRows(
      [],
      [clap('com.acme.verb', 'Verb', { kind: 'audio-effect' })],
      [vst3('CCDD', 'Comp', { kind: 'audio-effect' })],
    );
    expect(rows).toEqual([]);
  });

  it('falls back to the plugin id when a bundle declares no name', () => {
    const rows = instrumentRows([], [clap('com.acme.anon', '')], []);
    expect(rows[0]!.label).toBe('com.acme.anon');
  });

  it('carries the selection through, so the picker can mark what is playing', () => {
    const rows = instrumentRows([source('poly-synth', 'Poly Synth', { selected: true })], [], []);
    expect(rows[0]!.selected).toBe(true);
  });
});

describe('filterInstruments', () => {
  const rows = instrumentRows(
    [source('sampler:808-kit', '808 Kit'), source('poly-synth', 'Poly Synth')],
    [clap('com.uhe.diva', 'Diva', { vendor: 'u-he' })],
    [],
  );

  it('is not a filter when the box is empty', () => {
    expect(filterInstruments(rows, '')).toHaveLength(3);
    expect(filterInstruments(rows, '   ')).toHaveLength(3);
  });

  it('matches the name, the format and the vendor', () => {
    expect(filterInstruments(rows, '808').map((row) => row.label)).toEqual(['808 Kit']);
    expect(filterInstruments(rows, 'clap').map((row) => row.label)).toEqual(['Diva']);
    expect(filterInstruments(rows, 'u-he').map((row) => row.label)).toEqual(['Diva']);
  });

  it('ignores case, because nobody types a badge in capitals', () => {
    expect(filterInstruments(rows, 'DIVA').map((row) => row.label)).toEqual(['Diva']);
    expect(filterInstruments(rows, 'sampler').map((row) => row.label)).toEqual(['808 Kit']);
  });

  it('says nothing rather than everything when nothing matches', () => {
    expect(filterInstruments(rows, 'wavetable')).toEqual([]);
  });
});
