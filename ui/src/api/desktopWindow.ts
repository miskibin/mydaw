import { getCurrentWindow } from '@tauri-apps/api/window';

import { isTauri } from './backend';

/** Native chrome stays outside the engine contract and is absent in browser dev. */
export function desktopWindow() {
  if (!isTauri()) return null;
  const native = getCurrentWindow();
  return {
    minimize: () => native.minimize(),
    toggleMaximize: () => native.toggleMaximize(),
    close: () => native.close(),
    watchMaximized(onChange: (maximized: boolean) => void, onError: (error: unknown) => void) {
      let disposed = false;
      let unlisten: (() => void) | undefined;
      let revision = 0;
      const refresh = async () => {
        const request = ++revision;
        try {
          const maximized = await native.isMaximized();
          if (!disposed && request === revision) onChange(maximized);
        } catch (error) {
          if (!disposed) onError(error);
        }
      };
      void native
        .onResized(() => void refresh())
        .then(
          (stop) => {
            if (disposed) stop();
            else {
              unlisten = stop;
              void refresh();
            }
          },
          (error) => {
            if (!disposed) onError(error);
          },
        );
      return () => {
        disposed = true;
        unlisten?.();
      };
    },
  };
}
