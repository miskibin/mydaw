// Pure take-lane / comping logic. Mirrors crates/engine/src/model.rs (`Comp::write`,
// `merge_regions`) and comp.rs (`land_takes`) so the UI, the mock backend and the engine
// agree. Times are comp beats unless a function says otherwise.
import { DEFAULT_XFADE, type Clip, type Comp, type CompRegion, type Id, type Project, type Take } from '../api/types';

const EPS = 1e-9;

/** Comp beat ↔ timeline beat for a clip that plays the comp. */
export const toComp = (c: Clip, beat: number) => beat - c.start + c.offset;
export const toTimeline = (c: Clip, cb: number) => cb - c.offset + c.start;

export const takeEnd = (t: Take) => t.start + t.len;

/** Joins touching regions of the same take (input sorted by `from`). */
export function mergeRegions(sorted: CompRegion[]): CompRegion[] {
  const out: CompRegion[] = [];
  for (const r of sorted) {
    const m = out[out.length - 1];
    if (m && m.take === r.take && Math.abs(r.from - m.to) < EPS) m.to = Math.max(m.to, r.to);
    else out.push({ ...r });
  }
  return out;
}

/**
 * Swipe comping: `take` plays over [from, to) (clipped to the take), replacing whatever
 * played there; neighbouring pieces of the same take merge. Returns new regions.
 */
export function writeRange(comp: Comp, take: Id, from: number, to: number): CompRegion[] {
  const t = comp.takes.find(x => x.id === take);
  if (!t) return comp.regions.map(r => ({ ...r }));
  const a = Math.max(from, t.start), b = Math.min(to, takeEnd(t));
  if (b - a <= EPS) return comp.regions.map(r => ({ ...r }));
  const out: CompRegion[] = [];
  for (const r of comp.regions) {
    if (r.to <= a || r.from >= b) { out.push({ ...r }); continue; }
    if (r.from < a) out.push({ ...r, to: a });
    if (r.to > b) out.push({ ...r, from: b, xfade: null });
  }
  out.push({ take, from: a, to: b });
  out.sort((x, y) => x.from - y.from);
  return mergeRegions(out).map(r => (r.xfade == null ? { take: r.take, from: r.from, to: r.to } : r));
}

/** The region playing at comp beat `b` (index), or -1. */
export function regionAt(regions: CompRegion[], b: number): number {
  for (let i = 0; i < regions.length; i++) if (b >= regions[i].from && b < regions[i].to) return i;
  return -1;
}

/** A boundary of the comp: where a region starts or ends. `left`/`right` are region indices (−1 = silence). */
export interface Boundary { at: number; left: number; right: number }
export function boundaries(regions: CompRegion[]): Boundary[] {
  const out: Boundary[] = [];
  regions.forEach((r, i) => {
    const prev = out[out.length - 1];
    if (prev && Math.abs(prev.at - r.from) < EPS) prev.right = i;
    else out.push({ at: r.from, left: -1, right: i });
    out.push({ at: r.to, left: i, right: -1 });
  });
  return out;
}

/** Crossfade length (s) used at a boundary: the region starting there, else the region ending there. */
export function boundaryXfade(comp: Comp, b: Boundary): number {
  const r = b.right >= 0 ? comp.regions[b.right] : comp.regions[b.left];
  return r?.xfade ?? comp.xfade ?? DEFAULT_XFADE;
}

/** Sets the crossfade of one boundary (stored on the region that starts there, else the one that ends there). */
export function setBoundaryXfade(comp: Comp, b: Boundary, secs: number | null): CompRegion[] {
  const i = b.right >= 0 ? b.right : b.left;
  return comp.regions.map((r, k) => (k === i ? { ...r, xfade: secs } : { ...r }));
}

/**
 * Moves boundary `b` to comp beat `to`: both touching regions follow, each keeps at least
 * `minLen` and stays inside its take. Returns new regions.
 */
export function moveBoundary(comp: Comp, b: Boundary, to: number, minLen: number): CompRegion[] {
  const rs = comp.regions.map(r => ({ ...r }));
  const L = b.left >= 0 ? rs[b.left] : null, R = b.right >= 0 ? rs[b.right] : null;
  let lo = -Infinity, hi = Infinity;
  if (L) {
    const t = comp.takes.find(x => x.id === L.take);
    lo = Math.max(lo, L.from + minLen); if (t) hi = Math.min(hi, takeEnd(t));
    if (!R && b.left + 1 < rs.length) hi = Math.min(hi, rs[b.left + 1].from);
  }
  if (R) {
    const t = comp.takes.find(x => x.id === R.take);
    hi = Math.min(hi, R.to - minLen); if (t) lo = Math.max(lo, t.start);
    if (!L && b.right > 0) lo = Math.max(lo, rs[b.right - 1].to);
  }
  if (lo > hi) return rs;
  const x = Math.min(hi, Math.max(lo, to));
  if (L) L.to = x;
  if (R) R.from = x;
  return rs;
}

