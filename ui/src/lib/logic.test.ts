import { describe, expect, it } from 'vitest';
import { encodeFrame, MAGIC, SCOPE_LEN, SPEC_BANDS, TelemetryDecoder } from '../api/telemetry';
import { demoProject, mockCatalog } from '../api/mockData';
import type { Clip } from '../api/types';
import { applyAll, OpError } from './applyOps';
import { clampMove, duplicateSpan, insertTrackOps, moveClips, pasteClips, resizeClip, sliceClip } from './clipOps';
import { eqCurve } from './eq';
import { exportRange, exportSeconds, fmtExportLen, songEnd } from './export';
import { fmtClock, fmtPos, panText, TapTracker, volText } from './format';
import { loadPreset, newFx, paramsMatch, presetInfo, pFmt, pNorm, pVal, resolvePreset } from './presets';
import { autoSnap, snapFloor, snapLabel, snapRes, snapRound } from './snap';

const cat = mockCatalog();
const clip = (o: Partial<Clip>): Clip => ({ id: 1, track: 0, start: 0, len: 4, offset: 0, kind: 'pat', ref: 1, muted: false, gain: 0, fadeIn: 0, fadeOut: 0, pan: 0, norm: false, rev: false, ...o });

describe('snap', () => {
  it('auto picks the smallest unit at least 10 px wide', () => {
    expect(autoSnap(40)).toBe(0.25);
    expect(autoSnap(39.9)).toBe(0.5);
    expect(autoSnap(22)).toBe(0.5);
    expect(autoSnap(10)).toBe(1);
    expect(autoSnap(6)).toBe(2);
    expect(autoSnap(4)).toBe(4);
    expect(autoSnap(1)).toBe(4);
  });
  it('labels show the resolved auto unit', () => {
    expect(snapLabel(-1, 22)).toBe('Snap: auto · 1/2 beat');
    expect(snapLabel(-1, 60)).toBe('Snap: auto · 1/4 beat');
    expect(snapLabel(-1, 5)).toBe('Snap: auto · 2 beaty');
    expect(snapLabel(4, 22)).toBe('Snap: bar');
  });
  it('rounds and floors', () => {
    expect(snapRes(-1, 22)).toBe(0.5);
    expect(snapRes(1, 22)).toBe(1);
    expect(snapRound(1.3, 0.5)).toBe(1.5);
    expect(snapFloor(1.9, 0.5)).toBe(1.5);
    expect(snapFloor(2, 0.5)).toBe(2);
    expect(snapRound(1.30001, 0)).toBeCloseTo(1.296875);
  });
});

describe('telemetry decoder', () => {
  it('round-trips a full frame', () => {
    const spec = Array.from({ length: SPEC_BANDS }, (_, i) => -i);
    const sc = Array.from({ length: SCOPE_LEN }, (_, i) => Math.sin(i));
    const buf = encodeFrame({
      seq: 42, beat: -2.5, playing: true, recording: true, previewing: false, inLevel: 0.5, previewPos: -1, cpu: 0.25,
      ins: [{ id: 0, l: 0.1, r: 0.2 }, { id: 7, l: 0.3, r: 0.4 }],
      fx: [{ id: 904, inPeak: 0.6, outPeak: 0.7, grDb: -3, specIn: spec, specOut: spec.map(x => x - 1), scopeIn: sc, scopeOut: sc }],
      rec: { track: 5, clipStart: 16.25, seconds: 1.5, peakOffset: 100, peaks: [-0.5, 0.5, -0.25, 0.25] },
    });
    const v = new DataView(buf);
    expect(v.getUint32(0, true)).toBe(MAGIC);
    const d = new TelemetryDecoder();
    expect(d.decode(buf)).toBe(true);
    const f = d.frame;
    expect(f.seq).toBe(42); expect(f.beat).toBe(-2.5);
    expect(f.playing).toBe(true); expect(f.recording).toBe(true); expect(f.previewing).toBe(false);
    expect(f.inLevel).toBeCloseTo(0.5); expect(f.cpu).toBeCloseTo(0.25); expect(f.previewPos).toBe(-1);
    expect(f.ins.get(7)).toEqual({ l: expect.closeTo(0.3), r: expect.closeTo(0.4) });
    expect(f.fxIds).toEqual([904]);
    const fx = d.fx.get(904)!;
    expect(fx.grDb).toBeCloseTo(-3); expect(fx.specIn[10]).toBe(-10); expect(fx.specOut[127]).toBe(-128);
    expect(fx.scopeOut[3]).toBeCloseTo(Math.sin(3));
    expect(f.rec).toMatchObject({ track: 5, clipStart: 16.25, peakOffset: 100, nPeaks: 2 });
    expect(Array.from(f.rec!.peaks)).toEqual([-0.5, 0.5, -0.25, 0.25]);
  });
  it('keeps the last analysis when a frame carries no fx, and rejects bad magic', () => {
    const d = new TelemetryDecoder();
    const base = { seq: 1, beat: 0, playing: false, recording: false, previewing: false, inLevel: 0, previewPos: -1, cpu: 0, ins: [], rec: null };
    d.decode(encodeFrame({ ...base, fx: [{ id: 1, inPeak: 1, outPeak: 1, grDb: 0, specIn: [], specOut: [], scopeIn: [], scopeOut: [] }] }));
    d.decode(encodeFrame({ ...base, seq: 2, fx: [] }));
    expect(d.frame.fxIds).toEqual([]);
    expect(d.fx.get(1)!.seq).toBe(1);
    expect(d.frame.rec).toBeNull();
    const bad = encodeFrame({ ...base, fx: [] }); new DataView(bad).setUint32(0, 1, true);
    expect(d.decode(bad)).toBe(false);
  });
});

