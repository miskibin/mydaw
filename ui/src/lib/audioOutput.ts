/** Buffer sizes Settings offers. `0` is the device default. */
export const AUDIO_BUFFER_CHOICES = [0, 128, 256, 512, 1024, 2048] as const;

export type AudioBufferChoice = (typeof AUDIO_BUFFER_CHOICES)[number];

/**
 * Snap a requested buffer onto a legal choice. `0` stays `0` (device default).
 * Matches `AudioSettings::snap_buffer_frames` in Rust.
 */
export function snapAudioBufferFrames(frames: number): number {
  if (!Number.isFinite(frames) || frames <= 0) return 0;
  const choices = [128, 256, 512, 1024, 2048];
  return choices.reduce((best, choice) =>
    Math.abs(choice - frames) < Math.abs(best - frames) ? choice : best,
  );
}

export function audioBufferLabel(frames: number): string {
  if (frames <= 0) return 'Device';
  return String(frames);
}

export function audioBufferMs(frames: number, sampleRate: number): string {
  if (frames <= 0 || !(sampleRate > 0)) return '';
  const ms = (frames / sampleRate) * 1000;
  return `${ms.toFixed(1)} ms`;
}
