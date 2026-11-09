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

// ---- extra themes: generated from a hue/saturation for surfaces and an accent ----
function hsl(h: number, sat: number, l: number): string {
  const s = sat / 100, L = l / 100, k = (n: number) => (n + h / 30) % 12, a = s * Math.min(L, 1 - L);
  const f = (n: number) => Math.round(255 * (L - a * Math.max(-1, Math.min(k(n) - 3, 9 - k(n), 1)))).toString(16).padStart(2, '0');
  return '#' + f(0) + f(8) + f(4);
}

/** Extra CSS variables that are not canvas tokens. */
type Css = Record<string, string>;
export interface ThemeDef { id: string; name: string; light: boolean; t: Tokens; css: Css; swatch: [string, string, string] }

function darkTheme(id: string, name: string, h: number, s: number, accent: [string, string], o: { lift?: number; fgS?: number; warm?: [string, string, string] } = {}): ThemeDef {
  const d = o.lift ?? 0, L = (l: number) => hsl(h, s, Math.max(0, l + d)), F = (l: number) => hsl(h, o.fgS ?? Math.min(s, 14), l);
  const [green, amber, red] = o.warm ?? [DARK.green, DARK.amber, DARK.red];
  const t: Tokens = {
    ...DARK,
    bg0: L(4.5), bg1: L(6.5), bg2: L(8.4), bg3: L(9.8), bg4: L(10.6), bg5: L(11), bg6: L(13), bg7: L(14.8),
    line: L(17.6), line2: L(20.6), line3: L(21.8),
    fg: F(86), fgHi: F(96), fg2: F(74), fg3: F(64), fg4: F(53), fg5: F(40), fg6: F(32),
    blue: accent[0], blueHi: accent[1], green, amber, red, onBg: L(24),
    viz: L(5.5), vizPanel: L(7.5), vizGrid: L(11), vizGrid2: L(15), vizAxis: L(19), vizLabel: F(32),
    grid: L(8.4), gridBar: L(17), gridBeat: L(12.5), laneAlt: L(9.5), rulerBg: L(9.8), rulerText: F(53), clipBody: L(7.5),
    keyB: L(15), clipBase: L(7.5), laneArm: hsl(330, 18, 8.5 + d),
  };
  const css: Css = {
    '--sel-bg': L(17), '--sel-bg2': L(15), '--sel-ring': L(28), '--drag-bg': L(21), '--mixer-bg': L(7.6), '--dialog-bg': L(12.5),
    '--arm-bg': hsl(330, 14, 11 + d), '--knob-track': L(20.6), '--knob-cap': L(20.6), '--viz': t.viz,
  };
  return { id, name, light: false, t, css, swatch: [t.bg2, t.bg6, accent[0]] };
}

function lightTheme(id: string, name: string, h: number, s: number, accent: [string, string]): ThemeDef {
  const L = (l: number) => hsl(h, s, l), F = (l: number) => hsl(h, Math.min(s, 16), l);
  const t: Tokens = {
    ...LIGHT,
    bg0: L(86.5), bg1: L(94), bg2: L(97), bg3: L(95.5), bg4: L(97.5), bg5: L(93), bg6: L(90.5), bg7: L(99.5),
    line: L(85), line2: L(80.5), line3: L(78),
    fg: F(19), fgHi: F(7), fg2: F(27), fg3: F(36), fg4: F(45), fg5: F(56), fg6: F(64),
    blue: accent[0], blueHi: accent[1], onBg: hsl(h, 40, 90),
    viz: L(98.8), vizPanel: L(96.5), vizGrid: L(93.5), vizGrid2: L(89), vizAxis: L(81), grid: L(97), gridBar: L(83), gridBeat: L(91),
    laneAlt: L(94.5), rulerBg: L(93.5), clipBody: L(99.5), clipBase: L(99.5),
  };
  const css: Css = { '--sel-bg': hsl(h, 45, 90), '--sel-bg2': hsl(h, 40, 92), '--sel-ring': hsl(h, 45, 76), '--drag-bg': hsl(h, 45, 86), '--mixer-bg': L(91.5), '--dialog-bg': L(99.5), '--viz': t.viz };
  return { id, name, light: true, t, css, swatch: [t.bg2, t.bg6, accent[0]] };
}

