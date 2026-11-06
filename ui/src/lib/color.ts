// sRGB colour mixing (what the prototype did with CSS color-mix), cached.
const cache = new Map<string, string>();

function parse(hex: string): [number, number, number] {
  let h = hex.replace('#', '');
  if (h.length === 3) h = h.split('').map(c => c + c).join('');
  const n = parseInt(h.slice(0, 6), 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

/** `pct` percent of `a` mixed with `b`. */
export function mix(a: string, pct: number, b: string): string {
  const k = a + pct + b;
  let v = cache.get(k);
  if (v) return v;
  const x = parse(a), y = parse(b), t = pct / 100;
  v = 'rgb(' + x.map((c, i) => Math.round(c * t + y[i] * (1 - t))).join(',') + ')';
  cache.set(k, v);
  return v;
}

export function alpha(hex: string, a: number): string {
  const [r, g, b] = parse(hex);
  return `rgba(${r},${g},${b},${a})`;
}
