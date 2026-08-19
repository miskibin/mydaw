import { describe, expect, it } from 'vitest';

import { addedInsertId, insertsOnStrip } from './inserts';
import type { MixerState } from '../api/types';

describe('addedInsertId', () => {
  it('returns the insert that was not in the previous chain', () => {
    const before = new Set([1, 2]);
    expect(addedInsertId(before, [{ id: 1 }, { id: 2 }, { id: 9 }])).toBe(9);
  });

  it('returns undefined when nothing was added', () => {
    const before = new Set([1, 2]);
    expect(addedInsertId(before, [{ id: 1 }, { id: 2 }])).toBeUndefined();
  });
});

describe('insertsOnStrip', () => {
  const mixer = {
    tracks: [{ inserts: [{ id: 11 }] }, { inserts: [{ id: 22 }] }],
    buses: [{ inserts: [{ id: 33 }] }],
    master: { inserts: [{ id: 44 }] },
  } as unknown as MixerState;

  it('reads the chain for the strip that was edited, not the selected one', () => {
    expect(insertsOnStrip(mixer, { kind: 'track', index: 1 }).map((i) => i.id)).toEqual([22]);
    expect(insertsOnStrip(mixer, { kind: 'bus', index: 0 }).map((i) => i.id)).toEqual([33]);
    expect(insertsOnStrip(mixer, { kind: 'master', index: 0 }).map((i) => i.id)).toEqual([44]);
  });
});
