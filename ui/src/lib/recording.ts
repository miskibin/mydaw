/**
 * The transport bar's record control, as arithmetic and sentences.
 *
 * Kept free of Solid and of the DOM so the three states, their tooltips and the
 * elapsed readout can be reasoned about (and tested) without a browser -- the
 * same split `panels/mixer/scale.ts` makes for the fader.
 */

import type { InputStatusDto } from '../api/types';

/** Below this the take is treated as silent, and the tooltip says so. */
export const SILENT_PEAK = 0.001;

/** How much of the length limit has to be gone before the bar warns. */
export const LIMIT_WARN_FRACTION = 0.9;

/**
 * Elapsed take time as `m:ss`, or `h:mm:ss` past an hour.
 *
 * Not the transport's `mm:ss.mmm`: milliseconds on a take counter are noise --
 * nobody records to the millisecond -- and the extra four characters would push
 * the transport group wider every time recording started.
 */
export function formatElapsed(seconds: number): string {
  const total = Math.max(0, Math.floor(Number.isFinite(seconds) ? seconds : 0));
  const secs = total % 60;
  const minutes = Math.floor(total / 60) % 60;
  const hours = Math.floor(total / 3600);
  const pad = (value: number) => value.toString().padStart(2, '0');
  return hours > 0 ? `${hours}:${pad(minutes)}:${pad(secs)}` : `${minutes}:${pad(secs)}`;
}

/**
 * What the REC button does next.
 *
 * FL's record mode: REC arms (the input opens, is metered and monitored), and
 * while armed the transport records -- Play starts a take, Stop lands it on the
 * playlist. With the transport already running, REC punches in and out.
 *
 * - `unavailable` -- no capture device at all
 * - `arm`         -- idle and stopped: enter record mode
 * - `punch-in`    -- playing and not recording: start a take now
 * - `punch-out`   -- recording: end the take, keep playing
 * - `disarm`      -- armed and stopped: leave record mode
 */
export type RecordAction = 'unavailable' | 'arm' | 'punch-in' | 'punch-out' | 'disarm';

export function recordAction(
  status: InputStatusDto | undefined,
  hasDevices: boolean,
  playing = false,
): RecordAction {
  if (!hasDevices) return 'unavailable';
  const state = status?.state ?? 'idle';
  if (state === 'recording') return 'punch-out';
  if (playing) return 'punch-in';
  return state === 'armed' ? 'disarm' : 'arm';
}

/**
 * The REC button's tooltip.
 *
 * Every state says what a click will do *and* what the state means, because a
 * disabled or armed record button with no explanation is the single most
 * confusing control a DAW has.
 */
export function recordTitle(
  status: InputStatusDto | undefined,
  hasDevices: boolean,
  playing = false,
): string {
  const device = status?.deviceName || 'the input';
  switch (recordAction(status, hasDevices, playing)) {
    case 'unavailable':
      return 'No audio input device — plug in an interface or a microphone, then open Settings › Audio input';
    case 'arm':
      return 'Record (R) — arm the input, then press Play: takes land on the playlist where you sang them';
    case 'punch-in':
      return `Punch in (R) — start recording from ${device} now`;
    case 'punch-out':
      return 'Punch out (R) — end this take and keep playing; Stop also ends it';
    case 'disarm':
      return `Armed: Play (Space) records from ${device}. Click to leave record mode (R)`;
  }
}

/** The secondary button's tooltip: release the device, or bin the take. */
export function cancelTitle(status: InputStatusDto | undefined): string {
  return status?.state === 'recording'
    ? 'Discard this take and stay armed'
    : 'Release the input device';
}

/**
 * Whether the take is close enough to the length limit to warn.
 *
 * A limit that only announces itself by stopping is a limit that loses a take;
 * the last 10% of the budget turns the counter into a warning instead.
 */
export function nearLimit(status: InputStatusDto | undefined): boolean {
  if (!status || status.state !== 'recording' || !(status.maxSeconds > 0)) return false;
  return status.elapsedSeconds >= status.maxSeconds * LIMIT_WARN_FRACTION;
}

/**
 * One line describing a capture device for the Settings row.
 *
 * A device the host would not describe reports zeros; that is a real state --
 * it is plugged in and it will not open -- and saying so is more use than
 * hiding the row.
 */
export function inputDeviceNote(channels: number, sampleRate: number): string {
  if (channels <= 0 || sampleRate <= 0) {
    return 'The system will not report this device’s format — it cannot be opened';
  }
  const layout = channels === 1 ? 'Mono' : channels === 2 ? 'Stereo' : `${channels} channels`;
  return `${layout} · ${(sampleRate / 1000).toFixed(1)} kHz`;
}
