import { describe, expect, it } from 'vitest';

import { COMPACT_CARDS, editorKind, editorLayout } from './effectLayout';
import type { EffectInfoDto, ParamDefDto } from '../api/types';

function param(id: string, extra: Partial<ParamDefDto> = {}): ParamDefDto {
  return {
    id,
    name: id,
    unit: '',
    min: 0,
    max: 1,
    defaultValue: 0,
    curve: 'linear',
    step: null,
    smoothingMs: 10,
    ...extra,
  };
}

function info(typeId: string, ids: string[]): EffectInfoDto {
  return {
    typeId,
    name: typeId,
    description: '',
    params: [param('bypass', { smoothingMs: 0, step: 1, max: 1 }), ...ids.map((id) => param(id))],
    readouts: [],
  };
}

describe('editorLayout', () => {
  it('puts the EQ output on the primary row and the rest in Advanced', () => {
    const layout = editorLayout(
      info('effects.eq', ['hp.freq', 'bell1.gain', 'output']),
    );
    expect(layout.kind).toBe('eq');
    expect(layout.primary.map((d) => d.id)).toEqual(['output']);
    expect(layout.advanced.map((d) => d.id)).toEqual(['hp.freq', 'bell1.gain']);
    expect(layout.advanced.some((d) => d.id === 'bypass')).toBe(false);
  });

  it('keeps compressor threshold off the primary row so the curve owns it', () => {
    const layout = editorLayout(
      info('effects.compressor', ['threshold', 'ratio', 'attack', 'release', 'mix', 'makeup']),
    );
    expect(layout.kind).toBe('compressor');
    expect(layout.primary.map((d) => d.id)).toEqual(['attack', 'release', 'mix']);
    expect(layout.advanced.map((d) => d.id)).toContain('threshold');
  });

  it('falls back to the first four knobs for an unknown effect', () => {
    const layout = editorLayout(info('effects.chorus', ['a', 'b', 'c', 'd', 'e']));
    expect(layout.kind).toBe('generic');
    expect(layout.primary.map((d) => d.id)).toEqual(['a', 'b', 'c', 'd']);
    expect(layout.advanced.map((d) => d.id)).toEqual(['e']);
  });

  it('puts the limiter ceiling on the primary row next to gain', () => {
    const layout = editorLayout(info('effects.limiter', ['gain', 'ceiling', 'release', 'lookahead']));
    expect(layout.kind).toBe('limiter');
    expect(layout.primary.map((d) => d.id)).toEqual(['gain', 'ceiling', 'release']);
    expect(layout.advanced.map((d) => d.id)).toEqual(['lookahead']);
  });

  it('puts the reverb space knobs on the primary row', () => {
    const layout = editorLayout(info('effects.reverb', ['mix', 'decay', 'size', 'damp', 'predelay']));
    expect(layout.kind).toBe('reverb');
    expect(layout.primary.map((d) => d.id)).toEqual(['mix', 'decay', 'size', 'damp']);
    expect(layout.advanced.map((d) => d.id)).toEqual(['predelay']);
  });

  it('still has a compact card table for every built-in effect', () => {
    expect(Object.keys(COMPACT_CARDS).sort()).toEqual(
      [
        'effects.compressor',
        'effects.delay',
        'effects.eq',
        'effects.limiter',
        'effects.reverb',
        'effects.saturator',
        'effects.utility',
      ].sort(),
    );
    expect(editorKind('effects.delay')).toBe('delay');
  });
});
