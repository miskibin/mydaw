/**
 * The WAVE Shell palettes.
 *
 * A theme is this table. CSS, canvas and Tailwind all read the same keys as
 * custom properties on `:root`, so swapping a theme is swapping one object --
 * not hunting hex through stylesheets. shadcn's React widgets do not fit a
 * Solid DAW; this is the part of shadcn that does: a token contract.
 *
 * Solid-free on purpose: a table and two colour functions, which is also what
 * the piano-roll canvas paints with.
 */

/** One complete palette. Keys are the CSS custom property names, minus `--`. */
export interface Palette {
  bg: string;
  top: string;
  panel: string;
  panel2: string;
  head: string;
  line: string;
  line2: string;
  txt: string;
  txt2: string;
  dim: string;
  dim2: string;
  faint: string;
  sunk: string;
  chip: string;
  lane: string;
  grid1: string;
  grid2: string;
  /** Text drawn *on* an accent or a record pill. */
  ink: string;
  live: string;
  solo: string;
  rec: string;
  danger: string;
  /** Text drawn on `--live` / `--solo`. */
  liveInk: string;
  soloInk: string;
  playhead: string;
  paletteShadow: string;
  scrim: string;
  stepbeat: string;
  shade: string;
  grip: string;
  meterHi: string;
  meterMid: string;
  meterLo: string;
  eqHp: string;
  eqLow: string;
  eqBell2: string;
  eqHigh: string;
  ivory: string;
  ebony: string;
  ivoryEdge: string;
  sheen: string;
  keyLane: string;
  noteInk: string;
  thumbMark: string;
  noteStroke: string;
}

/**
 * Colours that mean the same thing in every theme: a record light is red in a
 * light room too. Spread into each palette so a custom theme can still override
 * them, but the built-in pair stays in lockstep.
 */
const SEMANTIC = {
  live: '#4ec08a',
  solo: '#e0a44a',
  rec: '#e05263',
  danger: '#e8607a',
  liveInk: '#07231a',
  soloInk: '#12151a',
  meterHi: '#f0c25a',
  meterMid: '#4ec08a',
  meterLo: '#3f9e72',
  eqHp: '#7aa2f7',
  eqLow: '#9ece6a',
  eqBell2: '#e0af68',
  eqHigh: '#bb9af7',
} as const;

export const DARK: Palette = {
  ...SEMANTIC,
  bg: '#0e1013',
  top: '#171b21',
  panel: '#161a21',
  panel2: '#1e242d',
  head: '#252b34',
  line: '#4a5360',
  line2: '#5c6573',
  txt: '#f2f4f7',
  txt2: '#d5dae2',
  dim: '#b4bac4',
  dim2: '#9aa2ad',
  faint: '#7d8592',
  sunk: '#0a0c0f',
  chip: '#2c333d',
  lane: '#12161c',
  grid1: '#1c222a',
  grid2: '#2e3641',
  ink: '#0b1220',
  playhead: '#eef1f5',
  paletteShadow: 'rgb(0 0 0 / 45%)',
  scrim: 'rgb(0 0 0 / 35%)',
  stepbeat: '#2a313b',
  shade: 'rgb(0 0 0 / 25%)',
  grip: 'rgb(0 0 0 / 16%)',
  ivory: '#e8ebf1',
  ebony: '#12151a',
  ivoryEdge: 'rgb(20 24 30 / 28%)',
  sheen: 'rgb(255 255 255 / 18%)',
  keyLane: 'rgb(0 0 0 / 28%)',
  noteInk: 'rgb(6 10 14 / 85%)',
  thumbMark: 'rgb(255 255 255 / 22%)',
  noteStroke: 'rgb(0 0 0 / 45%)',
};

export const LIGHT: Palette = {
  ...SEMANTIC,
  bg: '#e2e5eb',
  top: '#ffffff',
  panel: '#f2f3f6',
  panel2: '#ffffff',
  head: '#e6e8ed',
  line: '#9aa1ab',
  line2: '#7a828e',
  txt: '#101318',
  txt2: '#2c313a',
  dim: '#4a5160',
  dim2: '#565e6c',
  faint: '#6c7480',
  sunk: '#d5d8de',
  chip: '#c0c5ce',
  lane: '#eaecf0',
  grid1: '#d8dce3',
  grid2: '#c0c6d0',
  ink: '#ffffff',
  playhead: '#1c2028',
  paletteShadow: 'rgb(0 0 0 / 18%)',
  scrim: 'rgb(0 0 0 / 35%)',
  stepbeat: '#c5cad3',
  shade: 'rgb(0 0 0 / 25%)',
  grip: 'rgb(0 0 0 / 16%)',
  ivory: '#fbfcfe',
  ebony: '#1b1e24',
  ivoryEdge: 'rgb(20 24 30 / 16%)',
  sheen: 'rgb(255 255 255 / 18%)',
  keyLane: 'rgb(0 0 0 / 8%)',
  noteInk: 'rgb(255 255 255 / 95%)',
  thumbMark: 'rgb(0 0 0 / 28%)',
  noteStroke: 'rgb(0 0 0 / 45%)',
};

