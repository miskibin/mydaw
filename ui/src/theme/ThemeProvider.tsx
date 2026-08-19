import {
  createContext,
  createEffect,
  createSignal,
  useContext,
  type Accessor,
  type JSX,
} from 'solid-js';
import {
  accentFor,
  cssVariables,
  DEFAULT_ACCENT,
  paletteFor,
  type Palette,
  type ThemeName,
} from './tokens';

/**
 * Theme state, and the one place the custom properties are written.
 *
 * The variables go on `document.documentElement` rather than on a wrapper
 * element, for two reasons: the piano-roll canvas reads them with
 * `getComputedStyle` from wherever it happens to be mounted, and a popover that
 * one day escapes to a portal still inherits the palette.
 *
 * The choice is remembered in `localStorage`. Deliberately not `prefers-color-
 * scheme`: a DAW is a dark-room application whose theme is a working decision,
 * not a system preference, and being flipped to light by the OS at sunrise
 * mid-session would be a surprise rather than a service.
 */
export interface Theme {
  readonly name: Accessor<ThemeName>;
  readonly palette: Accessor<Palette>;
  /** The accent as configured, before the light-mode darkening. */
  readonly accent: Accessor<string>;
  /** The accent as drawn, i.e. what `--acc` holds. */
  readonly accentDrawn: Accessor<string>;
  readonly isDark: Accessor<boolean>;
  toggle(): void;
  setTheme(name: ThemeName): void;
  setAccent(accent: string): void;
}

const ThemeContext = createContext<Theme>();

const THEME_KEY = 'mydaw.theme';
const ACCENT_KEY = 'mydaw.accent';

/** What was stored last session, or the default when nothing legible is there. */
function storedTheme(): ThemeName {
  try {
    return localStorage.getItem(THEME_KEY) === 'light' ? 'light' : 'dark';
  } catch {
    // Private-mode browsers throw on `localStorage`; a theme is not worth a
    // blank screen.
    return 'dark';
  }
}

function storedAccent(): string {
  try {
    const value = localStorage.getItem(ACCENT_KEY);
    return value && /^#[0-9a-f]{6}$/i.test(value) ? value : DEFAULT_ACCENT;
  } catch {
    return DEFAULT_ACCENT;
  }
}

function remember(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Nothing to do and nothing worth saying: the session still works.
  }
}

export function ThemeProvider(props: { children: JSX.Element }): JSX.Element {
  const [name, setName] = createSignal<ThemeName>(storedTheme());
  const [accent, setAccentSignal] = createSignal<string>(storedAccent());

  createEffect(() => {
    const theme = name();
    const vars = cssVariables(theme, accent());
    const root = document.documentElement;
    for (const [property, value] of Object.entries(vars)) {
      root.style.setProperty(property, value);
    }
    // `data-theme` is what the stylesheet keys its handful of non-token rules
    // on -- scrollbar thumbs and the one shadow that differs between themes.
    root.dataset['theme'] = theme;
    root.style.colorScheme = theme;
  });

  const theme: Theme = {
    name,
    palette: () => paletteFor(name()),
    accent,
    accentDrawn: () => accentFor(accent(), name()),
    isDark: () => name() === 'dark',
    toggle() {
      setName((current) => {
        const next: ThemeName = current === 'dark' ? 'light' : 'dark';
        remember(THEME_KEY, next);
        return next;
      });
    },
    setTheme(next) {
      remember(THEME_KEY, next);
      setName(next);
    },
    setAccent(next) {
      remember(ACCENT_KEY, next);
      setAccentSignal(next);
    },
  };

  return <ThemeContext.Provider value={theme}>{props.children}</ThemeContext.Provider>;
}

export function useTheme(): Theme {
  const theme = useContext(ThemeContext);
  if (!theme) throw new Error('useTheme must be used inside <ThemeProvider>');
  return theme;
}
