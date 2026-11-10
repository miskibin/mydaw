// Automation (playlist envelopes): normalisation of every automatable control to 0..1 exactly
// as the engine does, the value of an envelope at a beat, which clip drives a target at the
// playhead, and the ops that create a new automation clip ("Automatyzuj na playliście").
import { t as tl, tName } from './i18n';
import { CLIP_GAIN_DB, KEYS_DYNAMICS, KEYS_RELEASE, type AutoPoint, type AutoTarget, type Automation, type Catalog, type Channel, type Clip, type Id, type Op, type Project, type Track } from '../api/types';
import { insertTrackOps } from './clipOps';
import { synthParam } from './synth';

const lg = (x: number, a: number, b: number) => Math.log(Math.max(a, Math.min(b, x)) / a) / Math.log(b / a);
const ex = (v: number, a: number, b: number) => a * Math.pow(b / a, v);
const lin = (x: number, a: number, b: number) => (Math.max(a, Math.min(b, x)) - a) / (b - a);
const unlin = (v: number, a: number, b: number) => a + v * (b - a);
const c01 = (v: number) => Math.min(1, Math.max(0, v));

/** EQ band sub-parameters: `band:<i>:f|g|q`. */
export function parseBandParam(param: string): { i: number; k: 'f' | 'g' | 'q' } | null {
  const m = /^band:(\d+):([fgq])$/.exec(param);
  return m ? { i: +m[1], k: m[2] as 'f' | 'g' | 'q' } : null;
}
export const BAND_RANGE = { f: [20, 20000, true], g: [-30, 30, false], q: [0.1, 18, true] } as const;

export function targetKey(t: AutoTarget): string {
  switch (t.kind) {
    case 'fx': return 'fx:' + t.fxId + ':' + t.param;
    case 'mix': return 'mix:' + t.fxId;
    case 'insertVol': return 'iv:' + t.id;
    case 'insertPan': return 'ip:' + t.id;
    case 'trackVol': return 'tv:' + t.id;
    case 'chanVol': return 'cv:' + t.id;
    case 'chanPan': return 'cp:' + t.id;
    case 'synth': return 'sy:' + t.chanId + ':' + t.param;
    case 'keys': return 'ky:' + t.chanId + ':' + t.param;
    case 'clipGain': return 'cg:' + t.id;
    case 'clipPan': return 'cpn:' + t.id;
  }
}

const chanOf = (p: Project, id: Id): Channel | undefined => p.channels?.find(c => c.id === id);
const clipOf = (p: Project, id: Id) => p.clips.find(c => c.id === id && c.kind === 'audio');

function fxOf(p: Project, id: Id) {
  for (const m of p.inserts) { const f = m.fx.find(x => x.id === id); if (f) return { m, f }; }
  return null;
}

/** Plain value → normalised 0..1. */
export function normalize(p: Project, cat: Catalog | null, t: AutoTarget, x: number): number {
  switch (t.kind) {
    case 'mix': case 'insertVol': case 'trackVol': case 'chanVol': return c01(x);
    case 'insertPan': case 'chanPan': case 'clipPan': return c01((x + 1) / 2);
    case 'clipGain': return lin(x, CLIP_GAIN_DB.min, CLIP_GAIN_DB.max);
    case 'keys': return t.param === 'release' ? lg(x, KEYS_RELEASE.min, KEYS_RELEASE.max) : lin(x, KEYS_DYNAMICS.min, KEYS_DYNAMICS.max);
    case 'synth': {
      const d = cat && synthParam(cat.synth, t.param);
      if (!d) return c01(x);
      return d.log ? lg(x, d.min, d.max) : lin(x, d.min, d.max);
    }
    case 'fx': {
      const b = parseBandParam(t.param);
      if (b) { const [a, z, log] = BAND_RANGE[b.k]; return log ? lg(x, a, z) : lin(x, a, z); }
      const ff = fxOf(p, t.fxId); const d = ff && cat?.types[ff.f.type]?.params.find(q => q.id === t.param);
      if (!d) return c01(x);
      return d.log ? lg(x, d.min, d.max) : lin(x, d.min, d.max);
    }
  }
}