/**
 * Ctrl+↑/↓: the range [from, to) switches to the previous (dir −1) or next (+1) take that
 * covers it. Returns null when there is no such take.
 */
export function swapRange(comp: Comp, from: number, to: number, dir: -1 | 1): { regions: CompRegion[]; take: Take } | null {
  const i = regionAt(comp.regions, (from + to) / 2);
  const cur = i >= 0 ? comp.takes.findIndex(t => t.id === comp.regions[i].take) : dir > 0 ? -1 : comp.takes.length;
  for (let k = cur + dir; k >= 0 && k < comp.takes.length; k += dir) {
    const t = comp.takes[k];
    if (t.start < to - EPS && takeEnd(t) > from + EPS) return { regions: writeRange(comp, t.id, from, to), take: t };
  }
  return null;
}

/** Comp without a take (its regions go silent). */
export function removeTake(comp: Comp, id: Id): Comp {
  return { ...comp, takes: comp.takes.filter(t => t.id !== id), regions: comp.regions.filter(r => r.take !== id) };
}
/** Takes no region plays. */
export const unusedTakes = (comp: Comp): Take[] => comp.takes.filter(t => !comp.regions.some(r => r.take === t.id));
/** Beats of the comp a take plays. */
export const usedBeats = (comp: Comp, id: Id) => comp.regions.filter(r => r.take === id).reduce((s, r) => s + r.to - r.from, 0);

/** Earliest take start and latest take end. */
export function compExtent(comp: Comp): [number, number] {
  if (!comp.takes.length) return [0, 0];
  return [Math.min(...comp.takes.map(t => t.start)), Math.max(...comp.takes.map(takeEnd))];
}

const plainAudio = (c: Clip) => c.kind === 'audio' && !c.rev && !c.norm && !(c.tune && (c.tune.notes.length || c.tune.pencil?.length));

/**
 * Puts freshly recorded passes — [source, timeline start, length] in beats, oldest first —
 * on `track` (mutates `p`; mirrors `comp::land_takes`). `alloc` hands out new ids.
 */
export function landTakes(p: Project, track: number, passes: [Id, number, number][], alloc: () => Id) {
  if (!passes.length) return;
  const comps = p.comps ?? (p.comps = []);
  const r0 = Math.min(...passes.map(x => x[1])), r1 = Math.max(...passes.map(x => x[1] + x[2]));
  const overlaps = (c: Clip) => c.track === track && c.start < r1 - EPS && c.start + c.len > r0 + EPS;
  const hit = p.clips.find(c => c.kind === 'comp' && overlaps(c) && comps.some(x => x.id === c.ref));
  if (hit) {
    const comp = comps.find(x => x.id === hit.ref)!;
    const origin = hit.start - hit.offset;
    const fresh: Id[] = [];
    for (const [src, start, len] of passes) {
      const id = alloc(); fresh.push(id);
      comp.takes.push({ id, name: 'Take ' + (comp.takes.length + 1), ref: src, start: start - origin, len, offset: 0 });
    }
    const lo = compExtent(comp)[0];
    if (lo < 0) {
      for (const t of comp.takes) t.start -= lo;
      for (const r of comp.regions) { r.from -= lo; r.to -= lo; }
      for (const c of p.clips) if (c.kind === 'comp' && c.ref === comp.id) c.offset -= lo;
    }
    // oldest first: the newest pass plays wherever it has audio
    for (const id of fresh) { const t = comp.takes.find(x => x.id === id)!; comp.regions = writeRange(comp, id, t.start, takeEnd(t)); }
    const ns = Math.min(hit.start, r0), ne = Math.max(hit.start + hit.len, r1);
    hit.offset -= hit.start - ns; hit.start = ns; hit.len = ne - ns;
    return;
  }
  const old = p.clips.filter(c => plainAudio(c) && overlaps(c)).sort((a, b) => a.start - b.start);
  if (!old.length && passes.length === 1) {
    const [src, start, len] = passes[0];
    p.clips.push({ id: alloc(), track, start, len, offset: 0, kind: 'audio', ref: src, muted: false, gain: 0, fadeIn: 0, fadeOut: 0, pan: 0, norm: false, rev: false });
    return;
  }
  const all: [Id, number, number, number][] = [...old.map(c => [c.ref, c.start, c.len, c.offset] as [Id, number, number, number]), ...passes.map(([s, a, l]) => [s, a, l, 0] as [Id, number, number, number])];
  const origin = Math.min(...all.map(x => x[1])), end = Math.max(...all.map(x => x[1] + x[2]));
  const comp: Comp = { id: alloc(), name: 'Comp ' + (comps.length + 1), takes: [], regions: [], xfade: DEFAULT_XFADE };
  all.forEach(([src, start, len, offset], k) => comp.takes.push({ id: alloc(), name: 'Take ' + (k + 1), ref: src, start: start - origin, len, offset }));
  // what played before, then each pass on top, oldest first
  for (const t of comp.takes) comp.regions = writeRange(comp, t.id, t.start, takeEnd(t));
  const gone = new Set(old.map(c => c.id));
  p.clips = p.clips.filter(c => !gone.has(c.id));
  p.clips.push({ id: alloc(), track, start: origin, len: end - origin, offset: 0, kind: 'comp', ref: comp.id, muted: false, gain: old[0]?.gain ?? 0, fadeIn: 0, fadeOut: 0, pan: old[0]?.pan ?? 0, norm: false, rev: false });
  comps.push(comp);
}

