import { For, Show, createResource, type JSX } from 'solid-js';
import { useAppState } from '../../state';
import type { Vst3PluginInfoDto, Vst3PluginKind } from '../../api/types';
import type { PanelProps } from '../types';

/** Shown in place of an empty vendor or version field. */
const UNKNOWN = '—';

/**
 * Reading order: the two kinds MYDAW can place come first, in the order the
 * mixer offers them (a track's source, then the master's insert). `unknown`
 * follows, because such a plugin *is* offered as an effect and a user looking
 * for it should not have to scroll past the ones MYDAW cannot use at all.
 */
const KIND_ORDER: Record<Vst3PluginKind, number> = {
  instrument: 0,
  'audio-effect': 1,
  unknown: 2,
  other: 3,
};

/** Badge text. Short enough to sit in a column beside the name. */
const KIND_LABEL: Record<Vst3PluginKind, string> = {
  instrument: 'Instrument',
  'audio-effect': 'Audio FX',
  unknown: 'Unclassified',
  other: 'Other',
};

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/**
 * The evidence behind the badge.
 *
 * A VST3 module that exposes only the older `IPluginFactory` carries no
 * subcategories at all, which is exactly why it lands in `unknown` -- saying so
 * is far more useful than an empty tooltip, because it is also the reason the
 * plugin turns up in the effects picker rather than nowhere.
 */
function subcategoriesTitle(plugin: Vst3PluginInfoDto): string {
  if (plugin.subcategories.length > 0) {
    return `${KIND_LABEL[plugin.kind]} · subcategories: ${plugin.subcategories.join(', ')}`;
  }
  return (
    `${KIND_LABEL[plugin.kind]} · this module declares no subcategories ` +
    '(no IPluginFactory2), so MYDAW offers it as an effect and checks its buses on load'
  );
}

/**
 * Lists the VST3 plugins the host found on this machine.
 *
 * The sibling of `ClapBrowserPanel`, and deliberately the same shape: a list, a
 * kind badge, a rescan. Two differences are worth knowing about, and both are
 * the format's rather than this panel's.
 *
 * # The first scan is genuinely slow, and it gets faster
 * Opening a VST3 module runs a vendor's initialisation, and a licence-protected
 * plugin whose check does not pass can put up a modal dialog and never return.
 * The Rust host gives each module its own thread and a deadline, and **remembers
 * the ones that hung** so later scans skip them. So the first scan on a
 * well-stocked machine can take a while and the second is quick -- which is why
 * "Scanning…" here is a real state and not a formality.
 *
 * # `id` is a class id, not a name
 * VST3 identifies a plugin class by sixteen raw bytes. Shown as hex, and shown
 * only as fallback text when a module declares no name, because thirty-two hex
 * digits is not something to read a list by.
 */
export function Vst3BrowserPanel(_props: PanelProps): JSX.Element {
  const app = useAppState();

  // Plain variable, not a signal: it is only read *inside* the fetcher, so
  // making it reactive would add a dependency that re-runs nothing.
  let force = false;
  const [plugins, { refetch }] = createResource(() => app.backend.getVst3Plugins(force));

  const count = () => plugins()?.length ?? 0;

  /** Sorted by kind, then by name. Copied first: the resource's array is not ours. */
  const rows = (): Vst3PluginInfoDto[] =>
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

      <Show
        when={!plugins.loading}
        fallback={
          <p class="clap-browser__empty">
            Scanning… the first VST3 scan opens every installed module, which can take a
            moment.
          </p>
        }
      >
        <Show
          when={count() > 0}
          fallback={
            <p class="clap-browser__empty">
              No VST3 plugins found. MYDAW looks in the standard install locations and in{' '}
              <code>%VST3_PATH%</code>.
            </p>
          }
        >
          <ul class="clap-list">
            {/* `<For>` and not `<Index>`: the list is replaced wholesale by a
                rescan and never edited in place. */}
            <For each={rows()}>
              {(plugin) => (
                <li class="clap-row" title={subcategoriesTitle(plugin)}>
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
        effect from the master's. The master insert holds one plugin — placing a VST3
        effect replaces a CLAP one, and the other way round.
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
