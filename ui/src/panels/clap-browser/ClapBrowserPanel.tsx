import { For, Show, createResource, type JSX } from 'solid-js';
import { useAppState } from '../../state';
import type { ClapPluginInfoDto, ClapPluginKind } from '../../api/types';
import type { PanelProps } from '../types';

/** Shown in place of an empty vendor or version field. */
const UNKNOWN = '—';

/**
 * Reading order for the list: the two kinds MYDAW can actually place come
 * first, and in the order the mixer offers them (a track's source, then the
 * master's insert). Everything MYDAW has nowhere to put sinks to the bottom
 * rather than being hidden -- "installed but unusable here" is worth knowing.
 */
const KIND_ORDER: Record<ClapPluginKind, number> = {
  instrument: 0,
  'audio-effect': 1,
  'note-effect': 2,
  other: 3,
};

/** Badge text. Short enough to sit in a column beside the name. */
const KIND_LABEL: Record<ClapPluginKind, string> = {
  instrument: 'Instrument',
  'audio-effect': 'Audio FX',
  'note-effect': 'Note FX',
  other: 'Other',
};

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/**
 * The evidence behind the badge. A bundle may declare no features at all, which
 * is legal and is exactly why it ends up classified as `other` -- saying so is
 * more useful than an empty tooltip.
 */
function featuresTitle(plugin: ClapPluginInfoDto): string {
  return plugin.features.length > 0
    ? `${KIND_LABEL[plugin.kind]} · features: ${plugin.features.join(', ')}`
    : `${KIND_LABEL[plugin.kind]} · this bundle declares no CLAP features`;
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
 *
 * # Why there is no button on a row
 * This panel answers "what is installed", which is a different question from
 * "what is this track playing". Placing a plugin needs a *destination*, and the
 * only place a destination exists is the mixer -- so a track's instrument is
 * picked from that track's own dropdown and the master's effect from the
 * master's, and each placed plugin's editor is opened from the strip that owns
 * it. The single "Open GUI" button this panel used to carry meant "the one
 * plugin named by an environment variable", which is no longer a thing that
 * exists; a per-row copy of it would have to invent a destination.
 *
 * Rows are sorted by kind so instruments and effects are told apart at a glance,
 * which is the one question this list is actually asked.
 */
export function ClapBrowserPanel(_props: PanelProps): JSX.Element {
  const app = useAppState();

  // Plain variable, not a signal: it only ever needs to be read *inside* the
  // fetcher, so making it reactive would add a dependency that re-runs nothing.
  // The first load serves the backend's cache; Rescan forces a fresh walk.
  let force = false;
  const [plugins, { refetch }] = createResource(() => app.backend.getClapPlugins(force));

  const count = () => plugins()?.length ?? 0;

  /** Sorted by kind, then by name. Copied first: the resource's array is not ours. */
  const rows = (): ClapPluginInfoDto[] =>
    [...(plugins() ?? [])].sort(
      (a, b) =>
        KIND_ORDER[a.kind] - KIND_ORDER[b.kind] ||
        (a.name || a.id).localeCompare(b.name || b.id),
    );

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
            {/* `<For>` and not `<Index>`: the list is replaced wholesale by a
                rescan and never edited in place, so keying by reference is both
                correct and the cheaper of the two here. */}
            <For each={rows()}>
              {(plugin) => (
                <li class="clap-row" title={featuresTitle(plugin)}>
                  {/* Name and path carry their own titles because both are
                      ellipsised: pointing at a truncated string should show
                      that string, not the row's summary. */}
                  <span class="clap-row__name" title={plugin.name || plugin.id}>
                    {plugin.name || plugin.id}
                  </span>
                  <span class={`clap-row__kind clap-row__kind--${plugin.kind}`}>
                    {KIND_LABEL[plugin.kind]}
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

      <p class="clap-browser__hint">
        Place plugins from the mixer: an instrument from a track's source dropdown, an
        effect from the master's. Each one's editor opens from the strip it sits in.
      </p>

      <footer class="clap-browser__footer">
        <span>
          {count()} {count() === 1 ? 'plugin' : 'plugins'}
        </span>
        <span class="clap-browser__actions">
          <button class="link-button" type="button" disabled={plugins.loading} onClick={rescan}>
            Rescan
          </button>
        </span>
      </footer>
    </div>
  );
}
