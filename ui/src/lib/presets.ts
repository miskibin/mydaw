// Preset resolution and matching against the catalog from `fx_catalog`.
import type { AnyBuiltinKind, Catalog, EqBand, Fx, FxKind, Id, ParamDef, Preset } from '../api/types';

export interface ResolvedPreset { params: Record<string, number>; bands?: EqBand[] }

export function paramDefs(cat: Catalog | null | undefined, type: FxKind): ParamDef[] {
  return cat?.types[type]?.params ?? [];
}
export function presetList(cat: Catalog | null | undefined, type: FxKind): Preset[] {
  return cat?.presets[type] ?? [];
}
export function defaultParams(cat: Catalog | null | undefined, type: FxKind): Record<string, number> {
  return Object.fromEntries(paramDefs(cat, type).map(d => [d.id, d.def]));
}

/** Same as catalog.rs `load_preset`: defaults, then overrides, then tempo-synced time, then bands. */
export function resolvePreset(cat: Catalog, type: FxKind, i: number, bpm: number, curBands?: EqBand[]): ResolvedPreset | null {
  const pr = presetList(cat, type)[i];
  if (!pr) return null;
  const params = { ...defaultParams(cat, type), ...pr.params };
  if (pr.beats != null) params.time = Math.min(1500, Math.max(10, pr.beats * 60000 / bpm));
  const bands = pr.bands ? pr.bands.map(b => ({ ...b })) : curBands?.map(b => ({ ...b }));
  return { params, bands };
}

const near = (a: number | undefined, b: number) => Math.abs((a ?? 0) - b) <= 1e-3 * Math.max(1, Math.abs(b));

export function paramsMatch(fx: Pick<Fx, 'params' | 'bands'>, r: ResolvedPreset): boolean {
  for (const k of Object.keys(r.params)) if (!near(fx.params[k], r.params[k])) return false;
  if (r.bands) {
    if (!fx.bands || fx.bands.length !== r.bands.length) return false;
    for (let i = 0; i < r.bands.length; i++) {
      const x = r.bands[i], y = fx.bands[i];
      if (y.on !== x.on || y.type !== x.type || !near(y.f, x.f) || !near(y.g, x.g) || !near(y.q, x.q)) return false;
      if ((y.slope ?? 12) !== (x.slope ?? 12) || (y.place ?? 'stereo') !== (x.place ?? 'stereo')) return false;
      const dx = x.dynamic ?? null, dy = y.dynamic ?? null;
      if (!!dx !== !!dy || (dx && dy && (!near(dy.range, dx.range) || !near(dy.thr, dx.thr)))) return false;
    }
  }
  return true;
}

export interface PresetInfo { idx: number; name: string; mod: boolean }

/** Which preset the effect currently is: an exact match wins, else the last loaded one marked modified. */
export function presetInfo(cat: Catalog | null, fx: Fx, bpm: number): PresetInfo {
  if (!cat || fx.type === 'vst3') return { idx: -1, name: fx.plugin?.vendor ?? '', mod: false };
  const list = presetList(cat, fx.type);
  for (let i = 0; i < list.length; i++) {
    const r = resolvePreset(cat, fx.type, i, bpm);
    if (r && paramsMatch(fx, r)) return { idx: i, name: list[i].name, mod: false };
  }
  if (fx.preset != null && list[fx.preset]) return { idx: fx.preset, name: list[fx.preset].name, mod: true };
  return { idx: -1, name: 'Własne ustawienia', mod: false };
}

export function loadPreset(cat: Catalog, fx: Fx, i: number, bpm: number): Fx {
  const list = presetList(cat, fx.type);
  if (!list.length) return fx;
  const idx = ((i % list.length) + list.length) % list.length;
  const r = resolvePreset(cat, fx.type, idx, bpm, fx.bands)!;
  return { ...fx, params: r.params, bands: r.bands ?? fx.bands, preset: idx };
}

/** Fallback EQ bands if the catalog carries none (mirrors catalog.rs default_bands). */
export function defaultBands(): EqBand[] {
  const b = (on: boolean, type: EqBand['type'], f: number, q: number): EqBand => ({ on, type, f, g: 0, q, slope: 12, place: 'stereo' });
  return [b(false, 'lowcut', 30, 0.71), b(true, 'lowshelf', 90, 0.71), b(true, 'peak', 250, 1), b(true, 'peak', 1000, 1),
    b(true, 'peak', 3500, 1), b(true, 'highshelf', 9000, 0.71), b(false, 'highcut', 18000, 0.71)];
}

/** A fresh built-in effect loaded with its first preset (catalog.rs `new_fx`). */
export function newFx(cat: Catalog, id: Id, type: AnyBuiltinKind, bpm: number): Fx {
  let fx: Fx = { id, type, bypass: false, preset: null, params: defaultParams(cat, type) };
  if (type === 'eq') fx.bands = defaultBands();
  if (presetList(cat, type).length) fx = loadPreset(cat, fx, 0, bpm);
  return fx;
}

// ---- parameter mapping (knob travel) ----
export const pNorm = (p: ParamDef, v: number) => p.log ? Math.log(v / p.min) / Math.log(p.max / p.min) : (v - p.min) / (p.max - p.min);
export const pVal = (p: ParamDef, t: number) => p.log ? p.min * Math.pow(p.max / p.min, t) : p.min + t * (p.max - p.min);
export function pFmt(p: ParamDef, v: number): string {
  const u = p.unit;
  if (u === 'Hz') return v >= 1000 ? (v / 1000).toFixed(v >= 10000 ? 1 : 2) + ' kHz' : Math.round(v) + ' Hz';
  if (u === 'ms') return (v < 10 ? v.toFixed(1) : Math.round(v)) + ' ms';
  if (u === 's') return v.toFixed(2) + ' s';
  if (u === '%') return Math.round(v) + '%';
  if (u === ':1') return v.toFixed(1) + ':1';
  if (u === 'µs') return Math.round(v) + ' µs';
  if (u === 'st') return (v > 0.05 ? '+' : '') + (Math.abs(v) < 0.05 ? '0' : v.toFixed(1)) + ' st';
  if (p.options) return p.options[Math.round(Math.min(p.max, Math.max(p.min, v)))] ?? String(v);
  if (u === 'dB') return (v > 0.05 ? '+' : '') + (Math.abs(v) < 0.05 ? '0.0' : v.toFixed(1)) + ' dB';
  return v.toFixed(2) + (u ? ' ' + u : '');
}