describe('presets', () => {
  it('new effects load preset 0 and match it', () => {
    const f = newFx(cat, 10, 'comp', 128);
    expect(f.preset).toBe(0);
    expect(presetInfo(cat, f, 128)).toEqual({ idx: 0, name: 'Domyślny', mod: false });
  });
  it('a moved parameter shows the last preset as modified', () => {
    const f = loadPreset(cat, newFx(cat, 10, 'reverb', 128), 3, 128);
    expect(presetInfo(cat, f, 128).name).toBe('Wokal plate');
    const g = { ...f, params: { ...f.params, mix: 40 } };
    expect(presetInfo(cat, g, 128)).toEqual({ idx: 3, name: 'Wokal plate', mod: true });
  });
  it('tempo-synced delay presets resolve time at the bpm', () => {
    const r = resolvePreset(cat, 'delay', 1, 120)!;
    expect(r.params.time).toBeCloseTo(500);
    const f = loadPreset(cat, newFx(cat, 1, 'delay', 120), 1, 120);
    expect(presetInfo(cat, f, 120).mod).toBe(false);
    expect(presetInfo(cat, f, 140).mod).toBe(true); // tempo changed: no longer the synced value
  });
  it('EQ presets compare bands', () => {
    const f = loadPreset(cat, newFx(cat, 1, 'eq', 128), 1, 128);
    expect(presetInfo(cat, f, 128).name).toBe('Wokal — obecność');
    const bands = f.bands!.map(b => ({ ...b })); bands[2].g = -1;
    expect(paramsMatch({ params: f.params, bands }, resolvePreset(cat, 'eq', 1, 128)!)).toBe(false);
    expect(presetInfo(cat, { ...f, bands }, 128).mod).toBe(true);
  });
  it('stepping wraps around', () => {
    const f = newFx(cat, 1, 'limit', 128);
    expect(loadPreset(cat, f, -1, 128).preset).toBe(3);
    expect(loadPreset(cat, f, 4, 128).preset).toBe(0);
  });
  it('knob mapping is log where asked', () => {
    const ratio = cat.types.comp!.params.find(p => p.id === 'ratio')!;
    expect(pVal(ratio, pNorm(ratio, 4))).toBeCloseTo(4);
    expect(pNorm(ratio, Math.sqrt(20))).toBeCloseTo(0.5);
    expect(pFmt(ratio, 4)).toBe('4.0:1');
    const hi = cat.types.reverb!.params.find(p => p.id === 'hi')!;
    expect(pFmt(hi, 8000)).toBe('8.00 kHz');
  });
});

