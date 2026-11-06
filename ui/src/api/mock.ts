// In-browser fake engine, used when the page is not running inside Tauri.
// Holds the demo project, applies ops with undo/gestures, plays a fake transport and
// streams binary telemetry frames (encoded exactly like the real ones).
import type { Backend, EventMap, OsDropEvent } from './backend';
import { encodeFrame, SCOPE_LEN, SPEC_BANDS, type EncFx } from './telemetry';
import { BUILTIN, demoProject, emptyProject, mockCatalog } from './mockData';
import { applyAll } from '../lib/applyOps';
import { eqCurve } from '../lib/eq';
import { clamp, dbg } from '../lib/format';
import { midiToHz, noteAt, scaleMask, snapNote } from '../lib/pitch';
import { newFx } from '../lib/presets';
import { synthVocal, vocalAt, vocalPeaks, type VocalSample } from './mockVocal';
import type {
  AudioConfig, AudioStatus, Catalog, Clip, Fx, Id, LibFile, LibraryList, Op, PitchAnalysis, Project, Source, StateDto, TransportDto, Vst3Info, Vst3Param,
} from './types';

const TC = ['#4f8ef7', '#5aa9e6', '#69c3d6', '#47b8a8', '#5fbf8a', '#7c8cf8', '#e0a44a', '#9a8cf0', '#d9678f', '#8b93a1'];
const BAND_F = new Float32Array(SPEC_BANDS).map((_, k) => 20 * Math.pow(1000, k / (SPEC_BANDS - 1)));
const delay = (ms = 0) => new Promise<void>(r => setTimeout(r, ms));
const hash = (s: string) => { let h = 2166136261; for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619); return h >>> 0; };

function rng(seed: number) {
  let s = seed || 7;
  return () => ((s = Math.imul(s, 16807) % 2147483647) & 0x7fffffff) / 2147483647;
}

/** Synthetic waveform peaks: a loop of transients with decays, 100 [min,max] pairs per second. */
function synthPeaks(seed: number, dur: number, bpm: number): Float32Array {
  const n = Math.max(1, Math.round(dur * 100)), out = new Float32Array(n * 2), r = rng(seed);
  const style = seed % 3, spb = 60 / bpm;
  for (let i = 0; i < n; i++) {
    const t = i / 100, ph = (t % (spb / (style === 1 ? 2 : 1))) / spb;
    const env = style === 0 ? Math.exp(-ph * 9) * 0.85 + 0.05 : style === 1 ? Math.exp(-ph * 4) * 0.6 + 0.2 : 0.45 + 0.25 * Math.sin(t * 2.1) * Math.sin(t * 0.7);
    const a = clamp(env * (0.75 + 0.25 * r()), 0, 1);
    out[2 * i] = -a * (0.85 + 0.15 * r()); out[2 * i + 1] = a;
  }
  return out;
}

interface MockSource extends Source { seed: number; vocal?: boolean; trimFrom?: { id: Id; offSec: number } }
const isVocalName = (n: string) => /vocal|vox|wokal|śpiew/i.test(n);
const VOCAL_DUR = 14;

/** Per-effect DSP state of the fake processors (so meters move like the engine's). */
interface FxSim { t: number; corr: number; center: number; held: number; last: number; m: number[]; gr: number; dyn: Float32Array }
const newSim = (): FxSim => ({ t: 0, corr: 0, center: 0, held: 0, last: 0, m: [0, 0, 0, 0, 0, 0], gr: 0, dyn: new Float32Array(32) });

/** Spectrum (dBFS per band) of a sung vowel at `midi` with level `lv`, plus sibilant noise `sib`. */
function vocalSpec(s: VocalSample, out: Float32Array) {
  const pw = new Float64Array(SPEC_BANDS);
  const lvDb = s.level > 1e-4 ? 20 * Math.log10(s.level) : -120;
  const idx = (f: number) => Math.round(Math.log(f / 20) / Math.log(1000) * (SPEC_BANDS - 1));
  if (s.midi != null && s.level > 0.01) {
    const f0 = midiToHz(s.midi);
    for (let h = 1; h * f0 < 12000; h++) {
      const f = h * f0, oct = Math.log2(f / 500);
      // formants around 700 Hz and 2.6 kHz on a −9 dB/oct tilt
      const env = -9 * Math.max(0, oct) + 9 * Math.exp(-((Math.log2(f / 700)) ** 2) * 3) + 7 * Math.exp(-((Math.log2(f / 2600)) ** 2) * 4);
      const k = idx(f); if (k < 0 || k >= SPEC_BANDS) continue;
      pw[k] += Math.pow(10, (lvDb - 14 + env) / 10);
    }
  }
  for (let k = 0; k < SPEC_BANDS; k++) {
    const f = BAND_F[k];
    const hiss = s.sib > 0 ? Math.pow(10, (-24 + 6 * s.sib - 18 * Math.max(0, Math.log2(4500 / f)) ** 2 * (f < 4500 ? 1 : 0) - 3 * Math.max(0, Math.log2(f / 9000))) / 10) : 0;
    const floor = Math.pow(10, (-92 - 3 * Math.log2(f / 1000)) / 10);
    out[k] = 10 * Math.log10(pw[k] + hiss + floor) + (Math.random() - 0.5) * 2.5;
  }
}