/** Normalised 0..1 → plain value (choice params snap to an option). */
export function denormalize(p: Project, cat: Catalog | null, t: AutoTarget, v: number): number {
  v = c01(v);
  switch (t.kind) {
    case 'mix': case 'insertVol': case 'trackVol': case 'chanVol': return v;
    case 'insertPan': case 'chanPan': case 'clipPan': return v * 2 - 1;
    case 'clipGain': return unlin(v, CLIP_GAIN_DB.min, CLIP_GAIN_DB.max);
    case 'keys': return t.param === 'release' ? ex(v, KEYS_RELEASE.min, KEYS_RELEASE.max) : unlin(v, KEYS_DYNAMICS.min, KEYS_DYNAMICS.max);
    case 'synth': {
      const d = cat && synthParam(cat.synth, t.param);
      if (!d) return v;
      const x = d.log ? ex(v, d.min, d.max) : unlin(v, d.min, d.max);
      return d.options ? Math.round(x) : x;
    }
    case 'fx': {
      const b = parseBandParam(t.param);
      if (b) { const [a, z, log] = BAND_RANGE[b.k]; return log ? ex(v, a, z) : unlin(v, a, z); }
      const ff = fxOf(p, t.fxId); const d = ff && cat?.types[ff.f.type]?.params.find(q => q.id === t.param);
      if (!d) return v;
      const x = d.log ? ex(v, d.min, d.max) : unlin(v, d.min, d.max);
      return d.options ? Math.round(x) : x;
    }
  }
}

/** The control's current (static) value in its own unit, or null if the target is gone. */
export function readTarget(p: Project, t: AutoTarget): number | null {
  switch (t.kind) {
    case 'mix': return fxOf(p, t.fxId)?.f.mix ?? (fxOf(p, t.fxId) ? 1 : null);
    case 'insertVol': return p.inserts.find(m => m.id === t.id)?.vol ?? null;
    case 'insertPan': return p.inserts.find(m => m.id === t.id)?.pan ?? null;
    case 'trackVol': return p.tracks.find(x => x.id === t.id)?.vol ?? null;
    case 'chanVol': return chanOf(p, t.id)?.vol ?? null;
    case 'chanPan': return chanOf(p, t.id)?.pan ?? null;
    case 'clipGain': return clipOf(p, t.id)?.gain ?? null;
    case 'clipPan': return clipOf(p, t.id)?.pan ?? null;
    case 'keys': { const c = chanOf(p, t.chanId); return c?.kind === 'keys' ? c.keys[t.param] : null; }
    case 'synth': {
      const c = chanOf(p, t.chanId); if (c?.kind !== 'synth') return null;
      // null for a parameter the patch leaves out: it plays its default (`createAutomationOps` reads it)
      return c.synth.params[t.param] ?? null;
    }
    case 'fx': {
      const ff = fxOf(p, t.fxId); if (!ff) return null;
      const b = parseBandParam(t.param);
      if (b) { const band = ff.f.bands?.[b.i]; return band ? band[b.k] : null; }
      return ff.f.params[t.param] ?? null;
    }
  }
}

/** Envelope value at `t` beats: linear between points, held at the ends (model.rs `value_at`). */
export function valueAt(points: AutoPoint[], t: number): number | null {
  if (!points.length) return null;
  if (t <= points[0].t) return points[0].v;
  let lo = 0, hi = points.length;
  while (lo < hi) { const m = (lo + hi) >> 1; if (points[m].t <= t) lo = m + 1; else hi = m; }
  if (lo >= points.length) return points[points.length - 1].v;
  const a = points[lo - 1], b = points[lo];
  return a.v + (b.v - a.v) * (t - a.t) / Math.max(1e-9, b.t - a.t);
}

/** Automations per target key (for "is this knob automated?"). */
export function automatedKeys(p: Project): Set<string> {
  const used = new Set<Id>();
  for (const c of p.clips) if (c.kind === 'auto' && !c.muted) used.add(c.ref);
  const s = new Set<string>();
  for (const a of p.automations ?? []) if (used.has(a.id)) s.add(targetKey(a.target));
  return s;
}

/**
 * Normalised value driving `target` at `beat` (the last unmuted clip under the playhead wins),
 * or null when no automation clip covers it.
 */
export function automatedValue(p: Project, target: AutoTarget, beat: number): number | null {
  const key = targetKey(target);
  let out: number | null = null;
  for (const c of p.clips) {
    if (c.kind !== 'auto' || c.muted || beat < c.start || beat >= c.start + c.len) continue;
    const a = p.automations?.find(x => x.id === c.ref);
    if (!a || targetKey(a.target) !== key) continue;
    const v = valueAt(a.points, beat - c.start + c.offset);
    if (v != null) out = v;
  }
  return out;
}

