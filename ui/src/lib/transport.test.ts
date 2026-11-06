import { beforeAll, describe, expect, it } from 'vitest';

// MockBackend touches `window` in its constructor; vitest runs in node.
beforeAll(() => {
  (globalThis as unknown as { window: typeof globalThis }).window = globalThis;
});

describe('recording transport (mock)', () => {
  it('disarming Rec while playing commits the take (punch-out)', async () => {
    const { MockBackend } = await import('../api/mock');
    const mock = new MockBackend();
    await mock.transportSet({ rec: true, countIn: false });
    await mock.transportPlay(4);
    await mock.subscribeTelemetry(() => {});
    await new Promise(r => setTimeout(r, 280));
    await mock.transportSet({ rec: false });
    const state = await mock.getState();
    const takes = state.project.clips.filter(c => c.kind === 'audio');
    expect(takes.length).toBeGreaterThan(0);
    expect(takes[0].start).toBe(4);
    expect(state.transport.playing).toBe(true);
    await mock.transportStop();
  });

  it('Stop keeps the start marker and leaves transport idle', async () => {
    const { MockBackend } = await import('../api/mock');
    const mock = new MockBackend();
    await mock.transportPlay(16);
    await new Promise(r => setTimeout(r, 40));
    await mock.transportStop();
    const tr = (await mock.getState()).transport;
    expect(tr.playing).toBe(false);
    expect(tr.startBeat).toBe(16);
  });

  it('play works again after stop ends a recording', async () => {
    const { MockBackend } = await import('../api/mock');
    const mock = new MockBackend();
    await mock.transportSet({ rec: true, countIn: false });
    await mock.transportPlay(0);
    await mock.subscribeTelemetry(() => {});
    await new Promise(r => setTimeout(r, 280));
    await mock.transportStop();
    await mock.transportPlay(0);
    expect((await mock.getState()).transport.playing).toBe(true);
    await mock.transportStop();
  });
});
