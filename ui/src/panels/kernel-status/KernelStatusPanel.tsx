import { For, Show, type JSX } from 'solid-js';
import type { PluginState } from '../../api/types';
import { useAppState } from '../../state';
import type { PanelProps } from '../types';

const STATE_LABEL: Record<PluginState, string> = {
  registered: 'Registered',
  started: 'Started',
  stopped: 'Stopped',
};

/** Lists the kernel plugin registry with a colored dot per lifecycle state. */
export function KernelStatusPanel(_props: PanelProps): JSX.Element {
  const app = useAppState();

  return (
    <div class="kernel-status">
      <Show when={app.kernelState() === 'error'}>
        <p class="kernel-status__error">
          Snapshot failed: {app.kernelError() ?? 'unknown error'}
        </p>
      </Show>

      <Show
        when={app.plugins().length > 0}
        fallback={<p class="kernel-status__empty">No plugins reported.</p>}
      >
        <ul class="plugin-list">
          <For each={app.plugins()}>
            {(plugin) => (
              <li class="plugin-row">
                <span class={`state-dot state-dot--${plugin.state}`} aria-hidden="true" />
                <span class="plugin-row__name" title={plugin.name}>
                  {plugin.name}
                </span>
                <span class="plugin-row__id">{plugin.id}</span>
                <span class={`plugin-row__state plugin-row__state--${plugin.state}`}>
                  {STATE_LABEL[plugin.state]}
                </span>
              </li>
            )}
          </For>
        </ul>
      </Show>

      <footer class="kernel-status__footer">
        <span>{app.plugins().length} plugins</span>
        <button class="link-button" type="button" onClick={() => app.refresh()}>
          Refresh
        </button>
      </footer>
    </div>
  );
}