/** The two shipped themes, by name. A third theme is another `Palette` here. */
export type ThemeName = 'dark' | 'light' | 'paper' | 'ocean' | 'ember' | 'mono';

export const THEME_OPTIONS: readonly { name: ThemeName; label: string; description: string }[] = [
  { name: 'dark', label: 'Studio Dark', description: 'Neutralny, klasyczny interfejs DAW' },
  { name: 'light', label: 'Studio Light', description: 'Jasny interfejs do pracy w dzień' },
  { name: 'paper', label: 'Paper', description: 'Minimalistyczna biel z subtelnym kontrastem' },
  { name: 'ocean', label: 'Ocean', description: 'Chłodny granat z turkusowym akcentem' },
  { name: 'ember', label: 'Ember', description: 'Ciepły, kontrastowy tryb nocny' },
  { name: 'mono', label: 'Mono', description: 'Surowa paleta do skupionej pracy' },
];

const PAPER: Palette = { ...LIGHT, bg: '#ffffff', top: '#ffffff', panel: '#ffffff', panel2: '#f7f8fa', head: '#f1f3f5', line: '#e2e5e9', line2: '#cbd1d8', txt: '#15181c', txt2: '#363c44', dim: '#68717c', dim2: '#88919c', faint: '#aeb6bf', sunk: '#f5f6f7', chip: '#eef1f4', lane: '#fafbfc', grid1: '#edf0f2', grid2: '#dfe4e8' };
const OCEAN: Palette = { ...DARK, bg: '#08141b', top: '#0c202a', panel: '#0d242e', panel2: '#12313b', head: '#19404c', line: '#2d6470', line2: '#438793', txt: '#e5fbfb', txt2: '#c2e4e5', dim: '#91c3c7', dim2: '#6ca3aa', faint: '#4f828a', sunk: '#061016', chip: '#1a4652', lane: '#0a1c24', grid1: '#12313a', grid2: '#1d4a55' };
const EMBER: Palette = { ...DARK, bg: '#17100f', top: '#251715', panel: '#241715', panel2: '#34201c', head: '#482a21', line: '#765044', line2: '#996452', txt: '#fff1e8', txt2: '#ead2c3', dim: '#c59f8c', dim2: '#a47d6c', faint: '#805f52', sunk: '#100908', chip: '#533126', lane: '#1d1210', grid1: '#33201b', grid2: '#563227' };
const MONO: Palette = { ...DARK, bg: '#111111', top: '#1a1a1a', panel: '#191919', panel2: '#242424', head: '#303030', line: '#686868', line2: '#858585', txt: '#f1f1f1', txt2: '#d0d0d0', dim: '#a8a8a8', dim2: '#858585', faint: '#6b6b6b', sunk: '#090909', chip: '#383838', lane: '#151515', grid1: '#222222', grid2: '#3b3b3b' };

/** How a named theme mixes and tints the accent. */
interface ThemeMeta {
  accSoft: number;
  accentMixAmount: number;
  accentMix: string;
}

const THEME_META: Record<ThemeName, ThemeMeta> = {
  dark: { accSoft: 0.26, accentMixAmount: 0, accentMix: '#0b1c3d' },
  light: { accSoft: 0.16, accentMixAmount: 0.28, accentMix: '#0b1c3d' },
  paper: { accSoft: 0.12, accentMixAmount: 0.1, accentMix: '#eef1f4' },
  ocean: { accSoft: 0.24, accentMixAmount: 0, accentMix: '#062b35' },
  ember: { accSoft: 0.24, accentMixAmount: 0, accentMix: '#35150c' },
  mono: { accSoft: 0.22, accentMixAmount: 0, accentMix: '#242424' },
};

/** The accent the app opens on. Same value the mock defaults its colour prop to. */
export const DEFAULT_ACCENT = '#4f8ef7';

/**
 * The accents offered in the transport's picker.
 *
 * The mock exposes accent as a free colour prop; a DAW wants a short list, and
 * these are the four the mock itself suggests.
 */
export const ACCENT_CHOICES = ['#4f8ef7', '#2f6fe0', '#47b8a8', '#7c8cf8'] as const;

