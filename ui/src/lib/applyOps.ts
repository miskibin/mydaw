// A TS port of crates/engine/src/ops.rs `apply` — used by the in-browser mock backend.
// The real engine is the source of truth; this only has to agree on the happy path and
// reject the same obviously-invalid batches.
import { CLIP_PITCH, MAX_BANDS, STRETCH_MAX, STRETCH_MIN, type Id, type Op, type Project } from '../api/types';
import { applyCompOp } from './comp';
import { applyNotebookOp } from './notebook';
import { applyPatternOp, fitLen as fitPatternLen, followPatternLength, PatternOpError } from './pattern';
import { applyTransform } from './transforms';

export class OpError extends Error {}

const bump = (p: Project, id: Id) => { if (id >= p.nextId) p.nextId = id + 1; };

export function applyOp(p: Project, op: Op): void {
  switch (op.op) {
    case 'putClips': {
      for (const c of op.clips) {
        if (!(c.len > 0) || c.start < 0 || !isFinite(c.start) || !isFinite(c.len)) throw new OpError(`clip ${c.id} has an invalid range`);
        if (c.track >= p.tracks.length || c.track < 0) throw new OpError(`clip ${c.id} is on track ${c.track} which does not exist`);
        const s = c.stretch ?? 1;
        if (!(Number.isFinite(s) && s >= STRETCH_MIN - 1e-9 && s <= STRETCH_MAX + 1e-9)) throw new OpError(`clip ${c.id}: stretch must be ${STRETCH_MIN}..${STRETCH_MAX}`);
        if (c.kind !== 'audio' && (s !== 1 || c.resample)) throw new OpError(`clip ${c.id}: only audio clips can be stretched`);
        const pt = c.pitch ?? 0;
        if (!(Number.isFinite(pt) && Math.abs(pt) <= CLIP_PITCH + 1e-4)) throw new OpError(`clip ${c.id}: pitch must be within ±${CLIP_PITCH} semitones`);
        if (c.kind !== 'audio' && pt !== 0) throw new OpError(`clip ${c.id}: only audio clips can be transposed`);
        const known = c.kind === 'pat' ? p.patterns.some(x => x.id === c.ref) : c.kind === 'auto' ? (p.automations ?? []).some(x => x.id === c.ref) : c.kind === 'comp' ? (p.comps ?? []).some(x => x.id === c.ref) : p.sources.some(x => x.id === c.ref);
        if (!known) throw new OpError(`no clip source with id ${c.ref}`);
        const r = c.tune?.reference;
        if (r) {
          if (!r.melody && !p.sources.some(x => x.id === r.source)) throw new OpError(`no reference source with id ${r.source}`);
          if (r.melody?.notes.some(n => !(n.end > n.start) || n.key < 0 || n.key > 127)) throw new OpError(`clip ${c.id}: melody notes need end > start and a key 0..127`);
          if (r.speed != null && !(r.speed >= 0 && r.speed <= 1000)) throw new OpError(`clip ${c.id}: reference speed must be 0..1000 ms`);
          if ([r.correction, r.vibrato, r.influence].some(v => !(v >= 0 && v <= 1))) throw new OpError(`clip ${c.id}: reference correction, vibrato and influence must be 0..1`);
          if ((r.octave != null && (r.octave % 12 !== 0 || Math.abs(r.octave) > 36)) || (r.key ?? 0) > 11 || (r.scale ?? 0) > 4) throw new OpError(`clip ${c.id}: reference octave must be a multiple of 12 within ±36, key 0..11, scale 0..4`);
        }
      }
      for (const c of op.clips) {
        bump(p, c.id);
        const nc = structuredClone(c);
        // the engine writes the defaults out (stretch 1, not resampled)
        if (nc.stretch === 1) delete nc.stretch;
        if (nc.resample === false) delete nc.resample;
        if (nc.pitch === 0) delete nc.pitch;
        const i = p.clips.findIndex(x => x.id === c.id);
        if (i >= 0) p.clips[i] = nc; else p.clips.push(nc);
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
      if (fx.type === 'eq' && (!fx.bands || fx.bands.length > MAX_BANDS)) throw new OpError(`an EQ has 0..${MAX_BANDS} bands`);
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
    case 'putPattern': case 'putChannel': case 'removeChannel':
      try { applyPatternOp(p, op, true); } catch (e) { throw new OpError((e as PatternOpError).message); }
      return;
    case 'putSource': {
      const s = p.sources.find(x => x.id === op.source.id);
      if (!s) throw new OpError(`no source with id ${op.source.id}`);
      s.name = op.source.name; s.color = op.source.color;
      return;
    }
    case 'removeSource':
      applyPatternOp(p, op, true);
      p.sources = p.sources.filter(s => s.id !== op.id);
      p.clips = p.clips.filter(c => !(c.kind === 'audio' && c.ref === op.id));
      for (const c of p.clips) if (c.tune?.reference && !c.tune.reference.melody && c.tune.reference.source === op.id) c.tune.reference = null;
      applyCompOp(p, op);
      return;
    case 'putComp':
    case 'removeComp':
      try { applyCompOp(p, op); } catch (e) { throw new OpError((e as Error).message); }
      return;
    case 'putNotebook':
      try { applyNotebookOp(p, op.notebook); } catch (e) { throw new OpError((e as Error).message); }
      return;
    case 'setBpm':
      if (!(op.bpm >= 20 && op.bpm <= 999)) throw new OpError('bpm must be 20..999');
      p.bpm = op.bpm; return;
    case 'setLoop':
      if (op.loop && !(op.loop.b > op.loop.a && op.loop.a >= 0)) throw new OpError('loop end must be after its start');
      p.loop = op.loop ? { ...op.loop } : null; return;
    case 'transformNotes': {
      const pt = p.patterns.find(x => x.id === op.pattern);
      if (!pt) throw new OpError(`no pattern with id ${op.pattern}`);
      // generator sizes are bounded exactly like the engine, before anything allocates
      const sizeOk = (x: unknown) => typeof x === 'number' && Number.isInteger(x) && x >= 1 && x <= 512;
      const v = op.verb;
      if (v.kind === 'euclidFill' && (!sizeOk(v.k) || !sizeOk(v.n))) throw new OpError('euclidFill k/n must be 1..=512');
      if (v.kind === 'melody') {
        const bars = v.bars ?? 2;
        if (!Number.isInteger(bars) || bars < 1 || bars > 32) throw new OpError('melody bars must be 1..=32');
      }
      if (v.kind === 'euclidDrums') {
        const bars = v.bars ?? 2;
        if (!Number.isInteger(bars) || bars < 1 || bars > 32) throw new OpError('drums bars must be 1..=32');
        if (v.voices.some(x => !sizeOk(x.k) || !sizeOk(x.n))) throw new OpError('drums voice k/n must be 1..=512');
      }
      const roots = (p.channels ?? []).map(c => ({ id: c.id, root: c.root ?? 60 }));
      const notes = structuredClone(pt.notes);
      applyTransform(notes, op.verb, op.seed ?? 0, op.select ?? null, roots, p.key ?? null, p.scale ?? 1, Math.max(16, Math.round(pt.len * 4)));
      for (const n of notes) {
        if (!(isFinite(n.step) && n.step >= 0 && isFinite(n.len) && n.len > 0)) throw new OpError(`pattern ${op.pattern}: a note has an invalid step/len`);
        const vel = n.v ?? 1;
        if (!(isFinite(vel) && vel >= 0 && vel <= 1)) throw new OpError('note velocity `v` must be 0..1');
        if (n.p != null && !(Number.isInteger(n.p) && n.p >= 0 && n.p <= 127)) throw new OpError('note key `p` must be 0..127');
      }
      const known = new Set((p.channels ?? []).map(c => c.id));
      if (notes.some(n => !known.has(n.ch))) throw new OpError(`pattern ${op.pattern}: a note refers to a missing channel`);
      const oldLen = pt.len;
      pt.notes = notes;
      fitPatternLen(pt);
      // a generator can push notes past the pattern's end: whole-pass clips follow, like putPattern
      if (Math.abs(oldLen - pt.len) > 1e-9) followPatternLength(p, op.pattern, oldLen, pt.len);
      return;
    }
    case 'setKey':
      if (op.key != null && (!Number.isInteger(op.key) || op.key < 0 || op.key > 11)) throw new OpError('key must be 0..11 (0 = C)');
      if (!Number.isInteger(op.scale ?? 1) || (op.scale ?? 1) < 0 || (op.scale ?? 1) > 4) throw new OpError('scale must be 0..4');
      p.key = op.key;
      p.scale = op.scale ?? 1;
      return;
    case 'putAutomation': {
      const a = structuredClone(op.automation);
      if (a.points.some(q => !isFinite(q.t) || !isFinite(q.v) || (q.tension != null && !isFinite(q.tension)))) throw new OpError('automation points must be numbers');
      a.points.sort((x, y) => x.t - y.t);
      for (const q of a.points) {
        q.v = Math.min(1, Math.max(0, q.v));
        // as the engine stores them: no tension / shape fields when straight
        if (q.tension != null) { q.tension = Math.min(1, Math.max(-1, q.tension)); if (!q.tension) delete q.tension; }
        if (q.shape === 'curve') delete q.shape;
      }
      bump(p, a.id);
      const list = p.automations ?? (p.automations = []);
      const i = list.findIndex(x => x.id === a.id);
      if (i >= 0) list[i] = a; else list.push(a);
      return;
    }
    case 'removePattern':
      if (!p.patterns.some(x => x.id === op.id)) throw new OpError(`no pattern with id ${op.id}`);
      p.patterns = p.patterns.filter(x => x.id !== op.id);
      p.clips = p.clips.filter(c => !(c.kind === 'pat' && c.ref === op.id));
      return;
    case 'removeAutomation':
      p.automations = (p.automations ?? []).filter(a => a.id !== op.id);
      p.clips = p.clips.filter(c => !(c.kind === 'auto' && c.ref === op.id));
      return;
  }
}

/** All ops or none. Returns the new project (the input is not modified). */
export function applyAll(p: Project, ops: Op[]): Project {
  const work = structuredClone(p);
  for (const op of ops) applyOp(work, op);
  return work;
}
