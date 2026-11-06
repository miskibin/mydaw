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