export class MockBackend implements Backend {
  readonly isMock = true;
  /** No window chrome in a browser; `?chrome` previews the custom title bar with inert buttons. */
  readonly win = typeof location !== 'undefined' && new URLSearchParams(location.search).has('chrome')
    ? { minimize: async () => {}, toggleMaximize: async () => {}, close: async () => {}, isMaximized: async () => false, startDragging: async () => {}, onResized: async () => () => {} }
    : null;
  private cat: Catalog = mockCatalog();
  private p: Project = demoProject(this.cat);
  private revision = 1;
  private savedRev = 1;
  private path: string | null = null;
  private undoStack: Project[] = [];
  private redoStack: Project[] = [];
  private gesture: Project | null = null;
  private tr: TransportDto = { playing: false, rec: false, monitor: false, metro: false, countIn: true, startBeat: 0 };
  private listeners = new Map<string, Set<(p: unknown) => void>>();
  private teleCb: ((b: ArrayBuffer) => void) | null = null;
  private watched: Id[] = [];
  private seq = 0;
  private playT0 = 0; private playFrom = 0;
  private rec: null | { track: number; clipStart: number; t0: number; peaks: number[]; sent: number } = null;
  private preview: null | { t0: number; dur: number; off: number } = null;
  private srcMeta = new Map<Id, MockSource>();
  private takeN = 0;
  private kits = [
    { id: 'wave-drums', name: 'WAVE Drums', description: 'Stopy, werble, hi-haty i pętle perkusyjne', bytes: 86e6, count: 240, installed: false },
    { id: 'wave-keys', name: 'WAVE Keys & Pads', description: 'Akordy, pady i tekstury', bytes: 142e6, count: 96, installed: true },
    { id: 'wave-fx', name: 'WAVE FX', description: 'Uplightery, impakty, szumy', bytes: 38e6, count: 64, installed: false },
  ];
  private folders = ['C:\\Users\\michal\\Music\\Samples'];
  private vstParams = new Map<Id, Vst3Param[]>();
  private audio: AudioStatus = {
    host: 'wasapi', output: 'Głośniki (Realtek High Definition Audio)', input: 'Mikrofon (USB Audio)', sampleRate: null, bufferSize: null,
    running: true, actualRate: 48000, actualBuffer: 480, latencyMs: 20, inputChannels: 2, error: null, note: 'Mock — brak prawdziwego audio', asioCompiled: false,
  };

  constructor() {
    setInterval(() => this.tick(), 1000 / 60);
    (window as unknown as { __waveMock: MockBackend }).__waveMock = this;
  }

  // ---------- state ----------
  private dto(): StateDto {
    return structuredClone({ project: this.p, revision: this.revision, canUndo: this.undoStack.length > 0, canRedo: this.redoStack.length > 0,
      transport: this.tr, path: this.path, dirty: this.revision !== this.savedRev });
  }
  private emit<K extends keyof EventMap>(ev: K, payload: EventMap[K]) {
    const ls = this.listeners.get(ev); if (!ls) return;
    setTimeout(() => ls.forEach(cb => cb(payload)), 0);
  }
  private changed() { this.revision++; this.emit('state', this.dto()); }
  private pushUndo(prev: Project) { this.undoStack.push(prev); if (this.undoStack.length > 300) this.undoStack.shift(); this.redoStack = []; }

  async getState() { return this.dto(); }
  async applyOps(ops: Op[], opts?: { label?: string; transient?: boolean }) {
    await delay();
    const prev = this.p;
    this.p = applyAll(this.p, ops); // throws (rejects) on an invalid batch
    if (!(opts?.transient && this.gesture)) this.pushUndo(prev);
    this.changed();
    return { revision: this.revision };
  }
  async beginGesture() { this.gesture = structuredClone(this.p); }
  async endGesture() {
    const g = this.gesture; this.gesture = null;
    if (g && JSON.stringify(g) !== JSON.stringify(this.p)) {
      this.pushUndo(g);
      // pitch edits are rendered offline at the end of a gesture: the state event comes after the render
      const tuned = this.p.clips.some(c => { const o = g.clips.find(x => x.id === c.id); return JSON.stringify(o?.tune ?? null) !== JSON.stringify(c.tune ?? null); });
      if (tuned) { this.revision++; this.renderReady = performance.now() + 380; setTimeout(() => this.emit('state', this.dto()), 380); } else this.changed();
    }
  }
  async undo() {
    const prev = this.undoStack.pop();
    if (prev) { this.redoStack.push(this.p); this.p = prev; this.changed(); }
    return this.dto();
  }
  async redo() {
    const next = this.redoStack.pop();
    if (next) { this.undoStack.push(this.p); this.p = next; this.changed(); }
    return this.dto();
  }

