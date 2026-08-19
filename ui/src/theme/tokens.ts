/**
 * The WAVE Shell v2 palettes.
 *
 * Lifted verbatim from `.design/wave-shell-v2.html` (the `DARK` and `LIGHT`
 * consts). Every colour the shell draws with comes from here and reaches the DOM
 * as a CSS custom property, so a theme swap is one write to `:root` rather than
 * a re-render of every component -- which is what keeps the mixer's 30 Hz meters
 * out of the theme's way.
 *
 * Solid-free on purpose: this is a table and two colour functions, and it is
 * what the piano-roll canvas reads to paint itself in the same palette the DOM
 * uses.
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
  /** Text drawn *on* an accent or a solo/mute pill: near-black in dark, white in light. */
  ink: string;
}

export const DARK: Palette = {
  bg: '#0e1013',
  top: '#15181c',
  panel: '#12151a',
  panel2: '#171b21',
  head: '#1b1f26',
  line: '#23272f',
  line2: '#2d323b',
  txt: '#e7e9ec',
  txt2: '#c3c8d0',
  dim: '#8b929c',
  dim2: '#646b76',
  faint: '#4b525c',
  sunk: '#0b0d10',
  chip: '#1d2128',
  lane: '#101318',
  grid1: '#171b21',
  grid2: '#1f242c',
  ink: '#0b1220',
};

export const LIGHT: Palette = {
  bg: '#eef0f3',
  top: '#ffffff',
  panel: '#f6f7f9',
  panel2: '#ffffff',
  head: '#eef0f3',
  line: '#dfe2e7',
  line2: '#cbd0d8',
  txt: '#14171c',
  txt2: '#3b414b',
  dim: '#6a7280',
  dim2: '#8a919b',
  faint: '#a8aeb8',
  sunk: '#eaecf0',
  chip: '#e5e8ed',
  lane: '#f2f4f7',
  grid1: '#e6e9ee',
  grid2: '#d8dce3',
  ink: '#ffffff',
};

/** The two themes, by name. */
export type ThemeName = 'dark' | 'light';

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
  return theme === 'light' ? LIGHT : DARK;
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
 * The accent as drawn in `theme`.
 *
 * Light mode darkens it towards navy, exactly as the mock's `acc()` does: the
 * same blue that reads as bright on `#0e1013` is illegible as small text on
 * `#eef0f3`, and a single accent value for both themes is how a light theme ends
 * up with unreadable labels.
 */
export function accentFor(accent: string, theme: ThemeName): string {
  return theme === 'light' ? mix(accent, '#0b1c3d', 0.28) : accent;
}

/**
 * Every custom property the shell reads, for one theme and accent.
 *
 * Returned as a plain record so both the provider (which writes it to
 * `document.documentElement`) and any test can see the whole set at once.
 */
export function cssVariables(theme: ThemeName, accent: string): Record<string, string> {
  const palette = paletteFor(theme);
  const acc = accentFor(accent, theme);
  const vars: Record<string, string> = {};
  for (const [name, value] of Object.entries(palette)) vars[`--${name}`] = value;
  vars['--acc'] = acc;
  vars['--accSoft'] = alpha(acc, 0.14);
  return vars;
}

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
export const METER_GRADIENT = 'linear-gradient(180deg,#f0c25a,#4ec08a 26%,#3f9e72)';

/** Solo yellow and the arm/record red, which are not part of either palette. */
export const SOLO_COLOR = '#e0a44a';
export const REC_COLOR = '#e05263';
/** The "this is live" green: the play button, an instrument's dot. */
export const LIVE_COLOR = '#4ec08a';
