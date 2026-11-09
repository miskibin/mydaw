// The built-in synth's channel data: presets, parameter values and new channels. Pure logic over
// the catalog (`Catalog.synth`, served by the engine; the mock serves the generated copy in
// api/synthCatalog.ts). Mirrors crates/engine/src/poly/params.rs.
import { VOL_UNITY, type Channel, type Id, type ParamDef, type SynthCatalog, type SynthPreset, type SynthState } from '../api/types';

/** Every parameter definition, in panel order. */
export const synthParams = (cat: SynthCatalog): ParamDef[] => cat.sections.flatMap(s => s.params);

export function synthParam(cat: SynthCatalog, id: string): ParamDef | undefined {
  for (const s of cat.sections) for (const d of s.params) if (d.id === id) return d;
  return undefined;
}

/** The default value of every parameter (a blank patch). */
export function synthDefaults(cat: SynthCatalog): Record<string, number> {
  return Object.fromEntries(synthParams(cat).map(d => [d.id, d.def]));
}

/** The value a channel plays for `id` (its own, else the default). */
export function synthValue(cat: SynthCatalog, s: SynthState, id: string): number {
  return s.params[id] ?? synthParam(cat, id)?.def ?? 0;
}

/** Clamps like the engine (`params::clamp`): into the range, choices rounded to an option. */
export function clampSynth(d: ParamDef, v: number): number {
  if (!isFinite(v)) return d.def;
  const x = Math.min(d.max, Math.max(d.min, v));
  return d.options ? Math.round(x) : x;
}

export const synthPreset = (cat: SynthCatalog, name: string): SynthPreset | undefined => cat.presets.find(p => p.name === name);

/** A synth state holding preset `name` (the blank patch when there is no such preset). */
export function presetState(cat: SynthCatalog, name: string | null): SynthState {
  const p = name ? synthPreset(cat, name) : undefined;
  return p ? { preset: p.name, params: { ...p.params } } : { preset: null, params: synthDefaults(cat) };
}

/** Key a step plays: basses an octave and a half lower than everything else. */
export const synthRoot = (category: string | undefined) => (category === '808' || category === 'Bas' ? 36 : 60);

/** A new synth channel playing preset `name` (or the blank patch). */
export function newSynthChannel(cat: SynthCatalog, id: Id, name: string | null, color = '#9b7cf8'): Channel {
  const synth = presetState(cat, name);
  return {
    id, name: synth.preset ?? 'Synth', color, kind: 'synth', synth,
    vol: VOL_UNITY, pan: 0, mute: false, solo: false, insert: null, root: synthRoot(name ? synthPreset(cat, name)?.category : undefined),
  };
}

/** `c` with one parameter changed (clamped); the preset name is dropped (the sound is edited). */
export function withSynthParam(cat: SynthCatalog, c: Channel, id: string, v: number): Channel {
  if (c.kind !== 'synth') return c;
  const d = synthParam(cat, id);
  if (!d) return c;
  return { ...c, synth: { preset: null, params: { ...c.synth.params, [id]: clampSynth(d, v) } } };
}

/** `c` switched to preset `name` (keeps the channel's name, routing and levels). */
export function withSynthPreset(cat: SynthCatalog, c: Channel, name: string): Channel {
  if (c.kind !== 'synth' || !synthPreset(cat, name)) return c;
  return { ...c, synth: presetState(cat, name) };
}

/** Engine validation of a synth state (`ops.rs` PutChannel): null when valid, else the error. */
export function synthError(cat: SynthCatalog, s: SynthState): string | null {
  for (const [k, v] of Object.entries(s.params ?? {})) {
    if (!synthParam(cat, k)) return `synth has no parameter \`${k}\``;
    if (typeof v !== 'number' || !isFinite(v)) return `synth parameter \`${k}\` must be a number`;
  }
  return null;
}