  // ---------- transport ----------
  private spb() { return 60 / this.p.bpm; }
  private songEnd() { return Math.ceil(Math.max(4, ...this.p.clips.map(c => c.start + c.len)) / 4) * 4; }
  private beatNow(): number {
    if (!this.tr.playing) return this.tr.startBeat;
    let b = this.playFrom + (performance.now() - this.playT0) / 1000 / this.spb();
    if (this.rec) return b;
    const lp = this.p.loop, a = lp ? lp.a : 0, e = lp ? lp.b : this.songEnd();
    if (b >= e && e > a) { b = a + ((b - a) % (e - a)); }
    return b;
  }
  async transportPlay(fromBeat: number) {
    if (this.tr.playing) this.finishRec();
    const pre = this.tr.rec && this.tr.countIn ? 4 : 0;
    this.tr = { ...this.tr, playing: true, startBeat: fromBeat };
    this.playFrom = fromBeat - pre; this.playT0 = performance.now();
    if (this.tr.rec) this.startRec(fromBeat);
    this.changedTransport();
  }
  async transportStop() {
    this.finishRec();
    this.tr = { ...this.tr, playing: false };
    this.changedTransport();
  }
  async transportSet(patch: Partial<TransportDto>) {
    const wasRec = this.tr.rec;
    this.tr = { ...this.tr, ...patch };
    if (wasRec && !this.tr.rec) this.finishRec();
    if (!wasRec && this.tr.rec && this.tr.playing) this.startRec(this.beatNow());
    this.changedTransport();
    return { ...this.tr };
  }
  private changedTransport() { this.emit('state', this.dto()); }
  private startRec(clipStart: number) {
    const t = this.p.tracks.findIndex(x => x.arm);
    this.rec = { track: t < 0 ? 0 : t, clipStart: Math.max(0, clipStart), t0: 0, peaks: [], sent: 0 };
  }
  private finishRec() {
    const r = this.rec; this.rec = null;
    if (!r || r.peaks.length < 10) return;
    const prev = structuredClone(this.p);
    const id = this.p.nextId++, cid = this.p.nextId++, dur = r.peaks.length / 2 / 100;
    const src: MockSource = { id, name: 'Take ' + (++this.takeN), color: '#d9678f', path: 'recordings/take' + this.takeN + '.wav', dur, sr: 48000, ch: 1, peakDb: -6, seed: id };
    this.srcMeta.set(id, src);
    this.recPeaks.set(id, Float32Array.from(r.peaks));
    this.p.sources.push(strip(src));
    this.p.clips.push(mkClip(cid, r.track, r.clipStart, dur / this.spb(), 'audio', id));
    this.pushUndo(prev); this.changed();
  }
  private recPeaks = new Map<Id, Float32Array>();

  // ---------- telemetry ----------
  async subscribeTelemetry(cb: (b: ArrayBuffer) => void) { this.teleCb = cb; }
  async watchFx(ids: Id[]) { this.watched = ids.slice(0, 8); }

  private tick() {
    if (!this.teleCb) return;
    const seq = ++this.seq, beat = this.beatNow(), playing = this.tr.playing, t = performance.now() / 1000;
    const spb = this.spb(), frac = ((beat % 1) + 1) % 1;
    // meters: inserts whose tracks have a live clip under the playhead
    const live = new Map<Id, number>();
    if (playing && beat >= 0) for (const c of this.p.clips) {
      if (c.muted || beat < c.start || beat >= c.start + c.len) continue;
      const tr = this.p.tracks[c.track]; if (!tr || tr.muted) continue;
      const lvl = (c.kind === 'pat' ? 0.25 + 0.55 * Math.exp(-frac * 5) : 0.45 + 0.2 * Math.sin(t * 7)) * 2 * tr.vol ** 3 * dbg(c.gain);
      live.set(tr.insert, Math.max(live.get(tr.insert) ?? 0, lvl));
    }
    let master = 0;
    const lvOf = new Map<Id, number>();
    for (const m of this.p.inserts) if (m.id !== 0) {
      const lv = (live.get(m.id) ?? 0) * (m.muted ? 0 : 2 * m.vol ** 3) * (0.9 + 0.1 * Math.random());
      lvOf.set(m.id, lv); master += lv * 0.6;
    }
    const ins = this.p.inserts.map(m => {
      const lv = m.id === 0 ? Math.min(1.2, master * (m.muted ? 0 : 2 * m.vol ** 3)) : lvOf.get(m.id) ?? 0;
      return { id: m.id, l: lv * Math.min(1, 1 - m.pan), r: lv * Math.min(1, 1 + m.pan) };
    });
    const insLvl = new Map(ins.map(e => [e.id, Math.max(e.l, e.r)]));
    const fx: EncFx[] = [];
    if (seq % 2 === 0) for (const id of this.watched) {
      const found = this.findFx(id); if (!found) continue;
      fx.push(this.fakeAnalysis(found.fx, found.ins, playing ? (insLvl.get(found.ins) ?? 0) + 0.05 : 0, t, beat, playing));
    }
    let rec = null;
    if (this.rec && playing && beat >= this.rec.clipStart) {
      const r = this.rec;
      if (!r.t0) r.t0 = performance.now();
      const want = Math.floor((performance.now() - r.t0) / 10);
      while (r.peaks.length / 2 < want) { const a = 0.15 + 0.6 * Math.abs(Math.sin(r.peaks.length / 37)) * Math.random(); r.peaks.push(-a, a); }
      const off = r.sent; r.sent = r.peaks.length / 2;
      rec = { track: r.track, clipStart: r.clipStart, seconds: r.peaks.length / 200, peakOffset: off, peaks: r.peaks.slice(off * 2) };
    }
    let previewPos = -1;
    if (this.preview) {
      previewPos = this.preview.off + (performance.now() - this.preview.t0) / 1000;
      if (previewPos > this.preview.off + this.preview.dur) { this.preview = null; previewPos = -1; }
    }
    const inLevel = this.tr.rec ? 0.2 + 0.5 * Math.abs(Math.sin(t * 3)) * Math.random() : 0;
    void spb;
    this.teleCb(encodeFrame({ seq, beat, playing, recording: !!this.rec && beat >= (this.rec?.clipStart ?? 0), previewing: !!this.preview,
      inLevel, previewPos, cpu: playing ? 0.12 + 0.03 * Math.random() : 0.02, ins, fx, rec }));
  }

  private findFx(id: Id): { fx: Fx; ins: Id } | null {
    for (const m of this.p.inserts) { const f = m.fx.find(x => x.id === id); if (f) return { fx: f, ins: m.id }; }
    return null;
  }

