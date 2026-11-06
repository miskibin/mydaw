// Binary telemetry frame (little-endian), see docs/architecture.md.
//
//   u32 magic 'WAVT' · u32 seq · f64 beat · u8 flags · u8 pad · u16 pad
//   f32 inLevel · f32 previewPos · f32 cpu
//   u32 nIns × {u32 insertId, f32 peakL, f32 peakR}
//   u32 nFx  × {u32 fxId, f32 inPeak, f32 outPeak, f32 grDb, f32 specIn[128], f32 specOut[128], f32 scopeIn[256], f32 scopeOut[256]}
//   u32 rec (0 | 1 + {u32 track, f64 clipStart, f32 seconds, u32 peakOffset, u32 nPeaks, f32 peaks[2*nPeaks]})
//
// The decoder writes into long-lived objects so a 60 Hz stream does not allocate per frame.

export const MAGIC = 0x54564157;
export const SPEC_BANDS = 128;
export const SCOPE_LEN = 256;
const FX_BYTES = 144 + 4 * (2 * SPEC_BANDS + 2 * SCOPE_LEN);

export interface FxFrame {
  id: number;
  inPeak: number; outPeak: number; grDb: number;
  /** per-type live readings (pitch, envelope, …), see docs/architecture.md */
  meters: Float32Array;
  specIn: Float32Array; specOut: Float32Array; scopeIn: Float32Array; scopeOut: Float32Array;
  /** Telemetry seq of the last analysis frame that carried this fx. */
  seq: number;
}

export interface RecFrame {
  track: number; clipStart: number; seconds: number; peakOffset: number; nPeaks: number;
  peaks: Float32Array; // view of length 2*nPeaks (reused buffer)
}

export interface Frame {
  seq: number; beat: number;
  playing: boolean; recording: boolean; previewing: boolean;
  inLevel: number; previewPos: number; cpu: number;
  ins: Map<number, { l: number; r: number }>;
  /** fx present in *this* frame (analysis frames only). */
  fxIds: number[];
  rec: RecFrame | null;
}

export class TelemetryDecoder {
  frame: Frame = {
    seq: 0, beat: 0, playing: false, recording: false, previewing: false,
    inLevel: 0, previewPos: -1, cpu: 0, ins: new Map(), fxIds: [], rec: null,
  };
  /** Latest analysis per fx id (persists across frames that carry no fx). */
  fx = new Map<number, FxFrame>();
  private recBuf = new Float32Array(1024);
  private recObj: RecFrame = { track: 0, clipStart: 0, seconds: 0, peakOffset: 0, nPeaks: 0, peaks: new Float32Array(0) };

  /** Returns false (and leaves state untouched) for a malformed frame. */
  decode(buf: ArrayBuffer): boolean {
    if (buf.byteLength < 36) return false;
    const v = new DataView(buf);
    if (v.getUint32(0, true) !== MAGIC) return false;
    const f = this.frame;
    try {
      f.seq = v.getUint32(4, true);
      f.beat = v.getFloat64(8, true);
      const flags = v.getUint8(16);
      f.playing = (flags & 1) !== 0;
      f.recording = (flags & 2) !== 0;
      f.previewing = (flags & 4) !== 0;
      f.inLevel = v.getFloat32(20, true);
      f.previewPos = v.getFloat32(24, true);
      f.cpu = v.getFloat32(28, true);
      let o = 32;
      const nIns = v.getUint32(o, true); o += 4;
      for (let i = 0; i < nIns; i++) {
        const id = v.getUint32(o, true);
        let e = f.ins.get(id);
        if (!e) { e = { l: 0, r: 0 }; f.ins.set(id, e); }
        e.l = v.getFloat32(o + 4, true);
        e.r = v.getFloat32(o + 8, true);
        o += 12;
      }
      const nFx = v.getUint32(o, true); o += 4;
      f.fxIds.length = 0;
      for (let i = 0; i < nFx; i++) {
        if (o + FX_BYTES > buf.byteLength) return false;
        const id = v.getUint32(o, true);
        let e = this.fx.get(id);
        if (!e) {
          e = { id, inPeak: 0, outPeak: 0, grDb: 0, seq: 0, meters: new Float32Array(32),
            specIn: new Float32Array(SPEC_BANDS), specOut: new Float32Array(SPEC_BANDS),
            scopeIn: new Float32Array(SCOPE_LEN), scopeOut: new Float32Array(SCOPE_LEN) };
          this.fx.set(id, e);
        }
        e.inPeak = v.getFloat32(o + 4, true);
        e.outPeak = v.getFloat32(o + 8, true);
        e.grDb = v.getFloat32(o + 12, true);
        o += 16;
        o = readF32(v, o, e.meters);
        o = readF32(v, o, e.specIn);
        o = readF32(v, o, e.specOut);
        o = readF32(v, o, e.scopeIn);
        o = readF32(v, o, e.scopeOut);
        e.seq = f.seq;
        f.fxIds.push(id);
      }
      const rec = o + 4 <= buf.byteLength ? v.getUint32(o, true) : 0; o += 4;
      if (rec === 1) {
        const r = this.recObj;
        r.track = v.getUint32(o, true);
        r.clipStart = v.getFloat64(o + 4, true);
        r.seconds = v.getFloat32(o + 12, true);
        r.peakOffset = v.getUint32(o + 16, true);
        r.nPeaks = v.getUint32(o + 20, true);
        o += 24;
        const n = r.nPeaks * 2;
        if (o + n * 4 > buf.byteLength) return false;
        if (this.recBuf.length < n) this.recBuf = new Float32Array(Math.max(n, this.recBuf.length * 2));
        for (let i = 0; i < n; i++) this.recBuf[i] = v.getFloat32(o + i * 4, true);
        r.peaks = this.recBuf.subarray(0, n);
        f.rec = r;
      } else f.rec = null;
    } catch {
      return false;
    }
    return true;
  }
}

