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
    const layout = editorLayout(info('effects.vocoder', ['a', 'b', 'c', 'd', 'e']));
    expect(layout.kind).toBe('generic');
    expect(layout.primary.map((d) => d.id)).toEqual(['a', 'b', 'c', 'd']);
    expect(layout.advanced.map((d) => d.id)).toEqual(['e']);
  });

  it('gives the modulation effects their own displays and knob rows', () => {
    const flanger = editorLayout(
      info('effects.flanger', ['rate', 'depth', 'delay', 'feedback', 'spread', 'mix']),
    );
    expect(flanger.kind).toBe('flanger');
    expect(flanger.primary.map((d) => d.id)).toEqual([
      'delay',
      'rate',
      'depth',
      'feedback',
      'mix',
    ]);
    expect(flanger.advanced.map((d) => d.id)).toEqual(['spread']);

    // The auto-filter's cutoff and resonance are dragged on the curve, so they
    // belong in the drawer rather than on the row under it -- but they must
    // still be there, because a curve is not a keyboard.
    const filter = editorLayout(
      info('effects.autofilter', ['mode', 'cutoff', 'res', 'lfo_amount', 'lfo_rate', 'mix']),
    );
    expect(filter.kind).toBe('autofilter');
    expect(filter.primary.map((d) => d.id)).toEqual(['lfo_amount', 'lfo_rate', 'mix']);
    expect(filter.advanced.map((d) => d.id)).toContain('cutoff');
    expect(filter.advanced.map((d) => d.id)).toContain('res');
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
        'effects.autofilter',
        'effects.bitcrusher',
        'effects.chorus',
        'effects.compressor',
        'effects.delay',
        'effects.eq',
        'effects.flanger',
        'effects.limiter',
        'effects.phaser',
        'effects.reverb',
        'effects.saturator',
        'effects.utility',
      ].sort(),
    );
    expect(editorKind('effects.delay')).toBe('delay');
    // Every effect with a compact card also has a bespoke editor, or the
    // window would open on a generic knob grid the card already implied.
    for (const typeId of Object.keys(COMPACT_CARDS)) {
      expect(editorKind(typeId)).not.toBe('generic');
    }
  });
});
