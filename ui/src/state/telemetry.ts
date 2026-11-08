// Telemetry stream → plain mutable objects read by the frame loop (no per-frame Solid churn).
// Only edges (play/stop, preview on/off) are written into the reactive UI store.
import { be } from '../api/backend';
import { TelemetryDecoder, type FxFrame } from '../api/telemetry';
import { setU, U } from './ui';

export const tele = new TelemetryDecoder();
/** Bumped on every decoded frame. */
export const teleClock = { frames: 0, lastAt: 0 };

/** Peaks of the take being recorded, accumulated from the per-frame deltas. */
export const recState = {
  active: false, track: 0, clipStart: 0, seconds: 0,
  peaks: new Float32Array(4096), nPeaks: 0, version: 0,
  /** Loop recording: passes started so far (0 = first pass); each pass becomes a take. */
  pass: 0,
};

/** Per-effect frame listeners: called for every decoded analysis frame (bursts included), not once per animation frame. */
const fxListeners = new Map<number, Set<(a: FxFrame, at: number) => void>>();
export function onFxFrame(id: number, cb: (a: FxFrame, at: number) => void): () => void {
  let s = fxListeners.get(id); if (!s) fxListeners.set(id, s = new Set());
  s.add(cb);
  return () => { s!.delete(cb); };
}

function onFrame() {
  const f = tele.frame;
  teleClock.frames++; teleClock.lastAt = performance.now();
  for (const id of f.fxIds) {
    const ls = fxListeners.get(id), a = tele.fx.get(id);
    if (ls && a) for (const cb of ls) { try { cb(a, teleClock.lastAt); } catch (e) { console.error(e); } }
  }
  if (f.playing !== U.playing) setU('playing', f.playing);
  if (f.previewing !== U.previewing) setU('previewing', f.previewing);
  if ((f.patBeat >= 0) !== U.patPlaying) setU('patPlaying', f.patBeat >= 0);
  const r = f.rec;
  if (r) {
    if (!recState.active || r.clipStart !== recState.clipStart || r.track !== recState.track) {
      recState.active = true; recState.nPeaks = 0; recState.track = r.track; recState.clipStart = r.clipStart; recState.pass = 0;
    } else if (r.peakOffset === 0 && recState.nPeaks > 0) {
      // loop recording wrapped: the engine restarts the live clip for the next pass (take)
      recState.nPeaks = 0; recState.pass++;
    }
    recState.seconds = r.seconds;
    const need = (r.peakOffset + r.nPeaks) * 2;
    if (recState.peaks.length < need) { const nb = new Float32Array(Math.max(need, recState.peaks.length * 2)); nb.set(recState.peaks); recState.peaks = nb; }
    recState.peaks.set(r.peaks, r.peakOffset * 2);
    recState.nPeaks = Math.max(recState.nPeaks, r.peakOffset + r.nPeaks);
    recState.version++;
  } else if (recState.active) {
    recState.active = false; recState.version++;
  }
}

export async function startTelemetry() {
  await be().subscribeTelemetry(buf => { if (tele.decode(buf)) onFrame(); });
}

/** Current playhead beat: telemetry while playing, otherwise the start marker. */
export function curBeat(): number {
  return tele.frame.playing ? tele.frame.beat : U.start;
}