  private sims = new Map<Id, FxSim>();
  /** The vocal heard on insert `ins` now: a vocal clip under the playhead, or (for vocal processors) the looping demo take. */
  private vocalFor(fx: Fx, ins: Id, beat: number, t: number, playing: boolean): VocalSample | null {
    if (!playing) return null;
    for (const c of this.p.clips) {
      if (c.kind !== 'audio' || c.muted || beat < c.start || beat >= c.start + c.len) continue;
      const tr = this.p.tracks[c.track]; if (!tr || tr.insert !== ins || tr.muted) continue;
      const m = this.srcMeta.get(c.ref); if (!m?.vocal) continue;
      return vocalAt(synthVocal(m.seed, m.dur), (c.offset + beat - c.start) * this.spb());
    }
    return fx.type === 'tune' || fx.type === 'deess' || fx.type === 'fet' ? vocalAt(synthVocal(4242, VOCAL_DUR), t) : null;
  }

  private fakeAnalysis(fx: Fx, ins: Id, level: number, t: number, beat: number, playing: boolean): EncFx {
    const specIn = new Float32Array(SPEC_BANDS), specOut = new Float32Array(SPEC_BANDS);
    const scopeIn = new Float32Array(SCOPE_LEN), scopeOut = new Float32Array(SCOPE_LEN);
    const meters = new Float32Array(32);
    let sim = this.sims.get(fx.id); if (!sim) { sim = newSim(); this.sims.set(fx.id, sim); }
    const dt = sim.t ? clamp(t - sim.t, 0, 0.25) : 1 / 30; sim.t = t;
    const voc = this.vocalFor(fx, ins, beat, t, playing);
    if (voc) { voc.level *= 0.4; level = voc.level; }
    const lvDb = level > 1e-4 ? 20 * Math.log10(level) : -120;
    const pulse = voc ? 1 - voc.level : Math.exp(-(((beat % 1) + 1) % 1) * 4);
    if (voc) vocalSpec(voc, specIn);
    else {
      for (let k = 0; k < SPEC_BANDS; k++) {
        const f = BAND_F[k], oct = Math.log2(f / 1000);
        const shape = -4.5 * oct - 6 * Math.max(0, oct - 2.5) ** 2 + 10 * Math.exp(-((Math.log2(f / 70)) ** 2) * 2) * pulse;
        specIn[k] = level > 0 ? lvDb - 28 + shape + (Math.random() - 0.5) * 5 : -120;
      }
    }
    const pr = fx.params;
    let gr = 0, out = level;
    if (fx.type === 'eq' && fx.bands) {
      // dynamic bands: the band's level vs its threshold, 6 dB soft knee, full range 12 dB over (dsp.rs)
      fx.bands.forEach((b, k) => {
        if (!b.on || !b.dynamic) { sim!.dyn[k] = 0; return; }
        const bi = clamp(Math.round(Math.log(b.f / 20) / Math.log(1000) * (SPEC_BANDS - 1)), 0, SPEC_BANDS - 1);
        let e = 0; for (let j = Math.max(0, bi - 3); j <= Math.min(SPEC_BANDS - 1, bi + 3); j++) e += Math.pow(10, specIn[j] / 10);
        const lvl = 10 * Math.log10(e + 1e-12) + 6;
        const target = b.dynamic.range * clamp((lvl - b.dynamic.thr + 3) / 15, 0, 1);
        const tau = Math.abs(target) > Math.abs(sim!.dyn[k]) ? 0.01 : 0.08; // fast in, slower out
        sim!.dyn[k] += (target - sim!.dyn[k]) * (1 - Math.exp(-dt / tau));
        meters[k] = sim!.dyn[k];
      });
      const c = eqCurve(fx.bands, pr.out ?? 0, BAND_F, new Float32Array(SPEC_BANDS), 'mid', sim.dyn);
      for (let k = 0; k < SPEC_BANDS; k++) specOut[k] = specIn[k] + (fx.bypass ? 0 : c[k]);
    } else if (fx.type === 'tune') {
      this.simTune(fx, sim, voc, dt);
      for (let i = 0; i < 6; i++) meters[i] = sim.m[i];
      for (let k = 0; k < SPEC_BANDS; k++) specOut[k] = specIn[k];
    } else if (fx.type === 'fet') {
      const inDb = level > 1e-5 ? 20 * Math.log10(level) + (pr.input ?? 8) : -120;
      const ri = Math.round(pr.ratio ?? 0), abi = ri >= 4, ratio = [4, 8, 12, 20, 20][clamp(ri, 0, 4)];
      const over = inDb + 10, knee = abi ? 2 : ratio <= 4 ? 8 : 4, slope = 1 / ratio - 1;
      let target = over <= -knee / 2 ? 0 : over >= knee / 2 ? slope * over : slope * (over + knee / 2) ** 2 / (2 * knee);
      if (abi && over > 0) target -= 4 * Math.min(1, over / 6) * (1 - Math.min(1, over / 30));
      const rel = (pr.release ?? 300) / 1000 * (abi ? 1.4 : 1);
      sim.gr = target < sim.gr ? target : target + (sim.gr - target) * Math.exp(-dt / rel);
      gr = sim.gr;
      const outDb = inDb + gr + (pr.output ?? 0);
      meters[0] = gr; meters[1] = Math.max(-120, inDb); meters[2] = Math.max(-120, outDb);
      out = dbg(outDb);
      for (let k = 0; k < SPEC_BANDS; k++) specOut[k] = specIn[k] + (pr.input ?? 8) + gr + (pr.output ?? 0);
    } else if (fx.type === 'denoise') {
      // noise floor ~ −62 dB tilted toward the lows, per-band reduction where the input is near it
      const red = pr.reduction ?? 12;
      let tot = 0;
      for (let b = 0; b < 16; b++) {
        const f = 50 * Math.pow(320, (b + 0.5) / 16), nz = -58 - 10 * Math.log10(f / 50);
        let near = 0, c = 0;
        for (let k = 0; k < SPEC_BANDS; k++) if (BAND_F[k] >= 50 * Math.pow(320, b / 16) && BAND_F[k] < 50 * Math.pow(320, (b + 1) / 16)) { near += specIn[k]; c++; }
        const lv = c ? near / c : -120, snr = lv - nz;
        const gdb = -Math.min(red, Math.max(0, 10 - snr) * red / 10);
        meters[b] = nz; meters[16 + b] = gdb; tot += gdb;
      }
      for (let k = 0; k < SPEC_BANDS; k++) { const b = Math.max(0, Math.min(15, Math.floor(Math.log(BAND_F[k] / 50) / Math.log(320) * 16))); specOut[k] = specIn[k] + meters[16 + b]; }
      gr = tot / 16; out = level * dbg(gr * 0.3);
    } else if (fx.type === 'deess') {
      const sib = voc?.sib ?? 0, lv = voc?.level ?? 0;
      const hiDb = lv > 1e-4 ? 20 * Math.log10(lv) + (sib > 0 ? 3 + 4 * sib : -26 + (Math.random() - 0.5) * 3) : -120;
      const over = Math.max(0, hiDb - (pr.thr ?? -32));
      const target = -Math.min(over * 0.8 * sib, pr.range ?? 8);
      sim.gr = target < sim.gr ? target + (sim.gr - target) * Math.exp(-dt / 0.004) : target + (sim.gr - target) * Math.exp(-dt / 0.06);
      gr = sim.gr;
      meters[0] = gr; meters[1] = Math.max(-120, hiDb); meters[2] = sib; meters[3] = pr.thr ?? -32;
      out = level * dbg(gr * (pr.mode ? 1 : 0.85));
      const fc = pr.freq ?? 6000;
      for (let k = 0; k < SPEC_BANDS; k++) specOut[k] = specIn[k] + (pr.mode || BAND_F[k] >= fc ? gr : 0);
    } else if (fx.type === 'comp' || fx.type === 'limit') {
      const inDb = lvDb + (fx.type === 'limit' ? pr.gain : 0), thr = fx.type === 'limit' ? pr.ceil : pr.thr;
      const over = inDb - thr;
      gr = level > 0 && over > 0 ? -(fx.type === 'limit' ? over : over - over / pr.ratio) : 0;
      for (let k = 0; k < SPEC_BANDS; k++) specOut[k] = specIn[k] + gr + (pr.gain ?? 0);
      out = level * dbg(gr);
    } else if (fx.type === 'reverb') {
      const hi = pr.hi ?? 8000, lo = pr.lo ?? 120, wet = 20 * Math.log10(Math.max(1e-3, (pr.mix ?? 25) / 100));
      const sustain = 6 * Math.log10(1 + (pr.size ?? 2));
      for (let k = 0; k < SPEC_BANDS; k++) {
        const f = BAND_F[k], r = (f / lo) ** 2;
        specOut[k] = level > 0 ? specIn[k] - 6 * (1 - pulse) + sustain + wet - 10 * Math.log10(1 + (f / hi) ** 2) + 20 * Math.log10(r / Math.sqrt(1 + r * r)) : -120;
      }
    } else if (fx.type === 'sat') {
      for (let k = 0; k < SPEC_BANDS; k++) specOut[k] = specIn[k] + (pr.drive ?? 0) * 0.15 + (k > 60 ? (pr.drive ?? 0) * 0.3 : 0);
    } else {
      for (let k = 0; k < SPEC_BANDS; k++) specOut[k] = specIn[k] + 20 * Math.log10(1 + (pr.mix ?? 25) / 100);
    }
    const drive = dbg(pr.drive ?? 0), f0 = 2 + 3 * Math.sin(t * 0.3);
    for (let i = 0; i < SCOPE_LEN; i++) {
      const x = level * (Math.sin(i / SCOPE_LEN * Math.PI * 2 * f0 + t * 9) * 0.8 + (Math.random() - 0.5) * 0.12);
      scopeIn[i] = x;
      scopeOut[i] = fx.type === 'sat' ? Math.tanh(x * drive * 2.5) / Math.tanh(2.5) * dbg(pr.out ?? 0) : x * dbg(gr);
    }
    return { id: fx.id, inPeak: level, outPeak: out, grDb: gr, meters, specIn, specOut, scopeIn, scopeOut };
  }

