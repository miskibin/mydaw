import { describe, expect, it } from 'vitest';
import { sketchProject, mockCatalog } from '../api/mockData';
import { CLIP_GAIN_DB, KEYS_RELEASE, VOL_UNITY, type AutoTarget, type Channel, type Clip, type Project } from '../api/types';
import { applyAll } from './applyOps';
import { createAutomationOps, denormalize, normalize, readTarget, targetKey, targetLabel } from './automation';
import { newSynthChannel, synthParam } from './synth';

const cat = mockCatalog();

/** The demo song plus a synth channel, a WAVE Keys channel and an audio clip to automate. */
function project(): Project {
  const p = sketchProject(cat);
  const synth = { ...newSynthChannel(cat.synth, 7001, null), name: 'Lead' };
  const keys: Channel = { ...p.channels![0], id: 7002, name: 'Piano', kind: 'keys', keys: { release: 350, dynamics: 80 } } as Channel;
  p.channels = [...(p.channels ?? []), synth, keys];
  const src = p.sources[0] ?? { id: 7100, name: 'Loop', path: 'loop.wav', color: '#4f8ef7', dur: 2, sr: 48000, ch: 2, peakDb: -1 };
  if (!p.sources.length) p.sources.push(src as Project['sources'][number]);
  const clip: Clip = { id: 7003, track: 0, start: 0, len: 4, offset: 0, kind: 'audio', ref: src.id, muted: false, gain: -6, fadeIn: 0, fadeOut: 0, pan: 0.5, norm: false, rev: false };
  p.clips.push(clip);
  return p;
}

const targets: AutoTarget[] = [
  { kind: 'chanVol', id: 7001 },
  { kind: 'chanPan', id: 7001 },
  { kind: 'synth', chanId: 7001, param: 'flt_cutoff' },
  { kind: 'keys', chanId: 7002, param: 'release' },
  { kind: 'keys', chanId: 7002, param: 'dynamics' },
  { kind: 'clipGain', id: 7003 },
  { kind: 'clipPan', id: 7003 },
];

describe('automation of channels, synth and Keys parameters, and audio clips', () => {
  it('gives every target its own key', () => {
    const keys = targets.map(targetKey);
    expect(new Set(keys).size).toBe(keys.length);
    // a channel's volume and a mixer insert's volume with the same id are different controls
    expect(targetKey({ kind: 'chanVol', id: 1 })).not.toBe(targetKey({ kind: 'insertVol', id: 1 }));
  });

  it('normalises like the engine (rt.rs apply_automation)', () => {
    const p = project();
    // channel volume is the fader position; pans map −1..1 to 0..1
    expect(normalize(p, cat, { kind: 'chanVol', id: 7001 }, VOL_UNITY)).toBeCloseTo(VOL_UNITY, 9);
    expect(normalize(p, cat, { kind: 'chanPan', id: 7001 }, -1)).toBe(0);
    expect(denormalize(p, cat, { kind: 'clipPan', id: 7003 }, 1)).toBe(1);
    // clip gain is linear in dB over CLIP_GAIN_DB
    expect(denormalize(p, cat, { kind: 'clipGain', id: 7003 }, 0)).toBe(CLIP_GAIN_DB.min);
    expect(denormalize(p, cat, { kind: 'clipGain', id: 7003 }, 1)).toBe(CLIP_GAIN_DB.max);
    expect(normalize(p, cat, { kind: 'clipGain', id: 7003 }, 0)).toBeCloseTo(24 / 36, 9);
    // release is logarithmic: engine `lo * (hi / lo)^v`
    const rel = { kind: 'keys' as const, chanId: 7002, param: 'release' as const };
    expect(denormalize(p, cat, rel, 0.5)).toBeCloseTo(KEYS_RELEASE.min * Math.sqrt(KEYS_RELEASE.max / KEYS_RELEASE.min), 6);
    // a synth parameter uses its catalog range (cutoff is logarithmic)
    const d = synthParam(cat.synth, 'flt_cutoff')!;
    expect(d.log).toBe(true);
    expect(denormalize(p, cat, targets[2], 0)).toBeCloseTo(d.min, 6);
    expect(denormalize(p, cat, targets[2], 1)).toBeCloseTo(d.max, 6);
  });

  it('round-trips every target through its normalisation', () => {
    const p = project();
    for (const t of targets) {
      const x = readTarget(p, t) ?? (t.kind === 'synth' ? synthParam(cat.synth, t.param)!.def : 0);
      expect(denormalize(p, cat, t, normalize(p, cat, t, x)), targetKey(t)).toBeCloseTo(x, 4);
    }
  });

  it('snaps a synth choice to an option', () => {
    const p = project();
    const choice = cat.synth.sections.flatMap(s => s.params).find(d => d.options)!;
    const t: AutoTarget = { kind: 'synth', chanId: 7001, param: choice.id };
    expect(Number.isInteger(denormalize(p, cat, t, 0.37))).toBe(true);
  });

  it('reads the current values, and nothing for a control that is gone', () => {
    const p = project();
    expect(readTarget(p, { kind: 'clipGain', id: 7003 })).toBe(-6);
    expect(readTarget(p, { kind: 'clipPan', id: 7003 })).toBe(0.5);
    expect(readTarget(p, { kind: 'keys', chanId: 7002, param: 'dynamics' })).toBe(80);
    expect(readTarget(p, { kind: 'clipGain', id: 424242 })).toBeNull();
    // a pattern clip has no gain to automate
    const pat = p.clips.find(c => c.kind === 'pat')!;
    expect(readTarget(p, { kind: 'clipGain', id: pat.id })).toBeNull();
    // keys parameters of a channel that is not WAVE Keys
    expect(readTarget(p, { kind: 'keys', chanId: 7001, param: 'release' })).toBeNull();
  });

  it('starts a new automation at the control\'s current value (a synth default when the patch leaves it out)', () => {
    const p = project();
    let next = 9000;
    const ch = p.channels!.find(c => c.id === 7001)!;
    if (ch.kind === 'synth') delete ch.synth.params.flt_cutoff;
    const r = createAutomationOps(p, cat, targets[2], 'Lead · Cutoff', '#9b7cf8', 0, 16, () => next++);
    const a = applyAll(p, r.ops).automations!.find(x => x.id === r.autoId)!;
    const def = synthParam(cat.synth, 'flt_cutoff')!.def;
    expect(a.points[0].v).toBeCloseTo(normalize(p, cat, targets[2], def), 9);
    const g = createAutomationOps(p, cat, { kind: 'clipGain', id: 7003 }, 'Loop · Clip volume', '#fff', 0, 16, () => next++);
    expect(applyAll(p, g.ops).automations!.find(x => x.id === g.autoId)!.points[0].v).toBeCloseTo(18 / 36, 9);
  });

  it('labels each target by what it controls', () => {
    const p = project();
    expect(targetLabel(p, cat, { kind: 'chanVol', id: 7001 })).toMatch(/^Lead · /);
    expect(targetLabel(p, cat, targets[2])).toMatch(/^Lead · /);
    expect(targetLabel(p, cat, { kind: 'keys', chanId: 7002, param: 'release' })).toMatch(/^Piano · /);
    expect(targetLabel(p, cat, { kind: 'clipGain', id: 7003 })).toContain(' · ');
  });
});
