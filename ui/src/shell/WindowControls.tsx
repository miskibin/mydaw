import { Show, createSignal, onCleanup } from 'solid-js';

import { desktopWindow } from '../api/desktopWindow';
import { useAppState } from '../state';
import { IconWindow } from '../ui/icons';

export function WindowControls() {
  const native = desktopWindow();
  const app = useAppState();
  const [maximized, setMaximized] = createSignal(false);
  const reportError = (error: unknown) => app.say('Window', String(error));
  if (native) onCleanup(native.watchMaximized(setMaximized, reportError));
  const run = (action: () => Promise<void>) => {
    void action().catch(reportError);
  };

  return (
    <Show when={native}>
      {(window) => (
        <div class="wv-window-controls" role="group" aria-label="Window controls">
          <button
            type="button"
            class="wv-window-button"
            aria-label="Minimize window"
            title="Minimize"
            onClick={() => run(window().minimize)}
          >
            <IconWindow kind="minimize" />
          </button>
          <button
            type="button"
            class="wv-window-button"
            aria-label={maximized() ? 'Restore window' : 'Maximize window'}
            title={maximized() ? 'Restore' : 'Maximize'}
            onClick={() => run(window().toggleMaximize)}
          >
            <IconWindow kind={maximized() ? 'restore' : 'maximize'} />
          </button>
          <button
            type="button"
            class="wv-window-button wv-window-button--close"
            aria-label="Close window"
            title="Close"
            onClick={() => run(window().close)}
          >
            <IconWindow kind="close" />
          </button>
        </div>
      )}
    </Show>
  );
}
