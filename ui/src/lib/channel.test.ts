import { describe, expect, it } from 'vitest';

import { CHANNEL_KIND_COLORS } from '../theme/tokens';
import { channelColor, channelKind, engineOf, hasSampleWindow, isAudioClipChannel } from './channel';

const SOURCE = { clap: null, vst3: null } as const;

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

describe('channelColor', () => {
  it('paints every drum the same colour, independent of which drum it is', () => {
    const kick = channelColor({ sourceId: 'kick', engine: 'synth', ...SOURCE });
    const snare = channelColor({ sourceId: 'snare', engine: 'synth', ...SOURCE });
    expect(kick).toBe(snare);
    expect(kick).toBe(CHANNEL_KIND_COLORS.drum);
  });

  it('gives drums, synths, samples, plugins and tones distinct colours', () => {
    const colours = [
      channelColor({ sourceId: 'kick', engine: 'synth', ...SOURCE }),
      channelColor({ sourceId: 'poly-synth', engine: 'synth', ...SOURCE }),
      channelColor({ sourceId: 'oneshot:1', engine: 'sample', ...SOURCE }),
      channelColor({ sourceId: 'clap:x', engine: 'clap', ...SOURCE }),
      channelColor({ sourceId: 'test-tone', engine: 'tone', ...SOURCE }),
    ];
    expect(new Set(colours).size).toBe(5);
  });

  it('falls back to the synth colour when the track is missing', () => {
    expect(channelColor(undefined)).toBe(CHANNEL_KIND_COLORS.synth);
  });
});
