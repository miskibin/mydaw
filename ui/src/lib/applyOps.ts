// A TS port of crates/engine/src/ops.rs `apply` — used by the in-browser mock backend.
// The real engine is the source of truth; this only has to agree on the happy path and
// reject the same obviously-invalid batches.
import type { Id, Op, Project } from '../api/types';

export class OpError extends Error {}

const bump = (p: Project, id: Id) => { if (id >= p.nextId) p.nextId = id + 1; };

export function applyOp(p: Project, op: Op): void {
  switch (op.op) {
    case 'putClips': {
      for (const c of op.clips) {
        if (!(c.len > 0) || c.start < 0 || !isFinite(c.start) || !isFinite(c.len)) throw new OpError(`clip ${c.id} has an invalid range`);
        if (c.track >= p.tracks.length || c.track < 0) throw new OpError(`clip ${c.id} is on track ${c.track} which does not exist`);
        const known = c.kind === 'pat' ? p.patterns.some(x => x.id === c.ref) : p.sources.some(x => x.id === c.ref);
        if (!known) throw new OpError(`no clip source with id ${c.ref}`);
      }
      for (const c of op.clips) {
        bump(p, c.id);
        const i = p.clips.findIndex(x => x.id === c.id);
        if (i >= 0) p.clips[i] = structuredClone(c); else p.clips.push(structuredClone(c));
      }
      return;
    }
    case 'removeClips': p.clips = p.clips.filter(c => !op.ids.includes(c.id)); return;
    case 'putTrack': {
      if (!p.inserts.some(m => m.id === op.track.insert)) throw new OpError(`no insert with id ${op.track.insert}`);
      bump(p, op.track.id);
      const i = p.tracks.findIndex(t => t.id === op.track.id);
      if (i >= 0) p.tracks[i] = { ...op.track }; else p.tracks.push({ ...op.track });
      return;
    }
    case 'putInsert': {
      bump(p, op.insert.id);
      const i = p.inserts.findIndex(m => m.id === op.insert.id);
      if (i >= 0) p.inserts[i] = { ...structuredClone(op.insert), fx: p.inserts[i].fx };
      else {
        const m = structuredClone(op.insert);
        for (const f of m.fx) bump(p, f.id);
        m.fx = m.fx.filter(f => f.type !== 'vst3');
        p.inserts.push(m);
      }
      return;
    }
    case 'removeTrack': {
      const idx = p.tracks.findIndex(t => t.id === op.id);
      if (idx < 0) throw new Error('no track ' + op.id);
      if (p.tracks.length === 1) throw new Error('the last track cannot be removed');
      p.tracks.splice(idx, 1);
      p.clips = p.clips.filter(c => c.track !== idx);
      for (const c of p.clips) if (c.track > idx) c.track -= 1;
      return;
    }
    case 'removeInsert': {
      if (op.id === 0) throw new OpError('the master cannot be removed');
      if (!p.inserts.some(m => m.id === op.id)) throw new OpError(`no insert with id ${op.id}`);
      p.inserts = p.inserts.filter(m => m.id !== op.id);
      for (const t of p.tracks) if (t.insert === op.id) t.insert = 0;
      return;
    }
    case 'putFx': {
      const fx = op.fx;
      if (fx.type === 'eq' && (!fx.bands || fx.bands.length !== 7)) throw new OpError('an EQ needs exactly 7 bands');
      if (fx.type === 'vst3' && !fx.plugin) throw new OpError('a VST3 slot needs `plugin`');
      for (const [k, v] of Object.entries(fx.params)) if (!isFinite(v)) throw new OpError(`parameter ${k} is not a number`);
      for (const m of p.inserts) {
        const j = m.fx.findIndex(f => f.id === fx.id);
        if (j < 0) continue;
        const old = m.fx[j];
        if (m.id !== op.insertId || old.type !== fx.type) throw new OpError(`effect ${fx.id} cannot change insert or type; remove and add it`);
        if ((old.plugin?.path ?? '') + (old.plugin?.cid ?? '') !== (fx.plugin?.path ?? '') + (fx.plugin?.cid ?? '')) throw new OpError('a VST3 slot cannot swap its plugin; remove and add it');
        m.fx[j] = structuredClone(fx);
        return;
      }
      const m = p.inserts.find(x => x.id === op.insertId);
      if (!m) throw new OpError(`no insert with id ${op.insertId}`);
      const at = Math.min(op.index ?? m.fx.length, m.fx.length);
      m.fx.splice(at, 0, structuredClone(fx));
      bump(p, fx.id);
      return;
    }
    case 'removeFx': {
      let found = false;
      for (const m of p.inserts) { const n = m.fx.length; m.fx = m.fx.filter(f => f.id !== op.fxId); found ||= m.fx.length !== n; }
      if (!found) throw new OpError(`no effect with id ${op.fxId}`);
      return;
    }
    case 'moveFx': {
      const m = p.inserts.find(x => x.fx.some(f => f.id === op.fxId));
      if (!m) throw new OpError(`no effect with id ${op.fxId}`);
      const from = m.fx.findIndex(f => f.id === op.fxId);
      const [f] = m.fx.splice(from, 1);
      m.fx.splice(Math.min(op.to, m.fx.length), 0, f);
      return;
    }
    case 'putPattern': {
      if (!(op.pattern.len > 0)) throw new OpError('pattern length must be positive');
      bump(p, op.pattern.id);
      const i = p.patterns.findIndex(x => x.id === op.pattern.id);
      if (i >= 0) p.patterns[i] = structuredClone(op.pattern); else p.patterns.push(structuredClone(op.pattern));
      return;
    }
    case 'putSource': {
      const s = p.sources.find(x => x.id === op.source.id);
      if (!s) throw new OpError(`no source with id ${op.source.id}`);
      s.name = op.source.name; s.color = op.source.color;
      return;
    }
    case 'removeSource':
      p.sources = p.sources.filter(s => s.id !== op.id);
      p.clips = p.clips.filter(c => !(c.kind === 'audio' && c.ref === op.id));
      return;
    case 'setBpm':
      if (!(op.bpm >= 20 && op.bpm <= 999)) throw new OpError('bpm must be 20..999');
      p.bpm = op.bpm; return;
    case 'setLoop':
      if (op.loop && !(op.loop.b > op.loop.a && op.loop.a >= 0)) throw new OpError('loop end must be after its start');
      p.loop = op.loop ? { ...op.loop } : null; return;
  }
}

/** All ops or none. Returns the new project (the input is not modified). */
export function applyAll(p: Project, ops: Op[]): Project {
  const work = structuredClone(p);
  for (const op of ops) applyOp(work, op);
  return work;
}
