import { describe, expect, it } from 'vitest';

import { audioBufferLabel, snapAudioBufferFrames } from './audioOutput';

describe('audio buffer choices', () => {
  it('keeps zero as the device default', () => {
    expect(snapAudioBufferFrames(0)).toBe(0);
    expect(snapAudioBufferFrames(Number.NaN)).toBe(0);
    expect(audioBufferLabel(0)).toBe('Device');
  });

  it('snaps unknown sizes onto the chips Settings shows', () => {
    expect(snapAudioBufferFrames(256)).toBe(256);
    expect(snapAudioBufferFrames(300)).toBe(256);
    expect(snapAudioBufferFrames(400)).toBe(512);
    expect(snapAudioBufferFrames(9000)).toBe(2048);
  });
});
