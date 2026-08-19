import { For, Show, type JSX } from 'solid-js';
import { PanelFrame } from '../panels/PanelFrame';
import { isWorkspacePanel } from '../panels/registry';
import { useAppState } from '../state';

/**
 * Left dock. Renders every panel the kernel contributed, in manifest order;
 * unresolvable component keys still get a frame (see `PanelFrame`) so a stale
 * UI build is visible rather than silently lossy.
 *
 * Everything except the handful the shell routes to the workspace instead --
 * see `isWorkspacePanel`. The count shown is of what is actually here, because
 * a header saying "7" over six cards is a bug report waiting to happen.
 */
export function PanelDock(): JSX.Element {
  const app = useAppState();
  const docked = () => app.panels().filter((panel) => !isWorkspacePanel(panel.component));

  return (
    <aside class="dock" aria-label="Panel dock">
      <div class="dock__header">
        <span class="dock__title">Panels</span>
        <span class="dock__count">{docked().length}</span>
      </div>
      <div class="dock__scroll">
        <Show
          when={docked().length > 0}
          fallback={
            <p class="dock__empty">
              {app.kernelState() === 'error'
                ? 'No panel manifest — kernel unreachable.'
                : 'Waiting for panel manifest…'}
            </p>
          }
        >
          <For each={docked()}>{(manifest) => <PanelFrame manifest={manifest} />}</For>
        </Show>
      </div>
    </aside>
  );
}
