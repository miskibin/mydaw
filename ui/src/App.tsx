import type { JSX } from 'solid-js';
import { AppStateProvider } from './state';
import { TransportBar } from './shell/TransportBar';
import { PanelDock } from './shell/PanelDock';
import { MainArea } from './shell/MainArea';
import { StatusBar } from './shell/StatusBar';

/** Application shell: transport bar, left dock, workspace, status bar. */
export function App(): JSX.Element {
  return (
    <AppStateProvider>
      <div class="shell">
        <TransportBar />
        <div class="shell__body">
          <PanelDock />
          <MainArea />
        </div>
        <StatusBar />
      </div>
    </AppStateProvider>
  );
}