  /** vocal.rs `Tune::process`, stepped every 4 ms hop. */
  private simTune(fx: Fx, s: FxSim, voc: VocalSample | null, dt: number) {
    const p = fx.params, key = Math.round(p.key ?? 0), mask = scaleMask(p.scale ?? 0);
    const vib = (p.vibrato ?? 0) / 100, flex = (p.flex ?? 0) / 100, hum = (p.humanize ?? 0) / 100, tr = p.transpose ?? 0;
    const hop = 0.004, steps = Math.max(1, Math.round(dt / hop));
    for (let i = 0; i < steps; i++) {
      if (voc && voc.midi != null && voc.level > 0.02) {
        const m = voc.midi + (Math.random() - 0.5) * 0.02;
        if (s.center === 0 || Math.abs(m - s.center) > 1.5) s.center = m;
        s.center += (m - s.center) * Math.min(1, hop / 0.15);
        const fol = s.center * vib + m * (1 - vib);
        const note = snapNote(fol, key, mask);
        if (note !== s.last) { s.last = note; s.held = 0; } else s.held += hop;
        const err = note - fol, win = 1 - 0.9 * flex;
        const want = flex > 0 && Math.abs(err) > win * 0.5 ? 0 : err;
        let tau = (p.retune ?? 20) / 1000;
        if (hum > 0 && s.held > 0.25) tau += hum * 0.4 * Math.min(1, (s.held - 0.25) / 0.5);
        const k = tau <= 0.0005 ? 1 : 1 - Math.exp(-hop / tau);
        s.corr += (want - s.corr) * k;
        s.m = [midiToHz(m), midiToHz(note + tr), midiToHz(m + s.corr + tr), 0.88 + Math.random() * 0.1, (m - note) * 100, s.corr * 100];
      } else {
        s.corr *= 0.9; s.center = 0;
        s.m = [0, 0, 0, 0.15 + Math.random() * 0.2, 0, 0];
      }
    }
  }

