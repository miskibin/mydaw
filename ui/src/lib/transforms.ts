// The TypeScript mirror of `crates/engine/src/transform.rs`: the MockBackend applies
// the same seeded, deterministic note transforms as the engine, bit for bit (same
// splitmix64, same rounding), so browser demos and engine-driven sessions agree.
import type { Note, TransformSelect, TransformVerb } from '../api/types';
import { scaleMask } from './pattern';

/** splitmix64 — the engine's PRNG; keep identical to the Rust implementation. */
export class SplitMix64 {
  private s: bigint;
  private static readonly GOLDEN = 0x9e3779b97f4a7c15n;
  private static readonly M1 = 0xbf58476d1ce4e5b9n;
  private static readonly M2 = 0x94d049bb133111ebn;

  constructor(seed = 0) { this.s = BigInt(Math.max(0, Math.floor(seed))) + SplitMix64.GOLDEN; }
  next(): bigint {
    this.s = (this.s + SplitMix64.GOLDEN) & 0xffff_ffff_ffff_ffffn;
    let z = this.s;
    z = ((z ^ (z >> 30n)) * SplitMix64.M1) & 0xffff_ffff_ffff_ffffn;
    z = ((z ^ (z >> 27n)) * SplitMix64.M2) & 0xffff_ffff_ffff_ffffn;
    return z ^ (z >> 31n);
  }
  /** Uniform −1..1 (the engine computes this in f32: round where it rounds). */
  pm1(): number { return Math.fround(Math.fround(Number(this.next() >> 11n) / 2 ** 53) * 2 - 1); }
  unit(): number { return Math.fround(Number(this.next() >> 11n) / 2 ** 53); }
  below(n: number): number { return Number(this.next() % BigInt(n)); }
}

const clamp01 = (v: number) => Math.min(1, Math.max(0, v));

/** `pitch::snap` — nearest in-scale MIDI note, ties keep the lower note. */
function snap(m: number, key: number, mask: number): number {
  const base = Math.round(m);
  let best = base, bd = Infinity;
  for (let d = -6; d <= 6; d++) {
    const n = base + d;
    const pc = ((n - key) % 12 + 12) % 12;
    if (mask & (1 << pc)) {
      const dist = Math.abs(n - m);
      if (dist < bd) { bd = dist; best = n; }
    }
  }
  return best;
}

function euclideanOnsets(k: number, n: number, rotate: number): boolean[] {
  k = Math.max(1, k); n = Math.max(1, n);
  return Array.from({ length: n }, (_, i) => {
    const slot = ((i + rotate) % n + n) % n;
    return (slot * k) % n < k;
  });
}

