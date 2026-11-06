// Replays real engine telemetry of Pitch Correct (60 Hz, a sung A3–B3–C4–D4–E4 melody) through
// the real decoder into the Pitch Correct history, at even and at bursty cadences, and checks that
// the plotted lines follow the melody with evenly spaced sample times.
import { describe, expect, it } from 'vitest';
import { encodeFrame, SCOPE_LEN, SPEC_BANDS, TelemetryDecoder } from '../../api/telemetry';
import { hzToMidi } from '../../lib/pitch';
import fixture from '../../lib/__fixtures__/tune-frames.json';
import { TimeRing } from './common';

const zeros = (n: number) => new Float32Array(n);
function frames(): ArrayBuffer[] {
  return (fixture.frames as number[][]).map(([seq, inPeak, outPeak, ...m]) => encodeFrame({
    seq, beat: 0, playing: true, recording: false, previewing: false, inLevel: 0, previewPos: -1, cpu: 0, ins: [],
    fx: [{ id: 5000, inPeak, outPeak, grDb: 0, meters: m, specIn: zeros(SPEC_BANDS), specOut: zeros(SPEC_BANDS), scopeIn: zeros(SCOPE_LEN), scopeOut: zeros(SCOPE_LEN) }], rec: null,
  }));
}

/** Feed frames: `at(i)` = arrival time (ms) of frame i. Mirrors TuneViz.ingest. */
function replay(at: (i: number) => number) {
  const d = new TelemetryDecoder(), ring = new TimeRing(600, 3);
  frames().forEach((buf, i) => {
    expect(d.decode(buf)).toBe(true);
    const a = d.fx.get(5000)!, m = a.meters, v = m[0] > 0;
    ring.push(at(i), [v ? hzToMidi(m[0]) : NaN, v && m[2] > 0 ? hzToMidi(m[2]) : NaN, v && m[1] > 0 ? hzToMidi(m[1]) : NaN], a.seq);
  });
  return ring;
}

function checkMelody(ring: TimeRing) {
  // sample times: monotonic, evenly spaced at the engine's 60 Hz
  const dts: number[] = [];
  for (let i = 1; i < ring.n; i++) dts.push(ring.t[ring.at(i)] - ring.t[ring.at(i - 1)]);
  expect(Math.min(...dts.slice(10))).toBeGreaterThan(8);
  expect(Math.max(...dts.slice(10))).toBeLessThan(28);
  // targets step up through A3 B3 C4 D4 E4; the corrected line sits within 50 ct of them
  const targets: number[] = [];
  for (let i = 0; i < ring.n; i++) {
    const j = ring.at(i), tg = ring.ch[2][j], out = ring.ch[1][j];
    if (Number.isNaN(tg)) continue;
    if (targets[targets.length - 1] !== Math.round(tg)) targets.push(Math.round(tg));
    expect(Math.abs(out - tg)).toBeLessThan(0.5);
  }
  expect(targets.slice(0, 4)).toEqual([57, 59, 60, 62]);
}

describe('Pitch Correct history with real telemetry', () => {
  it('even 60 Hz arrivals', () => checkMelody(replay(i => 1000 + i * 16.7)));
  it('bursty arrivals (6 frames every 100 ms) keep true timing', () => {
    const ring = replay(i => 1000 + Math.ceil((i + 1) / 6) * 100 + (i % 6) * 0.05); // delivered late, 6 at a time
    checkMelody(ring);
    // the history spans the real duration, not a compressed one
    const span = ring.t[ring.at(ring.n - 1)] - ring.t[ring.at(0)];
    expect(span).toBeGreaterThan(179 * 16.7 * 0.85);
  });
  it('the display clock never stalls or jumps with bursty arrivals', () => {
    const fr = frames(), d = new TelemetryDecoder(), ring = new TimeRing(600, 1);
    let next = 0, last = -Infinity; const steps: number[] = [];
    for (let f = 0; f < 200; f++) {
      const now = 1000 + f * 16.7;
      while (next < fr.length && 1000 + Math.ceil((next + 1) / 6) * 100 <= now) { d.decode(fr[next]); ring.push(now, [0], d.fx.get(5000)!.seq); next++; }
      const c = ring.clock(now);
      if (f > 30 && next < fr.length) steps.push(c - last);
      last = c;
    }
    expect(Math.min(...steps)).toBeGreaterThan(0);
    expect(Math.max(...steps)).toBeLessThan(16.7 * 1.6);
  });
});
