// Audio that stops or comes back by itself (device unplugged, PipeWire/ALSA restarted, a crash
// in the audio callback) is reported in the status line, wherever the user is: without it the
// transport just looked frozen.
import type { AudioStatus } from '../api/types';
import { t } from '../lib/i18n';

/** What to tell the user when the audio status changes from `prev` to `next`; null = nothing new. */
export function audioNotice(prev: AudioStatus | null, next: AudioStatus): string | null {
  if (next.error && (!prev || prev.error !== next.error)) return t('d.audio.stopped', { err: next.error });
  if (prev && next.restarts > prev.restarts) return t('d.audio.restarted', { why: next.note ?? '' });
  return null;
}

/** Follows `audio-status` events and says what changed by itself. */
export async function watchAudio(
  first: AudioStatus,
  on: (cb: (s: AudioStatus) => void) => Promise<unknown>,
  say: (msg: string) => void,
) {
  let last = first;
  await on(s => {
    const msg = audioNotice(last, s);
    last = s;
    if (msg) say(msg);
  });
}
