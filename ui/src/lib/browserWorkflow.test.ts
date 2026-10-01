import { describe, expect, it } from 'vitest';
import { LatestRequest, sampleKeyAction, waveformPath } from './browserWorkflow';

describe('sample audition and replacement gestures', () => {
  it('lets Enter replace, Shift+Enter add and Space audition', () => {
    expect(sampleKeyAction('Enter', false)).toBe('replace');
    expect(sampleKeyAction('Enter', true)).toBe('add');
    expect(sampleKeyAction(' ', false)).toBe('preview');
    expect(sampleKeyAction('Delete', false)).toBeNull();
  });
  it('rejects a slow preview that resolves after the next sample or Stop', () => {
    const requests = new LatestRequest();
    const slow = requests.begin();
    const fast = requests.begin();
    expect(requests.current(slow)).toBe(false);
    expect(requests.current(fast)).toBe(true);
    requests.cancel();
    expect(requests.current(fast)).toBe(false);
  });
  it('preserves transient min/max instead of replacing peaks with an average', () => {
    expect(
      waveformPath([
        [-1, 1],
        [0, 0],
      ]),
    ).toBe('M64.00 2.00V46.00M192.00 24.00V24.00');
    expect(waveformPath([])).toBe('');
  });
});
