import { describe, expect, it } from 'vitest';
import { SYNTH_CATALOG as cat } from '../api/synthCatalog';
import { emptyProject, mockCatalog } from '../api/mockData';
import { applyPatternOp, isPitched, PatternOpError } from './pattern';
import { clampSynth, newSynthChannel, presetState, synthDefaults, synthParam, synthParams, synthValue, withSynthParam, withSynthPreset } from './synth';

describe('synth catalog', () => {
  it('has the panel sections in order and complete presets', () => {
    expect(cat.sections.map(s => s.id)).toEqual(['osc1', 'osc2', 'subnoise', 'unison', 'filter', 'ampenv', 'modenv', 'lfo1', 'lfo2', 'matrix', 'voice', 'fx']);
    const n = synthParams(cat).length;
    expect(cat.presets.length).toBeGreaterThanOrEqual(30);
    for (const p of cat.presets) expect(Object.keys(p.params).length).toBe(n);
    expect(cat.presets.some(p => p.name === '808 Czysty')).toBe(true);
    expect(mockCatalog().synth.presets.length).toBe(cat.presets.length);
  });
});

describe('synth channels', () => {
  it('a new channel plays its preset, basses at a low root', () => {
    const c = newSynthChannel(cat, 7, '808 Czysty');
    expect(c.kind).toBe('synth');
    if (c.kind !== 'synth') return;
    expect(c.synth.preset).toBe('808 Czysty');
    expect(c.root).toBe(36);
    expect(isPitched(c)).toBe(true);
    const blank = newSynthChannel(cat, 8, null);
    if (blank.kind === 'synth') expect(blank.synth.params).toEqual(synthDefaults(cat));
  });

  it('a knob clamps, keeps the other values and drops the preset name', () => {
    const c = newSynthChannel(cat, 7, 'Pad Ciepły');
    const d = withSynthParam(cat, c, 'flt_cutoff', 99999);
    if (d.kind !== 'synth' || c.kind !== 'synth') throw new Error('kind');
    expect(d.synth.params.flt_cutoff).toBe(synthParam(cat, 'flt_cutoff')!.max);
    expect(d.synth.preset).toBeNull();
    expect(d.synth.params.amp_r).toBe(c.synth.params.amp_r);
    expect(clampSynth(synthParam(cat, 'o1_wave')!, 2.6)).toBe(3);
    expect(synthValue(cat, { preset: null, params: {} }, 'amp_s')).toBe(synthParam(cat, 'amp_s')!.def);
    const e = withSynthPreset(cat, d, 'Supersaw Lead');
    if (e.kind === 'synth') expect(e.synth).toEqual(presetState(cat, 'Supersaw Lead'));
  });

  it('putChannel validates synth parameters like the engine', () => {
    const p = emptyProject(mockCatalog());
    const c = newSynthChannel(cat, p.nextId, 'Pluck Trap');
    expect(applyPatternOp(p, { op: 'putChannel', channel: c }, true)).toBe(true);
    expect(p.channels!.some(x => x.id === c.id)).toBe(true);
    const bad = structuredClone(c);
    if (bad.kind === 'synth') bad.synth.params.nope = 1;
    expect(() => applyPatternOp(p, { op: 'putChannel', channel: bad }, true)).toThrow(PatternOpError);
  });
});
