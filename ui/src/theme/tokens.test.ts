import { readdirSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';

import { DEFAULT_ACCENT, DARK, LIGHT, CHANNEL_KIND_COLORS, cssVariables, type Palette } from './tokens';

const STYLES_DIR = join(dirname(fileURLToPath(import.meta.url)), '../styles');

function sheet(name: string): string {
  return readFileSync(join(STYLES_DIR, name), 'utf8');
}

/** WCAG 2 relative luminance of a `#rrggbb` colour. */
function luminance(hex: string): number {
  const channel = (start: number) => {
    const value = Number.parseInt(hex.slice(start, start + 2), 16) / 255;
    return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * channel(1) + 0.7152 * channel(3) + 0.0722 * channel(5);
}

/** Contrast ratio of two `#rrggbb` colours, ≥ 1. */
function contrast(a: string, b: string): number {
  const x = luminance(a);
  const y = luminance(b);
  const hi = Math.max(x, y);
  const lo = Math.min(x, y);
  return (hi + 0.05) / (lo + 0.05);
}

function expectAtLeast(actual: number, minimum: number, pair: string): void {
  expect(actual, `${pair} is ${actual.toFixed(2)}:1, need ≥ ${minimum}:1`).toBeGreaterThanOrEqual(minimum);
}

/**
 * Text the shell actually draws as labels (view titles, tabs, param names)
 * must meet WCAG AA. Decorative hints (`faint`) must still hit non-text
 * contrast so empty states and hotkeys remain readable.
 */
function expectReadableText(palette: Palette, name: string): void {
  expectAtLeast(contrast(palette.txt, palette.bg), 12, `${name} txt on bg`);
  expectAtLeast(contrast(palette.txt2, palette.bg), 7, `${name} txt2 on bg`);
  expectAtLeast(contrast(palette.dim, palette.bg), 4.5, `${name} dim on bg`);
  expectAtLeast(contrast(palette.dim2, palette.bg), 4.5, `${name} dim2 on bg`);
  expectAtLeast(contrast(palette.faint, palette.bg), 3, `${name} faint on bg`);
}

/** Panels and chips have to read as separate surfaces, not one grey slab. */
function expectReadableStructure(palette: Palette, name: string): void {
  expectAtLeast(contrast(palette.line, palette.panel), 1.8, `${name} line on panel`);
  expectAtLeast(contrast(palette.line2, palette.panel), 2.4, `${name} line2 on panel`);
  expectAtLeast(contrast(palette.chip, palette.bg), 1.35, `${name} chip on bg`);
  expectAtLeast(contrast(palette.panel2, palette.bg), 1.15, `${name} panel2 on bg`);
}

describe('palette contrast', () => {
  it('keeps dark-mode labels and chrome readable', () => {
    expectReadableText(DARK, 'dark');
    expectReadableStructure(DARK, 'dark');
  });

  it('keeps light-mode labels and chrome readable', () => {
    expectReadableText(LIGHT, 'light');
    expectReadableStructure(LIGHT, 'light');
  });

  it('puts ink, not white, on the default accent in both themes', () => {
    for (const theme of ['dark', 'light'] as const) {
      const vars = cssVariables(theme, DEFAULT_ACCENT);
      expectAtLeast(contrast(vars['--ink'] ?? '', vars['--acc'] ?? ''), 4.5, `${theme} ink on acc`);
    }
  });
});

describe('Tailwind colour bridge', () => {
  it('exposes every palette token and the shadcn-shaped aliases', () => {
    const css = sheet('tailwind.css');

    const extras = ['acc', 'accSoft'];
    for (const key of [...Object.keys(DARK), ...extras]) {
      expect(css, `missing Tailwind colour for --${key}`).toContain(`--color-${key}: var(--${key})`);
    }

    const aliases: Record<string, string> = {
      background: 'bg',
      foreground: 'txt',
      card: 'panel2',
      popover: 'panel2',
      primary: 'acc',
      'primary-foreground': 'ink',
      secondary: 'chip',
      'secondary-foreground': 'txt2',
      muted: 'sunk',
      'muted-foreground': 'dim2',
      destructive: 'danger',
      'destructive-foreground': 'ink',
      border: 'line',
      input: 'line',
      ring: 'acc',
    };
    for (const [alias, token] of Object.entries(aliases)) {
      expect(css, `missing alias ${alias}`).toContain(`--color-${alias}: var(--${token})`);
    }
  });
});

describe('theme config is the only colour source', () => {
  it('writes every palette key plus accent and meter onto :root', () => {
    const vars = cssVariables('dark', DEFAULT_ACCENT);
    for (const key of Object.keys(DARK)) {
      expect(vars[`--${key}`], `missing --${key}`).toBe(DARK[key as keyof Palette]);
    }
    expect(vars['--acc']).toBeDefined();
    expect(vars['--accSoft']).toBeDefined();
    expect(vars['--meter']).toContain(DARK.meterMid);
    expect(vars['--ch-drum']).toBe(CHANNEL_KIND_COLORS.drum);
    expect(vars['--ch-synth']).toBe(CHANNEL_KIND_COLORS.synth);
  });

  it('keeps hex and rgb out of stylesheets', () => {
    const hex = /#[0-9a-fA-F]{3,8}\b/;
    const rgb = /rgba?\(/;
    for (const name of readdirSync(STYLES_DIR).filter((file) => file.endsWith('.css'))) {
      const css = sheet(name).replace(/\/\*[\s\S]*?\*\//g, '');
      expect(css, `${name} has a hex literal`).not.toMatch(hex);
      expect(css, `${name} has an rgb() literal`).not.toMatch(rgb);
    }
  });
});
