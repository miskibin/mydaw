/**
 * What a rack channel *is*, for a badge and for which window a name-click
 * opens.
 *
 * Derived from the engine's `Source::engine()` plus the drum ids, so a
 * synthesised kick is a drum (the thing a step sequencer is for) and Analog
 * Lab is a plugin (the thing that has its own window).
 */

import type { TrackDto } from '../api/types';

const DRUM_IDS = new Set(['kick', 'snare', 'hat-closed', 'hat-open', 'drum-kit']);

export type ChannelKind = 'plugin' | 'sample' | 'drum' | 'synth' | 'tone';

export interface ChannelKindInfo {
  kind: ChannelKind;
  /** Short rack badge: `CLAP`, `VST3`, `CLIP`, `SMP`, `DRM`, `SYN`, `TONE`. */
  badge: string;
  /** Longer tooltip. */
  title: string;
}

/** The engine string `Source::engine()` publishes, plus the prefixes a mock has to invent. */
export function engineOf(sourceId: string): TrackDto['engine'] {
  if (sourceId.startsWith('clap:')) return 'clap';
  if (sourceId.startsWith('vst3:')) return 'vst3';
  if (sourceId.startsWith('clip:')) return 'clip';
  if (sourceId.startsWith('oneshot:')) return 'sample';
  if (sourceId.startsWith('sampler:')) return 'sampler';
  if (sourceId === 'test-tone') return 'tone';
  return 'synth';
}

export function channelKind(
  track: Pick<TrackDto, 'sourceId' | 'engine' | 'clap' | 'vst3'>,
): ChannelKindInfo {
  if (track.clap || track.engine === 'clap') {
    return { kind: 'plugin', badge: 'CLAP', title: 'Hosted CLAP plugin' };
  }
  if (track.vst3 || track.engine === 'vst3') {
    return { kind: 'plugin', badge: 'VST3', title: 'Hosted VST3 plugin' };
  }
  if (track.engine === 'clip' || track.sourceId.startsWith('clip:')) {
    return { kind: 'sample', badge: 'CLIP', title: 'Audio clip — plays from the playlist' };
  }
  if (track.engine === 'sample' || track.sourceId.startsWith('oneshot:')) {
    return { kind: 'sample', badge: 'SMP', title: 'Sampler — plays this file from notes' };
  }
  if (track.engine === 'sampler' || track.sourceId.startsWith('sampler:')) {
    return { kind: 'sample', badge: 'SMP', title: 'Sampled instrument' };
  }
  if (track.engine === 'tone' || track.sourceId === 'test-tone') {
    return { kind: 'tone', badge: 'TONE', title: 'Diagnostic test tone' };
  }
  if (DRUM_IDS.has(track.sourceId)) {
    return { kind: 'drum', badge: 'DRM', title: 'Synthesised drum' };
  }
  return { kind: 'synth', badge: 'SYN', title: 'Built-in synthesizer' };
}

/** Whether clicking the channel name should open the sample/instrument window. */
export function hasSampleWindow(
  track: Pick<TrackDto, 'sourceId' | 'engine' | 'clap' | 'vst3'>,
): boolean {
  return channelKind(track).kind !== 'plugin';
}

/** Playlist audio-clip channels have no step grid: they play from the arrangement. */
export function isAudioClipChannel(
  track: Pick<TrackDto, 'sourceId' | 'engine'>,
): boolean {
  return track.engine === 'clip' || track.sourceId.startsWith('clip:');
}
