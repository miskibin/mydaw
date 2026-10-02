/**
 * Local playhead interpolation.
 *
 * The RT side only reports the transport position at 30-60 Hz (and we do not
 * want to raise that -- see the IPC perf spike). Rendering directly from those
 * packets makes the playhead visibly step, so the UI runs its own clock:
 * every backend packet becomes an *anchor* (sample position + the wall clock
 * time we received it), and every animation frame extrapolates from the most
 * recent anchor using `performance.now()` and the sample rate.
 *
 * Anchors are authoritative: a new one that disagrees with the extrapolation
 * pulls the displayed position back. Small disagreements (normal jitter) are
 * absorbed gradually so the playhead never twitches backwards; large ones
 * (a seek, a loop wrap, a stall) snap immediately.
 */

/** Disagreement above this, in seconds, is treated as a seek and snapped. */
const SNAP_THRESHOLD_SECONDS = 0.25;

/** Fraction of the remaining error absorbed per second while correcting. */
const CORRECTION_RATE_PER_SECOND = 4;

export interface PlayheadOptions {
  /** Engine sample rate in Hz. Defaults to 48 kHz until the backend says otherwise. */
  sampleRate?: number;
  /** Injectable clock, in milliseconds. Defaults to `performance.now`. */
  now?: () => number;
}

export class Playhead {
  private sampleRate: number;
  private readonly clock: () => number;

  /** Last position reported by the backend, in frames. */
  private anchorSamples = 0;
  /** Clock reading when that position was received, in ms. */
  private anchorAtMs: number;
  private playing = false;

  /** Signed error (frames) still to be absorbed: displayed - authoritative. */
  private pendingError = 0;
  /** Clock reading of the last `positionSamples()` call, for error decay. */
  private lastReadAtMs: number;

  constructor(options: PlayheadOptions = {}) {
    this.sampleRate = options.sampleRate ?? 48_000;
    this.clock = options.now ?? (() => performance.now());
    this.anchorAtMs = this.clock();
    this.lastReadAtMs = this.anchorAtMs;
  }

  /** Engine sample rate changed (device switch). Re-anchors at the current position. */
  setSampleRate(sampleRate: number): void {
    if (sampleRate <= 0 || sampleRate === this.sampleRate) return;
    const current = this.positionSamples();
    this.sampleRate = sampleRate;
    this.anchorSamples = current;
    this.anchorAtMs = this.clock();
  }

  getSampleRate(): number {
    return this.sampleRate;
  }

  isPlaying(): boolean {
    return this.playing;
  }

  /**
   * Feed an authoritative update from the backend (one per `MeterFrame`).
   * Carries over the visible position as correction error so the display stays
   * continuous instead of jumping to the packet's value.
   */
  update(samplePos: number, playing: boolean): void {
    const nowMs = this.clock();
    const displayed = this.positionSamples(nowMs);

    this.anchorSamples = samplePos;
    this.anchorAtMs = nowMs;

    const error = displayed - samplePos;
    const snapThreshold = SNAP_THRESHOLD_SECONDS * this.sampleRate;
    // Transport state changes are always a hard cut -- no smoothing across them.
    this.pendingError =
      !playing || this.playing !== playing || Math.abs(error) > snapThreshold ? 0 : error;

    this.playing = playing;
    this.lastReadAtMs = nowMs;
  }

  /** Hard reset, e.g. after a user-initiated stop or a project load. */
  reset(samplePos = 0, playing = false): void {
    this.anchorSamples = samplePos;
    this.anchorAtMs = this.clock();
    this.lastReadAtMs = this.anchorAtMs;
    this.pendingError = 0;
    this.playing = playing;
  }

  /** Interpolated transport position in frames. Safe to call every animation frame. */
  positionSamples(nowMs: number = this.clock()): number {
    const elapsedSeconds = Math.max(0, nowMs - this.anchorAtMs) / 1000;
    const extrapolated =
      this.anchorSamples + (this.playing ? elapsedSeconds * this.sampleRate : 0);

    if (this.pendingError !== 0) {
      const dt = Math.max(0, nowMs - this.lastReadAtMs) / 1000;
      // Exponential decay towards zero error; framerate independent.
      const decay = Math.exp(-CORRECTION_RATE_PER_SECOND * dt);
      this.pendingError = Math.abs(this.pendingError) < 1 ? 0 : this.pendingError * decay;
    }
    this.lastReadAtMs = nowMs;

    return Math.max(0, extrapolated + this.pendingError);
  }

  /** Interpolated transport position in seconds. */
  positionSeconds(nowMs?: number): number {
    return this.positionSamples(nowMs) / this.sampleRate;
  }
}

/** Formats a sample position as `mm:ss.mmm` for the transport readout. */
export function formatTimecode(seconds: number): string {
  const clamped = Math.max(0, seconds);
  const minutes = Math.floor(clamped / 60);
  const secs = Math.floor(clamped % 60);
  const millis = Math.floor((clamped % 1) * 1000);
  return (
    `${minutes.toString().padStart(2, '0')}:` +
    `${secs.toString().padStart(2, '0')}.` +
    `${millis.toString().padStart(3, '0')}`
  );
}
