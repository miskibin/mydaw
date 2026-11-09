import { describe, expect, it } from 'vitest';
import { demoProject, mockCatalog } from '../api/mockData';
import type { Clip, ClipTune, EqBand, PitchAnalysis, RefTune } from '../api/types';
import { applyAll, OpError } from './applyOps';
import { automatedValue, createAutomationOps, denormalize, normalize, parseBandParam, valueAt } from './automation';
import { bandCurve, eqCurve, FreqGrid, rbj, stagesOf, usedPlacements } from './eq';
import {
  addPencil, baseCurve, correctPitch, curveCenter, DEFAULT_REF, dragShift, editedCurve, follow, graphMidi, graphX, graphY, hzToMidi, initTune,
  lowConfidenceRuns, mergeNotes, midiToHz, noteAt, noteCenter, noteCenters, noteName, pencilAt, scaleMask, simpleTarget, snapNote, splitNear,
  splitNote, tuneIsNeutral,
} from './pitch';
import { loadPreset, newFx, pFmt, presetInfo } from './presets';

const band = (o: Partial<EqBand>): EqBand => ({ on: true, type: 'peak', f: 1000, g: 0, q: 1, slope: 12, place: 'stereo', ...o });
const at = (b: EqBand, f: number, dyn?: number) => bandCurve(b, new FreqGrid([f]), new Float32Array(1), dyn)[0];
/** Independent single-biquad magnitude, to check cascades against. */
const mag = (c: number[], f: number) => {
  const w = 2 * Math.PI * f / 48000, [b0, b1, b2, a1, a2] = c;
  const re = (x0: number, x1: number, x2: number) => [x0 + x1 * Math.cos(w) + x2 * Math.cos(2 * w), -(x1 * Math.sin(w) + x2 * Math.sin(2 * w))];
  const [nr, ni] = re(b0, b1, b2), [dr, di] = re(1, a1, a2);
  return 10 * Math.log10((nr * nr + ni * ni) / (dr * dr + di * di));
};

describe('EQ response (engine-exact)', () => {
  it('a bell peaks at its gain, shelves reach theirs', () => {
    expect(at(band({ g: 6 }), 1000)).toBeCloseTo(6, 3);
    expect(at(band({ type: 'lowshelf', f: 200, g: -4, q: 0.71 }), 20)).toBeCloseTo(-4, 1);
    expect(at(band({ type: 'highshelf', f: 4000, g: 5, q: 0.71 }), 20000)).toBeCloseTo(5, 0);
  });
  it('cut slopes are cascades: user Q on stage 1, 0.707 on the rest', () => {
    for (const slope of [12, 24, 36, 48]) {
      const b = band({ type: 'lowcut', f: 200, q: 1.2, slope });
      expect(stagesOf(b)).toBe(slope / 12);
      const want = mag(rbj('hp', 200, 1.2, 0), 100) + (slope / 12 - 1) * mag(rbj('hp', 200, 0.707, 0), 100);
      expect(at(b, 100)).toBeCloseTo(want, 4);
    }
    // an octave below the corner the 48 dB/oct cut is ~4× deeper than the 12 dB/oct one
    const c12 = at(band({ type: 'highcut', f: 1000, q: 0.707, slope: 12 }), 4000), c48 = at(band({ type: 'highcut', f: 1000, q: 0.707, slope: 48 }), 4000);
    expect(c48 / c12).toBeGreaterThan(3.6);
    expect(stagesOf(band({ type: 'peak', slope: 48 }))).toBe(1);
  });
  it('tilt is a 0.5-Q high shelf at g followed by −g/2', () => {
    const b = band({ type: 'tilt', f: 1000, g: 6, q: 3 });
    expect(at(b, 20)).toBeCloseTo(-3, 1);
    expect(at(b, 20000)).toBeCloseTo(3, 0);
    expect(at(b, 1000)).toBeCloseTo(mag(rbj('hs', 1000, 0.5, 6), 1000) - 3, 6);
  });
  it('notch cuts deep (Q at least 2), band pass is 0 dB at f', () => {
    expect(at(band({ type: 'notch', f: 3000, q: 0.5 }), 3000)).toBeLessThan(-40);
    expect(at(band({ type: 'bandpass', f: 3000, q: 2 }), 3000)).toBeCloseTo(0, 3);
    expect(at(band({ type: 'bandpass', f: 3000, q: 2 }), 300)).toBeLessThan(-15);
  });
  it('mid/side/left/right bands only shape their own path', () => {
    const bands = [band({ g: 4 }), band({ f: 8000, g: -6, place: 'side' })];
    const g = new FreqGrid([1000, 8000]);
    const st = eqCurve(bands, 0, g, new Float32Array(2)), side = eqCurve(bands, 0, g, new Float32Array(2), 'side');
    expect(st[1]).toBeCloseTo(at(bands[0], 8000), 6);
    expect(side[1]).toBeCloseTo(at(bands[0], 8000) - 6, 1);
    expect(usedPlacements(bands)).toEqual(['side']);
    expect(usedPlacements([band({ on: false, place: 'mid' })])).toEqual([]);
  });
  it('dynamic gain offsets move only dynamic bands', () => {
    const bands = [band({ g: 0, dynamic: { range: -6, thr: -30 } }), band({ f: 100, g: 2 })];
    const g = new FreqGrid([1000]);
    expect(eqCurve(bands, 0, g, new Float32Array(1), 'stereo', [-4, -9])[0]).toBeCloseTo(-4 + at(bands[1], 1000), 3);
    expect(at(bands[0], 1000, -4)).toBeCloseTo(-4, 3);
  });
});

