import { describe, expect, it } from 'vitest';

import {
  DEFAULT_EXPORT_OPTIONS,
  exportBlocker,
  exportBodyTicks,
  exportSeconds,
  exportSummary,
  exportWarnings,
  fileNameOf,
  formatDuration,
  progressLabel,
  progressPercent,
  sampleRateLabel,
  sanitizeExportOptions,
} from './exportWav';

describe('export length', () => {
  it('song covers the arrangement end exactly, pattern one loop, both plus the tail', () => {
    // 120 BPM: a bar (3840 ticks) is 2 s. An audio clip ending mid-bar at 5760
    // ticks is 3 s, not rounded up to the 4 s bar line.
    expect(exportSeconds(exportBodyTicks('song', 5760, 3840), 120, 2)).toBeCloseTo(5);
    expect(exportSeconds(exportBodyTicks('pattern', 5760, 3840), 120, 0.5)).toBeCloseTo(2.5);
    expect(exportSeconds(0, 0, 2)).toBe(2);
  });

  it('refuses an empty playlist in song mode and anything over the hour limit', () => {
    expect(exportBlocker('song', 0, 2)).toMatch(/playlist is empty/);
    expect(exportBlocker('pattern', 0, 2)).toMatch(/pattern has no length/);
    expect(exportBlocker('song', 3840, 3601)).toMatch(/60 minutes/);
    expect(exportBlocker('song', 3840, 200)).toBeNull();
  });
});

describe('export wording', () => {
  it('formats durations without a 60-second minute', () => {
    expect(formatDuration(204.46)).toBe('3:24.5');
    expect(formatDuration(5)).toBe('0:05.0');
    expect(formatDuration(59.97)).toBe('1:00.0');
    expect(formatDuration(-1)).toBe('0:00.0');
  });

  it('labels rates and file names', () => {
    expect(sampleRateLabel(44100)).toBe('44.1 kHz');
    expect(sampleRateLabel(48000)).toBe('48 kHz');
    expect(fileNameOf('C:\\Music\\Night Drive.wav')).toBe('Night Drive.wav');
    expect(fileNameOf('/home/me/a.wav')).toBe('a.wav');
  });

  it('never shows 100% before the worker reports completion', () => {
    const running = { jobId: 'a', status: 'running' as const, progress: 0.999 };
    expect(progressPercent(running)).toBe(99);
    expect(progressPercent({ ...running, progress: undefined as never })).toBe(0);
    expect(progressPercent({ jobId: 'a', status: 'completed' })).toBe(100);
    expect(progressLabel({ ...running, progress: 0.5 })).toBe('Rendering…');
    expect(progressLabel(running)).toBe('Writing file…');
  });

  it('summarises the file and warns about overs per format', () => {
    const done = {
      jobId: 'a',
      status: 'completed' as const,
      seconds: 206,
      bitDepth: 24,
      sampleRate: 44100,
      peakDbfs: -0.83,
      normalizeGainDb: null,
      clippedSamples: 0,
    };
    expect(exportSummary(done)).toBe('3:26.0 · 24-bit · 44.1 kHz · peak −0.8 dBFS');
    expect(exportSummary({ ...done, normalizeGainDb: 2.04 })).toContain('normalized +2.0 dB');
    expect(exportWarnings(done)).toEqual([]);
    expect(exportWarnings({ ...done, clippedSamples: 1200 })[0]).toMatch(/1,200 samples clipped/);
    expect(exportWarnings({ ...done, bitDepth: 32, clippedSamples: 1 })[0]).toMatch(
      /1 sample above 0 dBFS\. Kept in 32-bit float/,
    );
    expect(exportWarnings({ ...done, silent: true })[0]).toMatch(/silent/);
  });
});

describe('remembered options', () => {
  it('keeps valid values and replaces anything unusable with the defaults', () => {
    expect(sanitizeExportOptions(null)).toEqual(DEFAULT_EXPORT_OPTIONS);
    expect(DEFAULT_EXPORT_OPTIONS).toMatchObject({ mode: 'song', bitDepth: 24, tailSeconds: 2, normalize: false });
    expect(
      sanitizeExportOptions({ mode: 'pattern', sampleRate: 48000, bitDepth: 16, tailSeconds: 4, normalize: true }),
    ).toEqual({ mode: 'pattern', sampleRate: 48000, bitDepth: 16, tailSeconds: 4, normalize: true });
    expect(
      sanitizeExportOptions({ mode: 'loop', sampleRate: 22050, bitDepth: 8, tailSeconds: 99, normalize: 'yes' }),
    ).toEqual(DEFAULT_EXPORT_OPTIONS);
  });
});
