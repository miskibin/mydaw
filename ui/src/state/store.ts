// The engine's state, mirrored. Rust owns the project and undo; this store holds the latest
// StateDto plus local optimistic edits (applied immediately so drags feel instant).
import { batch } from 'solid-js';
import { createStore, produce, reconcile } from 'solid-js/store';
import type { Catalog, Clip, Fx, Id, Insert, Op, Project, StateDto, TransportDto } from '../api/types';
import { applyCompOp, compExtent } from '../lib/comp';
import { applyPatternOp } from '../lib/pattern';

export interface EngineState {
  ready: boolean;
  project: Project;
  revision: number;
  canUndo: boolean;
  canRedo: boolean;
  transport: TransportDto;
  path: string | null;
  dirty: boolean;
  catalog: Catalog | null;
  /** Comp id → take auditioned instead of the comp (live only). */
  audition: Record<string, Id>;
}

const emptyProject = (): Project => ({ version: 2, bpm: 128, tracks: [], inserts: [], patterns: [], sources: [], clips: [], loop: null, nextId: 1 });

export const [S, setS] = createStore<EngineState>({
  ready: false, project: emptyProject(), revision: 0, canUndo: false, canRedo: false,
  transport: { playing: false, rec: false, monitor: false, metro: false, countIn: true, startBeat: 0 },
  path: null, dirty: false, catalog: null, audition: {},
});

// ---- ids ----
let localNext = 1;
/** Allocate a new id (project.nextId, increased locally). */
export function allocId(): Id {
  const id = Math.max(localNext, S.project.nextId);
  localNext = id + 1;
  return id;
}

// ---- replacing from the engine ----
export function replaceState(dto: StateDto) {
  batch(() => {
    setS('project', reconcile(dto.project, { key: 'id', merge: true }));
    setS({ revision: dto.revision, canUndo: dto.canUndo, canRedo: dto.canRedo, path: dto.path, dirty: dto.dirty, ready: true });
    setS('transport', reconcile(dto.transport));
    setS('audition', reconcile(dto.audition ?? {}));
  });
  localNext = Math.max(localNext, dto.project.nextId);
}

// ---- optimistic local application (mutates in place so identities and signals stay fine-grained) ----
function assignClip(dst: Clip, src: Clip) {
  for (const k of Object.keys(src) as (keyof Clip)[]) {
    if (k === 'tune') { if (JSON.stringify(dst.tune ?? null) !== JSON.stringify(src.tune ?? null)) dst.tune = src.tune ? structuredClone(src.tune) : null; continue; }
    if (dst[k] !== src[k]) (dst as unknown as Record<string, unknown>)[k] = src[k];
  }
  if (!('tune' in src) && dst.tune) dst.tune = null;
}
function assignFx(dst: Fx, src: Fx) {
  if (dst.bypass !== src.bypass) dst.bypass = src.bypass;
  if ((dst.mix ?? 1) !== (src.mix ?? 1)) dst.mix = src.mix ?? 1;
  if (dst.preset !== src.preset) dst.preset = src.preset ?? null;
  for (const k of Object.keys(src.params)) if (dst.params[k] !== src.params[k]) dst.params[k] = src.params[k];
  for (const k of Object.keys(dst.params)) if (!(k in src.params)) delete dst.params[k];
  if (src.bands) {
    if (!dst.bands || dst.bands.length !== src.bands.length) dst.bands = src.bands.map(b => ({ ...b }));
    else src.bands.forEach((b, i) => {
      const d = dst.bands![i];
      if (d.on !== b.on) d.on = b.on; if (d.type !== b.type) d.type = b.type; if (d.f !== b.f) d.f = b.f; if (d.g !== b.g) d.g = b.g; if (d.q !== b.q) d.q = b.q;
      if (d.slope !== b.slope) d.slope = b.slope; if (d.place !== b.place) d.place = b.place;
      const dd = d.dynamic ?? null, sd = b.dynamic ?? null;
      if (!sd) { if (dd) d.dynamic = null; } else if (!dd || dd.range !== sd.range || dd.thr !== sd.thr) d.dynamic = { ...sd };
    });
  }
  if (src.plugin) dst.plugin = { ...src.plugin };
}
const clone = <T,>(v: T): T => structuredClone(v);

