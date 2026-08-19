import { describe, expect, it } from 'vitest';

import { channelKind, engineOf, hasSampleWindow, isAudioClipChannel } from './channel';

describe('engineOf', () => {
  it('reads the prefix the engine puts on a wire id', () => {
    expect(engineOf('clip:3')).toBe('clip');
    expect(engineOf('oneshot:3')).toBe('sample');
    expect(engineOf('sampler:salamander-piano')).toBe('sampler');
    expect(engineOf('test-tone')).toBe('tone');
    expect(engineOf('poly-synth')).toBe('synth');
  });
});

describe('channelKind', () => {
  it('badges an audio-clip channel as CLIP and a one-shot as SMP', () => {
    expect(channelKind({ sourceId: 'clip:1', engine: 'clip', clap: null, vst3: null })).toEqual({
      kind: 'sample',
      badge: 'CLIP',
      title: 'Audio clip — plays from the playlist',
    });
    expect(
      channelKind({ sourceId: 'oneshot:1', engine: 'sample', clap: null, vst3: null }),
    ).toMatchObject({ kind: 'sample', badge: 'SMP' });
  });

  it('opens the sample window for clip and one-shot channels', () => {
    expect(hasSampleWindow({ sourceId: 'clip:1', engine: 'clip', clap: null, vst3: null })).toBe(
      true,
    );
    expect(
      hasSampleWindow({ sourceId: 'oneshot:1', engine: 'sample', clap: null, vst3: null }),
    ).toBe(true);
    expect(
      hasSampleWindow({
        sourceId: 'clap:x',
        engine: 'clap',
        clap: {
          instance: 1,
          pluginId: 'x',
          name: 'X',
          bundlePath: '',
          hasEditor: true,
          guiOpen: false,
        },
        vst3: null,
      }),
    ).toBe(false);
  });

  it('treats only clip: channels as playlist audio-clip rows', () => {
    expect(isAudioClipChannel({ sourceId: 'clip:1', engine: 'clip' })).toBe(true);
    expect(isAudioClipChannel({ sourceId: 'oneshot:1', engine: 'sample' })).toBe(false);
    expect(isAudioClipChannel({ sourceId: 'kick', engine: 'synth' })).toBe(false);
  });
});