describe('op builders', () => {
  it('clamps group moves to the timeline and tracks', () => {
    const cs = [clip({ id: 1, start: 2, track: 1 }), clip({ id: 2, start: 8, track: 3 })];
    expect(clampMove(cs, -5, -3, 8)).toEqual([-2, -1]);
    expect(clampMove(cs, 1000, 10, 8)).toEqual([512 - 12, 4]);
    expect(moveClips(cs, 1, 1).map(c => [c.start, c.track])).toEqual([[3, 2], [9, 4]]);
  });
  it('slices inside only', () => {
    const c = clip({ start: 4, len: 8, offset: 1, fadeIn: 0.1, fadeOut: 0.2 });
    expect(sliceClip(c, 4, 9)).toBeNull();
    const [a, z] = sliceClip(c, 6, 9)!;
    expect([a.start, a.len, a.fadeOut]).toEqual([4, 2, 0]);
    expect([z.id, z.start, z.len, z.offset, z.fadeIn]).toEqual([9, 6, 6, 3, 0]);
  });
  it('resizes audio clips within the source', () => {
    const c = clip({ kind: 'audio', start: 4, len: 4, offset: 1 });
    expect(resizeClip(c, 'r', 20, 0.25, 6).len).toBe(5); // source is 6 beats, offset 1
    const l = resizeClip(c, 'l', 0, 0.25, 6);
    expect([l.start, l.offset, l.len]).toEqual([3, 0, 5]);
  });
  it('duplicate span rounds up to the snap', () => {
    expect(duplicateSpan([clip({ start: 0, len: 3 })], 4)).toBe(4);
    expect(duplicateSpan([clip({ start: 1, len: 2 }), clip({ start: 4, len: 1 })], 1)).toBe(4);
  });
  it('pastes relative to the first clip', () => {
    let id = 100;
    const out = pasteClips([clip({ start: 8, track: 2 }), clip({ start: 10, track: 3 })], 0, 0, 8, () => id++);
    expect(out.map(c => [c.id, c.start, c.track])).toEqual([[100, 0, 0], [101, 2, 1]]);
  });
  it('inserting a track shifts lanes and clips (valid batch)', () => {
    const p = demoProject(cat);
    const ops = insertTrackOps(p, 1, p.nextId);
    const q = applyAll(p, ops);
    expect(q.tracks.length).toBe(9);
    expect(q.tracks[1].name).toBe('Track 9');
    expect(q.tracks[2].name).toBe('Hats');
    expect(q.tracks.map(t => t.id)).toEqual([1, 2, 3, 4, 5, 6, 7, 8, p.nextId]);
    expect(q.clips.find(c => c.id === 3)!.track).toBe(2);
    expect(q.clips.find(c => c.id === 1)!.track).toBe(0);
  });
});

describe('mock op application', () => {
  it('is atomic', () => {
    const p = demoProject(cat);
    expect(() => applyAll(p, [{ op: 'setBpm', bpm: 100 }, { op: 'removeInsert', id: 0 }])).toThrow(OpError);
    expect(p.bpm).toBe(128);
  });
  it('moves effects and bumps nextId', () => {
    const p = applyAll(demoProject(cat), [{ op: 'moveFx', fxId: 902, to: 2 }, { op: 'putFx', insertId: 2, fx: newFx(cat, 5000, 'sat', 128) }]);
    expect(p.inserts[6].fx.map(f => f.id)).toEqual([903, 904, 902]);
    expect(p.nextId).toBe(5001);
  });
});

describe('formatting & eq', () => {
  it('formats', () => {
    expect(volText(0.7937)).toBe('0.0 dB');
    expect(volText(0)).toBe('-∞ dB');
    expect(panText(-0.25)).toBe('25L');
    expect(fmtPos(5.5)).toBe('2.2.3');
    expect(fmtClock(5.5)).toBe('2:02:03');
    expect(fmtClock(-1.2)).toBe('COUNT 2');
  });
  it('a flat EQ is flat and a bell peaks at its gain', () => {
    const bands = newFx(cat, 1, 'eq', 128).bands!;
    const fr = Float32Array.from([30, 100, 1000, 10000]);
    expect(Array.from(eqCurve(bands, 0, fr, new Float32Array(4))).every(v => Math.abs(v) < 0.05)).toBe(true);
    bands[3] = { ...bands[3], g: 6 };
    expect(eqCurve(bands, 0, Float32Array.from([1000]), new Float32Array(1))[0]).toBeCloseTo(6, 1);
  });
  it('TapTracker averages taps, cancels jitter and resets after pause', () => {
    const t = new TapTracker(8, 2000);
    expect(t.tap(1000)).toEqual({ bpm: null, count: 1, fresh: true });
    // 120 BPM = 500 ms per beat, with ±10 ms human jitter on intermediate taps
    expect(t.tap(1510, false).bpm).toBeCloseTo(117.65, 1);
    expect(t.tap(1990, false).bpm).toBeCloseTo(121.21, 1);
    expect(t.tap(2500, true)).toEqual({ bpm: 120, count: 4, fresh: false });
    // after > 2000 ms pause, starts a fresh tap sequence
    expect(t.tap(5000)).toEqual({ bpm: null, count: 1, fresh: true });
    expect(t.tap(5400)).toEqual({ bpm: 150, count: 2, fresh: false });
  });
});

describe('export range', () => {
  it('uses song by default and loop when asked', () => {
    const p = demoProject(cat);
    expect(songEnd(p)).toBe(32);
    expect(exportRange(p, false)).toEqual({ from: 0, to: 32 });
    expect(exportRange(p, true)).toEqual({ from: 0, to: 32 });
    p.loop = { a: 4, b: 12 };
    expect(exportRange(p, false)).toEqual({ from: 0, to: 32 });
    expect(exportRange(p, true)).toEqual({ from: 4, to: 12 });
    expect(exportSeconds(0, 32, 128, 2)).toBeCloseTo(17, 5);
    expect(fmtExportLen(17)).toBe('17 s');
    expect(fmtExportLen(75)).toBe('1:15');
  });
});
