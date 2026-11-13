// The browser port of the `editTune` op must agree with the engine (crates/engine/src/tune_edit.rs), which writes
// this fixture from its own results (`WAVE_WRITE_FIXTURES=1 cargo test -p wave-engine tune_edit`).
import { describe, expect, it } from 'vitest';
import fixture from './__fixtures__/tune-edit.json';
import { applyTuneEdit, segmentationFingerprint } from './pitch';
import { applyAll } from './applyOps';
import type { ClipTune, NoteEdit, Project, TuneEdit } from '../api/types';

type Row = { name: string; tune: ClipTune | null; edit: TuneEdit; result: ClipTune | null; error: boolean };

// the engine writes `speed` only when set and `pencil` only when not empty
const norm = (t: ClipTune | null) => t && {
  notes: t.notes.map((n: NoteEdit) => ({ start: n.start, end: n.end, shift: n.shift, drift: n.drift, formant: n.formant, gain: n.gain, speed: n.speed ?? null })),
  pencil: t.pencil ?? [],
  reference: t.reference ?? null,
};

function close(a: unknown, b: unknown, path = ''): void {
  if (typeof a === 'number' && typeof b === 'number') { expect(Math.abs(a - b), path).toBeLessThan(1e-6); return; }
  if (Array.isArray(a) && Array.isArray(b)) { expect(a.length, path).toBe(b.length); a.forEach((x, i) => close(x, b[i], `${path}[${i}]`)); return; }
  if (a && b && typeof a === 'object' && typeof b === 'object') {
    const ka = Object.keys(a).sort(), kb = Object.keys(b).sort();
    expect(ka, path).toEqual(kb);
    for (const k of ka) close((a as Record<string, unknown>)[k], (b as Record<string, unknown>)[k], `${path}.${k}`);
    return;
  }
  expect(a, path).toEqual(b);
}

describe('editTune', () => {
  it('fingerprints segmentations like the engine', () => {
    for (const f of fixture.fingerprints) expect(segmentationFingerprint(f.spans as [number, number][])).toBe(f.hash);
  });

  for (const row of fixture.cases as unknown as Row[]) {
    it(row.name, () => {
      if (row.error) { expect(() => applyTuneEdit(row.tune, row.edit)).toThrow(); return; }
      close(norm(applyTuneEdit(row.tune, row.edit)), norm(row.result));
    });
  }

  it('refuses unknown fields like the engine', () => {
    expect(() => applyTuneEdit(null, { notez: [] } as unknown as TuneEdit)).toThrow(/unknown field/);
    expect(() => applyTuneEdit({ notes: [{ start: 0, end: 1, shift: 0, drift: 0, formant: 0, gain: 0 }] }, { notes: [{ i: 0, shfit: 3 }] } as unknown as TuneEdit)).toThrow(/unknown field/);
  });

  it('as an op it edits only the clip tune, atomically', () => {
    const clip = { id: 7, track: 0, start: 0, len: 4, offset: 0, kind: 'audio', ref: 3, muted: false, gain: -2, fadeIn: 0.1, fadeOut: 0, pan: 0.3, norm: false, rev: false };
    const p0 = { tracks: [{ id: 1 }], sources: [{ id: 3 }], clips: [clip, { ...clip, id: 8 }], patterns: [], inserts: [], channels: [], nextId: 9 } as unknown as Project;
    const before = structuredClone(p0);
    const edit: TuneEdit = { init: [[0, 0.5], [0.5, 1]], notes: [{ i: 1, shift: -30 }], split: [{ i: 0, at: 0.25 }] };
    const p = applyAll(p0, [{ op: 'editTune', clip: 7, edit }]);
    const c = p.clips[0];
    expect(c.tune?.notes.map(n => [n.start, n.end, n.shift])).toEqual([[0, 0.25, 0], [0.25, 0.5, 0], [0.5, 1, -30]]);
    expect({ ...c, tune: undefined }).toEqual({ ...before.clips[0], tune: undefined });
    expect(p.clips[1]).toEqual(before.clips[1]);
    // a failing op in a batch changes nothing
    const snap = structuredClone(p);
    expect(() => applyAll(p, [{ op: 'editTune', clip: 7, edit: { notes: [{ i: 0, shift: 5 }] } }, { op: 'editTune', clip: 7, edit: { notes: [{ i: 9, shift: 5 }] } }])).toThrow();
    expect(p).toEqual(snap);
    expect(() => applyAll(p, [{ op: 'editTune', clip: 99, edit: {} }])).toThrow(/no clip/);
  });
});