describe('pitch math and graph mapping', () => {
  it('converts and names notes', () => {
    expect(hzToMidi(440)).toBeCloseTo(69, 9);
    expect(midiToHz(57)).toBeCloseTo(220, 9);
    expect(noteName(57)).toBe('A3');
    expect(noteName(60.4)).toBe('C4');
    expect(noteName(61)).toBe('C#4');
  });
  it('snaps like pitch.rs', () => {
    const maj = scaleMask(1);
    expect(snapNote(60.8, 0, maj)).toBe(60);
    expect(snapNote(61.2, 0, maj)).toBe(62);
    expect(snapNote(61.4, 0, scaleMask(0))).toBe(61);
    expect(snapNote(70.6, 9, scaleMask(2))).toBe(71); // A minor: A# not allowed → B
  });
  it('maps time and pitch to the scrolling graph and back', () => {
    expect(graphX(1000, 1000, 600, 6000)).toBe(600);
    expect(graphX(-5000, 1000, 600, 6000)).toBe(0);
    expect(graphY(60, 60, 14, 280)).toBe(140);
    expect(graphY(67, 60, 14, 280)).toBe(0);
    expect(graphMidi(graphY(63.3, 60, 14, 280), 60, 14, 280)).toBeCloseTo(63.3, 9);
  });
  it('the range follower glides without overshoot and is frame-rate independent', () => {
    let a: [number, number] = [60, 0], b: [number, number] = [60, 0], max = 60;
    for (let i = 0; i < 120; i++) { a = follow(a[0], a[1], 67, 1 / 120); max = Math.max(max, a[0]); }
    for (let i = 0; i < 60; i++) b = follow(b[0], b[1], 67, 1 / 60);
    expect(max).toBeLessThanOrEqual(67 + 1e-9);
    expect(a[0]).toBeCloseTo(b[0], 6);
    expect(a[0]).toBeGreaterThan(64);
  });
});