function readF32(v: DataView, o: number, out: Float32Array): number {
  for (let i = 0; i < out.length; i++) out[i] = v.getFloat32(o + i * 4, true);
  return o + out.length * 4;
}

// ---- encoder (used by the in-browser mock backend and tests) ----

export interface EncFx {
  id: number; inPeak: number; outPeak: number; grDb: number; meters?: ArrayLike<number>;
  specIn: ArrayLike<number>; specOut: ArrayLike<number>; scopeIn: ArrayLike<number>; scopeOut: ArrayLike<number>;
}
export interface EncFrame {
  seq: number; beat: number; playing: boolean; recording: boolean; previewing: boolean;
  inLevel: number; previewPos: number; cpu: number;
  ins: { id: number; l: number; r: number }[];
  fx: EncFx[];
  rec: null | { track: number; clipStart: number; seconds: number; peakOffset: number; peaks: ArrayLike<number> };
}

export function encodeFrame(f: EncFrame): ArrayBuffer {
  const recBytes = f.rec ? 24 + f.rec.peaks.length * 4 : 0;
  const size = 36 + f.ins.length * 12 + 4 + f.fx.length * FX_BYTES + 4 + recBytes;
  const buf = new ArrayBuffer(size);
  const v = new DataView(buf);
  v.setUint32(0, MAGIC, true);
  v.setUint32(4, f.seq >>> 0, true);
  v.setFloat64(8, f.beat, true);
  v.setUint8(16, (f.playing ? 1 : 0) | (f.recording ? 2 : 0) | (f.previewing ? 4 : 0));
  v.setFloat32(20, f.inLevel, true);
  v.setFloat32(24, f.previewPos, true);
  v.setFloat32(28, f.cpu, true);
  let o = 32;
  v.setUint32(o, f.ins.length, true); o += 4;
  for (const e of f.ins) { v.setUint32(o, e.id, true); v.setFloat32(o + 4, e.l, true); v.setFloat32(o + 8, e.r, true); o += 12; }
  v.setUint32(o, f.fx.length, true); o += 4;
  const w = (a: ArrayLike<number>, n: number) => { for (let i = 0; i < n; i++) v.setFloat32(o + i * 4, a[i] ?? 0, true); o += n * 4; };
  for (const e of f.fx) {
    v.setUint32(o, e.id, true); v.setFloat32(o + 4, e.inPeak, true); v.setFloat32(o + 8, e.outPeak, true); v.setFloat32(o + 12, e.grDb, true);
    o += 16;
    w(e.meters ?? [], 32);
    w(e.specIn, SPEC_BANDS); w(e.specOut, SPEC_BANDS); w(e.scopeIn, SCOPE_LEN); w(e.scopeOut, SCOPE_LEN);
  }
  if (f.rec) {
    v.setUint32(o, 1, true); o += 4;
    v.setUint32(o, f.rec.track, true);
    v.setFloat64(o + 4, f.rec.clipStart, true);
    v.setFloat32(o + 12, f.rec.seconds, true);
    v.setUint32(o + 16, f.rec.peakOffset, true);
    v.setUint32(o + 20, f.rec.peaks.length / 2, true);
    o += 24;
    w(f.rec.peaks, f.rec.peaks.length);
  } else {
    v.setUint32(o, 0, true); o += 4;
  }
  return buf;
}
