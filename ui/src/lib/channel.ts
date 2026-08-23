/**
 * What a rack channel *is*, for its colour across the shell and for which
 * window a name-click opens.
 *
 * Derived from the engine's `Source::engine()`, so a sampled kit is a sample
 * (the thing the sample window opens on) and Analog Lab is a plugin (the thing
 * that has its own window).
 *
 * There used to be a `drum` kind for the synthesised drums. They are gone --
 * `sampler:808-kit` replaced them -- and a kit is a `sample`, so the kind went
 * with them rather than staying as a branch no source can reach.
 */

import type { TrackDto } from '../api/types';
import { CHANNEL_KIND_COLORS } from '../theme/tokens';

export type ChannelKind = 'plugin' | 'sample' | 'synth' | 'tone';

export interface ChannelKindInfo {
  kind: ChannelKind;
  /** Short label kept for tooltips and the sample window, not drawn on the rack. */
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
  return { kind: 'synth', badge: 'SYN', title: 'Built-in synthesizer' };
}

/**
 * The colour this channel wears everywhere: rack LED, mixer strip, piano roll,
 * playlist marks. Same kind → same colour, regardless of track id.
 */
export function channelColor(
  track: Pick<TrackDto, 'sourceId' | 'engine' | 'clap' | 'vst3'> | undefined | null,
): string {
  if (!track) return CHANNEL_KIND_COLORS.synth;
  return CHANNEL_KIND_COLORS[channelKind(track).kind];
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