/** A small analysis: two notes (A3 flat with vibrato, C4), an unvoiced gap. */
function analysis(): PitchAnalysis {
  const rate = 200, curve: (number | null)[] = [], levels: number[] = [];
  for (let f = 0; f < 200; f++) {
    const t = f / rate;
    if (t < 0.4) curve.push(56.8 + 0.3 * Math.sin(2 * Math.PI * 5 * t));
    else if (t < 0.5) curve.push(null);
    else curve.push(60.1);
    levels.push(curve[f] == null ? 0.2 : 0.7);
  }
  return { rate, curve, levels, notes: [{ start: 0, end: 0.4, pitch: 56.8, level: 0.7 }, { start: 0.5, end: 1, pitch: 60.1, level: 0.7 }] };
}

describe('tune edits', () => {
  const an = analysis();
  it('starts from the analysis and finds note centres (median)', () => {
    const t = initTune(an);
    expect(t.notes).toHaveLength(2);
    expect(tuneIsNeutral(t)).toBe(true);
    expect(noteCenter(an, 0, 0.4)!).toBeCloseTo(56.8, 1);
    expect(noteCenter(an, 0.4, 0.5)).toBeNull();
    expect(noteAt(t.notes, 0.45)).toBe(-1);
    expect(noteAt(t.notes, 0.7)).toBe(1);
  });
  it('split and merge are inverse, and refuse slivers', () => {
    const t = initTune(an);
    const s = splitNote(t, 0, 0.2)!;
    expect(s.notes.map(n => [n.start, n.end])).toEqual([[0, 0.2], [0.2, 0.4], [0.5, 1]]);
    expect(splitNear(s.notes, 0.201, 0.005)).toBe(0);
    expect(splitNear(s.notes, 0.45, 0.01)).toBe(-1); // 0.4 → 0.5 do not touch
    expect(mergeNotes(s, 0)!.notes).toEqual(t.notes);
    expect(mergeNotes(t, 0)).toBeNull();
    expect(splitNote(t, 0, 0.01)).toBeNull();
  });
  it('the edited curve applies shift, drift and pencil like tune_render.rs; unvoiced stays unvoiced', () => {
    const t: ClipTune = initTune(an);
    t.notes[0].shift = 20; t.notes[0].drift = 1;
    const e = editedCurve(an, t);
    const c = noteCenter(an, 0, 0.4)!;
    expect(e[10]).toBeCloseTo(c + 0.2, 4); // fully flattened, then shifted 20 ct
    expect(Number.isNaN(e[90])).toBe(true);
    expect(e[150]).toBeCloseTo(60.1, 4);
    t.pencil = [[0.6, 62], [0.62, 62], [0.64, 63]];
    const p = editedCurve(an, t);
    expect(p[Math.round(0.61 * 200)]).toBeCloseTo(62, 4);
    expect(p[Math.round(0.7 * 200)]).toBeCloseTo(60.1, 4); // after the run
  });
  it('pencil runs end at gaps over 30 ms; strokes replace their span', () => {
    const pts: [number, number][] = [[0, 60], [0.02, 61], [0.1, 62]];
    expect(pencilAt(pts, 0.01)).toBeCloseTo(60.5, 6);
    expect(pencilAt(pts, 0.05)).toBeNull();
    const next = addPencil(pts, [[0.015, 70], [0.03, 70]]);
    expect(next.map(q => q[0])).toEqual([0, 0.015, 0.03, 0.1]);
  });
  it('drag snaps the note to the scale (Alt = free cents)', () => {
    // a note centred at 56.8 (A3 −20 ct) dragged up ~1 semitone in C major → lands on A (57) or B (59)
    expect(dragShift(56.8, 0, 30, 0, scaleMask(1), false)).toBe(20);
    expect(dragShift(56.8, 0, 160, 0, scaleMask(1), false)).toBe(220);
    expect(dragShift(56.8, 0, 37, 0, scaleMask(1), true)).toBe(37);
  });
  it('"Popraw wysokość" moves centres to the scale by Center % and sets drift', () => {
    const t = correctPitch(initTune(an), an, [0, 1], 100, 50, 0, scaleMask(1));
    expect(t.notes[0].shift).toBe(Math.round((57 - noteCenter(an, 0, 0.4)!) * 100));
    expect(t.notes[1].shift).toBe(-10);
    expect(t.notes[0].drift).toBe(0.5);
    const half = correctPitch(initTune(an), an, [1], 50, 0, 0, scaleMask(1));
    expect(half.notes[1].shift).toBe(-5);
    expect(half.notes[0].shift).toBe(0);
  });
});