/** `putComp` / `removeComp`, and what `removeSource` does to takes (mutates `p`, like ops.rs). */
export function applyCompOp(p: Project, op: { op: 'putComp'; comp: Comp } | { op: 'removeComp'; id: Id } | { op: 'removeSource'; id: Id }, validate = true) {
  const comps = p.comps ?? (p.comps = []);
  if (op.op === 'putComp') {
    const c = validate ? validateComp(p, op.comp) : structuredClone(op.comp);
    for (const id of [c.id, ...c.takes.map(t => t.id)]) if (id >= p.nextId) p.nextId = id + 1;
    const i = comps.findIndex(x => x.id === c.id);
    if (i >= 0) comps[i] = c; else comps.push(c);
  } else if (op.op === 'removeComp') {
    if (validate && !comps.some(c => c.id === op.id)) throw new Error(`no comp with id ${op.id}`);
    p.comps = comps.filter(c => c.id !== op.id);
    p.clips = p.clips.filter(c => !(c.kind === 'comp' && c.ref === op.id));
  } else {
    const empty = new Set<Id>();
    p.comps = comps.map(c => {
      const gone = c.takes.filter(t => t.ref === op.id).map(t => t.id);
      if (!gone.length) return c;
      const n = gone.reduce((x, id) => removeTake(x, id), c);
      if (!n.takes.length) empty.add(c.id);
      return n;
    }).filter(c => !empty.has(c.id));
    p.clips = p.clips.filter(c => !(c.kind === 'comp' && empty.has(c.ref)));
  }
}

/** Validation shared by the mock backend's applyOps (same rules as ops.rs `validate_comp`). */
export function validateComp(p: Project, comp: Comp): Comp {
  if (!comp.takes.length) throw new Error(`comp ${comp.id} has no takes`);
  if (!(comp.xfade >= 0 && comp.xfade <= 0.5)) throw new Error('crossfade must be 0..0.5 s');
  const ids = new Set<Id>();
  for (const t of comp.takes) {
    if (!(isFinite(t.start) && t.len > 0 && t.offset >= 0)) throw new Error(`take ${t.id} has an invalid range`);
    if (ids.has(t.id)) throw new Error(`take id ${t.id} is used twice`);
    ids.add(t.id);
    if (!p.sources.some(s => s.id === t.ref)) throw new Error(`no take source with id ${t.ref}`);
  }
  const rs = comp.regions.map(r => ({ ...r }));
  for (const r of rs) {
    if (!(isFinite(r.from) && isFinite(r.to)) || r.to <= r.from) throw new Error(`comp region ${r.from}..${r.to} is empty or invalid`);
    if (!ids.has(r.take)) throw new Error(`no take with id ${r.take}`);
    if (r.xfade != null && !(r.xfade >= 0 && r.xfade <= 0.5)) throw new Error('crossfade must be 0..0.5 s');
  }
  rs.sort((a, b) => a.from - b.from);
  for (let i = 1; i < rs.length; i++) if (rs[i].from < rs[i - 1].to - EPS) throw new Error('comp regions overlap');
  return { ...structuredClone(comp), regions: mergeRegions(rs) };
}
