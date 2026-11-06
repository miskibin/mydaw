// Colour tokens. CSS reads the same names as variables (styles.css, `:root` / `[data-theme=light]`);
// canvas code reads them from `T`, which is mutated in place when the theme changes. Cached
// canvas layers include `theme.epoch` in their key (frameLoop `Layer`), so they re-render.
import { createSignal } from 'solid-js';

export interface Tokens {
  // surfaces
  bg0: string; bg1: string; bg2: string; bg3: string; bg4: string; bg5: string; bg6: string; bg7: string;
  line: string; line2: string; line3: string;
  // text
  fg: string; fgHi: string; fg2: string; fg3: string; fg4: string; fg5: string; fg6: string;
  // accents
  blue: string; blueHi: string; green: string; amber: string; red: string; onBg: string;
  // plots (effect windows, editors)
  viz: string; vizPanel: string; vizGrid: string; vizGrid2: string; vizAxis: string; vizLabel: string;
  /** main curve / ink on plots */
  ink: string;
  /** "before" signal (spectrum pre, input pitch, input level) */
  pre: string;
  /** "after" signal (spectrum post, corrected pitch, output) */
  post: string;
  playhead: string;
  /** text drawn on a saturated fill */
  onFill: string;
  /** playlist */
  grid: string; gridBar: string; gridBeat: string; laneAlt: string; rulerBg: string; rulerText: string; clipBody: string;
  shade: string;
  /** piano keys (pitch views) */
  keyW: string; keyB: string; keyWText: string; keyBText: string;
  /** text on a band/accent-coloured handle */
  onBand: string;
  /** a light-tinted accent fill strength multiplier (light needs a bit more) */
  tint: number;
  /** playlist clips: body/header mixes of the clip colour with a base, ink for notes/waveforms */
  clipBase: string; clipBodyPct: number; clipHeadPct: number; clipHeadSelPct: number; clipHeadSelTo: string;
  clipInkTo: string; clipInkPct: number; clipText: string; clipTextSel: string; laneArm: string; gridShade: string;
}

export const DARK: Tokens = {
  bg0: '#0a0c0f', bg1: '#0e1013', bg2: '#111419', bg3: '#14181e', bg4: '#161a21', bg5: '#171b21', bg6: '#1c2128', bg7: '#1e242d',
  line: '#252b34', line2: '#2c333d', line3: '#2e3641',
  fg: '#d5dae2', fgHi: '#f2f4f7', fg2: '#b4bac4', fg3: '#9aa2ad', fg4: '#7d8592', fg5: '#5c6573', fg6: '#4a5360',
  blue: '#4f8ef7', blueHi: '#8fb6ff', green: '#4ec08a', amber: '#e0a44a', red: '#e05263', onBg: '#2f3a4b',
  viz: '#0c0e12', vizPanel: '#101318', vizGrid: '#161b22', vizGrid2: '#20262f', vizAxis: '#2a313b', vizLabel: '#4a5360',
  ink: '#f2f4f7', pre: '#9aa2ad', post: '#8fb6ff', playhead: '#eef1f5', onFill: '#0b1220',
  grid: '#111419', gridBar: '#232a33', gridBeat: '#1a1f27', laneAlt: '#13171c', rulerBg: '#14181e', rulerText: '#7d8592', clipBody: '#101318',
  shade: 'rgba(0,0,0,.55)',
  keyW: '#c9ced6', keyB: '#20252d', keyWText: '#1a1e24', keyBText: '#e6e9ee', onBand: '#0b1220', tint: 1,
  clipBase: '#101318', clipBodyPct: 14, clipHeadPct: 42, clipHeadSelPct: 75, clipHeadSelTo: '#ffffff',
  clipInkTo: '#ffffff', clipInkPct: 80, clipText: '#f2f4f7', clipTextSel: '#0b1220', laneArm: '#16121a', gridShade: 'rgba(255,255,255,.018)',
};