  // ---------- catalog / audio files ----------
  async fxCatalog() { await delay(); return structuredClone(this.cat); }
  async importAudio(a: { paths: string[]; track?: number; beat?: number }) {
    await delay(30);
    const prev = structuredClone(this.p);
    const sourceIds: Id[] = [], clipIds: Id[] = [];
    a.paths.forEach((path, k) => {
      const name = (path.split(/[\\/]/).pop() ?? path).replace(/\.[^.]+$/, '');
      const h = hash(path), id = this.p.nextId++;
      const vocal = isVocalName(name);
      const mins = /(\d+)\s*min/i.exec(name); // e.g. "Long take 4min.wav" (dev/perf checks)
      const dur = mins ? +mins[1] * 60 : vocal ? VOCAL_DUR : /loop/i.test(name) ? 4 * 4 * this.spb() : 0.6 + (h % 700) / 100;
      const src: MockSource = { id, name, color: vocal ? '#d9678f' : TC[(this.p.sources.length + 3) % TC.length], path, dur, sr: 48000, ch: vocal ? 1 : 2, peakDb: -1 - (h % 60) / 10, seed: h, vocal };
      this.srcMeta.set(id, src); this.p.sources.push(strip(src)); sourceIds.push(id);
      if (a.track != null && a.beat != null) {
        const tr = clamp(a.track + k, 0, this.p.tracks.length - 1), cid = this.p.nextId++;
        const len = Math.min(dur / this.spb(), 512 - a.beat);
        this.p.clips.push(mkClip(cid, tr, a.beat, len, 'audio', id)); clipIds.push(cid);
      }
    });
    if (clipIds.length) this.pushUndo(prev);
    this.changed();
    return { sourceIds, clipIds };
  }
  async pickAudioFiles(): Promise<string[] | null> {
    return new Promise(res => {
      const inp = document.createElement('input');
      inp.type = 'file'; inp.accept = 'audio/*'; inp.multiple = true;
      inp.onchange = () => res(inp.files && inp.files.length ? [...inp.files].map(f => f.name) : null);
      inp.click();
    });
  }
  async sourcePeaks(id: Id, norm?: boolean, rev?: boolean) {
    await delay(5);
    const s = this.srcMeta.get(id) ?? this.p.sources.find(x => x.id === id);
    if (!s) throw new Error('no source ' + id);
    let pk: Float32Array;
    const meta = this.srcMeta.get(id);
    if (this.recPeaks.has(id)) pk = this.recPeaks.get(id)!.slice();
    else if (meta?.trimFrom) {
      const full = await this.sourcePeaks(meta.trimFrom.id);
      const a = Math.round(meta.trimFrom.offSec * 100) * 2;
      pk = full.slice(a, a + Math.round(s.dur * 100) * 2);
    } else if (meta?.vocal) pk = vocalPeaks(synthVocal(meta.seed, s.dur));
    else pk = synthPeaks(meta?.seed ?? id, s.dur, 128);
    if (rev) { const n = pk.length / 2, o = new Float32Array(pk.length); for (let i = 0; i < n; i++) { o[2 * i] = pk[2 * (n - 1 - i)]; o[2 * i + 1] = pk[2 * (n - 1 - i) + 1]; } pk = o; }
    if (norm) { let m = 0; for (const v of pk) m = Math.max(m, Math.abs(v)); const g = m > 1e-6 ? dbg(-0.3) / m : 1; for (let i = 0; i < pk.length; i++) pk[i] *= g; }
    return pk;
  }
  private renderReady = 0;
  async clipPeaks(clipId: Id) {
    await delay(Math.max(0, this.renderReady - performance.now())); // the render of the last gesture
    const c = this.p.clips.find(x => x.id === clipId); if (!c || c.kind !== 'audio') throw new Error('no such clip');
    const tune = structuredClone(c.tune ?? null);
    const pk = await this.sourcePeaks(c.ref, c.norm, c.rev);
    await delay(20);
    if (tune?.notes.length) {
      const n = pk.length / 2;
      for (let i = 0; i < n; i++) {
        const j = noteAt(tune.notes, i / 100); if (j < 0) continue;
        const nt = tune.notes[j], edge = clamp(Math.min(i / 100 - nt.start, nt.end - i / 100) / 0.01, 0, 1);
        const g = 1 + (dbg(nt.gain) - 1) * edge;
        pk[2 * i] = clamp(pk[2 * i] * g, -1, 0); pk[2 * i + 1] = clamp(pk[2 * i + 1] * g, 0, 1);
      }
    }
    return pk;
  }
  async pitchAnalyze(clipId: Id): Promise<PitchAnalysis> {
    await delay(120); // the engine analyses on a worker thread; keep the loading state visible
    const c = this.p.clips.find(x => x.id === clipId);
    if (!c) throw new Error('no such clip');
    if (c.kind !== 'audio') throw new Error('only audio clips have pitch');
    const s = this.srcMeta.get(c.ref) ?? this.p.sources.find(x => x.id === c.ref) as MockSource | undefined;
    if (!s) throw new Error('audio not loaded');
    const v = synthVocal(s.seed ?? s.id, s.dur);
    return structuredClone({ rate: v.rate, curve: v.curve, levels: v.levels, notes: v.notes });
  }
  async trimClip(clipId: Id) {
    const c = this.p.clips.find(x => x.id === clipId); if (!c) throw new Error('no clip');
    const s = this.srcMeta.get(c.ref); if (!s) throw new Error('no source');
    const prev = structuredClone(this.p), spb = this.spb(), id = this.p.nextId++;
    const ns: MockSource = { ...s, id, dur: c.len * spb, path: s.path.replace(/(\.\w+)?$/, '-trim.wav'), trimFrom: { id: s.id, offSec: c.offset * spb } };
    this.srcMeta.set(id, ns); this.p.sources.push(strip(ns));
    Object.assign(this.p.clips.find(x => x.id === clipId)!, { ref: id, offset: 0, norm: false, rev: false });
    this.pushUndo(prev); this.changed();
    return { sourceId: id };
  }
  async previewClip(clipId: Id) {
    const c = this.p.clips.find(x => x.id === clipId); if (!c) return;
    this.preview = { t0: performance.now(), dur: c.len * this.spb(), off: c.offset * this.spb() };
  }
  async previewFile(path: string) { this.preview = { t0: performance.now(), dur: 0.6 + (hash(path) % 400) / 100, off: 0 }; }
  async previewStop() { this.preview = null; }

