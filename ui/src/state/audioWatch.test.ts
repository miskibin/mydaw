import { beforeAll, describe, expect, it } from 'vitest';
import type { AudioStatus } from '../api/types';
import { MockBackend } from '../api/mock';
import { audioNotice, watchAudio } from './audioWatch';

beforeAll(() => { (globalThis as { window?: unknown }).window ??= {}; });

const tick = () => new Promise(r => setTimeout(r, 0));
const base = { running: true, error: null, note: null, restarts: 0 } as AudioStatus;

describe('audio that stops or returns by itself', () => {
  it('is quiet while nothing changes', () => {
    expect(audioNotice(base, { ...base })).toBeNull();
    expect(audioNotice(null, base)).toBeNull();
  });
  it('says when the output was reopened, with the reason', () => {
    const m = audioNotice(base, { ...base, restarts: 1, note: 'Audio restarted by itself: device gone.' });
    expect(m).toContain('device gone');
  });
  it('says when audio could not come back, once per error', () => {
    const down = { ...base, running: false, error: 'Audio stopped (reopening failed). Retrying…' };
    expect(audioNotice(base, down)).toContain('Retrying');
    expect(audioNotice(down, { ...down })).toBeNull();
  });
  it('follows the mock backend like the app', async () => {
    const be = new MockBackend(); const said: string[] = [];
    await watchAudio(await be.audioGetConfig(), cb => be.on('audio-status', cb), m => said.push(m));
    be.audioDeviceLost();
    await tick();
    expect(said).toHaveLength(1);
    expect(said[0]).toContain('no longer available');
    // a settings change the user made is not a "restarted by itself"
    await be.audioSetConfig({ ...(await be.audioGetConfig()), bufferSize: 256 });
    await tick();
    expect(said).toHaveLength(1);
  });
});
