export const clamp = (v: number, a: number, b: number) => Math.min(b, Math.max(a, v));
export const mod = (a: number, n: number) => ((a % n) + n) % n;
export const gainLin = (t: number) => 2 * t * t * t;
export const dbg = (db: number) => Math.pow(10, db / 20);

export function volText(t: number): string {
  if (t < 0.002) return '-∞ dB';
  const d = Math.round(20 * Math.log10(gainLin(t)) * 10) / 10;
  return (d > 0 ? '+' : '') + (d === 0 ? '0.0' : d.toFixed(1)) + ' dB';
}
export const dbText = (g: number) => (g >= 0 ? '+' : '') + g.toFixed(1) + ' dB';
export function panText(v: number): string {
  const p = Math.round(v * 100);
  return p === 0 ? 'C' : p < 0 ? (-p) + 'L' : p + 'R';
}
export const hzText = (v: number) => v >= 1000 ? (v / 1000).toFixed(v >= 10000 ? 1 : 2) + ' kHz' : Math.round(v) + ' Hz';

export function fmtPos(b: number): string {
  return (Math.floor(b / 4) + 1) + '.' + (mod(Math.floor(b), 4) + 1) + (b % 1 ? '.' + (Math.floor(mod(b, 1) * 4) + 1) : '');
}
export function fmtClock(b: number): string {
  if (b < 0) return 'COUNT ' + Math.ceil(-b);
  return (Math.floor(b / 4) + 1) + ':' + String(Math.floor(b) % 4 + 1).padStart(2, '0') + ':' + String(Math.floor((b % 1) * 4) + 1).padStart(2, '0');
}
export function fmtBytes(n: number): string {
  if (n >= 1e9) return (n / 1e9).toFixed(1) + ' GB';
  if (n >= 1e6) return (n / 1e6).toFixed(0) + ' MB';
  if (n >= 1e3) return (n / 1e3).toFixed(0) + ' kB';
  return n + ' B';
}
export const baseName = (p: string) => p.split(/[\\/]/).pop() ?? p;

/** Tap tempo accumulator: resets after `resetMs` of silence, keeps up to `maxTaps` timestamps,
 *  and fits a least-squares line t(k) = a + T·k to cancel individual tap jitter. */
export class TapTracker {
  taps: number[] = [];
  constructor(private readonly maxTaps = 16, private readonly resetMs = 2000) {}

  tap(nowMs: number, round = true): { bpm: number | null; count: number; fresh: boolean } {
    const last = this.taps[this.taps.length - 1];
    const fresh = last == null || nowMs - last > this.resetMs;
    if (fresh) this.taps = [nowMs];
    else {
      this.taps.push(nowMs);
      if (this.taps.length > this.maxTaps) this.taps.shift();
    }
    const n = this.taps.length;
    if (n < 2) return { bpm: null, count: n, fresh };
    let num = 0, den = 0;
    const meanK = (n - 1) / 2;
    const meanT = this.taps.reduce((a, b) => a + b, 0) / n;
    for (let k = 0; k < n; k++) {
      const dk = k - meanK;
      num += dk * (this.taps[k] - meanT);
      den += dk * dk;
    }
    const periodMs = num / den;
    if (!(periodMs > 0)) return { bpm: null, count: n, fresh };
    const raw = 60000 / periodMs;
    const bpm = clamp(round ? Math.round(raw) : Math.round(raw * 100) / 100, 20, 999);
    return { bpm, count: n, fresh };
  }

  reset() { this.taps = []; }
}
