import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createRoot } from 'solid-js';

import { MockBackend } from '../api/mock';
import type { ExportJobStatus } from '../api/types';
import { createExportController, EXPORT_POLL_MS } from './exportController';

async function withClip(): Promise<MockBackend> {
  const backend = new MockBackend();
  const state = await backend.getPattern();
  await backend.addPatternClip(state.patternSet.activeId, 0, 0, 3840);
  return backend;
}

describe('export controller', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('runs settings → running → completed and reports the saved file once', async () => {
    const backend = await withClip();
    const finished: ExportJobStatus[] = [];
    const exporter = createRoot(() => createExportController(backend, (status) => finished.push(status)));
    exporter.show();
    expect(exporter.open()).toBe(true);
    expect(exporter.phase()).toBe('settings');

    await exporter.start();
    expect(exporter.phase()).toBe('running');
    expect(exporter.job()?.path).toMatch(/Untitled\.wav$/);
    exporter.hide();
    expect(exporter.open()).toBe(true);
    exporter.setOptions({ bitDepth: 16 });
    expect(exporter.options().bitDepth).toBe(24);

    await vi.advanceTimersByTimeAsync(5000);
    expect(exporter.phase()).toBe('completed');
    expect(exporter.job()).toMatchObject({ status: 'completed', bitDepth: 24, sampleRate: 44100 });
    expect(finished).toHaveLength(1);

    exporter.hide();
    expect(exporter.open()).toBe(false);
    exporter.show();
    expect(exporter.phase()).toBe('settings');
    expect(exporter.job()).toBeNull();
    exporter.dispose();
  });

  it('cancels a running export and stops polling', async () => {
    const backend = await withClip();
    const exporter = createRoot(() => createExportController(backend));
    await exporter.start();
    await exporter.cancel();
    await vi.advanceTimersByTimeAsync(EXPORT_POLL_MS * 2);
    expect(exporter.phase()).toBe('cancelled');
    expect(exporter.error()).toBeNull();
    const spy = vi.spyOn(backend, 'getExportStatus');
    await vi.advanceTimersByTimeAsync(EXPORT_POLL_MS * 5);
    expect(spy).not.toHaveBeenCalled();
    exporter.dispose();
  });

  it('shows a refused start in place and lets a cancelled dialog leave nothing behind', async () => {
    const empty = new MockBackend();
    const exporter = createRoot(() => createExportController(empty));
    await exporter.start();
    expect(exporter.phase()).toBe('settings');
    expect(exporter.error()).toMatch(/playlist is empty/);

    const backend = await withClip();
    vi.spyOn(backend, 'exportAudio').mockResolvedValueOnce(null);
    const chooser = createRoot(() => createExportController(backend));
    await chooser.start();
    expect(chooser.phase()).toBe('settings');
    expect(chooser.job()).toBeNull();
    expect(chooser.error()).toBeNull();
    exporter.dispose();
    chooser.dispose();
  });

  it('turns a failed worker into a failed phase with its reason', async () => {
    const backend = await withClip();
    const job: ExportJobStatus = { jobId: 'x', status: 'running', progress: 0, path: 'C:\\a.wav' };
    vi.spyOn(backend, 'exportAudio').mockResolvedValue(job);
    vi.spyOn(backend, 'getExportStatus').mockResolvedValue({
      jobId: 'x',
      status: 'failed',
      error: 'track 3 has no live plugin state to snapshot',
    });
    const exporter = createRoot(() => createExportController(backend));
    await exporter.start();
    await vi.advanceTimersByTimeAsync(EXPORT_POLL_MS);
    expect(exporter.phase()).toBe('failed');
    expect(exporter.error()).toMatch(/no live plugin state/);
    exporter.dispose();
  });
});