describe('Tune to Reference', () => {
  const an = analysis();
  /** The same two notes sung in tune, an octave up, at a different tempo (longer). */
  function reference(): PitchAnalysis {
    const rate = 200, curve: (number | null)[] = [], levels: number[] = [];
    for (let f = 0; f < 300; f++) {
      const t = f / rate;
      curve.push(t < 0.6 ? 69 + 0.1 * Math.sin(2 * Math.PI * 6 * t) : t < 0.75 ? null : 72);
      levels.push(0.7);
    }
    return { rate, curve, levels, notes: [{ start: 0, end: 0.6, pitch: 69, level: 0.7 }, { start: 0.75, end: 1.5, pitch: 72, level: 0.7 }] };
  }
  const p: RefTune = { source: 9, ...DEFAULT_REF };

  it('pulls note centres onto the reference, an octave down, keeping the singer’s vibrato', () => {
    const t = simpleTarget(an, reference(), an.notes, p);
    expect(t.octave).toBe(-12);
    expect(curveCenter(t.targetF0, t.rate, 0, 0.4)!).toBeCloseTo(57, 1); // A3 −20 ct → A3
    expect(curveCenter(t.targetF0, t.rate, 0.5, 1)!).toBeCloseTo(60, 1); // C4 +10 ct → C4
    // the 5 Hz vibrato of the take survives: target − centre follows take − centre
    const c0 = noteCenter(an, 0, 0.4)!, tc = curveCenter(t.targetF0, t.rate, 0, 0.4)!;
    for (const f of [10, 25, 60]) expect(t.targetF0[f]! - tc).toBeCloseTo(an.curve[f]! - c0, 2);
    expect(t.targetF0[90]).toBeNull(); // unvoiced stays unvoiced
  });
  it('correction, vibrato and influence blend like the engine', () => {
    const none = simpleTarget(an, reference(), an.notes, { ...p, correction: 0 });
    expect(none.targetF0[150]).toBeCloseTo(an.curve[150]!, 6);
    const flat = simpleTarget(an, reference(), an.notes, { ...p, vibrato: 0 });
    expect(flat.targetF0[25]).toBeCloseTo(57, 1);
    // influence 0: the nearest scale note instead (C major: 56.8 → A 57)
    const scale = simpleTarget(an, reference(), an.notes, { ...p, influence: 0, key: 0, scale: 1 });
    expect(curveCenter(scale.targetF0, 200, 0.5, 1)!).toBeCloseTo(60, 1);
    // a forced octave wins over the automatic one
    expect(simpleTarget(an, reference(), an.notes, { ...p, octave: 0 }).octave).toBe(0);
  });
  it('the edited curve starts from the target; note edits apply on top', () => {
    const t = simpleTarget(an, reference(), an.notes, p);
    expect(baseCurve(an, t)[150]).toBeCloseTo(60, 1);
    expect(Number.isNaN(baseCurve(an, t)[90])).toBe(true);
    const tune: ClipTune = { ...initTune(an), reference: p };
    tune.notes[1].shift = 100;
    const e = editedCurve(an, tune, undefined, t);
    expect(e[150]).toBeCloseTo(61, 1);
    expect(noteCenters(an, tune.notes, t)[1]!).toBeCloseTo(60, 1);
    expect(tuneIsNeutral({ notes: [], reference: p })).toBe(false);
  });
  it('marks stretches the alignment is unsure of', () => {
    const t = simpleTarget(an, reference(), an.notes, p);
    t.alignmentConfidence = t.alignmentConfidence.map((c, i) => (i >= 20 && i < 60 ? 0.05 : c));
    expect(lowConfidenceRuns(t)).toEqual([[0.1, 0.3]]);
  });
  it('mock ops check references like the engine and drop them with their source', () => {
    const proj = demoProject(mockCatalog());
    const src = (id: number, name: string) => ({ id, name, color: '#888', path: name + '.wav', dur: 4, sr: 48000, ch: 1, peakDb: -3 });
    proj.sources.push(src(7001, 'take'), src(7002, 'acapella'));
    const clip: Clip = { id: 7003, track: 0, start: 0, len: 8, offset: 0, kind: 'audio', ref: 7001, muted: false, gain: 0, fadeIn: 0, fadeOut: 0, pan: 0, norm: false, rev: false };
    const put = (r: RefTune) => applyAll(proj, [{ op: 'putClips', clips: [{ ...clip, tune: { notes: [], reference: r } }] }]);
    expect(() => put({ ...p, source: 99999 })).toThrow(OpError);
    expect(() => put({ ...p, source: 7002, correction: 1.5 })).toThrow(OpError);
    expect(() => put({ ...p, source: 7002, octave: 7 })).toThrow(OpError);
    const ok = put({ ...p, source: 7002 });
    expect(ok.clips.find(c => c.id === 7003)!.tune!.reference!.source).toBe(7002);
    const gone = applyAll(ok, [{ op: 'removeSource', id: 7002 }]);
    expect(gone.clips.find(c => c.id === 7003)!.tune!.reference).toBeNull();
  });
});

