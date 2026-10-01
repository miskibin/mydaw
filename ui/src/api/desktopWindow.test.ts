import { beforeEach, describe, expect, it, vi } from 'vitest';

const mocked = vi.hoisted(() => ({
  isTauri: vi.fn(),
  native: {
    minimize: vi.fn(),
    toggleMaximize: vi.fn(),
    close: vi.fn(),
    isMaximized: vi.fn(),
    onResized: vi.fn(),
  },
}));
vi.mock('./backend', () => ({ isTauri: mocked.isTauri }));
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: () => mocked.native }));

import { desktopWindow } from './desktopWindow';

const flush = async () => {
  await Promise.resolve();
  await Promise.resolve();
};

describe('native window chrome', () => {
  beforeEach(() => {
    vi.resetAllMocks();
    mocked.isTauri.mockReturnValue(true);
  });

  it('does not expose window controls in a browser', () => {
    mocked.isTauri.mockReturnValue(false);
    expect(desktopWindow()).toBeNull();
  });

  it('forwards actions to the actual desktop window', async () => {
    const window = desktopWindow()!;
    await window.minimize();
    await window.toggleMaximize();
    await window.close();
    expect(mocked.native.minimize).toHaveBeenCalledOnce();
    expect(mocked.native.toggleMaximize).toHaveBeenCalledOnce();
    expect(mocked.native.close).toHaveBeenCalledOnce();
  });

  it('tracks native resizes and ignores a stale maximization response', async () => {
    let resize = () => {};
    let resolveInitial!: (value: boolean) => void;
    const stop = vi.fn();
    mocked.native.onResized.mockImplementation((handler) => {
      resize = handler;
      return Promise.resolve(stop);
    });
    mocked.native.isMaximized
      .mockReturnValueOnce(
        new Promise<boolean>((resolve) => {
          resolveInitial = resolve;
        }),
      )
      .mockResolvedValue(true);
    const changed = vi.fn();
    const dispose = desktopWindow()!.watchMaximized(changed, vi.fn());
    await flush();
    resize();
    await flush();
    resolveInitial(false);
    await flush();
    expect(changed.mock.calls).toEqual([[true]]);
    dispose();
    resize();
    await flush();
    expect(changed).toHaveBeenCalledOnce();
    expect(stop).toHaveBeenCalledOnce();
  });

  it('removes a listener that finishes registering after disposal', async () => {
    let resolveListener!: (stop: () => void) => void;
    mocked.native.onResized.mockReturnValue(
      new Promise<() => void>((resolve) => {
        resolveListener = resolve;
      }),
    );
    const changed = vi.fn();
    const stop = vi.fn();
    desktopWindow()!.watchMaximized(changed, vi.fn())();
    resolveListener(stop);
    await flush();
    expect(stop).toHaveBeenCalledOnce();
    expect(changed).not.toHaveBeenCalled();
    expect(mocked.native.isMaximized).not.toHaveBeenCalled();
  });

  it('reports native permission errors without leaving rejected promises', async () => {
    const error = new Error('window permission denied');
    mocked.native.onResized.mockRejectedValue(error);
    const failed = vi.fn();
    desktopWindow()!.watchMaximized(vi.fn(), failed);
    await flush();
    expect(failed).toHaveBeenCalledWith(error);
  });
});
