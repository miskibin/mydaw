import { describe, expect, it } from 'vitest';

import { addedInsertId, insertMove, insertsOnStrip } from './inserts';
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

describe('insertMove', () => {
  const chain = [{ id: 7 }, { id: 2 }, { id: 9 }];

  it('addresses a reorder by stable ids', () => {
    expect(insertMove(chain, 9, 7)).toEqual({ from: 2, to: 0 });
    expect(insertMove(chain, 7, 2)).toEqual({ from: 0, to: 1 });
  });

  it('ignores a self-drop or an insert that disappeared', () => {
    expect(insertMove(chain, 2, 2)).toBeUndefined();
    expect(insertMove(chain, 99, 2)).toBeUndefined();
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
