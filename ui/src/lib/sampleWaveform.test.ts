import { describe, expect, it } from 'vitest';

import { overviewWaveformPath, stereoWaveformPaths, type WaveformView } from './sampleWaveform';

const peaks: [number, number][] = [
  [-1, 1],
  [-0.8, 0.9],
  [-0.5, 0.75],
  [-0.25, 0.5],
  [-0.1, 0.2],
];

const ROOT: WaveformView = {
  gainDb: 0,
  pan: 0,
  pitchCents: 0,
  fadeInMs: 0,
  fadeOutMs: 0,
  sampleRate: 48_000,
  totalFrames: 48_000,
  startFrames: 0,
  endFrames: 0,
};

describe('stereoWaveformPaths', () => {
  it('draws separate left and right paths and follows pan', () => {
    const centred = stereoWaveformPaths(peaks, ROOT);
    const right = stereoWaveformPaths(peaks, { ...ROOT, pan: 1 });
    expect(centred.left).not.toBe('');
    expect(centred.right).not.toBe('');
    expect(right.left).not.toBe(centred.left);
    expect(right.right).not.toBe(centred.right);
  });

  it('changes geometry for gain, fades, region and pitch', () => {
    const root = stereoWaveformPaths(peaks, ROOT);
    expect(stereoWaveformPaths(peaks, { ...ROOT, gainDb: -12 }).left).not.toBe(root.left);
    expect(stereoWaveformPaths(peaks, { ...ROOT, fadeInMs: 500 }).left).not.toBe(root.left);
    expect(
      stereoWaveformPaths(peaks, { ...ROOT, startFrames: 12_000, endFrames: 36_000 }).left,
    ).not.toBe(root.left);
    expect(stereoWaveformPaths(peaks, { ...ROOT, pitchCents: 1_200 }).widthPercent).toBe(50);
    expect(stereoWaveformPaths(peaks, { ...ROOT, pitchCents: -1_200 }).widthPercent).toBe(200);
  });

  it('treats a stored end of zero as the end of the file', () => {
    const implicit = stereoWaveformPaths(peaks, ROOT);
    const explicit = stereoWaveformPaths(peaks, { ...ROOT, endFrames: ROOT.totalFrames });
    expect(implicit).toEqual(explicit);
  });
});

function pathPoints(d: string): { x: number; y: number }[] {
  return d
    .replace(/[MLZ]/gi, ' ')
    .trim()
    .split(/\s+/)
    .filter((token) => token.includes(','))
    .map((token) => {
      const [x, y] = token.split(',').map(Number);
      return { x: x ?? 0, y: y ?? 0 };
    });
}

function heightAt(points: { x: number; y: number }[], x: number): number {
  const at = points.filter((point) => Math.abs(point.x - x) < 0.01);
  const ys = at.map((point) => point.y);
  return Math.max(...ys) - Math.min(...ys);
}

describe('overviewWaveformPath', () => {
  it('pairs each min with the max of the same bin, not the mirrored one', () => {
    const decaying: [number, number][] = Array.from({ length: 8 }, (_, index) => {
      const amplitude = 0.9 * Math.exp(-0.6 * index);
      return [-amplitude, amplitude];
    });
    const points = pathPoints(overviewWaveformPath(decaying));
    const left = heightAt(points, 0);
    const right = heightAt(points, 100);
    expect(left).toBeGreaterThan(0);
    expect(right).toBeGreaterThan(0);
    expect(left).toBeGreaterThan(right * 2);
  });

  it('uses the first peak on the left edge and the last peak on the right', () => {
    const d = overviewWaveformPath([
      [-1, 1],
      [-0.1, 0.1],
    ]);
    const points = pathPoints(d);
    const left = points.filter((point) => point.x === 0).map((point) => point.y);
    const right = points.filter((point) => point.x === 100).map((point) => point.y);
    expect(Math.min(...left)).toBeCloseTo(50 - 46);
    expect(Math.max(...left)).toBeCloseTo(50 + 46);
    expect(Math.min(...right)).toBeCloseTo(50 - 4.6);
    expect(Math.max(...right)).toBeCloseTo(50 + 4.6);
  });

  it('is empty when there are no peaks', () => {
    expect(overviewWaveformPath([])).toBe('');
  });
});
