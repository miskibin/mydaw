import { describe, expect, it, vi } from 'vitest';
import type { PlayMode } from '../api/types';
import { createTransportControl } from './transportControl';

function deferred() {
  let resolve!: () => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<void>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
const settle = async () => { for (let i = 0; i < 80; i += 1) await Promise.resolve(); };

function fixture() {
  let mode: PlayMode = 'pat';
  let running = false;
  let position = 0;
  let samplesPerBeat = 24_000;
  const apply = vi.fn(async (next: PlayMode) => { mode = next; });
  const play = vi.fn(async () => {});
  const stop = vi.fn(async () => {});
  const seek = vi.fn(async (sample: number) => { position = sample; });
  const playing = vi.fn((value: boolean) => { running = value; });
  const move = vi.fn((sample: number) => { position = sample; });
  const report = vi.fn();
  const control = createTransportControl({
    mode: () => mode, setMode: apply, play, stop, seek, playing, move, report,
    isPlaying: () => running, position: () => position,
    samplesPerBeat: () => samplesPerBeat,
  });
  return {
    control, apply, play, stop, seek, playing, move, report,
    mode: () => mode, position: () => position,
    advance: (sample: number) => { position = sample; },
    tempo: (bpm: number) => { samplesPerBeat = 48_000 * 60 / bpm; },
    externalMode: (next: PlayMode) => { mode = next; },
  };
}

describe('editor transport ordering', () => {
  it('waits for all rapid editor changes before Play and deduplicates repeated focus', async () => {
    const f = fixture();
    const ipc = deferred();
    f.apply.mockImplementationOnce(async (mode) => { await ipc.promise; f.externalMode(mode); });
    f.control.setMode('song');
    f.control.setMode('song');
    f.control.setMode('pat');
    f.control.setMode('song');
    f.control.play();
    await settle();
    expect(f.apply.mock.calls).toEqual([['song']]);
    expect(f.play).not.toHaveBeenCalled();
    ipc.resolve();
    await settle();
    expect(f.apply.mock.calls).toEqual([['song'], ['pat'], ['song']]);
    expect(f.mode()).toBe('song');
    expect(f.play).toHaveBeenCalledOnce();
  });

  it('Stop cancels a Play waiting on a mode command', async () => {
    const f = fixture();
    const ipc = deferred();
    f.apply.mockImplementationOnce(() => ipc.promise);
    f.control.setMode('song');
    f.control.play();
    f.control.stop();
    ipc.resolve();
    await settle();
    expect(f.play).not.toHaveBeenCalled();
    expect(f.stop).toHaveBeenCalledOnce();
    expect(f.playing).toHaveBeenLastCalledWith(false);
  });

  it('does not play the wrong mode after an IPC failure, and can retry', async () => {
    const f = fixture();
    const error = new Error('queue full');
    f.apply.mockRejectedValueOnce(error);
    f.control.setMode('song');
    f.control.play();
    await settle();
    expect(f.play).not.toHaveBeenCalled();
    expect(f.playing).toHaveBeenLastCalledWith(false);
    expect(f.report).toHaveBeenCalledExactlyOnceWith(error);
    f.control.setMode('song');
    f.control.play();
    await settle();
    expect(f.mode()).toBe('song');
    expect(f.play).toHaveBeenCalledOnce();
  });

  it('reconciles the next editor gesture after an external CLI changes the mode', async () => {
    const f = fixture();
    f.control.setMode('song');
    await settle();
    f.externalMode('pat');
    f.control.setMode('song');
    await settle();
    expect(f.mode()).toBe('song');
    expect(f.apply).toHaveBeenCalledTimes(2);
  });

  it('reports a Play failure and rolls back the optimistic playing state', async () => {
    const f = fixture();
    const error = new Error('device unavailable');
    f.play.mockRejectedValueOnce(error);
    f.control.play();
    await settle();
    expect(f.playing.mock.calls).toEqual([[true], [false]]);
    expect(f.report).toHaveBeenCalledExactlyOnceWith(error);
  });

  it('Stop rewinds to zero and the next Play starts the pattern from its beginning', async () => {
    const f = fixture();
    f.control.play();
    await settle();
    f.advance(57_600);
    f.control.stop();
    await settle();
    expect(f.position()).toBe(0);
    f.control.play();
    await settle();
    expect(f.seek.mock.calls).toEqual([[0], [0], [0]]);
    expect(f.play).toHaveBeenCalledTimes(2);
  });

  it('Pause leaves the engine position untouched and resumes without a Seek', async () => {
    const f = fixture();
    f.control.play();
    await settle();
    f.advance(57_600);
    f.seek.mockClear();
    f.control.pause();
    await settle();
    expect(f.position()).toBe(57_600);
    expect(f.seek).not.toHaveBeenCalled();
    f.control.play();
    await settle();
    expect(f.seek).not.toHaveBeenCalled();
    expect(f.play).toHaveBeenCalledTimes(2);
  });

  it('remembers separate ruler positions for pattern and arrangement', async () => {
    const f = fixture();
    f.control.seek(12_000);
    f.control.setMode('song');
    f.control.seek(192_000);
    await settle();
    f.control.play();
    await settle();
    f.advance(240_000);
    f.control.stop();
    await settle();
    expect(f.position()).toBe(192_000);
    f.control.setMode('pat');
    await settle();
    expect(f.position()).toBe(12_000);
    f.control.setMode('song');
    await settle();
    expect(f.position()).toBe(192_000);
  });

  it('changes a playing scope in Stop, Mode, Seek, Play order', async () => {
    const f = fixture();
    f.control.play();
    await settle();
    f.advance(500_000);
    const order: string[] = [];
    f.stop.mockImplementation(async () => { order.push('stop'); });
    f.apply.mockImplementation(async (mode) => { order.push(mode); f.externalMode(mode); });
    f.seek.mockImplementation(async (sample) => { order.push(`seek:${sample}`); f.advance(sample); });
    f.play.mockImplementation(async () => { order.push('play'); });
    f.control.setMode('song');
    await settle();
    expect(order).toEqual(['stop', 'song', 'seek:0', 'play']);
    expect(f.playing).toHaveBeenLastCalledWith(true);
  });

  it('orders a Stop and immediate restart even when the Stop IPC is slow', async () => {
    const f = fixture();
    f.control.play();
    await settle();
    const ipc = deferred();
    f.stop.mockImplementationOnce(() => ipc.promise);
    f.play.mockClear();
    f.control.stop();
    f.control.play();
    await settle();
    expect(f.control.busy()).toBe(true);
    expect(f.play).not.toHaveBeenCalled();
    ipc.resolve();
    await settle();
    expect(f.play).toHaveBeenCalledOnce();
    expect(f.control.busy()).toBe(false);
  });

  it('does not start after a failed scope Seek, and recovers on the next gesture', async () => {
    const f = fixture();
    const error = new Error('seek failed');
    f.seek.mockRejectedValueOnce(error);
    f.control.setMode('song');
    f.control.play();
    await settle();
    expect(f.play).not.toHaveBeenCalled();
    expect(f.report).toHaveBeenCalledExactlyOnceWith(error);
    f.control.play();
    await settle();
    expect(f.play).toHaveBeenCalledOnce();
  });

  it('coalesces fast ruler drags and resets saved starts for a new project', async () => {
    const f = fixture();
    f.control.seek(100);
    f.control.seek(200);
    f.control.seek(300);
    await settle();
    expect(f.seek.mock.calls).toEqual([[300]]);
    f.control.setMode('song');
    f.control.seek(400);
    await settle();
    f.control.reset();
    await settle();
    expect(f.position()).toBe(0);
    f.control.setMode('pat');
    await settle();
    expect(f.position()).toBe(0);
  });

  it('keeps a ruler start on its musical beat after a tempo change', async () => {
    const f = fixture();
    f.control.seek(96_000); // Beat 5 at 120 BPM / 48 kHz.
    await settle();
    f.tempo(60);
    f.control.play();
    await settle();
    expect(f.position()).toBe(192_000);
    f.control.stop();
    await settle();
    expect(f.position()).toBe(192_000);
  });
});
