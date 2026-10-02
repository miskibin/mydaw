import { describe, expect, it } from 'vitest';
import { Playhead, formatTimecode } from './playhead';

describe('paused transport readout', () => {
  it('holds the authoritative paused position without a drifting timecode', () => {
    let now = 0;
    const head = new Playhead({ sampleRate: 48_000, now: () => now });
    // Optimistic Pause precedes the engine acknowledgement by a small amount.
    head.reset(26_304, false);
    now = 100;
    head.update(26_352, false);
    const stopped = formatTimecode(head.positionSeconds());
    now = 500;
    expect(head.positionSamples()).toBe(26_352);
    expect(formatTimecode(head.positionSeconds())).toBe(stopped);
    now = 2_000;
    head.update(26_352, false);
    expect(formatTimecode(head.positionSeconds())).toBe(stopped);
  });
});