/** The palette for a theme. */
export function paletteFor(theme: ThemeName): Palette {
  switch (theme) {
    case 'light': return LIGHT;
    case 'paper': return PAPER;
    case 'ocean': return OCEAN;
    case 'ember': return EMBER;
    case 'mono': return MONO;
    default: return DARK;
  }
}

/** Blend two `#rrggbb` colours, `t` of the way from `a` to `b`. */
export function mix(a: string, b: string, t: number): string {
  const parse = (hex: string) => [1, 3, 5].map((i) => Number.parseInt(hex.slice(i, i + 2), 16));
  const x = parse(a);
  const y = parse(b);
  const channel = (i: number) =>
    Math.round((x[i] ?? 0) + ((y[i] ?? 0) - (x[i] ?? 0)) * t)
      .toString(16)
      .padStart(2, '0');
  return `#${channel(0)}${channel(1)}${channel(2)}`;
}

/** A `#rrggbb` colour as `rgba(...)` at `a` opacity. */
export function alpha(hex: string, a: number): string {
  const parts = [1, 3, 5].map((i) => Number.parseInt(hex.slice(i, i + 2), 16));
  return `rgba(${parts.join(',')},${a})`;
}

/**
 * A wash that works on both `#rrggbb` and `var(--token)`, for inline styles.
 */
export function wash(color: string, amount: number): string {
  return `color-mix(in srgb, ${color} ${Math.round(amount * 100)}%, transparent)`;
}

/**
 * The accent as drawn in `theme`.
 *
 * Light mode darkens it towards navy: the same blue that reads as bright on a
 * near-black canvas is illegible as small text on a pale one.
 */
export function accentFor(accent: string, theme: ThemeName): string {
  const meta = THEME_META[theme];
  return meta.accentMixAmount > 0 ? mix(accent, meta.accentMix, meta.accentMixAmount) : accent;
}

/**
 * Every custom property the shell reads, for one theme and accent.
 *
 * Returned as a plain record so both the provider (which writes it to
 * `document.documentElement`) and any test can see the whole set at once.
 */
export function cssVariables(theme: ThemeName, accent: string): Record<string, string> {
  const palette = paletteFor(theme);
  const meta = THEME_META[theme];
  const acc = accentFor(accent, theme);
  const vars: Record<string, string> = {};
  for (const [name, value] of Object.entries(palette)) vars[`--${name}`] = value;
  vars['--acc'] = acc;
  vars['--accSoft'] = alpha(acc, meta.accSoft);
  vars['--meter'] = `linear-gradient(180deg,${palette.meterHi},${palette.meterMid} 26%,${palette.meterLo})`;
  for (const [kind, color] of Object.entries(CHANNEL_KIND_COLORS)) {
    vars[`--ch-${kind}`] = color;
  }
  return vars;
}

/**
 * One colour per *kind* of rack channel, so a channel keeps its colour in the
 * mixer, the playlist marks and the piano roll, not just next to the name.
 *
 * Distinct from {@link TRACK_COLORS}, which still paints patterns, buses and
 * audio files that have no instrument kind.
 */
export const CHANNEL_KIND_COLORS = {
  plugin: DEFAULT_ACCENT,
  sample: SEMANTIC.live,
  synth: '#7c8cf8',
  tone: '#8b93a1',
} as const;

/**
 * The per-track colours, in the order tracks are created.
 *
 * The engine has no opinion about what colour a track is -- `TrackDto` carries a
 * name and a source, not a swatch -- so the shell assigns one by index from the
 * mock's own ramp. Keyed on the track's stable `id` rather than its position, so
 * removing a track does not repaint the ones after it.
 */
export const TRACK_COLORS = [
  '#4f8ef7',
  '#5aa9e6',
  '#69c3d6',
  '#47b8a8',
  '#5fbf8a',
  '#7c8cf8',
  '#e0a44a',
  '#9a8cf0',
  '#d9678f',
  '#8b93a1',
] as const;

/** The colour for a track with this stable id. */
export function trackColor(id: number): string {
  const index = ((id % TRACK_COLORS.length) + TRACK_COLORS.length) % TRACK_COLORS.length;
  return TRACK_COLORS[index] ?? DEFAULT_ACCENT;
}

/** The meter gradient both the strips and the master use. */
export const METER_GRADIENT = `linear-gradient(180deg,${SEMANTIC.meterHi},${SEMANTIC.meterMid} 26%,${SEMANTIC.meterLo})`;

/** Solo yellow and the arm/record red, which are not part of either surface ramp. */
export const SOLO_COLOR = SEMANTIC.solo;
export const REC_COLOR = SEMANTIC.rec;
/** The "this is live" green: the play button, an instrument's dot. */
export const LIVE_COLOR = SEMANTIC.live;