/** One `transformNotes` op against a pattern's notes; mutates `notes` in place. Throws on invalid params. */
export function applyTransform(notes: Note[], verb: TransformVerb, seed: number, select: TransformSelect | null, roots: { id: number; root: number }[], key: number | null, scale: number, lenSteps: number): void {
  const rng = new SplitMix64(seed);
  const chanOk = (id: number) => roots.some(r => r.id === id);
  if ((verb.kind === 'euclidFill' || verb.kind === 'melody') && !chanOk(verb.ch)) throw new Error(`no channel with id ${verb.ch}`);
  if (verb.kind === 'euclidDrums' && verb.voices.some(v => !chanOk(v.ch))) throw new Error('no channel with that id');

  // the notes the verb sees — the engine builds the same index list
  const idx = !select
    ? notes.map((_, i) => i)
    : select.kind === 'indices'
      ? (select.indices.some(i => i >= notes.length)
        ? (() => { throw new Error('select: a note index is out of range'); })()
        : [...select.indices])
      : notes.map((n, i) => (n.step >= Math.min(select.from, select.to) && n.step < Math.max(select.from, select.to) && (!select.ch || n.ch === select.ch) ? i : -1))
          .filter(i => i >= 0);
  const target: Note[] = idx.map(i => structuredClone(notes[i]));

  switch (verb.kind) {
    case 'humanize': {
      const maxJitter = Math.fround(0.34 * clamp01(verb.timing ?? 0.5));
      const vel = clamp01(verb.velocity ?? 0.5);
      for (const n of target) {
        n.step = Math.max(0, Math.fround(n.step + Math.fround(rng.pm1() * maxJitter)));
        const nudge = Math.fround(1 + Math.fround(Math.fround(rng.pm1() * 0.5) * vel));
        n.v = Math.min(1, Math.max(0.05, Math.fround(Math.fround((n.v ?? 1) * nudge))));
      }
      break;
    }
    case 'quantize': {
      const grid = Math.fround(Math.max(0.25, verb.grid ?? 1));
      const amount = Math.fround(clamp01(verb.amount ?? 1));
      const ks = verb.scaleAware && key != null ? { key, mask: scaleMask(scale ?? 1) } : null;
      for (const n of target) {
        const tgt = Math.fround(Math.round(Math.fround(n.step / grid)) * grid);
        n.step = Math.fround(n.step + Math.fround((tgt - n.step) * amount));
        n.v = n.v ?? 1;
        if (ks) {
          const root = roots.find(r => r.id === n.ch)?.root ?? 60;
          n.p = snap(n.p ?? root, ks.key, ks.mask);
        }
      }
      break;
    }
    case 'euclidFill': {
      for (let i = target.length - 1; i >= 0; i--) if (target[i].ch === verb.ch) target.splice(i, 1);
      const onsets = euclideanOnsets(verb.k, verb.n, verb.rotate ?? 0);
      const slot = Math.fround(lenSteps / Math.max(1, verb.n));
      let hits = 0;
      onsets.forEach((on, i) => {
        if (on) {
          // every fourth emitted onset accents, whatever rotation does to the slots
          target.push({ ch: verb.ch, step: Math.fround(i * slot), len: Math.fround(Math.min(1, Math.max(0.25, slot))), v: hits++ % 4 === 0 ? 1 : verb.vel ?? 0.7 });
        }
      });
      break;
    }
    case 'strum': {
      // group by (channel, step) wherever the notes sit in the array
      const groups = new Map<string, number[]>();
      target.forEach((n, i) => {
        const k = `${n.ch}:${n.step}`;
        const g = groups.get(k);
        if (g) g.push(i); else groups.set(k, [i]);
      });
      const width = Math.fround(verb.width ?? 0.5);
      const ramp = Math.fround(verb.ramp ?? 0);
      for (const g of groups.values()) {
        if (g.length < 2) continue;
        g.sort((a, b) => (target[a].p ?? 60) - (target[b].p ?? 60));
        g.forEach((i, k) => {
          const n = target[i];
          n.step = Math.fround(n.step + Math.fround(k * width));
          n.v = Math.min(1, Math.max(0.05, Math.fround((n.v ?? 1) + Math.fround(k * ramp))));
        });
      }
      break;
    }
    case 'euclidDrums': {
      const bars = Math.max(1, verb.bars ?? 2);
      for (const v of verb.voices) {
        for (let i = target.length - 1; i >= 0; i--) if (target[i].ch === v.ch) target.splice(i, 1);
        const onsets = euclideanOnsets(v.k, v.n, v.rotate ?? 0);
        const slot = Math.fround((bars * 16) / Math.max(1, v.n));
        let hits = 0;
        onsets.forEach((on, i) => {
          if (on) {
            target.push({ ch: v.ch, step: Math.fround(i * slot), len: Math.fround(Math.min(1, Math.max(0.25, slot))), p: undefined, v: hits++ % 4 === 0 ? 1 : v.vel ?? 0.7 });
          }
        });
      }
      break;
    }
    case 'melody': {
      const bars = Math.max(1, verb.bars ?? 2);
      const rep = Math.max(1, verb.repeat ?? 1);
      const density = Math.min(1, Math.max(0.05, verb.density ?? 0.5));
      const mask = scaleMask(scale ?? 1);
      const degrees = Array.from({ length: 12 }, (_, pc) => pc).filter(pc => mask & (1 << pc));
      const n = degrees.length;
      let deg = 0;
      const seq: { d: number; len: number }[] = [];
      for (let bar = 0; bar < bars; bar++) {
        // the first `repeat` bars invent the motif; later bars replay their cycle's
        // rhythm with the walk nudged ±1 degree
        const build = bar < rep;
        for (let slot = 0; slot < 16; slot++) {
          let d: number; let len: number;
          if (build) {
            len = rng.unit() < Math.fround(0.3) ? 2 : 1;
            if (rng.unit() < density) {
              deg = Math.min(n - 1, Math.max(0, deg + [0, -1, 1, 2, -2][rng.below(5)]));
              seq.push({ d: deg, len });
              d = deg;
            } else {
              seq.push({ d: -1, len });
              d = -1;
            }
          } else {
            const src = seq[(bar % rep) * 16 + slot];
            if (src.d < 0) { d = -1; len = src.len; }
            else { deg = Math.min(n - 1, Math.max(0, deg + [0, -1, 1][rng.below(3)])); d = deg; len = src.len; }
          }
          if (d < 0) continue;
          const midi = 48 + (key ?? 0) + degrees[d % n];
          target.push({ ch: verb.ch, step: bar * 16 + slot, len, p: midi, v: 0.8 });
        }
      }
      break;
    }
  }

  if (!select) {
    notes.splice(0, notes.length, ...target);
  } else if (verb.kind === 'euclidFill' || verb.kind === 'euclidDrums' || verb.kind === 'melody') {
    // generators change cardinality: everything unselected stays, the full output
    // replaces the selection (positional overwrite would drop or misalign notes)
    const selected = new Set(idx);
    const merged = notes.filter((_, i) => !selected.has(i));
    merged.push(...target);
    notes.splice(0, notes.length, ...merged);
  } else {
    idx.forEach((ni, k) => { notes[ni] = target[k]; });
  }
}