describe('choice parameters', () => {
  const cat = mockCatalog();
  it('the catalog marks discrete params with options', () => {
    const ratio = cat.types.fet!.params.find(p => p.id === 'ratio')!;
    expect(ratio.options).toEqual(['4', '8', '12', '20', 'All']);
    expect(ratio.max).toBe(4);
    expect(pFmt(ratio, 4)).toBe('All');
    expect(pFmt(cat.types.tune!.params.find(p => p.id === 'key')!, 9)).toBe('A');
    expect(pFmt(cat.types.fet!.params.find(p => p.id === 'attack')!, 300)).toBe('300 µs');
  });
  it('new vocal processors load their first preset and match it', () => {
    for (const ty of ['tune', 'deess', 'fet'] as const) {
      const f = newFx(cat, 1, ty, 128);
      expect(presetInfo(cat, f, 128)).toEqual({ idx: 0, name: cat.presets[ty]![0].name, mod: false });
      const g = loadPreset(cat, f, 2, 128);
      expect(presetInfo(cat, g, 128).idx).toBe(2);
    }
  });
  it('automation of a choice param snaps to an option', () => {
    const p = demoProject(cat);
    p.inserts[1].fx.push(newFx(cat, 777, 'fet', 128));
    const t = { kind: 'fx' as const, fxId: 777, param: 'ratio' };
    expect(denormalize(p, cat, t, 0.6)).toBe(2);
    expect(normalize(p, cat, t, 4)).toBe(1);
  });
});