export const LIGHT: Tokens = {
  bg0: '#d9dde3', bg1: '#eef0f3', bg2: '#f6f7f9', bg3: '#f1f3f5', bg4: '#f7f8fa', bg5: '#e9ecf0', bg6: '#e2e6eb', bg7: '#ffffff',
  line: '#d3d8df', line2: '#c6ccd4', line3: '#bfc6cf',
  fg: '#2a3038', fgHi: '#0f1318', fg2: '#3c444f', fg3: '#525b67', fg4: '#69727e', fg5: '#858d98', fg6: '#9aa1ab',
  blue: '#2563d9', blueHi: '#1f56c2', green: '#1f9461', amber: '#b9770f', red: '#cf2f45', onBg: '#d6e3fb',
  viz: '#fbfcfd', vizPanel: '#f3f5f8', vizGrid: '#eceff3', vizGrid2: '#dfe3e9', vizAxis: '#c9cfd7', vizLabel: '#8a929d',
  ink: '#151a21', pre: '#7f8894', post: '#2563d9', playhead: '#11161c', onFill: '#ffffff',
  grid: '#f6f7f9', gridBar: '#cfd5dc', gridBeat: '#e3e7ec', laneAlt: '#eff1f4', rulerBg: '#eceff3', rulerText: '#5d6672', clipBody: '#ffffff',
  shade: 'rgba(255,255,255,.62)',
  keyW: '#ffffff', keyB: '#3a4049', keyWText: '#1a1e24', keyBText: '#eef1f4', onBand: '#0b1220', tint: 1.4,
  clipBase: '#ffffff', clipBodyPct: 16, clipHeadPct: 62, clipHeadSelPct: 100, clipHeadSelTo: '#000000',
  clipInkTo: '#000000', clipInkPct: 78, clipText: '#0f1318', clipTextSel: '#ffffff', laneArm: '#f7eef1', gridShade: 'rgba(0,0,0,.025)',
};

/** Current tokens (mutated in place on theme change; read at draw time). */
export const T: Tokens = { ...DARK };
export type ThemeMode = 'dark' | 'light' | 'system';
export const theme = { epoch: 0, light: false };
const [themeVersion, setThemeVersion] = createSignal(0);
const [themeMode, setThemeModeSig] = createSignal<ThemeMode>('dark');
export { themeMode, themeVersion };

const KEY = 'wave.theme';
let mq: MediaQueryList | null = null;

function resolve(mode: ThemeMode): boolean {
  if (mode === 'system') return !!mq?.matches;
  return mode === 'light';
}

function apply(light: boolean) {
  Object.assign(T, light ? LIGHT : DARK);
  theme.light = light;
  theme.epoch++;
  if (typeof document !== 'undefined') document.documentElement.dataset.theme = light ? 'light' : 'dark';
  setThemeVersion(v => v + 1);
}

/** Switch theme (persisted). */
export function setTheme(mode: ThemeMode) {
  try { localStorage.setItem(KEY, mode); } catch { /* private mode */ }
  setThemeModeSig(mode);
  apply(resolve(mode));
}

/** Read the saved theme and follow the OS when set to "system". Call once at startup. */
export function initTheme() {
  if (typeof window === 'undefined') return;
  mq = window.matchMedia?.('(prefers-color-scheme: light)') ?? null;
  let mode: ThemeMode = 'dark';
  try { const v = localStorage.getItem(KEY); if (v === 'light' || v === 'dark' || v === 'system') mode = v; } catch { /* ignore */ }
  setThemeModeSig(mode);
  apply(resolve(mode));
  mq?.addEventListener?.('change', () => { if (themeMode() === 'system') apply(resolve('system')); });
}

// ---- colour helpers for canvas ----
const rgbaCache = new Map<string, string>();
/** `hex` at opacity `a` (cached). */
export function rgba(hex: string, a: number): string {
  const k = hex + a;
  let v = rgbaCache.get(k);
  if (!v) {
    let h = hex.replace('#', '');
    if (h.length === 3) h = h.split('').map(c => c + c).join('');
    const n = parseInt(h.slice(0, 6), 16);
    v = `rgba(${(n >> 16) & 255},${(n >> 8) & 255},${n & 255},${a})`;
    rgbaCache.set(k, v);
  }
  return v;
}