  // ---------- audio device ----------
  async audioHosts() { return [{ id: 'wasapi', name: 'WASAPI' }, { id: 'asio', name: 'ASIO' }]; }
  async audioDevices(host: string) {
    return host === 'asio'
      ? { outputs: ['Focusrite USB ASIO'], inputs: ['Focusrite USB ASIO'] }
      : { outputs: ['Głośniki (Realtek High Definition Audio)', 'Słuchawki (USB Audio)'], inputs: ['Mikrofon (USB Audio)', 'Line In (Realtek)'] };
  }
  async audioGetConfig() { return { ...this.audio }; }
  async audioSetConfig(cfg: AudioConfig) {
    await delay(80);
    const asioErr = cfg.host === 'asio' && !this.audio.asioCompiled;
    this.audio = { ...this.audio, ...cfg, running: !asioErr, error: asioErr ? 'ASIO nie jest wkompilowane w tę wersję' : null,
      actualRate: cfg.sampleRate ?? 48000, actualBuffer: cfg.bufferSize ?? 480, latencyMs: Math.round(((cfg.bufferSize ?? 480) * 2 / (cfg.sampleRate ?? 48000)) * 10000) / 10 };
    this.emit('audio-status', { ...this.audio });
    return { ...this.audio };
  }

  // ---------- library ----------
  async libraryList(): Promise<LibraryList> { await delay(); return { kits: this.kits.map(k => ({ ...k })), folders: [...this.folders] }; }
  async libraryInstall(id: string) {
    const kit = this.kits.find(k => k.id === id); if (!kit) throw new Error('no kit');
    let done = 0; const total = kit.bytes;
    const iv = setInterval(() => {
      done = Math.min(total, done + total / 25);
      this.emit('library-progress', { id, done, total });
      if (done >= total) { clearInterval(iv); kit.installed = true; }
    }, 60);
  }
  async libraryFiles(q: { kit?: string; folder?: string }): Promise<LibFile[]> {
    await delay(10);
    const root = q.kit ? 'C:\\WAVE\\Library\\' + q.kit : q.folder ?? '';
    const groups: Record<string, string[]> = q.kit === 'wave-keys'
      ? { Pady: ['Warm Pad C', 'Glass Pad Am', 'Choir Pad F'], Akordy: ['Rhodes Chord 1', 'Rhodes Chord 2', 'Piano Stab'], Pętle: ['Keys Loop 120', 'Pad Loop Am-F'] }
      : q.kit === 'wave-fx' ? { Uplightery: ['Riser 8 bar', 'Riser 4 bar'], Impakty: ['Impact Deep', 'Impact Metal'], Szumy: ['White Sweep', 'Vinyl Crackle'] }
      : q.kit ? { Kick: ['Kick 808', 'Kick Punch', 'Kick Deep'], Snare: ['Snare Tight', 'Snare Room', 'Clap 909'], Hats: ['Hat Closed', 'Hat Open', 'Ride'], Loops: ['Drum Loop 128', 'Perc Loop 128', 'Top Loop'] }
      : { '': ['vox_take1', 'vox_take2', 'guitar_di', 'bass_di', 'fx_loop'] };
    return Object.entries(groups).flatMap(([group, names]) => names.map(n => ({ name: n + '.wav', path: root + '\\' + (group ? group + '\\' : '') + n + '.wav', group })));
  }
  async libraryAddFolder() { this.folders.push('D:\\Sample packs\\Pack ' + (this.folders.length + 1)); return [...this.folders]; }
  async libraryRemoveFolder(path: string) { this.folders = this.folders.filter(f => f !== path); return [...this.folders]; }

  // ---------- project files ----------
  private resetTo(p: Project, path: string | null) {
    this.p = p; this.undoStack = []; this.redoStack = []; this.path = path;
    this.tr = { ...this.tr, playing: false }; this.revision++; this.savedRev = this.revision;
    this.emit('state', this.dto());
  }
  async projectNew() { this.resetTo(emptyProject(this.cat), null); return this.dto(); }
  async projectOpen(path?: string) { this.resetTo(demoProject(this.cat), path ?? 'C:\\Users\\michal\\Music\\Demo.wave'); return this.dto(); }
  async projectSave(saveAs?: boolean) {
    if (!this.path || saveAs) this.path = 'C:\\Users\\michal\\Music\\Projekt ' + (1 + (this.revision % 9)) + '.wave';
    this.savedRev = this.revision; this.emit('state', this.dto());
    return { path: this.path };
  }