/** A track to put a new automation clip on: an empty track (no clips at all), else a new one at the end. */
export function trackForAutomation(p: Project): { index: number; ops: Op[] } {
  const used = new Set(p.clips.map(c => c.track));
  for (let i = 0; i < p.tracks.length; i++) if (!used.has(i) && !p.tracks[i].arm) return { index: i, ops: [] };
  return { index: p.tracks.length, ops: [] };
}

/**
 * Ops that create an automation for `target` holding `value` (plain units): the envelope with two
 * points, a clip of `len` beats at `start`, on an empty or new track.
 */
export function createAutomationOps(p: Project, cat: Catalog | null, target: AutoTarget, name: string, color: string, start: number, len: number, alloc: () => Id): { ops: Op[]; clipId: Id; autoId: Id; track: number } {
  const cur = readTarget(p, target) ?? (target.kind === 'synth' && cat ? synthParam(cat.synth, target.param)?.def : undefined) ?? 0;
  const v = normalize(p, cat, target, cur);
  const autoId = alloc();
  const auto: Automation = { id: autoId, name, color, target, points: [{ t: 0, v }, { t: len, v }] };
  const ops: Op[] = [{ op: 'putAutomation', automation: auto }];
  let track = trackForAutomation(p).index;
  if (track >= p.tracks.length) {
    const tid = alloc();
    ops.push(...insertTrackOps(structuredClone(p), p.tracks.length, tid));
    const put = ops.find(o => o.op === 'putTrack' && o.track.id === tid) as { op: 'putTrack'; track: Track } | undefined;
    if (put) { put.track.name = tl('a.au.label'); put.track.color = color; put.track.h = 48; }
    track = p.tracks.length;
  }
  const clipId = alloc();
  const clip: Clip = { id: clipId, track, start, len, offset: 0, kind: 'auto', ref: autoId, muted: false, gain: 0, fadeIn: 0, fadeOut: 0, pan: 0, norm: false, rev: false };
  ops.push({ op: 'putClips', clips: [clip] });
  return { ops, clipId, autoId, track };
}

/** Human label of a target ("Vocal · EQ · Band 3 Gain"). */
export function targetLabel(p: Project, cat: Catalog | null, t: AutoTarget): string {
  const insName = (id: Id) => tName(p.inserts.find(m => m.id === id)?.name ?? '?');
  switch (t.kind) {
    case 'insertVol': return insName(t.id) + ' · ' + tl('a.au.vol');
    case 'insertPan': return insName(t.id) + ' · ' + tl('a.au.pan');
    case 'trackVol': return (p.tracks.find(x => x.id === t.id)?.name ?? '?') + ' · ' + tl('a.au.trackVol');
    case 'chanVol': return (chanOf(p, t.id)?.name ?? '?') + ' · ' + tl('a.au.vol');
    case 'chanPan': return (chanOf(p, t.id)?.name ?? '?') + ' · ' + tl('a.au.pan');
    case 'keys': return (chanOf(p, t.chanId)?.name ?? '?') + ' · ' + tl(t.param === 'release' ? 'b.kp.release' : 'b.kp.dynamics');
    case 'synth': return (chanOf(p, t.chanId)?.name ?? '?') + ' · ' + tName((cat && synthParam(cat.synth, t.param)?.label) ?? t.param);
    case 'clipGain': case 'clipPan': {
      const c = clipOf(p, t.id), src = c && p.sources.find(s => s.id === c.ref);
      return (src?.name ?? '?') + ' · ' + tl(t.kind === 'clipGain' ? 'a.au.clipGain' : 'a.au.clipPan');
    }
    case 'mix': { const ff = fxOf(p, t.fxId); return ff ? tName(ff.m.name) + ' · ' + tName(cat?.types[ff.f.type]?.name ?? ff.f.type) + ' · Mix' : 'Mix'; }
    case 'fx': {
      const ff = fxOf(p, t.fxId); if (!ff) return t.param;
      const fxn = tName(cat?.types[ff.f.type]?.name ?? ff.f.type);
      const b = parseBandParam(t.param);
      const pn = b ? 'Band ' + (b.i + 1) + ' ' + ({ f: 'Freq', g: 'Gain', q: 'Q' })[b.k] : tName(cat?.types[ff.f.type]?.params.find(q => q.id === t.param)?.label ?? t.param);
      return tName(ff.m.name) + ' · ' + fxn + ' · ' + pn;
    }
  }
}

/** Clamp/sort helper for edited envelopes. */
export function sortPoints(pts: AutoPoint[]): AutoPoint[] {
  return pts.map(q => ({ t: Math.max(0, q.t), v: c01(q.v) })).sort((a, b) => a.t - b.t);
}
