/**
 * File › Export › WAV, the arithmetic and the wording.
 *
 * Pure, so the dialog and the mock agree with each other and with the engine:
 * Song renders exactly to the end of the last playlist clip (audio clips
 * included), Pattern renders one loop of the active pattern, and both add the
 * tail. Nothing is rounded up to a bar -- the tail is what covers the ring-out.
 */

import type { ExportJobStatus, ExportOptions } from '../api/types';

/** FL's Ctrl+R defaults, minus FL's MP3 habit: a mastering-grade 24-bit WAV. */
export const DEFAULT_EXPORT_OPTIONS: ExportOptions = {
  mode: 'song',
  sampleRate: 44100,
  bitDepth: 24,
  tailSeconds: 2,
  normalize: false,
};

export const EXPORT_SAMPLE_RATES = [44100, 48000] as const;
export const EXPORT_BIT_DEPTHS = [16, 24, 32] as const;
export const EXPORT_TAILS = [0, 1, 2, 4, 8] as const;
/** Engine limit, in seconds, for the body plus the tail. */
export const MAX_EXPORT_SECONDS = 60 * 60;

const TICKS_PER_BEAT = 960;

/** The ticks the render plays before the tail, or 0 when there is nothing to play. */
export function exportBodyTicks(
  mode: ExportOptions['mode'],
  playlistEndTicks: number,
  patternLengthTicks: number,
): number {
  return Math.max(0, mode === 'song' ? playlistEndTicks : patternLengthTicks);
}

/** Seconds of audio the file will hold. */
export function exportSeconds(bodyTicks: number, bpm: number, tailSeconds: number): number {
  if (!(bpm > 0)) return tailSeconds;
  return (bodyTicks / TICKS_PER_BEAT) * (60 / bpm) + tailSeconds;
}

/** Why Export cannot start, in the user's words; `null` when it can. */
export function exportBlocker(
  mode: ExportOptions['mode'],
  bodyTicks: number,
  seconds: number,
): string | null {
  if (bodyTicks <= 0) {
    return mode === 'song'
      ? 'The playlist is empty. Place clips in the playlist, or export the pattern.'
      : 'The active pattern has no length.';
  }
  if (seconds > MAX_EXPORT_SECONDS) {
    return `Longer than ${MAX_EXPORT_SECONDS / 60} minutes — the export limit.`;
  }
  return null;
}

/** `m:ss.s` — a song length, readable at a glance. */
export function formatDuration(seconds: number): string {
  const safe = Math.max(0, seconds);
  const minutes = Math.floor(safe / 60);
  const rest = safe - minutes * 60;
  const text = rest.toFixed(1).padStart(4, '0');
  // 59.96 rounds to "60.0": carry it rather than print 0:60.0.
  return text === '60.0' ? `${minutes + 1}:00.0` : `${minutes}:${text}`;
}

export function bitDepthLabel(bits: number): string {
  return bits === 32 ? '32-bit float' : `${bits}-bit`;
}

export function sampleRateLabel(rate: number): string {
  return `${rate % 1000 === 0 ? rate / 1000 : (rate / 1000).toFixed(1)} kHz`;
}

/** The file name, for a status line that has no room for the folder. */
export function fileNameOf(path: string): string {
  return path.split(/[\\/]/).pop() || path;
}

export function isFinished(status: ExportJobStatus | null): boolean {
  return status !== null && status.status !== 'running';
}

/** Whole percent, never 100 before the worker says it is done. */
export function progressPercent(status: ExportJobStatus): number {
  if (status.status === 'completed') return 100;
  const value = Math.min(1, Math.max(0, status.progress ?? 0));
  return Math.min(99, Math.floor(value * 100));
}

/** What the worker is doing: rendering the graph, then encoding the file. */
export function progressLabel(status: ExportJobStatus): string {
  return (status.progress ?? 0) >= 0.95 ? 'Writing file…' : 'Rendering…';
}

/** One line under "Saved": length, format, peak. */
export function exportSummary(status: ExportJobStatus): string {
  const parts: string[] = [];
  if (status.seconds !== undefined) parts.push(formatDuration(status.seconds));
  if (status.bitDepth !== undefined) parts.push(bitDepthLabel(status.bitDepth));
  if (status.sampleRate !== undefined) parts.push(sampleRateLabel(status.sampleRate));
  if (status.peakDbfs !== undefined && status.peakDbfs !== null) {
    parts.push(`peak ${status.peakDbfs.toFixed(1).replace('-', '−')} dBFS`);
  }
  if (status.normalizeGainDb !== undefined && status.normalizeGainDb !== null) {
    const gain = status.normalizeGainDb;
    parts.push(`normalized ${gain >= 0 ? '+' : '−'}${Math.abs(gain).toFixed(1)} dB`);
  }
  return parts.join(' · ');
}

/** Things worth fixing before sending the file to anyone. */
export function exportWarnings(status: ExportJobStatus): string[] {
  const warnings: string[] = [];
  if (status.silent) {
    warnings.push('The file is silent. Check mutes, solos and the master volume.');
  }
  const over = status.clippedSamples ?? 0;
  if (over > 0) {
    const count = `${over.toLocaleString('en-US')} sample${over === 1 ? '' : 's'}`;
    warnings.push(
      status.bitDepth === 32
        ? `${count} above 0 dBFS. Kept in 32-bit float, but most players will clip them.`
        : `${count} clipped at 0 dBFS. Lower the master or turn on Normalize.`,
    );
  }
  return warnings;
}

/** Options remembered from the last export, or the defaults for anything unusable. */
export function sanitizeExportOptions(value: unknown): ExportOptions {
  const raw = (typeof value === 'object' && value !== null ? value : {}) as Partial<
    Record<keyof ExportOptions, unknown>
  >;
  const pick = <T,>(candidate: unknown, allowed: readonly T[], fallback: T): T =>
    allowed.includes(candidate as T) ? (candidate as T) : fallback;
  return {
    mode: pick(raw.mode, ['song', 'pattern'] as const, DEFAULT_EXPORT_OPTIONS.mode),
    sampleRate: pick(raw.sampleRate, EXPORT_SAMPLE_RATES, DEFAULT_EXPORT_OPTIONS.sampleRate),
    bitDepth: pick(raw.bitDepth, EXPORT_BIT_DEPTHS, DEFAULT_EXPORT_OPTIONS.bitDepth),
    tailSeconds:
      typeof raw.tailSeconds === 'number' &&
      Number.isFinite(raw.tailSeconds) &&
      raw.tailSeconds >= 0 &&
      raw.tailSeconds <= 30
        ? raw.tailSeconds
        : DEFAULT_EXPORT_OPTIONS.tailSeconds,
    normalize: typeof raw.normalize === 'boolean' ? raw.normalize : DEFAULT_EXPORT_OPTIONS.normalize,
  };
}