  // ---------- VST3 ----------
  async vst3Scan(rescan: boolean): Promise<Vst3Info[]> {
    await delay(rescan ? 600 : 20);
    const mk = (name: string, vendor: string, category: string) => ({ name, vendor, category, path: 'C:\\Program Files\\Common Files\\VST3\\' + name + '.vst3', cid: hash(name).toString(16).padStart(32, '0') });
    return [mk('Pro-Q 3', 'FabFilter', 'Fx|EQ'), mk('Pro-C 2', 'FabFilter', 'Fx|Dynamics'), mk('Valhalla VintageVerb', 'Valhalla DSP', 'Fx|Reverb'),
      mk('Decapitator', 'Soundtoys', 'Fx|Distortion'), mk('EchoBoy', 'Soundtoys', 'Fx|Delay'), mk('OTT', 'Xfer Records', 'Fx|Dynamics'), mk('Serum FX', 'Xfer Records', 'Fx')];
  }
  async vst3Editor() {}
  async vst3Params(fxId: Id): Promise<Vst3Param[]> {
    if (!this.vstParams.has(fxId)) {
      const names = ['Input', 'Output', 'Mix', 'Drive', 'Tone', 'Attack', 'Release', 'Threshold', 'Ratio', 'Width', 'Low', 'High'];
      this.vstParams.set(fxId, names.map((name, i) => { const v = ((hash(name + fxId) % 100) / 100); return { id: i, name, value: v, def: 0.5, display: (v * 100).toFixed(0) + ' %' }; }));
    }
    return structuredClone(this.vstParams.get(fxId)!);
  }
  async vst3SetParam(fxId: Id, id: number, value: number) {
    const p = this.vstParams.get(fxId)?.find(x => x.id === id);
    if (p) { p.value = value; p.display = (value * 100).toFixed(0) + ' %'; }
  }

  async cliStatus() { return { installed: false, command: 'wave', path: null, note: 'Podgląd w przeglądarce — CLI dostępne tylko w aplikacji' }; }

  async on<K extends keyof EventMap>(ev: K, cb: (p: EventMap[K]) => void) {
    let s = this.listeners.get(ev); if (!s) this.listeners.set(ev, s = new Set());
    const f = cb as (p: unknown) => void; s.add(f);
    return () => { s!.delete(f); };
  }
  async onOsDrop(cb: (e: OsDropEvent) => void) {
    const over = (e: DragEvent) => { if (e.dataTransfer && [...e.dataTransfer.types].includes('Files')) { e.preventDefault(); cb({ type: 'over', paths: [], x: e.clientX, y: e.clientY }); } };
    const leave = (e: DragEvent) => { if (!e.relatedTarget) cb({ type: 'leave', paths: [], x: 0, y: 0 }); };
    const drop = (e: DragEvent) => {
      e.preventDefault();
      const files = [...(e.dataTransfer?.files ?? [])].filter(f => f.type.startsWith('audio') || /\.(wav|mp3|ogg|flac|m4a|aif+)$/i.test(f.name));
      cb({ type: 'drop', paths: files.map(f => f.name), x: e.clientX, y: e.clientY });
    };
    window.addEventListener('dragover', over); window.addEventListener('dragleave', leave); window.addEventListener('drop', drop);
    return () => { window.removeEventListener('dragover', over); window.removeEventListener('dragleave', leave); window.removeEventListener('drop', drop); };
  }
  setTitle(title: string) { document.title = title; }

  /** Dev helper: a sung vocal take on the Vocal track and a vocal chain on its insert. */
  async addDemoVocal() {
    const r = await this.importAudio({ paths: ['Vocal take 1.wav'], track: 5, beat: 0 });
    const ins = this.p.inserts.find(m => m.name === 'Vocal') ?? this.p.inserts[6];
    const mk = (type: Fx['type']) => newFx(this.cat, this.p.nextId++, type as never, this.p.bpm);
    const eq = ins.fx.find(f => f.type === 'eq');
    if (eq?.bands) {
      eq.bands.push({ on: true, type: 'peak', f: 5200, g: 0, q: 2.2, slope: 12, place: 'stereo', dynamic: { range: -6, thr: -40 } });
      eq.bands.push({ on: true, type: 'highshelf', f: 9000, g: 2.5, q: 0.71, slope: 12, place: 'side' });
    }
    const fx = ins.fx.filter(f => f.type !== 'comp');
    ins.fx = [mk('tune'), ...fx.filter(f => f.type === 'eq'), mk('deess'), mk('fet'), ...fx.filter(f => f.type !== 'eq')];
    this.changed();
    return r;
  }

  /** Dev helper: drop two demo loops on Pad/Perc like the prototype did. */
  async addDemoAudio() {
    await this.importAudio({ paths: ['Pad Loop Am-F.wav'], track: 3, beat: 32 });
    await this.importAudio({ paths: ['Perc Loop.wav'], track: 4, beat: 48 });
  }
}

function strip(s: MockSource): Source {
  const { seed: _s, trimFrom: _t, ...rest } = s; void _s; void _t;
  return rest;
}
function mkClip(id: Id, track: number, start: number, len: number, kind: Clip['kind'], ref: Id): Clip {
  return { id, track, start, len, offset: 0, kind, ref, muted: false, gain: 0, fadeIn: 0, fadeOut: 0, pan: 0, norm: false, rev: false };
}
void BUILTIN;