describe('automation', () => {
  const cat = mockCatalog();
  it('normalises like the engine: log params logarithmically, bands, vol and pan', () => {
    const p = demoProject(cat);
    const rel = { kind: 'fx' as const, fxId: 903, param: 'rel' }; // comp release 10..1000 ms, log
    expect(normalize(p, cat, rel, 100)).toBeCloseTo(0.5, 9);
    expect(denormalize(p, cat, rel, 0.5)).toBeCloseTo(100, 6);
    expect(parseBandParam('band:3:g')).toEqual({ i: 3, k: 'g' });
    expect(normalize(p, cat, { kind: 'fx', fxId: 902, param: 'band:0:f' }, 632.455532)).toBeCloseTo(0.5, 5);
    expect(normalize(p, cat, { kind: 'fx', fxId: 902, param: 'band:0:g' }, 0)).toBeCloseTo(0.5, 9);
    expect(normalize(p, cat, { kind: 'insertPan', id: 1 }, -1)).toBe(0);
    expect(denormalize(p, cat, { kind: 'insertPan', id: 1 }, 0.75)).toBeCloseTo(0.5, 9);
  });
  it('interpolates between points and holds at the ends', () => {
    const pts = [{ t: 0, v: 0.2 }, { t: 4, v: 0.6 }];
    expect(valueAt(pts, -1)).toBe(0.2);
    expect(valueAt(pts, 2)).toBeCloseTo(0.4, 9);
    expect(valueAt(pts, 9)).toBe(0.6);
    expect(valueAt([], 1)).toBeNull();
  });
  it('creates an automation clip on an empty track, plays it at the playhead, and is removed with its clips', () => {
    const p = demoProject(cat);
    let next = p.nextId;
    const target = { kind: 'insertVol' as const, id: 3 };
    const r = createAutomationOps(p, cat, target, 'Bass · Głośność', '#e0a44a', 8, 16, () => next++);
    const used = new Set(p.clips.map(c => c.track));
    expect(used.has(r.track)).toBe(false);
    const q = applyAll(p, r.ops);
    const a = q.automations!.find(x => x.id === r.autoId)!;
    expect(a.points).toEqual([{ t: 0, v: p.inserts.find(m => m.id === 3)!.vol }, { t: 16, v: p.inserts.find(m => m.id === 3)!.vol }]);
    const q2 = applyAll(q, [{ op: 'putAutomation', automation: { ...a, points: [{ t: 0, v: 0 }, { t: 16, v: 1 }] } }]);
    expect(automatedValue(q2, target, 16)).toBeCloseTo(0.5, 9);
    expect(automatedValue(q2, target, 7.9)).toBeNull();
    const q3 = applyAll(q2, [{ op: 'removeAutomation', id: r.autoId }]);
    expect(q3.clips.some(c => c.kind === 'auto')).toBe(false);
  });
  it('adds a track when none is empty', () => {
    const p = demoProject(cat);
    p.clips.push(...p.tracks.map((_, i) => ({ id: 5000 + i, track: i, start: 100, len: 4, offset: 0, kind: 'pat' as const, ref: 1, muted: false, gain: 0, fadeIn: 0, fadeOut: 0, pan: 0, norm: false, rev: false })));
    let next = 9000;
    const r = createAutomationOps(p, cat, { kind: 'mix', fxId: 903 }, 'Mix', '#fff', 0, 16, () => next++);
    expect(r.track).toBe(p.tracks.length);
    expect(applyAll(p, r.ops).tracks).toHaveLength(p.tracks.length + 1);
  });
});

describe('continuous scrolling under jittery telemetry', () => {
  it('the display clock advances smoothly at 60 fps while frames arrive at ~30 Hz with jitter', async () => {
    const { TimeRing } = await import('../views/fx/common');
    const r = new TimeRing(600, 1);
    let seed = 7; const rnd = () => ((seed = (seed * 16807) % 2147483647) / 2147483647);
    let nextArrival = 0;
    const steps: number[] = [];
    let last = -1;
    for (let frame = 0; frame < 600; frame++) {
      const now = frame * 1000 / 60;
      while (nextArrival <= now) { r.push(nextArrival, [frame]); nextArrival += 33.3 + (rnd() - 0.5) * 14; }
      const c = r.clock(now);
      if (frame > 60) { steps.push(c - last); }
      last = c;
    }
    const min = Math.min(...steps), max = Math.max(...steps);
    expect(min).toBeGreaterThan(0);          // never freezes or goes back
    expect(max).toBeLessThan(16.7 * 1.6);    // never jumps a telemetry step at once
    const mean = steps.reduce((s, x) => s + x, 0) / steps.length;
    expect(mean).toBeCloseTo(16.67, 0);
  });
});