function applyInPlace(p: Project, op: Op) {
  switch (op.op) {
    case 'putClips': {
      const idx = new Map<Id, number>(); p.clips.forEach((c, i) => idx.set(c.id, i));
      for (const c of op.clips) {
        const i = idx.get(c.id);
        if (i != null) assignClip(p.clips[i], c); else { p.clips.push(clone(c)); idx.set(c.id, p.clips.length - 1); }
        if (c.id >= p.nextId) p.nextId = c.id + 1;
      }
      return;
    }
    case 'removeClips': { const s = new Set(op.ids); p.clips = p.clips.filter(c => !s.has(c.id)); return; }
    case 'putTrack': {
      const t = p.tracks.find(x => x.id === op.track.id);
      if (t) Object.assign(t, op.track); else p.tracks.push(clone(op.track));
      if (op.track.id >= p.nextId) p.nextId = op.track.id + 1;
      return;
    }
    case 'putInsert': {
      const m = p.inserts.find(x => x.id === op.insert.id);
      const { fx, ...rest } = op.insert;
      if (m) Object.assign(m, rest); else p.inserts.push({ ...clone(rest), fx: clone(fx.filter(f => f.type !== 'vst3')) } as Insert);
      if (op.insert.id >= p.nextId) p.nextId = op.insert.id + 1;
      return;
    }
    case 'removeInsert':
      p.inserts = p.inserts.filter(m => m.id !== op.id);
      for (const t of p.tracks) if (t.insert === op.id) t.insert = 0;
      return;
    case 'putFx': {
      for (const m of p.inserts) {
        const f = m.fx.find(x => x.id === op.fx.id);
        if (f) { assignFx(f, op.fx); return; }
      }
      const m = p.inserts.find(x => x.id === op.insertId); if (!m) return;
      m.fx.splice(Math.min(op.index ?? m.fx.length, m.fx.length), 0, clone(op.fx));
      if (op.fx.id >= p.nextId) p.nextId = op.fx.id + 1;
      return;
    }
    case 'removeFx': for (const m of p.inserts) { const i = m.fx.findIndex(f => f.id === op.fxId); if (i >= 0) m.fx.splice(i, 1); } return;
    case 'moveFx': {
      const m = p.inserts.find(x => x.fx.some(f => f.id === op.fxId)); if (!m) return;
      const i = m.fx.findIndex(f => f.id === op.fxId), to = Math.min(op.to, m.fx.length - 1);
      if (i === to) return;
      const [f] = m.fx.splice(i, 1); m.fx.splice(to, 0, f);
      return;
    }
    case 'putPattern': case 'putChannel': case 'removeChannel': applyPatternOp(p, op, false); return;
    case 'putSource': { const s = p.sources.find(x => x.id === op.source.id); if (s) { s.name = op.source.name; s.color = op.source.color; } return; }
    case 'putComp': case 'removeComp': applyCompOp(p, op, false); return;
    case 'removeSource':
      applyCompOp(p, op, false);
      applyPatternOp(p, op, false);
      p.sources = p.sources.filter(s => s.id !== op.id);
      p.clips = p.clips.filter(c => !(c.kind === 'audio' && c.ref === op.id));
      return;
    case 'setBpm': p.bpm = op.bpm; return;
    case 'setLoop': p.loop = op.loop ? { ...op.loop } : null; return;
    case 'putAutomation': {
      const a = clone(op.automation); a.points.sort((x, y) => x.t - y.t);
      if (!p.automations) p.automations = [];
      const x = p.automations.find(q => q.id === a.id);
      if (x) { x.name = a.name; x.color = a.color; x.target = a.target; x.points = a.points; } else p.automations.push(a);
      if (a.id >= p.nextId) p.nextId = a.id + 1;
      return;
    }
    case 'removeAutomation':
      p.automations = (p.automations ?? []).filter(a => a.id !== op.id);
      p.clips = p.clips.filter(c => !(c.kind === 'auto' && c.ref === op.id));
      return;
  }
}

export function applyLocal(ops: Op[]) {
  if (!ops.length) return;
  setS('project', produce(p => { for (const op of ops) applyInPlace(p, op); }));
  if (!S.dirty) setS('dirty', true);
}

// ---- lookups ----
export function findFx(fxId: Id): { m: Insert; f: Fx } | null {
  for (const m of S.project.inserts) { const f = m.fx.find(x => x.id === fxId); if (f) return { m, f }; }
  return null;
}
export const insertById = (id: Id) => S.project.inserts.find(m => m.id === id);
export const srcOf = (c: Clip) => c.kind === 'pat' ? S.project.patterns.find(p => p.id === c.ref) : c.kind === 'auto' ? S.project.automations?.find(a => a.id === c.ref) : c.kind === 'comp' ? S.project.comps?.find(a => a.id === c.ref) : S.project.sources.find(s => s.id === c.ref);
export const autoById = (id: Id) => S.project.automations?.find(a => a.id === id);
export const spb = () => 60 / S.project.bpm;
export function srcLenBeats(kind: Clip['kind'], ref: Id): number {
  if (kind === 'pat') return S.project.patterns.find(p => p.id === ref)?.len ?? 4;
  if (kind === 'comp') { const cp = S.project.comps?.find(x => x.id === ref); return cp ? compExtent(cp)[1] : 4; }
  if (kind === 'auto') return Infinity; // an envelope holds its last value: auto clips can be any length
  const s = S.project.sources.find(x => x.id === ref);
  return s ? s.dur / spb() : 4;
}
export function insName(m: Insert): string {
  const i = S.project.inserts.indexOf(m);
  return m.id === 0 ? 'Master' : 'Ins ' + i + ' · ' + m.name;
}
export function setTransport(t: Partial<TransportDto>) { setS('transport', t); }