/** Every theme the settings offer. Ids are saved on this computer, never rename them. */
export const THEMES: ThemeDef[] = [
  { id: 'dark', name: 'Grafit', light: false, t: DARK, css: {}, swatch: [DARK.bg2, DARK.bg6, DARK.blue] },
  darkTheme('midnight', 'Północ', 220, 34, ['#5b8cff', '#9db8ff'], { lift: 1 }),
  darkTheme('violet', 'Fiolet', 265, 22, ['#a07cf6', '#c6b0ff'], { lift: 1 }),
  darkTheme('forest', 'Las', 155, 16, ['#3fbf8a', '#86e0b8'], { warm: ['#5cc98f', '#e0a44a', '#e05263'] }),
  darkTheme('studio', 'Studio', 28, 14, ['#f0973f', '#ffc285'], { lift: 1, warm: ['#7fc06a', '#e8b04a', '#e05a4f'] }),
  darkTheme('rose', 'Róż', 340, 14, ['#f06292', '#ffa6c4']),
  darkTheme('oled', 'Czerń (OLED)', 220, 6, ['#4f8ef7', '#8fb6ff'], { lift: -4, fgS: 6 }),
  { id: 'light', name: 'Jasny', light: true, t: LIGHT, css: {}, swatch: [LIGHT.bg2, LIGHT.bg6, LIGHT.blue] },
  lightTheme('paper', 'Papier', 38, 30, ['#c2651b', '#a9530f']),
  lightTheme('sky', 'Błękit', 210, 35, ['#1f6fe0', '#1858b8']),
];

/** Current tokens (mutated in place on theme change; read at draw time). */
export const T: Tokens = { ...DARK };
/** A theme id from THEMES, or 'system' (Grafit / Jasny following the OS). */
export type ThemeMode = string;
export const theme = { epoch: 0, light: false };
const [themeVersion, setThemeVersion] = createSignal(0);
const [themeMode, setThemeModeSig] = createSignal<ThemeMode>('dark');
export { themeMode, themeVersion };

const KEY = 'wave.theme';
let mq: MediaQueryList | null = null;
let cssSet: string[] = [];

function resolve(mode: ThemeMode): ThemeDef {
  const id = mode === 'system' ? (mq?.matches ? 'light' : 'dark') : mode;
  return THEMES.find(t => t.id === id) ?? THEMES[0];
}

const VAR_TOKENS = ['bg0', 'bg1', 'bg2', 'bg3', 'bg4', 'bg5', 'bg6', 'bg7', 'line', 'line2', 'line3', 'fg', 'fgHi', 'fg2', 'fg3', 'fg4', 'fg5', 'fg6', 'blue', 'blueHi', 'green', 'amber', 'red', 'onBg'] as const;

function apply(def: ThemeDef) {
  Object.assign(T, def.t);
  theme.light = def.light;
  theme.epoch++;
  if (typeof document !== 'undefined') {
    const root = document.documentElement;
    root.dataset.theme = def.light ? 'light' : 'dark';
    for (const k of cssSet) root.style.removeProperty(k);
    cssSet = [];
    if (def.id !== 'dark' && def.id !== 'light') {
      const vars: Css = { ...def.css };
      for (const k of VAR_TOKENS) vars['--' + k.replace(/[A-Z]/g, m => '-' + m.toLowerCase())] = def.t[k];
      for (const [k, v] of Object.entries(vars)) { root.style.setProperty(k, v); cssSet.push(k); }
    }
  }
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
  try { const v = localStorage.getItem(KEY); if (v && (v === 'system' || THEMES.some(t => t.id === v))) mode = v; } catch { /* ignore */ }
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
