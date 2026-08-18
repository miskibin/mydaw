import { For, Show, createResource, type JSX } from 'solid-js';
import { useAppState } from '../../state';
import type { PanelProps } from '../types';

/** Shown in place of an empty vendor or version field. */
const UNKNOWN = '—';

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/**
 * Lists the CLAP plugins the host found on this machine.
 *
 * The scan is deliberately lazy on both sides: the Rust host does not touch the
 * filesystem at boot (loading every installed plugin would make start-up as slow
 * and as fragile as the worst one installed), and this panel only asks for the
 * list when it is first mounted. `createResource` therefore doubles as the
 * loading state -- the first fetch really can take a second on a machine with a
 * large collection.
 */
export function ClapBrowserPanel(_props: PanelProps): JSX.Element {
  const app = useAppState();

  // Plain variable, not a signal: it only ever needs to be read *inside* the
  // fetcher, so making it reactive would add a dependency that re-runs nothing.
  // The first load serves the backend's cache; Rescan forces a fresh walk.
  let force = false;
  const [plugins, { refetch }] = createResource(() => app.backend.getClapPlugins(force));

  const count = () => plugins()?.length ?? 0;
  const rescan = () => {
    force = true;
    void refetch();
  };

  return (
    <div class="clap-browser">
      <Show when={plugins.error}>
        <p class="clap-browser__error">Scan failed: {errorMessage(plugins.error)}</p>
      </Show>

      <Show when={!plugins.loading} fallback={<p class="clap-browser__empty">Scanning…</p>}>
        <Show
          when={count() > 0}
          fallback={
            <p class="clap-browser__empty">
              No CLAP plugins found. MYDAW looks in the standard install locations and in{' '}
              <code>%CLAP_PATH%</code>.
            </p>
          }
        >
          <ul class="clap-list">
            <For each={plugins()}>
              {(plugin) => (
                <li class="clap-row">
                  <span class="clap-row__name" title={plugin.name || plugin.id}>
                    {plugin.name || plugin.id}
                  </span>
                  <span class="clap-row__vendor">{plugin.vendor || UNKNOWN}</span>
                  <span class="clap-row__version">{plugin.version || UNKNOWN}</span>
                  <span class="clap-row__path" title={plugin.path}>
                    {plugin.path}
                  </span>
                </li>
              )}
            </For>
          </ul>
        </Show>
      </Show>

      <footer class="clap-browser__footer">
        <span>
          {count()} {count() === 1 ? 'plugin' : 'plugins'}
        </span>
        <button class="link-button" type="button" disabled={plugins.loading} onClick={rescan}>
          Rescan
        </button>
      </footer>
    </div>
  );
}
