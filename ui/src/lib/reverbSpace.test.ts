import { describe, expect, it } from 'vitest';
import data from '../api/reverbSpaces.json';
import { decayAt, derive, envelope, roomDims, roomM, SPACES, timeSpan } from './reverbSpace';

const P = (o: Record<string, number>) => ({ size: 1, pre: 25, space: 0, tone: 0, bass: 1, early: 50, ...o });

describe('reverb display model', () => {
  it('reproduces the engine for every sample in the fixture (decay per band, scale, room size)', () => {
    for (const s of data.samples) {
      const p = P({ space: s.space, size: s.size, tone: s.tone, bass: s.bass });
      const d = derive(p);
      expect(d.tLow).toBeCloseTo(s.tLow, 4);
      expect(d.tMid).toBeCloseTo(s.tMid, 4);
      expect(d.tHigh).toBeCloseTo(s.tHigh, 4);
      expect(d.scale).toBeCloseTo(s.scale, 4);
      expect(roomM(p)).toBeCloseTo(s.roomM, 2);
    }
  });

  it('draws a hall bigger than a room, and a room bigger with a longer decay', () => {
    expect(roomM(P({ space: 2, size: 2 }))).toBeGreaterThan(roomM(P({ space: 1, size: 2 })) * 1.5);
    expect(roomM(P({ space: 1, size: 1.2 }))).toBeGreaterThan(roomM(P({ space: 1, size: 0.6 })));
    // the box keeps the equivalent volume
    const [l, w, h] = roomDims(P({ space: 2, size: 2 })), side = roomM(P({ space: 2, size: 2 }));
    expect(l * w * h).toBeCloseTo(side ** 3, 0);
    expect(l).toBeGreaterThan(w);
  });

  it('places the early reflections in the space\'s span after the pre-delay, falling with time', () => {
    const e = envelope(P({ space: 2, pre: 40, early: 50 }));
    expect(e.early).toHaveLength(24);
    expect(Math.min(...e.early.map(r => r.t))).toBeGreaterThanOrEqual(0.04);
    expect(Math.max(...e.early.map(r => r.t))).toBeLessThanOrEqual(0.04 + SPACES[2].erMs / 1000);
    expect(e.early[0].db).toBeGreaterThan(e.early[23].db);
    // Early at 0 and the plate have none
    expect(envelope(P({ space: 2, early: 0 })).early).toEqual([]);
    expect(envelope(P({ space: 0 })).early).toEqual([]);
  });

  it('maps a drag of the tail end back to the decay, within its range', () => {
    const p = P({ space: 1, pre: 30 }), e = envelope(p);
    expect(decayAt(p, e.onset + 2.5)).toBeCloseTo(2.5, 6);
    expect(decayAt(p, 0)).toBe(0.2);
    expect(decayAt(p, 99)).toBe(6);
  });

  it('shows the whole tail on a round time axis', () => {
    for (const size of [0.2, 1, 2.5, 6]) for (const bass of [0.5, 1.5]) {
      const p = P({ space: 2, size, bass, pre: 150 });
      expect(timeSpan(p)).toBeGreaterThanOrEqual(envelope(p).end);
    }
    expect(timeSpan(P({ size: 0.2, pre: 0 }))).toBe(0.5);
  });
});
