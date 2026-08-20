import { For, Show } from 'solid-js';

import { useTheme } from '../theme/ThemeProvider';
import { THEME_OPTIONS, accentFor, paletteFor, type ThemeName } from '../theme/tokens';
import { useView } from '../shell/view';
import '../styles/settings.css';

const FONTS = [
  { name: 'geist' as const, label: 'Geist', note: 'Domyślny, neutralny' },
  { name: 'inter' as const, label: 'Inter', note: 'Czytelny i kompaktowy' },
  { name: 'mono' as const, label: 'Geist Mono', note: 'Techniczny, równy rytm' },
];

export function Settings() {
  const theme = useTheme();
  const view = useView();

  return (
    <main class="wv-settings">
      <div class="wv-settings__header">
        <div>
          <p class="wv-settings__eyebrow">WAVE / PREFERENCES</p>
          <h1>Settings</h1>
          <p class="wv-settings__lede">Dostosuj wygląd środowiska pracy. Zmiany zapisują się automatycznie.</p>
        </div>
        <button type="button" class="wv-settings__close" onClick={() => view.go('rack')}>Back to studio</button>
      </div>

      <section class="wv-settings__section">
        <div class="wv-settings__section-head"><div><h2>Color theme</h2><p>Wybierz charakter powierzchni roboczej.</p></div><span class="wv-settings__value">{theme.name().toUpperCase()}</span></div>
        <div class="wv-theme-grid">
          <For each={THEME_OPTIONS}>{(option) => (
            <button type="button" class="wv-theme-card" classList={{ 'is-active': theme.name() === option.name }} aria-pressed={theme.name() === option.name} onClick={() => theme.setTheme(option.name as ThemeName)}>
              <span class="wv-theme-card__preview" style={{ background: paletteFor(option.name).sunk }}>
                <i style={{ background: accentFor(theme.accent(), option.name) }} />
                <i style={{ background: paletteFor(option.name).line2 }} />
                <i style={{ background: paletteFor(option.name).live }} />
              </span>
              <span class="wv-theme-card__copy"><strong>{option.label}</strong><small>{option.description}</small></span>
              <Show when={theme.name() === option.name}><span class="wv-theme-card__check">●</span></Show>
            </button>
          )}</For>
        </div>
      </section>

      <section class="wv-settings__section">
        <div class="wv-settings__section-head"><div><h2>Interface font</h2><p>Font wpływa na wszystkie etykiety i kontrolki WAVE.</p></div><span class="wv-settings__value">{theme.font() === 'mono' ? 'MONO' : theme.font().toUpperCase()}</span></div>
        <div class="wv-font-list">
          <For each={FONTS}>{(font) => <button type="button" class="wv-font-row" classList={{ 'is-active': theme.font() === font.name }} onClick={() => theme.setFont(font.name)}><span class={`wv-font-row__sample wv-font-row__sample--${font.name}`}>Aa</span><span><strong>{font.label}</strong><small>{font.note}</small></span><Show when={theme.font() === font.name}><span class="wv-font-row__check">ACTIVE</span></Show></button>}</For>
        </div>
      </section>
    </main>
  );
}

export default Settings;
