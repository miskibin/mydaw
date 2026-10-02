import { describe, expect, it, vi } from 'vitest';
import type { PlayMode } from '../api/types';
import { createTransportControl } from './transportControl';

function deferred() {
  let resolve!: () => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<void>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
const settle = async () => { for (let i = 0; i < 12; i += 1) await Promise.resolve(); };

function fixture() {
  let mode: PlayMode = 'pat';
  const apply = vi.fn(async (next: PlayMode) => { mode = next; });
  const play = vi.fn(async () => {});
  const stop = vi.fn(async () => {});
  const playing = vi.fn();
  const report = vi.fn();
  const control = createTransportControl({ mode: () => mode, setMode: apply, play, stop, playing, report });
  return { control, apply, play, stop, playing, report, mode: () => mode, externalMode: (next: PlayMode) => { mode = next; } };
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
});
