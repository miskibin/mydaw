/**
 * Entry point.
 *
 * The browser-only backend is registered here rather than imported by
 * `api/backend.ts`, so a production build inside Tauri never pulls the fixture
 * file into the bundle: `setMockFactory` is only reached when the Tauri
 * internals are absent.
 */

import '@fontsource-variable/geist';
import '@fontsource-variable/geist-mono';
import './styles/wave.css';
import './styles/landing.css';

import { render } from 'solid-js/web';

import { App } from './App';
import { isTauri, setMockFactory } from './api/backend';
import { ViewProvider } from './shell/view';
import { AppStateProvider } from './state';
import { ThemeProvider } from './theme/ThemeProvider';

async function start(): Promise<void> {
  if (!isTauri()) {
    const { MockBackend } = await import('./api/mock');
    setMockFactory(() => new MockBackend());
  }
  const root = document.getElementById('root');
  if (!root) throw new Error('index.html has no #root');
  render(
    () => (
      <ThemeProvider>
        <AppStateProvider>
          <ViewProvider>
            <App />
          </ViewProvider>
        </AppStateProvider>
      </ThemeProvider>
    ),
    root,
  );
}

void start();
