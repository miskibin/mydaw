import { onCleanup, Show, type JSX } from 'solid-js';
import { AppStateProvider, useAppState } from './state';
import { ThemeProvider } from './theme/ThemeProvider';
import { LayoutProvider, useLayout } from './shell/layout';
import { TransportBar } from './shell/TransportBar';
import { BrowserPane } from './shell/BrowserPane';
import { Arrangement } from './shell/Arrangement';
import { BottomPanel } from './shell/BottomPanel';
import { Inspector } from './shell/Inspector';
import { DevicePalette } from './shell/DevicePalette';
import { StatusBar } from './shell/StatusBar';

/**
 * WAVE Shell v2.
 *
 * Three providers, in the order their dependencies run: the theme writes the
 * custom properties everything else is drawn with, the app state opens the
 * backend and the meter stream, and the layout holds the view state that is
 * nobody's business but this shell's.
 */
export function App(): JSX.Element {
  return (
    <ThemeProvider>
      <AppStateProvider>
        <LayoutProvider>
          <Shell />
        </LayoutProvider>
      </AppStateProvider>
    </ThemeProvider>
  );
}

/** Whether a key event is somebody typing, and therefore not a shortcut. */
function isTyping(event: KeyboardEvent): boolean {
  const target = event.target as HTMLElement | null;
  if (!target) return false;
  if (target.isContentEditable) return true;
  const tag = target.tagName;
  return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT';
}

function Shell(): JSX.Element {
  const app = useAppState();
  const layout = useLayout();

  /**
   * The shell's own accelerators.
   *
   * Deliberately few, and all of them things with no other way in: Space is the
   * transport, F7 is the maximise the tab bar also offers, and Ctrl+K is the
   * palette. The project shortcuts (Ctrl+N/O/S) live in `state.tsx` beside the
   * commands they run, and the computer-keyboard piano is there too -- putting
   * either here would be a second window listener arguing with the first.
   */
  const onKeyDown = (event: KeyboardEvent) => {
    if (event.repeat) return;
    if (event.ctrlKey && !event.shiftKey && !event.altKey && event.code === 'KeyK') {
      event.preventDefault();
      app.ensureDevices();
      layout.setPaletteOpen(!layout.paletteOpen());
      return;
    }
    if (isTyping(event)) return;
    if (event.code === 'F7') {
      event.preventDefault();
      layout.toggleMaximised();
      return;
    }
    if (event.code === 'Space' && !event.ctrlKey && !event.altKey && !event.metaKey) {
      event.preventDefault();
      if (app.playing()) app.stop();
      else app.play();
    }
  };

  window.addEventListener('keydown', onKeyDown);
  onCleanup(() => window.removeEventListener('keydown', onKeyDown));

  return (
    <div class="wv-shell">
      <TransportBar />
      <div class="wv-body">
        <BrowserPane />
        <div class="wv-centre">
          <Show when={!layout.maximised()}>
            <Arrangement />
          </Show>
          <BottomPanel />
        </div>
        <Inspector />
        <Show when={layout.paletteOpen()}>
          <DevicePalette />
        </Show>
      </div>
      <StatusBar />
    </div>
  );
}
