import { For, Show, type JSX } from 'solid-js';
import { PanelFrame } from '../panels/PanelFrame';
import { useAppState } from '../state';

/**
 * Left dock. Renders every panel the kernel contributed, in manifest order;
 * unresolvable component keys still get a frame (see `PanelFrame`) so a stale
 * UI build is visible rather than silently lossy.
 */
export function PanelDock(): JSX.Element {
  const app = useAppState();

  return (
    <aside class="dock" aria-label="Panel dock">
      <div class="dock__header">
        <span class="dock__title">Panels</span>
        <span class="dock__count">{app.panels().length}</span>
      </div>
      <div class="dock__scroll">
        <Show
          when={app.panels().length > 0}
          fallback={
            <p class="dock__empty">
              {app.kernelState() === 'error'
                ? 'No panel manifest — kernel unreachable.'
                : 'Waiting for panel manifest…'}
            </p>
          }
        >
          <For each={app.panels()}>{(manifest) => <PanelFrame manifest={manifest} />}</For>
        </Show>
      </div>
    </aside>
  );
}
